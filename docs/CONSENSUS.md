# Proof-of-work specification

On NEAR (current design, `NEAR_SETTLEMENT.md`), the parts that apply are section 1 (profiles), section 3 (the proof of work) and section 4 (difficulty). Sections 2, 5, 6 and 7 specify the own-chain alternative: block header, transactions, node API and bridge module.

Status: frozen for the private testnet. v2 is a hard fork of v0.1 (`consensus_version: 2`): it adds `state_root` to the header, coinbase maturity, transaction memos, the native bridge module and the RunningMax difficulty rule. The PoW function and its golden vectors are unchanged. Not frozen for any value-bearing network (`MAINNET_READY = false`, see RELEASE_READINESS.md).

Every rule here is implemented in `crates/zfx-verify` (consensus library, no GPU runtime) and `crates/zfx-node/src/chain.rs` (state rules). Where this document and the code disagree, the code plus `tests/vectors/fp8c_v0.1.json` and `tests/vectors/consensus_v2.json` win, and that disagreement is a bug to report.

Notation: `||` is byte concatenation. `u32le`/`u64le` are little-endian unsigned integers. `uint256_be` reads 32 bytes as a big-endian unsigned integer. `SHA256` is FIPS 180-4. `SHAKE256(m, n)` is the first `n` output bytes of FIPS 202 SHAKE256. Domain strings are ASCII bytes without a terminator or length prefix. Every input that follows a domain string has a fixed length, so concatenation is unambiguous.

## 1. Versions and profiles

The header `version` field selects the PoW profile.

| id | name | d_in | H (hidden) | hidden layers | d_out | shift_in | shift_hidden | MACs per attempt |
|---|---|---|---|---|---|---|---|---|
| 1 | `zfx-fp8c-v0.1` | 64 | 4096 | 3 | 256 | 0 | 2 | 51,642,368 |
| 2 | `zfx-fp8c-v0.1-dev` | 64 | 256 | 3 | 64 | 0 | 0 | 229,376 |

Profile 2 exists for unit tests and fast local devnets only. A chain uses exactly one profile, fixed by its genesis config. A header whose version differs from the chain profile is invalid.

## 2. Block header

156 bytes without the nonce, 164 bytes with it (v2).

| offset | size | field | encoding |
|---|---|---|---|
| 0 | 4 | magic | ASCII `ZFX2` |
| 4 | 4 | version | u32le (profile id) |
| 8 | 4 | chain_id | u32le |
| 12 | 8 | height | u64le |
| 20 | 32 | parent | block hash of parent |
| 52 | 32 | tx_root | section 5.4 |
| 84 | 32 | state_root | section 5.6, state after applying this block |
| 116 | 8 | timestamp | u64le, Unix seconds |
| 124 | 32 | target | uint256 big-endian |
| 156 | 8 | nonce | u64le |

`header_without_nonce` is bytes 0..156. `block_hash = SHA256("ZFX-block-v0.1" || header[0..164])`. The v0.1 layout (magic `ZFXH`, 124 + 8 bytes, no state root) is kept only to reproduce the v0.1 PoW golden vectors; it is invalid on v2 chains.

## 3. Proof of work: ZFX-FP8C

### 3.1 Operand domain

`D = {-16, ..., -1, 1, ..., 16}`, 32 values with no zero. A 5-bit index `u` maps to `val(u) = u + 1` for `u < 16` and `val(u) = 15 - u` for `u >= 16`. A byte `b` maps to `val(b & 31)`. That map is unbiased because 256 = 8 x 32.

Every value in D is exactly representable in OCP FP8 E4M3 (bias 7), AMD FP8 E4M3FNUZ (bias 8), int8, FP16, BF16 and FP32.

Exactness bound: every layer has inner dimension K <= 4096, so `|sum| <= K x 16 x 16 <= 2^20 < 2^24`. Each product and every partial sum is an integer of magnitude below 2^24. FP32 therefore represents every intermediate exactly, whatever the reduction order, blocking or split-K. Int32 is exact trivially. Consensus is defined on these integers. The storage format (int8, E4M3, E4M3FNUZ, float32) is an implementation choice.

### 3.2 Requantization

`requant(y, s) = val(SBOX[((y >> s) & 0xFF)] >> 3)`

Here `y` is a two's-complement int32, `>>` is an arithmetic shift, and `SBOX` is the AES S-box (FIPS-197 Figure 7). The S-box is used only as a fixed nonlinear byte permutation. Because it is a permutation, each of the 32 outputs comes from exactly 8 input bytes.

### 3.3 Inputs

```
challenge   = SHA256("ZFX-FP8C-v0.1" || header_without_nonce)
attempt_id  = SHA256("ZFX-attempt-v0.1" || challenge || u64le(nonce))
x0[i]       = val(SHAKE256("ZFX-FP8C-X0-v0.1" || attempt_id, d_in)[i] & 31)     i < d_in
```

### 3.4 Epoch weights

For a block at height `h >= 1`, `anchor_height = floor((h - 1) / 32) * 32`, and `anchor_hash` is the hash of that height's ancestor on the block's own branch. Heights 1..32 use the genesis hash.

