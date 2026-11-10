/**
 * OraclePrint v1 codec (`ZFX-oracle-print-v1`), mirroring `crates/zfx-perps-clearing/src/print.rs`.
 *
 * Layout (integers little-endian, strings u32-LE length + UTF-8, at most 256 bytes):
 *   DOMAIN | index_id str | version str | value_scale u8 | value u64
 *   source_start_ms i64 | source_end_ms i64 | observed_at_ms i64 | published_at_ms i64
 *   valid_until_ms i64 | source_hash [32] | formula_hash [32] | status u8 | data_status u8
 *   finality u8 | revision u32 | prev_print_hash [32]
 *
 * Reporters sign the canonical bytes (ed25519, no pre-hash). `printHash = sha256(bytes)`.
 */
import { ed25519 } from "@noble/curves/ed25519";
import { bytesToHex, hexToBytes } from "@noble/hashes/utils";
import { base58 } from "@scure/base";
import type { DataStatus, Finality, Market, OraclePrint, OracleStatus, OracleView, PrintSignature } from "./types.js";
import { sha256Hex, utf8 } from "./wire.js";

export const PRINT_DOMAIN = "ZFX-oracle-print-v1";
export const PRINT_MAX_STR_LEN = 256;

export const ORACLE_STATUSES: readonly OracleStatus[] = ["FRESH", "STALE", "DISPUTED", "UNAVAILABLE"];
export const DATA_STATUSES: readonly DataStatus[] = ["OK", "NOT_REPORTED", "UNPRICED", "NO_VALID_CLOSE_PRICE"];
export const FINALITIES: readonly Finality[] = ["PROVISIONAL", "FINAL", "CORRECTED"];

export type PrintCodecErrorCode = "BAD_DOMAIN" | "TRUNCATED" | "STRING_TOO_LONG" | "BAD_UTF8" | "BAD_ENUM" | "TRAILING_BYTES" | "OUT_OF_RANGE" | "BAD_HEX";

export class PrintCodecError extends Error {
  constructor(readonly code: PrintCodecErrorCode, message: string = code) {
    super(message);
    this.name = "PrintCodecError";
  }
}

const DOMAIN_BYTES = utf8(PRINT_DOMAIN);
const I64_MIN = -(1n << 63n);
const I64_MAX = (1n << 63n) - 1n;
const U64_MAX = (1n << 64n) - 1n;

function hex32(h: string, what: string): Uint8Array {
  if (!/^[0-9a-f]{64}$/.test(h)) throw new PrintCodecError("BAD_HEX", `${what}: 64 lower-case hex chars`);
  return hexToBytes(h);
}

/** Canonical bytes of a print. Throws `PrintCodecError` on oversize strings or out-of-range numbers. */
export function encodePrint(p: OraclePrint): Uint8Array {
  const parts: Uint8Array[] = [DOMAIN_BYTES];
  const str = (s: string) => {
    const b = utf8(s);
    if (b.length > PRINT_MAX_STR_LEN) throw new PrintCodecError("STRING_TOO_LONG");
    const n = new Uint8Array(4);
    new DataView(n.buffer).setUint32(0, b.length, true);
    parts.push(n, b);
  };
  const fixed = (len: number, f: (d: DataView) => void) => {
    const b = new Uint8Array(len);
    f(new DataView(b.buffer));
    parts.push(b);
  };
  const enumByte = <T extends string>(v: T, all: readonly T[]) => {
    const i = all.indexOf(v);
    if (i < 0) throw new PrintCodecError("BAD_ENUM", `unknown enum value ${v}`);
    fixed(1, (d) => d.setUint8(0, i));
  };
  const range = (v: bigint, lo: bigint, hi: bigint, what: string) => {
    if (v < lo || v > hi) throw new PrintCodecError("OUT_OF_RANGE", `${what} out of range`);
    return v;
  };

  str(p.indexId);
  str(p.version);
  if (!Number.isInteger(p.valueScale) || p.valueScale < 0 || p.valueScale > 255) throw new PrintCodecError("OUT_OF_RANGE", "valueScale");
  fixed(1, (d) => d.setUint8(0, p.valueScale));
  fixed(8, (d) => d.setBigUint64(0, range(p.value, 0n, U64_MAX, "value"), true));
  for (const [k, t] of [
    ["sourceStartMs", p.sourceStartMs],
    ["sourceEndMs", p.sourceEndMs],
    ["observedAtMs", p.observedAtMs],
    ["publishedAtMs", p.publishedAtMs],
    ["validUntilMs", p.validUntilMs],
  ] as const) {
    fixed(8, (d) => d.setBigInt64(0, range(t, I64_MIN, I64_MAX, k), true));
  }
  parts.push(hex32(p.sourceHash, "sourceHash"), hex32(p.formulaHash, "formulaHash"));
  enumByte(p.status, ORACLE_STATUSES);
  enumByte(p.dataStatus, DATA_STATUSES);
  enumByte(p.finality, FINALITIES);
  if (!Number.isInteger(p.revision) || p.revision < 0 || p.revision > 0xffff_ffff) throw new PrintCodecError("OUT_OF_RANGE", "revision");
  fixed(4, (d) => d.setUint32(0, p.revision, true));
  parts.push(hex32(p.prevPrintHash, "prevPrintHash"));

  const out = new Uint8Array(parts.reduce((n, x) => n + x.length, 0));
  let i = 0;
  for (const x of parts) {
    out.set(x, i);
    i += x.length;
  }
  return out;
}

