//! Append-only command journal.
//!
//! Layout in the data directory:
//! - `journal-<first seq, 20 digits>.log`: segments of records
//!   `[len u32][crc32 u32][seq u64][payload]`, little-endian. `crc32` covers seq and payload.
//! - `snap-<seq, 20 digits>.bin`: `[b"OBKS"][seq u64][hash 32][crc32 u32][len u64][state]`,
//!   written to a temp file, fsynced, then renamed.
//!
//! Every record extends a blake3 hash chain, `h_n = blake3(h_{n-1} || seq_n || payload_n)`, with
//! `h_0` all zeros. Settlement batches carry `(seq, h_seq)`, so anyone holding the journal can
//! replay it and check that the posted batch follows from the log.
//!
//! Recovery loads the newest valid snapshot, then replays records with `seq > snapshot seq` in
//! order. A torn or corrupt tail (crash during a write) is truncated; a gap or corruption before
//! the tail is an error.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const SNAP_MAGIC: &[u8; 4] = b"OBKS";
const HEADER: usize = 16;
pub const DEFAULT_SEGMENT_BYTES: u64 = 256 << 20;

pub type Hash = [u8; 32];

pub fn chain(prev: &Hash, seq: u64, payload: &[u8]) -> Hash {
    let mut h = blake3::Hasher::new();
    h.update(prev);
    h.update(&seq.to_le_bytes());
    h.update(payload);
    *h.finalize().as_bytes()
}

fn crc(seq: u64, payload: &[u8]) -> u32 {
    let mut c = crc32fast::Hasher::new();
    c.update(&seq.to_le_bytes());
    c.update(payload);
    c.finalize()
}

fn seg_name(first: u64) -> String {
    format!("journal-{first:020}.log")
}

fn parse_num(name: &str, prefix: &str, suffix: &str) -> Option<u64> {
    name.strip_prefix(prefix)?.strip_suffix(suffix)?.parse().ok()
}

fn list(dir: &Path, prefix: &str, suffix: &str) -> io::Result<Vec<(u64, PathBuf)>> {
    let mut v = vec![];
    for e in fs::read_dir(dir)? {
        let e = e?;
        if let Some(n) = e.file_name().to_str().and_then(|n| parse_num(n, prefix, suffix)) {
            v.push((n, e.path()));
        }
    }
    v.sort();
    Ok(v)
}

fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

/// State recovered from disk.
pub struct Recovery {
    /// (seq, journal hash at seq, state bytes) of the newest valid snapshot.
    pub snapshot: Option<(u64, Hash, Vec<u8>)>,
    /// Records after the snapshot, in order.
    pub records: Vec<(u64, Vec<u8>)>,
    /// Last seq and hash after all records.
    pub last_seq: u64,
    pub last_hash: Hash,
    /// Bytes dropped from a torn tail, if any.
    pub truncated: u64,
}

pub struct Journal {
    dir: PathBuf,
    out: BufWriter<File>,
    seg_bytes: u64,
    max_seg: u64,
    last_seq: u64,
    last_hash: Hash,
    fsync: bool,
}

