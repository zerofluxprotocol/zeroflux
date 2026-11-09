//! Encrypted, authenticated peer transport ("ZFXP2P/2").
//!
//! Handshake (initiator I, responder R), all messages length-prefixed:
//!   H1  I->R  magic | chain_tag(32) | x25519_eph_I(32) | mlkem768_ek_I(1184) | nonce_I(32)
//!   H2  R->I  x25519_eph_R(32) | mlkem768_ct(1088) | nonce_R(32) | mldsa65_pk_R(1952) | sig_R
//!           sig_R = ML-DSA-65(R, ctx "ZFX-p2p-hs-R", SHA256(H1 | H2-without-sig))
//!   H3  I->R  mldsa65_pk_I(1952) | sig_I
//!           sig_I = ML-DSA-65(I, ctx "ZFX-p2p-hs-I", SHA256(H1 | H2 | pk_I))
//! Keys: HKDF-SHA256(salt = SHA256(H1|H2|H3), ikm = ss_mlkem | ss_x25519),
//! info "ZFX-p2p-v2 i2r" / "r2i". Hybrid: an attacker must break both
//! ML-KEM-768 (FIPS 203) and X25519 to read traffic. Ephemeral keys on both
//! sides give forward secrecy. Identities are ML-DSA-65 (FIPS 204);
//! peer_id = SHA256(identity public key).
//! Frames: u32 BE length | ChaCha20-Poly1305(key_dir, nonce = 0^4 | ctr_be64).
//! A frame that fails authentication, arrives out of order or exceeds the
//! size cap terminates the connection.

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use fips203::ml_kem_768;
use fips203::traits::{Decaps, Encaps, KeyGen as KemKeyGen, SerDes as KemSerDes};
use fips204::ml_dsa_65;
use fips204::traits::{KeyGen as DsaKeyGen, SerDes as DsaSerDes, Signer, Verifier};
use hkdf::Hkdf;
use rand_core::{OsRng, RngCore};
use sha2::Sha256;
use std::io::{Read, Write};
use std::net::TcpStream;
use zfx_verify::hash::{sha256, Hash32};

pub const MAGIC: &[u8; 8] = b"ZFXP2P/2";
pub const MAX_FRAME: usize = 16 << 20;
const MAX_HANDSHAKE_MSG: usize = 16 << 10;
const HS_CTX_R: &[u8] = b"ZFX-p2p-hs-R";
const HS_CTX_I: &[u8] = b"ZFX-p2p-hs-I";

pub struct Identity {
    pub pk: ml_dsa_65::PublicKey,
    sk: ml_dsa_65::PrivateKey,
    pub peer_id: Hash32,
}

impl Identity {
    pub fn from_seed(seed: &[u8; 32]) -> Identity {
        let xi = sha256(&[b"ZFX-node-identity-v1", seed]);
        let (pk, sk) = ml_dsa_65::KG::keygen_from_seed(&xi);
        let peer_id = sha256(&[&pk.clone().into_bytes()]);
        Identity { pk, sk, peer_id }
    }

    /// Load `node_key` (32-byte seed, hex) from `path`, creating it 0600.
    pub fn load_or_create(path: &std::path::Path) -> Result<Identity, String> {
        let seed: [u8; 32] = match std::fs::read_to_string(path) {
            Ok(s) => hex::decode(s.trim()).ok().and_then(|v| v.try_into().ok()).ok_or("node_key file is corrupt")?,
            Err(_) => {
                let mut s = [0u8; 32];
                OsRng.fill_bytes(&mut s);
                std::fs::write(path, hex::encode(s)).map_err(|e| e.to_string())?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
                }
                s
            }
        };
        Ok(Identity::from_seed(&seed))
    }
}

fn write_raw(s: &mut TcpStream, b: &[u8]) -> std::io::Result<()> {
    s.write_all(&(b.len() as u32).to_be_bytes())?;
    s.write_all(b)?;
    s.flush()
}