/** Strict decode: wrong domain, unknown enum values, oversize strings, bad UTF-8 and trailing bytes throw. */
export function decodePrint(b: Uint8Array): OraclePrint {
  let i = 0;
  const dv = new DataView(b.buffer, b.byteOffset, b.byteLength);
  const take = (n: number) => {
    if (i + n > b.length) throw new PrintCodecError("TRUNCATED");
    const s = b.subarray(i, i + n);
    i += n;
    return s;
  };
  const at = (n: number) => {
    take(n);
    return i - n;
  };
  const u8 = () => dv.getUint8(at(1));
  const u32 = () => dv.getUint32(at(4), true);
  const u64 = () => dv.getBigUint64(at(8), true);
  const i64 = () => dv.getBigInt64(at(8), true);
  const h32 = () => bytesToHex(take(32));
  const str = () => {
    const n = u32();
    if (n > PRINT_MAX_STR_LEN) throw new PrintCodecError("STRING_TOO_LONG");
    const raw = take(n);
    try {
      return new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(raw);
    } catch {
      throw new PrintCodecError("BAD_UTF8");
    }
  };
  const en = <T>(all: readonly T[]) => {
    const v = all[u8()];
    if (v === undefined) throw new PrintCodecError("BAD_ENUM");
    return v;
  };

  const d = take(DOMAIN_BYTES.length);
  if (!d.every((x, k) => x === DOMAIN_BYTES[k])) throw new PrintCodecError("BAD_DOMAIN");
  const p: OraclePrint = {
    indexId: str(),
    version: str(),
    valueScale: u8(),
    value: u64(),
    sourceStartMs: i64(),
    sourceEndMs: i64(),
    observedAtMs: i64(),
    publishedAtMs: i64(),
    validUntilMs: i64(),
    sourceHash: h32(),
    formulaHash: h32(),
    status: en(ORACLE_STATUSES),
    dataStatus: en(DATA_STATUSES),
    finality: en(FINALITIES),
    revision: u32(),
    prevPrintHash: h32(),
  };
  if (i !== b.length) throw new PrintCodecError("TRAILING_BYTES");
  return p;
}

/** sha256 of the canonical bytes, lower-case hex. */
export function printHash(bytes: Uint8Array): string {
  return sha256Hex(bytes);
}

/** FRESH, data OK and a positive value: the only kind of print that can move the mark. */
export const printUsable = (p: OraclePrint) => p.status === "FRESH" && p.dataStatus === "OK" && p.value > 0n;

/** Raw 32-byte ed25519 key from `ed25519:<base58>` or 64 hex chars; null if malformed. */
export function publicKeyBytes(pk: string): Uint8Array | null {
  try {
    if (pk.startsWith("ed25519:")) {
      const b = base58.decode(pk.slice(8));
      return b.length === 32 ? b : null;
    }
    if (/^[0-9a-fA-F]{64}$/.test(pk)) return hexToBytes(pk.toLowerCase());
  } catch {
    /* malformed */
  }
  return null;
}

export interface PrintSignatureInput {
  publicKey?: string;
  public_key?: string;
  signatureHex?: string;
  signature_hex?: string;
}

export type SignatureRejection = "NOT_ALLOWLISTED" | "DUPLICATE_SIGNER" | "BAD_SIGNATURE" | "MALFORMED";

export interface PrintVerification {
  ok: boolean;
  /** Distinct allowlisted reporters (hex public keys) whose signature verified. */
  signers: string[];
  quorum: number;
  rejected: Array<{ publicKey: string; reason: SignatureRejection }>;
}

/**
 * Count distinct allowlisted reporters with a valid ed25519 signature over the canonical bytes,
 * like the clearing ledger does. `ok` requires `quorum > 0` and at least `quorum` such reporters.
 * Duplicate signers count once; keys outside `reporters` never count.
 */