impl Journal {
    /// Open (creating if needed) and recover. The returned journal appends after `last_seq`.
    pub fn open(dir: impl AsRef<Path>, fsync: bool) -> io::Result<(Journal, Recovery)> {
        let dir = dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir)?;
        let snapshot = Self::newest_snapshot(&dir)?;
        let (mut seq, mut hash) = snapshot.as_ref().map_or((0, [0u8; 32]), |(s, h, _)| (*s, *h));
        let mut records = vec![];
        let mut truncated = 0u64;
        let segs = list(&dir, "journal-", ".log")?;
        let mut tail: Option<(PathBuf, u64)> = None;
        for (i, (_, path)) in segs.iter().enumerate() {
            let last = i + 1 == segs.len();
            let mut buf = vec![];
            File::open(path)?.read_to_end(&mut buf)?;
            let mut off = 0usize;
            while off < buf.len() {
                let rec = Self::parse(&buf[off..]);
                let Some((rseq, payload, n)) = rec else {
                    if !last {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("corrupt record in {} at {off}", path.display()),
                        ));
                    }
                    truncated = (buf.len() - off) as u64;
                    break;
                };
                if rseq > seq {
                    if rseq != seq + 1 {
                        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("journal gap: {seq} -> {rseq}")));
                    }
                    hash = chain(&hash, rseq, payload);
                    seq = rseq;
                    records.push((rseq, payload.to_vec()));
                }
                off += n;
            }
            if last {
                tail = Some((path.clone(), off as u64));
            }
        }
        let out = match tail {
            Some((path, len)) => {
                let f = OpenOptions::new().write(true).open(&path)?;
                f.set_len(len)?;
                let mut f = f;
                f.seek(SeekFrom::End(0))?;
                f.sync_all()?;
                BufWriter::with_capacity(1 << 20, f)
            }
            None => BufWriter::with_capacity(1 << 20, Self::new_segment(&dir, seq + 1)?),
        };
        let seg_bytes = out.get_ref().metadata()?.len();
        let j = Journal {
            dir,
            out,
            seg_bytes,
            max_seg: DEFAULT_SEGMENT_BYTES,
            last_seq: seq,
            last_hash: hash,
            fsync,
        };
        Ok((
            j,
            Recovery {
                snapshot,
                records,
                last_seq: seq,
                last_hash: hash,
                truncated,
            },
        ))
    }

    fn new_segment(dir: &Path, first: u64) -> io::Result<File> {
        let f = OpenOptions::new().create(true).append(true).open(dir.join(seg_name(first)))?;
        sync_dir(dir)?;
        Ok(f)
    }

    fn parse(b: &[u8]) -> Option<(u64, &[u8], usize)> {
        if b.len() < HEADER {
            return None;
        }
        let len = u32::from_le_bytes(b[0..4].try_into().ok()?) as usize;
        let c = u32::from_le_bytes(b[4..8].try_into().ok()?);
        let seq = u64::from_le_bytes(b[8..16].try_into().ok()?);
        let payload = b.get(HEADER..HEADER + len)?;
        (crc(seq, payload) == c).then_some((seq, payload, HEADER + len))
    }

    pub fn set_max_segment_bytes(&mut self, n: u64) {
        self.max_seg = n;
    }

    pub fn last_seq(&self) -> u64 {
        self.last_seq
    }

    pub fn last_hash(&self) -> Hash {
        self.last_hash
    }

    /// Buffer one record. `seq` must be `last_seq + 1`. Not durable until `sync`.
    pub fn append(&mut self, seq: u64, payload: &[u8]) -> io::Result<Hash> {
        assert_eq!(seq, self.last_seq + 1, "journal seq must be contiguous");
        if self.seg_bytes >= self.max_seg {
            self.out.flush()?;
            if self.fsync {
                self.out.get_ref().sync_data()?;
            }
            self.out = BufWriter::with_capacity(1 << 20, Self::new_segment(&self.dir, seq)?);
            self.seg_bytes = 0;
        }
        let len = u32::try_from(payload.len()).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "record too large"))?;
        let mut h = [0u8; HEADER];
        h[0..4].copy_from_slice(&len.to_le_bytes());
        h[4..8].copy_from_slice(&crc(seq, payload).to_le_bytes());
        h[8..16].copy_from_slice(&seq.to_le_bytes());
        self.out.write_all(&h)?;
        self.out.write_all(payload)?;
        self.seg_bytes += (HEADER + payload.len()) as u64;
        self.last_seq = seq;
        self.last_hash = chain(&self.last_hash, seq, payload);
        Ok(self.last_hash)
    }

    /// Flush buffered records and (if enabled) fsync. Everything appended before this call is
    /// durable when it returns.
    pub fn sync(&mut self) -> io::Result<()> {
        self.out.flush()?;
        if self.fsync {
            self.out.get_ref().sync_data()?;
        }
        Ok(())
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Write a snapshot of the state after `seq`, keeping the newest `keep` snapshots.
    pub fn write_snapshot(dir: &Path, seq: u64, hash: &Hash, state: &[u8], keep: usize) -> io::Result<PathBuf> {
        let path = dir.join(format!("snap-{seq:020}.bin"));
        let tmp = dir.join(format!(".snap-{seq:020}.tmp"));
        {
            let mut f = BufWriter::new(File::create(&tmp)?);
            f.write_all(SNAP_MAGIC)?;
            f.write_all(&seq.to_le_bytes())?;
            f.write_all(hash)?;
            f.write_all(&crc32fast::hash(state).to_le_bytes())?;
            f.write_all(&(state.len() as u64).to_le_bytes())?;
            f.write_all(state)?;
            f.flush()?;
            f.get_ref().sync_all()?;
        }
        fs::rename(&tmp, &path)?;
        sync_dir(dir)?;
        let snaps = list(dir, "snap-", ".bin")?;
        if snaps.len() > keep {
            for (_, p) in &snaps[..snaps.len() - keep] {
                let _ = fs::remove_file(p);
            }
        }
        Ok(path)
    }

    fn read_snapshot(path: &Path) -> Option<(u64, Hash, Vec<u8>)> {
        let mut b = vec![];
        File::open(path).ok()?.read_to_end(&mut b).ok()?;
        if b.len() < 56 || &b[0..4] != SNAP_MAGIC {
            return None;
        }
        let seq = u64::from_le_bytes(b[4..12].try_into().ok()?);
        let hash: Hash = b[12..44].try_into().ok()?;
        let c = u32::from_le_bytes(b[44..48].try_into().ok()?);
        let len = u64::from_le_bytes(b[48..56].try_into().ok()?) as usize;
        let state = b.get(56..56 + len)?.to_vec();
        (crc32fast::hash(&state) == c).then_some((seq, hash, state))
    }

    fn newest_snapshot(dir: &Path) -> io::Result<Option<(u64, Hash, Vec<u8>)>> {
        for (_, p) in list(dir, "snap-", ".bin")?.into_iter().rev() {
            if let Some(s) = Self::read_snapshot(&p) {
                return Ok(Some(s));
            }
        }
        Ok(None)
    }

    /// Read every record in the directory from seq 1 (for audits). Requires the full journal.
    pub fn read_all(dir: impl AsRef<Path>) -> io::Result<Vec<(u64, Vec<u8>)>> {
        let mut out = vec![];
        for (_, path) in list(dir.as_ref(), "journal-", ".log")? {
            let mut buf = vec![];
            File::open(&path)?.read_to_end(&mut buf)?;
            let mut off = 0;
            while let Some((seq, p, n)) = Self::parse(&buf[off..]) {
                out.push((seq, p.to_vec()));
                off += n;
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_recover_and_hash_chain() {
        let d = tempfile::tempdir().unwrap();
        let mut want = [0u8; 32];
        {
            let (mut j, r) = Journal::open(d.path(), true).unwrap();
            assert_eq!(r.last_seq, 0);
            for i in 1..=100u64 {
                let p = format!("cmd-{i}");
                want = j.append(i, p.as_bytes()).unwrap();
            }
            j.sync().unwrap();
        }
        let (j, r) = Journal::open(d.path(), true).unwrap();
        assert_eq!(r.records.len(), 100);
        assert_eq!(r.last_seq, 100);
        assert_eq!(r.last_hash, want);
        assert_eq!(j.last_hash(), want);
        assert_eq!(r.records[41].1, b"cmd-42");
    }

    #[test]
    fn torn_tail_is_truncated_and_appends_continue() {
        let d = tempfile::tempdir().unwrap();
        {
            let (mut j, _) = Journal::open(d.path(), true).unwrap();
            for i in 1..=10u64 {
                j.append(i, &[i as u8; 40]).unwrap();
            }
            j.sync().unwrap();
        }
        // Chop the last record in half.
        let seg = d.path().join(seg_name(1));
        let len = fs::metadata(&seg).unwrap().len();
        OpenOptions::new().write(true).open(&seg).unwrap().set_len(len - 20).unwrap();
        let (mut j, r) = Journal::open(d.path(), true).unwrap();
        assert_eq!(r.last_seq, 9);
        assert_eq!(r.truncated, 36);
        j.append(10, b"again").unwrap();
        j.sync().unwrap();
        drop(j);
        let (_, r) = Journal::open(d.path(), true).unwrap();
        assert_eq!(r.last_seq, 10);
        assert_eq!(r.records[9].1, b"again");
    }

    #[test]
    fn snapshot_then_tail_replay_and_segment_roll() {
        let d = tempfile::tempdir().unwrap();
        let (mut j, _) = Journal::open(d.path(), true).unwrap();
        j.set_max_segment_bytes(200);
        for i in 1..=50u64 {
            j.append(i, &i.to_le_bytes()).unwrap();
            if i == 30 {
                j.sync().unwrap();
                Journal::write_snapshot(d.path(), 30, &j.last_hash(), b"state@30", 2).unwrap();
            }
        }
        j.sync().unwrap();
        let full_hash = j.last_hash();
        drop(j);
        assert!(list(d.path(), "journal-", ".log").unwrap().len() > 3);
        let (_, r) = Journal::open(d.path(), true).unwrap();
        let (s, _, st) = r.snapshot.unwrap();
        assert_eq!((s, st.as_slice()), (30, &b"state@30"[..]));
        assert_eq!(r.records.first().unwrap().0, 31);
        assert_eq!(r.records.len(), 20);
        assert_eq!(r.last_hash, full_hash);
        assert_eq!(Journal::read_all(d.path()).unwrap().len(), 50);
    }

    #[test]
    fn corrupt_snapshot_falls_back_to_older() {
        let d = tempfile::tempdir().unwrap();
        let (mut j, _) = Journal::open(d.path(), true).unwrap();
        for i in 1..=20u64 {
            j.append(i, b"x").unwrap();
            if i == 10 || i == 20 {
                j.sync().unwrap();
                Journal::write_snapshot(d.path(), i, &j.last_hash(), format!("s{i}").as_bytes(), 5).unwrap();
            }
        }
        j.sync().unwrap();
        drop(j);
        let p = d.path().join(format!("snap-{:020}.bin", 20));
        let mut b = fs::read(&p).unwrap();
        let n = b.len();
        b[n - 1] ^= 0xff;
        fs::write(&p, b).unwrap();
        let (_, r) = Journal::open(d.path(), true).unwrap();
        assert_eq!(r.snapshot.unwrap().0, 10);
        assert_eq!(r.records.len(), 10);
    }
}