```
seed           = SHA256("ZFX-FP8C-epoch-v0.1" || anchor_hash || u32le(profile_id))
W[l][j][k]     = val(SHAKE256("ZFX-FP8C-W-v0.1" || seed || u32le(l) || u32le(j), K_l)[k] & 31)
```

Layer `l` has shape `N_l x K_l` (output-major, one independent XOF stream per row `j`):

| l | K_l | N_l | requant shift |
|---|---|---|---|
| 0 | d_in | H | shift_in |
| 1 .. n_hidden | H | H | shift_hidden |
| n_hidden + 1 | H | d_out | none (output layer) |

### 3.5 Attempt

```
x = x0
for each layer l:
    y[j] = sum_k x[k] * W[l][j][k]          (exact integer, |y| < 2^24)
    if l is not the output layer: x[j] = requant(y[j], shift_l)
out = y                                     (d_out int32 values)
digest = SHA256("ZFX-result-v0.1" || challenge || u64le(nonce) || out[0] as i32le || ... || out[d_out-1] as i32le)
valid  iff uint256_be(digest) < uint256_be(header.target)
```

The miner submits only `(work_id, nonce)` to its node. Nodes and peers receive the full header and recompute the digest. No miner-supplied digest is ever trusted.

### 3.6 Reference vectors

`tests/vectors/fp8c_v0.1.json` (regenerate with `zfx-research vectors`):

| profile | epoch_seed | challenge | digest(nonce=0) |
|---|---|---|---|
| dev | `793855bd...70f43538` | `79753124...01a3bdaf` | `ed243470...2e89fdc1` |
| v01 | `3d3fefe9...2f2cb73c` | `4e4bca37...28dbe66f` | `66286df2...0b1119ca` |

Edge case: `dot([16]*4096, [16]*4096) = 1048576`, whose FP32 bits are `0x49800000`. Three independent implementations (Rust, C++, Python) and the Apple GPU backends reproduce every vector.

## 4. Difficulty and time

- Target is uint256, and the work of one block is `floor(2^256 / (target + 1))`. Fork choice picks the valid chain with the most cumulative work. On a tie, the first-seen tip wins.
- Retarget is LWMA-1 with N = 30, integer-only:
  `next = (sum_i target_i / N) * (sum_{i=1..N} i * clamp(t_i - t_{i-1}, 1, 6T)) / (N(N+1)/2 * T)`, clamped to `[1, pow_limit]`.
  The genesis target applies while the parent height is below 31. A header's target must equal the computed value exactly.
- Solvetime rule (v2, `RunningMax`): `t_i - t_{i-1}` above is replaced by `t_i - max(t_0..t_{i-1})`, clamped to `[1, 6T]`. With the v0.1 rule a miner holding 30% of work could forge timestamps and pull block times to 24.4 s against a 60 s target; with RunningMax the same attack yields 61.0 s (`bench/results/fork_resilience.json`).
- The genesis config may set `fixed_difficulty = true`, for test environments only.
- Timestamps: `timestamp > median(last 11 timestamps)` and `timestamp <= local_time + max_future_secs` (120 s on the testnet).
- No mainnet block time, reward schedule or supply cap is finalized. A node refuses a genesis config with `network: "mainnet"` until an owner-approved `subsidy_schedule` is set (section 5.5).

## 5. Transactions

### 5.1 Signatures

- Scheme 1: SLH-DSA-SHA2-128s (FIPS 205, NIST category 1). Public key 32 B, signature 7,856 B.
- Scheme 2: SLH-DSA-SHA2-192s (category 3). Public key 48 B, signature 16,224 B.
- Scheme 3: XMSS (SP 800-208). Reserved, rejected in v0.1.

Messages are signed with FIPS 205 pure SLH-DSA (hedged), with context string `"ZFX-v0.1"`. There is no elliptic-curve or RSA primitive anywhere in consensus.

### 5.2 P2MR addresses

```
leaf_hash    = SHA256("ZFX-p2mr-leaf-v0.1" || u8(scheme) || u16le(len(pk)) || pk)
node(a, b)   = SHA256("ZFX-p2mr-node-v0.1" || min(a,b) || max(a,b))       (sorted pair)
root         = binary Merkle root over leaf hashes, odd node promoted
address      = SHA256("ZFX-p2mr-addr-v0.1" || root)                        (32 bytes)
text         = "zfx1" || hex(address) || hex(SHA256("ZFX-addrck" || address)[0..4])
```

Spending reveals one leaf, its Merkle path (depth <= 8) and a signature. There is no key-path spend. Wallets default to two leaves: index 0 is 128s for daily use and index 1 is 192s as a backup.

### 5.3 Encoding and identifiers

```
core     = u32le(chain_id) || from(32) || to(32) || u64le(amount) || u64le(fee) || u64le(nonce)
           [|| 0x01 || u8(len memo) || memo]          (v2, only when a memo is set; UTF-8, <= 64 bytes)
message  = "ZFX-tx-v0.1" || core
witness  = u8(scheme) || u16le(len pk) || pk || u8(path len) || path(32 each) || u16le(len sig) || sig
txid     = SHA256("ZFX-txid-v0.1" || core)                  (witness excluded: non-malleable)
wtxid    = SHA256("ZFX-wtxid-v0.1" || core || witness)
coinbase = u64le(height) || to(32) || u64le(amount);  id = SHA256("ZFX-coinbase-v0.1" || coinbase)
```

