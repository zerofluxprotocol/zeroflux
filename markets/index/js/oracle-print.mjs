// Reference OraclePrint v1 codec for JavaScript (Node 18+ or browsers with
// WebCrypto Ed25519). No dependencies. Integers are BigInt; never JS numbers.
// Spec: markets/index/docs/ORACLE_PRINT_V1.md. Vectors: markets/index/vectors/oracle_print_v1.json.

export const DOMAIN = new TextEncoder().encode('ZFX-oracle-print-v1');
export const MAX_STR_LEN = 256;
export const ORACLE_STATUS = ['FRESH', 'STALE', 'DISPUTED', 'UNAVAILABLE'];
export const DATA_STATUS = ['OK', 'NOT_REPORTED', 'UNPRICED', 'NO_VALID_CLOSE_PRICE'];
export const FINALITY = ['PROVISIONAL', 'FINAL', 'CORRECTED'];

const enc = new TextEncoder();
const dec = new TextDecoder('utf-8', { fatal: true });

export function hexToBytes(h) {
  if (h.length % 2) throw new Error('odd hex');
  const b = new Uint8Array(h.length / 2);
  for (let i = 0; i < b.length; i++) b[i] = parseInt(h.substr(i * 2, 2), 16);
  return b;
}
export function bytesToHex(b) {
  return Array.from(b, (x) => x.toString(16).padStart(2, '0')).join('');
}

/** p: {indexId, version, valueScale, value: bigint, sourceStartMs: bigint, ..., sourceHash: Uint8Array(32), ...} */
export function encode(p) {
  const parts = [DOMAIN];
  const str = (s) => {
    const b = enc.encode(s);
    if (b.length > MAX_STR_LEN) throw new Error('string too long');
    const l = new Uint8Array(4);
    new DataView(l.buffer).setUint32(0, b.length, true);
    parts.push(l, b);
  };
  const u8 = (x) => parts.push(Uint8Array.of(x));
  const u32 = (x) => { const b = new Uint8Array(4); new DataView(b.buffer).setUint32(0, x, true); parts.push(b); };
  const u64 = (x) => { const b = new Uint8Array(8); new DataView(b.buffer).setBigUint64(0, BigInt(x), true); parts.push(b); };
  const i64 = (x) => { const b = new Uint8Array(8); new DataView(b.buffer).setBigInt64(0, BigInt(x), true); parts.push(b); };
  const b32 = (x) => { if (x.length !== 32) throw new Error('need 32 bytes'); parts.push(x); };
  str(p.indexId); str(p.version); u8(p.valueScale); u64(p.value);
  i64(p.sourceStartMs); i64(p.sourceEndMs); i64(p.observedAtMs); i64(p.publishedAtMs); i64(p.validUntilMs);
  b32(p.sourceHash); b32(p.formulaHash);
  u8(ORACLE_STATUS.indexOf(p.status)); u8(DATA_STATUS.indexOf(p.dataStatus)); u8(FINALITY.indexOf(p.finality));
  u32(p.revision); b32(p.prevPrintHash);
  const out = new Uint8Array(parts.reduce((n, x) => n + x.length, 0));
  let o = 0;
  for (const x of parts) { out.set(x, o); o += x.length; }
  return out;
}

export function decode(b) {
  let i = 0;
  const dv = new DataView(b.buffer, b.byteOffset, b.byteLength);
  const take = (n) => { if (i + n > b.length) throw new Error('Truncated'); const s = b.subarray(i, i + n); i += n; return s; };
  const dom = take(DOMAIN.length);
  if (!dom.every((x, k) => x === DOMAIN[k])) throw new Error('BadDomain');
  const u32 = () => { take(4); return dv.getUint32(i - 4, true); };
  const str = () => { const n = u32(); if (n > MAX_STR_LEN) throw new Error('StringTooLong'); return dec.decode(take(n)); };
  const u8 = () => take(1)[0];
  const u64 = () => { take(8); return dv.getBigUint64(i - 8, true); };
  const i64 = () => { take(8); return dv.getBigInt64(i - 8, true); };
  const en = (tbl) => { const x = u8(); if (x >= tbl.length) throw new Error('BadEnum'); return tbl[x]; };
  const p = {
    indexId: str(), version: str(), valueScale: u8(), value: u64(),
    sourceStartMs: i64(), sourceEndMs: i64(), observedAtMs: i64(), publishedAtMs: i64(), validUntilMs: i64(),
    sourceHash: take(32).slice(), formulaHash: take(32).slice(),
    status: en(ORACLE_STATUS), dataStatus: en(DATA_STATUS), finality: en(FINALITY),
    revision: u32(), prevPrintHash: take(32).slice(),
  };
  if (i !== b.length) throw new Error('TrailingBytes');
  return p;
}

/** Formats a scaled integer value as a decimal string, e.g. (42500000n, 6) -> "42.500000". */
export function formatValue(value, scale) {
  const s = value.toString().padStart(scale + 1, '0');
  return scale === 0 ? s : `${s.slice(0, -scale)}.${s.slice(-scale)}`;
}

export async function sha256(bytes) {
  const d = await globalThis.crypto.subtle.digest('SHA-256', bytes);
  return new Uint8Array(d);
}

/** Ed25519 verification of the canonical bytes (WebCrypto, available in Node 18.4+ and modern browsers). */
export async function verify(canonical, pubkey32, sig64) {
  const key = await globalThis.crypto.subtle.importKey('raw', pubkey32, { name: 'Ed25519' }, false, ['verify']);
  return globalThis.crypto.subtle.verify({ name: 'Ed25519' }, key, sig64, canonical);
}

/** Verifies a published print object ({canonicalHex, printHash, reporterSignatures}). */
export async function verifyPublishedPrint(print, trustedPubkeysHex) {
  const bytes = hexToBytes(print.canonicalHex);
  const decoded = decode(bytes);
  if (bytesToHex(await sha256(bytes)) !== print.printHash) return { ok: false, reason: 'hash mismatch' };
  for (const s of print.reporterSignatures) {
    const [scheme, pk, sig] = s.split(':');
    if (scheme !== 'ed25519' || !trustedPubkeysHex.includes(pk)) continue;
    if (await verify(bytes, hexToBytes(pk), hexToBytes(sig))) return { ok: true, decoded };
  }
  return { ok: false, reason: 'no valid signature from a trusted reporter' };
}