export function verifyPrint(bytes: Uint8Array, signatures: ReadonlyArray<PrintSignature | PrintSignatureInput>, reporters: readonly string[], quorum: number): PrintVerification {
  const allow = new Set<string>();
  for (const r of reporters) {
    const k = publicKeyBytes(r);
    if (k) allow.add(bytesToHex(k));
  }
  const signers = new Set<string>();
  const rejected: PrintVerification["rejected"] = [];
  for (const s of signatures) {
    const raw = s as PrintSignatureInput;
    const pkStr = raw.publicKey ?? raw.public_key ?? "";
    const sigHex = raw.signatureHex ?? raw.signature_hex ?? "";
    const pk = publicKeyBytes(pkStr);
    if (!pk || !/^[0-9a-f]{128}$/.test(sigHex)) {
      rejected.push({ publicKey: pkStr, reason: "MALFORMED" });
      continue;
    }
    const pkHex = bytesToHex(pk);
    if (!allow.has(pkHex)) {
      rejected.push({ publicKey: pkStr, reason: "NOT_ALLOWLISTED" });
      continue;
    }
    if (signers.has(pkHex)) {
      rejected.push({ publicKey: pkStr, reason: "DUPLICATE_SIGNER" });
      continue;
    }
    let good = false;
    try {
      good = ed25519.verify(hexToBytes(sigHex), bytes, pk);
    } catch {
      good = false;
    }
    if (!good) {
      rejected.push({ publicKey: pkStr, reason: "BAD_SIGNATURE" });
      continue;
    }
    signers.add(pkHex);
  }
  return { ok: quorum > 0 && signers.size >= quorum, signers: [...signers], quorum, rejected };
}

export type OracleProblem =
  | "NO_PRINT"
  | "DECODE_FAILED"
  | "HASH_MISMATCH"
  | "FIELD_MISMATCH"
  | "INDEX_MISMATCH"
  | "SCALE_MISMATCH"
  | "QUORUM_NOT_MET"
  | "NOT_USABLE"
  | "EXPIRED"
  | "TOO_OLD"
  | "MARK_MISMATCH"
  | "MARK_NOT_FRESH";

export interface OracleCheck {
  /** True only if every check passed. A UI should disable trading (fail closed) otherwise. */
  ok: boolean;
  problems: OracleProblem[];
  print: OraclePrint | null;
  verification: PrintVerification | null;
}

/**
 * Independently verify an oracle response: decode the canonical bytes, check they hash to the
 * claimed hash and match the summary fields, verify reporter signatures and quorum, and check
 * freshness at `nowMs`. Pass `reporters`/`quorum` read from the contract (`ClearingReader.state()`)
 * so the node's own allowlist is not trusted; they default to the values in the response.
 * Pass `market` to also check the index id, version, scale, max age and implied mark.
 */
export function checkOracle(view: OracleView, opts: { reporters?: readonly string[]; quorum?: number; nowMs?: number | bigint; market?: Market } = {}): OracleCheck {
  const problems: OracleProblem[] = [];
  const pv = view.print;
  if (!pv) return { ok: false, problems: ["NO_PRINT"], print: null, verification: null };
  let bytes: Uint8Array;
  let p: OraclePrint;
  try {
    bytes = hexToBytes(pv.canonicalHex);
    p = decodePrint(bytes);
  } catch {
    return { ok: false, problems: ["DECODE_FAILED"], print: null, verification: null };
  }
  if (printHash(bytes) !== pv.hash) problems.push("HASH_MISMATCH");
  if (
    p.value !== pv.value ||
    p.status !== pv.status ||
    p.dataStatus !== pv.dataStatus ||
    p.sourceStartMs !== pv.sourceStartMs ||
    p.sourceEndMs !== pv.sourceEndMs ||
    p.publishedAtMs !== pv.publishedAtMs ||
    p.validUntilMs !== pv.validUntilMs ||
    p.revision !== pv.revision
  )
    problems.push("FIELD_MISMATCH");
  if (p.indexId !== view.indexId || p.version !== view.indexVersion) problems.push("INDEX_MISMATCH");
  const verification = verifyPrint(bytes, pv.signatures, opts.reporters ?? view.reporters, opts.quorum ?? view.quorum);
  if (!verification.ok) problems.push("QUORUM_NOT_MET");
  if (!printUsable(p)) problems.push("NOT_USABLE");
  const now = BigInt(opts.nowMs ?? Date.now());
  if (now >= p.validUntilMs) problems.push("EXPIRED");
  const m = opts.market;
  if (m) {
    if (p.indexId !== m.params.indexId || p.version !== m.params.indexVersion) {
      if (!problems.includes("INDEX_MISMATCH")) problems.push("INDEX_MISMATCH");
    }
    if (p.valueScale !== m.params.valueScale) problems.push("SCALE_MISMATCH");
    if (now >= p.publishedAtMs + m.params.oracleMaxAgeSec * 1000n) problems.push("TOO_OLD");
    if (m.params.indexUnitsPerTick > 0n && printUsable(p) && p.value / m.params.indexUnitsPerTick !== view.mark) problems.push("MARK_MISMATCH");
  }
  if (!view.markFresh) problems.push("MARK_NOT_FRESH");
  return { ok: problems.length === 0, problems, print: p, verification };
}