### 5.4 Block body

`tx_root = SHA256("ZFX-txroot-v0.1" || u32le(count) || merkle(leaves))`, where the leaves are `[coinbase id, wtxid_1, ..., wtxid_n, op_id_1, ..., op_id_m]` (v2 appends the bridge op ids, section 7). Merkle nodes are `SHA256("ZFX-merkle-v0.1" || l || r)`, and an odd node is promoted without duplication (avoiding CVE-2012-2459). Limits are 1,000 transactions and 4 MiB of canonical bytes per block.

### 5.5 State rules (account model)

- A transaction is valid iff its authorization verifies, `nonce` equals the sender's next nonce, `amount + fee` fits u64 and does not exceed the sender's balance, and `from != to`.
- The coinbase amount must equal `subsidy(height) + sum(fees)` exactly. `subsidy` comes from the genesis `subsidy_schedule` (`Constant`, `Halving` or `SmoothDecay`, each with an optional tail; `crates/zfx-verify/src/monetary.rs`); without one, `block_subsidy` applies at every height. The testnet subsidy is 50 x 10^8 units per block and carries no monetary value.
- Coinbase maturity (v2): a coinbase output becomes spendable `coinbase_maturity` blocks after its block (testnet 20). Until then it is held in the immature set.
- Genesis allocations come from the genesis config. The genesis block commits to the config's fixed-layout encoding through `tx_root = SHA256("ZFX-genesis-v0.1" || SHA256(config_bytes))`.

### 5.6 State root (v2)

`state_root = SHA256("ZFX-state-v2" || body)`, where `body` is: `u64le(n)` and, for each of the `n` live accounts (balance or nonce nonzero) in ascending address order, `address(32) || u64le(balance) || u64le(nonce)`; then `"|immature"` and each immature entry `u64le(height) || address || u64le(amount)` in key order; then `"|supply" || u64le(supply)`; then, if the bridge is enabled, `"|bridge" || u32le(epoch) || members(32 each) || pending rotation (0x00, or 0x01 || keys || u64le(activate_height)) || u8(paused) || u64le(admin_nonce) || u128le(locked_total) || u128le(released_total) || u64le(deposits) || u64le(releases) || used withdrawal ids (32 each, ascending)`. Code: `State::root` in `crates/zfx-verify/src/state.rs`.

## 6. Node-to-miner API (v1)

`GET /v1/work?address=zfx1...` returns `{api_version, work_id, chain_id, version, height, header_without_nonce, challenge, target, epoch_seed, profile, expiry}`.

`POST /v1/submit {work_id, nonce}` returns `{accepted, block_hash | error}`.

Work expires when the tip changes. The node recomputes the PoW for every submission and rate-limits to 50 submissions per second per IP. Request bodies are capped at 64 KiB and P2P lines at 16 MiB. `GET /v1/status` returns height, target, peers and best hash. The miner checks that `challenge == SHA256("ZFX-FP8C-v0.1" || header_without_nonce)` before mining.

## 7. Native bridge module (v2)

Enabled by a `bridge` section in the genesis config: committee (3 SLH-DSA-SHA2-128s public keys), threshold 2, `rotation_timelock_blocks`, `min_lock`, `activation_height`, and the NEAR contract id.

- Lockbox: `lockbox = SHA256("ZFX-bridge-lockbox-v1" || u32le(chain_id))`. No key exists for it; only consensus-validated releases move funds out.
- Deposit: an ordinary transaction to the lockbox with amount >= `min_lock` and a memo holding a valid NEAR account id. `deposit_id = txid`. Rejected while paused.
- Release op: `{epoch, withdrawal_id, near_burn_ref, recipient, amount, expiry_height, approvals}`. Valid iff not paused, `epoch` is current, height <= `expiry_height`, the withdrawal id was never released, `amount` <= lockbox balance, and >= 2 distinct members signed `release_digest` (`docs/CONTRACT_API.md`).
- Admin op: `{epoch, op_nonce, action, approvals}` with `op_nonce == admin_nonce`. Pause needs 1 approval; unpause and rotate need 2. Rotate carries 3 new keys and `activate_height >= height + rotation_timelock_blocks`; at that height the committee is replaced and `epoch` increments.
- Approvals are SLH-DSA-SHA2-128s signatures with FIPS 205 context `ZFX-bridge-v1`. At most 64 bridge ops per block.
- `op_id = SHA256("ZFX-bridgeop-v1" || canonical op encoding)` (`encode_op` in `bridge.rs`); ops are tx_root leaves after the transactions.
- Audit: lockbox balance must equal `locked_total - released_total` after every block.

Golden vectors for all v2 encodings and digests: `tests/vectors/consensus_v2.json` (regenerate only with `zfx-research vectors-v2 --force`).