fn read_raw(s: &mut TcpStream, max: usize) -> Result<Vec<u8>, String> {
    let mut l = [0u8; 4];
    s.read_exact(&mut l).map_err(|e| e.to_string())?;
    let n = u32::from_be_bytes(l) as usize;
    if n > max {
        return Err(format!("frame of {} bytes exceeds cap {}", n, max));
    }
    let mut b = vec![0u8; n];
    s.read_exact(&mut b).map_err(|e| e.to_string())?;
    Ok(b)
}

pub struct SecureWriter {
    stream: TcpStream,
    cipher: ChaCha20Poly1305,
    ctr: u64,
}

pub struct SecureReader {
    stream: TcpStream,
    cipher: ChaCha20Poly1305,
    ctr: u64,
}

fn nonce(ctr: u64) -> Nonce {
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(&ctr.to_be_bytes());
    *Nonce::from_slice(&n)
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Session(peer {})", hex::encode(&self.peer_id[..8]))
    }
}

impl SecureWriter {
    pub fn send(&mut self, plaintext: &[u8]) -> Result<(), String> {
        if plaintext.len() + 16 > MAX_FRAME {
            return Err("message too large".into());
        }
        let ct = self.cipher.encrypt(&nonce(self.ctr), plaintext).map_err(|_| "encrypt failed".to_string())?;
        self.ctr += 1;
        write_raw(&mut self.stream, &ct).map_err(|e| e.to_string())
    }
    pub fn shutdown(&self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

impl SecureReader {
    /// Next authenticated plaintext frame; any error is fatal for the link.
    pub fn recv(&mut self) -> Result<Vec<u8>, String> {
        let ct = read_raw(&mut self.stream, MAX_FRAME)?;
        let pt = self.cipher.decrypt(&nonce(self.ctr), ct.as_ref()).map_err(|_| "frame authentication failed".to_string())?;
        self.ctr += 1;
        Ok(pt)
    }
}

pub struct Session {
    pub writer: SecureWriter,
    pub reader: SecureReader,
    pub peer_id: Hash32,
}

fn derive(transcript: &Hash32, ss_kem: &[u8], ss_dh: &[u8], initiator: bool) -> (ChaCha20Poly1305, ChaCha20Poly1305) {
    let mut ikm = ss_kem.to_vec();
    ikm.extend_from_slice(ss_dh);
    let hk = Hkdf::<Sha256>::new(Some(transcript), &ikm);
    let mut i2r = [0u8; 32];
    let mut r2i = [0u8; 32];
    hk.expand(b"ZFX-p2p-v2 i2r", &mut i2r).unwrap();
    hk.expand(b"ZFX-p2p-v2 r2i", &mut r2i).unwrap();
    let (tx, rx) = if initiator { (i2r, r2i) } else { (r2i, i2r) };
    (ChaCha20Poly1305::new(Key::from_slice(&tx)), ChaCha20Poly1305::new(Key::from_slice(&rx)))
}

fn split(stream: TcpStream, tx: ChaCha20Poly1305, rx: ChaCha20Poly1305, peer_id: Hash32) -> Result<Session, String> {
    let r = stream.try_clone().map_err(|e| e.to_string())?;
    Ok(Session { writer: SecureWriter { stream, cipher: tx, ctr: 0 }, reader: SecureReader { stream: r, cipher: rx, ctr: 0 }, peer_id })
}

fn check_allowed(peer_id: &Hash32, allow: Option<&[Hash32]>) -> Result<(), String> {
    match allow {
        Some(list) if !list.contains(peer_id) => Err(format!("peer {} not in allowlist", hex::encode(&peer_id[..8]))),
        _ => Ok(()),
    }
}

pub fn handshake_initiator(mut s: TcpStream, me: &Identity, chain_tag: &Hash32, allow: Option<&[Hash32]>) -> Result<Session, String> {
    s.set_read_timeout(Some(std::time::Duration::from_secs(10))).ok();
    let x_sk = x25519_dalek::StaticSecret::random_from_rng(OsRng);
    let x_pk = x25519_dalek::PublicKey::from(&x_sk);
    let (ek, dk) = ml_kem_768::KG::try_keygen().map_err(|e| e.to_string())?;
    let mut n_i = [0u8; 32];
    OsRng.fill_bytes(&mut n_i);
    let mut h1 = MAGIC.to_vec();
    h1.extend_from_slice(chain_tag);
    h1.extend_from_slice(x_pk.as_bytes());
    h1.extend_from_slice(&ek.into_bytes());
    h1.extend_from_slice(&n_i);
    write_raw(&mut s, &h1).map_err(|e| e.to_string())?;
    let h2 = read_raw(&mut s, MAX_HANDSHAKE_MSG)?;
    let (pk_len, sig_len) = (ml_dsa_65::PK_LEN, ml_dsa_65::SIG_LEN);
    let need = 32 + ml_kem_768::CT_LEN + 32 + pk_len + sig_len;
    if h2.len() != need {
        return Err("bad H2 length".into());
    }
    let r_x: [u8; 32] = h2[0..32].try_into().unwrap();
    let ct = ml_kem_768::CipherText::try_from_bytes(h2[32..32 + ml_kem_768::CT_LEN].try_into().unwrap()).map_err(|e| e.to_string())?;
    let o = 32 + ml_kem_768::CT_LEN + 32;
    let r_pk_bytes: [u8; ml_dsa_65::PK_LEN] = h2[o..o + pk_len].try_into().unwrap();
    let r_sig: [u8; ml_dsa_65::SIG_LEN] = h2[o + pk_len..].try_into().unwrap();
    let r_pk = ml_dsa_65::PublicKey::try_from_bytes(r_pk_bytes).map_err(|e| e.to_string())?;
    let th2 = sha256(&[&h1, &h2[..o + pk_len]]);
    if !r_pk.verify(&th2, &r_sig, HS_CTX_R) {
        return Err("responder handshake signature invalid".into());
    }
    let r_id = sha256(&[&r_pk_bytes]);
    if r_id == me.peer_id {
        return Err("connected to self".into());
    }
    check_allowed(&r_id, allow)?;
    let ss_kem = dk.try_decaps(&ct).map_err(|e| e.to_string())?.into_bytes();
    let ss_dh = x_sk.diffie_hellman(&x25519_dalek::PublicKey::from(r_x));
    let my_pk = me.pk.clone().into_bytes();
    let sig_i = me.sk.try_sign(&sha256(&[&th2, &my_pk]), HS_CTX_I).map_err(|e| e.to_string())?;
    let mut h3 = my_pk.to_vec();
    h3.extend_from_slice(&sig_i);
    write_raw(&mut s, &h3).map_err(|e| e.to_string())?;
    let transcript = sha256(&[&h1, &h2, &h3]);
    let (tx, rx) = derive(&transcript, &ss_kem, ss_dh.as_bytes(), true);
    s.set_read_timeout(None).ok();
    split(s, tx, rx, r_id)
}

pub fn handshake_responder(mut s: TcpStream, me: &Identity, chain_tag: &Hash32, allow: Option<&[Hash32]>) -> Result<Session, String> {
    s.set_read_timeout(Some(std::time::Duration::from_secs(10))).ok();
    let h1 = read_raw(&mut s, MAX_HANDSHAKE_MSG)?;
    if h1.len() != 8 + 32 + 32 + ml_kem_768::EK_LEN + 32 || &h1[..8] != MAGIC {
        return Err("bad H1 (not a ZFXP2P/2 peer)".into());
    }
    if &h1[8..40] != chain_tag {
        return Err("peer is on a different chain (genesis mismatch)".into());
    }
    let i_x: [u8; 32] = h1[40..72].try_into().unwrap();
    let ek = ml_kem_768::EncapsKey::try_from_bytes(h1[72..72 + ml_kem_768::EK_LEN].try_into().unwrap()).map_err(|e| e.to_string())?;
    let (ss, ct) = ek.try_encaps().map_err(|e| e.to_string())?;
    let x_sk = x25519_dalek::StaticSecret::random_from_rng(OsRng);
    let x_pk = x25519_dalek::PublicKey::from(&x_sk);
    let mut n_r = [0u8; 32];
    OsRng.fill_bytes(&mut n_r);
    let my_pk = me.pk.clone().into_bytes();
    let mut h2 = x_pk.as_bytes().to_vec();
    h2.extend_from_slice(&ct.into_bytes());
    h2.extend_from_slice(&n_r);
    h2.extend_from_slice(&my_pk);
    let th2 = sha256(&[&h1, &h2]);
    let sig = me.sk.try_sign(&th2, HS_CTX_R).map_err(|e| e.to_string())?;
    h2.extend_from_slice(&sig);
    write_raw(&mut s, &h2).map_err(|e| e.to_string())?;
    let h3 = read_raw(&mut s, MAX_HANDSHAKE_MSG)?;
    if h3.len() != ml_dsa_65::PK_LEN + ml_dsa_65::SIG_LEN {
        return Err("bad H3 length".into());
    }
    let i_pk_bytes: [u8; ml_dsa_65::PK_LEN] = h3[..ml_dsa_65::PK_LEN].try_into().unwrap();
    let i_sig: [u8; ml_dsa_65::SIG_LEN] = h3[ml_dsa_65::PK_LEN..].try_into().unwrap();
    let i_pk = ml_dsa_65::PublicKey::try_from_bytes(i_pk_bytes).map_err(|e| e.to_string())?;
    if !i_pk.verify(&sha256(&[&th2, &i_pk_bytes]), &i_sig, HS_CTX_I) {
        return Err("initiator handshake signature invalid".into());
    }
    let i_id = sha256(&[&i_pk_bytes]);
    if i_id == me.peer_id {
        return Err("connected to self".into());
    }
    check_allowed(&i_id, allow)?;
    let ss_dh = x_sk.diffie_hellman(&x25519_dalek::PublicKey::from(i_x));
    let transcript = sha256(&[&h1, &h2, &h3]);
    let (tx, rx) = derive(&transcript, &ss.into_bytes(), ss_dh.as_bytes(), false);
    s.set_read_timeout(None).ok();
    split(s, tx, rx, i_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn pair(tag_r: Hash32, allow_r: Option<Vec<Hash32>>) -> (Result<Session, String>, Result<Session, String>, Hash32, Hash32) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let a = Identity::from_seed(&[1; 32]);
        let b = Identity::from_seed(&[2; 32]);
        let (aid, bid) = (a.peer_id, b.peer_id);
        let t = std::thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            handshake_responder(s, &b, &tag_r, allow_r.as_deref())
        });
        let c = handshake_initiator(TcpStream::connect(addr).unwrap(), &a, &[7; 32], None);
        (c, t.join().unwrap(), aid, bid)
    }

