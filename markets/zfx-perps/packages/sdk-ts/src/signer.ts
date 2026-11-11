/**
 * Trading keys. A trading key is an ed25519 key the user generates in the app (kept in the
 * browser, e.g. IndexedDB) and registers once on the settlement contract with a wallet
 * transaction (`registerKeyAction`). It signs orders, cancels and withdrawals; it can never move
 * funds anywhere except back to the user's own account.
 */
import { ed25519 } from "@noble/curves/ed25519";
import { base58, base64 } from "@scure/base";

/** Anything that can sign with an ed25519 trading key, e.g. a non-extractable WebCrypto key. */
export interface TradingSigner {
  /** `ed25519:<base58>` */
  readonly publicKey: string;
  sign(message: Uint8Array): Promise<Uint8Array> | Uint8Array;
}

export class KeySigner implements TradingSigner {
  readonly publicKey: string;
  private constructor(private readonly seed: Uint8Array) {
    this.publicKey = `ed25519:${base58.encode(ed25519.getPublicKey(seed))}`;
  }

  static generate(): KeySigner {
    return new KeySigner(ed25519.utils.randomPrivateKey());
  }

  static fromSeed(seed: Uint8Array): KeySigner {
    if (seed.length !== 32) throw new Error("seed must be 32 bytes");
    return new KeySigner(seed.slice());
  }

  /** NEAR secret key format `ed25519:<base58 of seed||pubkey or seed>`. */
  static fromNearSecret(secret: string): KeySigner {
    if (!secret.startsWith("ed25519:")) throw new Error("expected ed25519:<base58>");
    const b = base58.decode(secret.slice(8));
    const s = KeySigner.fromSeed(b.slice(0, 32));
    if (b.length === 64 && base58.encode(b.slice(32)) !== s.publicKey.slice(8)) throw new Error("public half does not match seed");
    return s;
  }

  get nearSecret(): string {
    const pk = base58.decode(this.publicKey.slice(8));
    const b = new Uint8Array(64);
    b.set(this.seed, 0);
    b.set(pk, 32);
    return `ed25519:${base58.encode(b)}`;
  }

  sign(message: Uint8Array): Uint8Array {
    return ed25519.sign(message, this.seed);
  }
}

export const utf8 = (s: string) => new TextEncoder().encode(s);

let lastSignTs = 0;
/**
 * Timestamp for a signed request: `Date.now()`, but strictly increasing within the page or
 * process. The gateway rejects a repeated signature as a replay, and two requests signed in the
 * same millisecond would otherwise be byte-identical.
 */
export function signTsMs(): number {
  const now = Date.now();
  lastSignTs = now > lastSignTs ? now : lastSignTs + 1;
  return lastSignTs;
}

export async function signB64(signer: TradingSigner, message: string): Promise<string> {
  return base64.encode(await signer.sign(utf8(message)));
}

export function verify(publicKey: string, message: string, signatureB64: string): boolean {
  return ed25519.verify(base64.decode(signatureB64), utf8(message), base58.decode(publicKey.replace(/^ed25519:/, "")));
}
