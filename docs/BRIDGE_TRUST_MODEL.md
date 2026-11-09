# Bridge trust model (v1)

The ZeroFlux to NEAR bridge is a **2-of-3 trusted custodian**. Any two committee members acting together can steal every locked ZFX or mint unbacked wZFX. Nothing below changes that; it only limits what one member, the coordinator, or a relay can do.

## Who holds what

| party | key material | can do alone | cannot do alone |
|---|---|---|---|
| relayer i (3 of them) | one 32-byte seed (0600 file) deriving an ed25519 key (NEAR) and an SLH-DSA-SHA2-128s key (native) | pause both chains; refuse to sign | mint, release, unpause, rotate |
| any 2 relayers | two seeds | mint wZFX, release native ZFX, unpause, rotate the committee | nothing is out of reach: this is the trust assumption |
| coordinator | a NEAR account key for gas only, no committee key | submit bundles others signed; retry; halt itself | sign anything the contract or consensus accepts |
| contract owner (NEAR) | owner account key | pause; stage an upgrade, deploy it only after the timelock | unpause, mint, rotate |
| anyone | none | submit a correctly signed bundle (relay) | anything else |

Relayer i reads ZeroFlux node i and NEAR independently. It signs a mint only after its own node shows the deposit at `K` confirmations (default 12 on testnet, 6 in e2e) and NEAR shows the deposit id unminted; it signs a release only after its own NEAR view shows the burn at `final`. Each relayer keeps a local signing record, so it never signs two different amounts for one deposit or withdrawal id.

## What enforces the threshold

- Native side: consensus. `crates/zfx-verify/src/state.rs` rejects any block whose `Release` or `Admin` op lacks the required distinct SLH-DSA approvals (2 for release/unpause/rotate, 1 for pause), whose admin nonce is not the next one, or whose withdrawal id was already released. Every full node checks this; miners cannot include a bad op.
- NEAR side: the wZFX contract (`bridge/near/wzfx/src/lib.rs`). `bridge_mint` needs 2 distinct valid ed25519 committee signatures over the mint digest for the current epoch, before expiry, for a deposit id never minted.

Both are tested live: `tests/e2e/bridge.py` (1-of-3 mint and release rejected, forged member index rejected, replay rejected), `tests/e2e/bridge_governance.py` (1-of-3 unpause rejected, replayed admin approvals rejected, retired keys rejected after rotation).

## Finality and reorgs

- Deposits mint only after `K` native confirmations. A deposit reorged out below `K` is never minted (tested in `tests/e2e/bridge_reorg.py`, scenario 1).
- A reorg deeper than `K` after a mint leaves unbacked wZFX. The coordinator detects the orphan in reconciliation, writes an incident, and both chains get paused through 1-of-3 approvals. It never mints or releases to compensate (scenario 2). Choosing `K` larger than the deepest plausible reorg is a policy decision; the testnet default is 12.
- Releases wait for NEAR `final` finality of the burn.

## Invariants and halting

Reconciliation (`bridge/relayers/src/reconcile.rs`) reads only chain state:

    (locked - released) - (minted - burned) == pending_deposits - pending_releases

plus custody (lockbox balance equals locked minus released), wrapped supply (`ft_total_supply` equals minted minus burned), and an orphan scan. Any failure for 3 consecutive checks halts the coordinator, writes an incident record, and requests 1-of-3 pauses on both chains. Unpausing needs 2-of-3 after a human investigation. See `docs/OPERATOR_RUNBOOK.md`.

## Path to trust minimization

Measured in `bench/results/onchain_pow_gas.json` and `bench/results/stark_prototype.json`: re-executing one production (v0.1) PoW attempt inside a NEAR contract exceeds the 300 Tgas transaction limit by orders of magnitude, and a STARK of only the multiply-accumulate core of one attempt is modeled at 175 s or more of proving. A proof-verified bridge needs a full PoW circuit, external review, and a NEAR verifier cost measurement before it could replace the committee. Until then the committee stays.