    #[test]
    fn handshake_and_encrypted_roundtrip() {
        let (c, r, aid, bid) = pair([7; 32], None);
        let (mut c, mut r) = (c.unwrap(), r.unwrap());
        assert_eq!(c.peer_id, bid);
        assert_eq!(r.peer_id, aid);
        for i in 0..50u32 {
            let m = format!("{{\"type\":\"ping\",\"i\":{}}}", i);
            c.writer.send(m.as_bytes()).unwrap();
            assert_eq!(r.reader.recv().unwrap(), m.as_bytes());
            r.writer.send(b"pong").unwrap();
            assert_eq!(c.reader.recv().unwrap(), b"pong");
        }
    }

    #[test]
    fn wrong_chain_and_allowlist_rejected() {
        let (_c, r, _, _) = pair([8; 32], None);
        assert!(r.unwrap_err().contains("different chain"));
        let (_c, r, _, _) = pair([7; 32], Some(vec![[0; 32]]));
        assert!(r.unwrap_err().contains("allowlist"));
    }

    #[test]
    fn tampered_frame_is_fatal() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let t = std::thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            let mut sess = handshake_responder(s, &Identity::from_seed(&[4; 32]), &[7; 32], None).unwrap();
            sess.reader.recv()
        });
        let mut s = TcpStream::connect(addr).unwrap();
        let raw = s.try_clone().unwrap();
        let sess = handshake_initiator(raw, &Identity::from_seed(&[3; 32]), &[7; 32], None).unwrap();
        // Write a frame encrypted under the right key but with a flipped bit.
        let mut w = sess.writer;
        let ct = w.cipher.encrypt(&nonce(0), b"hello".as_ref()).unwrap();
        let mut bad = ct.clone();
        bad[0] ^= 1;
        write_raw(&mut s, &bad).unwrap();
        assert!(t.join().unwrap().unwrap_err().contains("authentication"));
    }
}
