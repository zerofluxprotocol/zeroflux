//! Test fixtures shared by the core's tests, the engine's tests and the contract's tests:
//! deterministic keys, a model-index market, signed orders and prints, and the ledger
//! conservation check. Enabled with the `testkit` feature.

use crate::ledger::{self, Now};
use crate::model::*;
use crate::print::*;
use crate::store::*;
use crate::wire::*;

pub const CHAIN: &str = "near:sandbox";
pub const CONTRACT: &str = "clearing.test.near";
pub const USDC: &str = "usdc.test.near";
pub const MKT: &str = "OPUS-PERP";
pub const INDEX: &str = "or-model-spend/anthropic/claude-opus-5.5-20260921";
pub const FORMULA: &str = "ZFX-OR-MODEL-SPEND-USD-v1";
pub const INSURANCE: &str = "insurance.test.near";
/// 2026-11-10 08:00:00 UTC.
pub const T0: u64 = 1_794_268_800_000_000_000 + 8 * 3_600 * NS_PER_SEC;

#[derive(Clone)]
pub struct Key(pub ed25519_dalek::SigningKey);

impl Key {
    pub fn from_seed(seed: [u8; 32]) -> Self {
        Key(ed25519_dalek::SigningKey::from_bytes(&seed))
    }
    pub fn public(&self) -> [u8; 32] {
        self.0.verifying_key().to_bytes()
    }
    pub fn sign(&self, msg: &[u8]) -> [u8; 64] {
        use ed25519_dalek::Signer;
        self.0.sign(msg).to_bytes()
    }
}

#[derive(Clone)]
pub struct Trader {
    pub name: String,
    pub key: Key,
}

impl Trader {
    pub fn new(name: &str, seed: u8) -> Self {
        let mut s = NativeCrypto.sha256(name.as_bytes());
        s[0] = seed;
        Trader {
            name: name.into(),
            key: Key::from_seed(s),
        }
    }
}

/// The reference model-index market: 1 tick = $1 of index per day, 1 lot = 1/1000 atom-dollar
/// scaling such that one lot at a mark of 1_000 ticks has a notional of 1_000_000 atoms ($1 at
/// 6-decimal USDC). 2x max leverage, 10% maintenance, 30% band, daily cycle from 22:00 UTC.
pub fn model_params() -> MarketParams {
    MarketParams {
        market_id: MKT.into(),
        market_type: MarketType::Perpetual,
        index_id: INDEX.into(),
        index_version: FORMULA.into(),
        value_scale: 6,
        index_units_per_tick: 1_000_000,
        tick_value: 1_000,
        min_qty: 1,
        max_qty: 1_000_000,
        max_price: 100_000_000,
        initial_margin_bps: 5_000,
        maintenance_margin_bps: 1_000,
        max_leverage: 2,
        max_open_interest: 20_000,
        max_position: 10_000,
        max_open_interest_notional: 0,
        maker_fee_bps: 2,
        taker_fee_bps: 5,
        liq_fee_bps: 100,
        liq_slippage_bps: 300,
        price_band_bps: 3_000,
        oracle_max_age_sec: 36 * 3_600,
        funding_interval_sec: 8 * 3_600,
        funding_divisor: 24,
        max_funding_ppm: 2_500,
        print_close_only_from_sec: 22 * 3_600,
        print_lag_days: 0,
        mark_limit_bps: 20_000,
        halt_jump_bps: 0,
        print_move_close_only_bps: 0,
    }
}

pub fn globals() -> Globals {
    Globals {
        chain_id: CHAIN.into(),
        contract_id: CONTRACT.into(),
        collateral: USDC.into(),
        insurance_account: INSURANCE.into(),
        fee_balance: 0,
        reporters: vec![],
        quorum: 1,
        last_op_ns: 0,
        settle_grace_ns: crate::batch::DEFAULT_SETTLE_GRACE_NS,
        max_order_ttl_ns: 30 * NS_PER_DAY,
        total_collateral: 0,
    }
}

pub struct World {
    pub s: MemStore,
    pub now: u64,
    pub reporter: Trader,
    pub nonce: u64,
    pub next_deposit: u64,
}

impl Default for World {
    fn default() -> Self {
        Self::new()
    }
}

impl World {
    /// One ACTIVE market with a print for the previous UTC day at mark 1_000.
    pub fn new() -> Self {
        let reporter = Trader::new("reporter", 201);
        let mut g = globals();
        g.reporters = vec![reporter.key.public()];
        let mut w = World {
            s: MemStore::new(g),
            now: T0,
            reporter,
            nonce: 1,
            next_deposit: 1,
        };
        let mut m = ledger::new_market(&NativeCrypto, model_params(), T0).unwrap();
        m.status = MarketStatus::Active;
        w.s.put_market(MKT, m);
        let p = w.print(1_000, 1);
        w.post_print(&p).unwrap();
        w
    }

    pub fn deposit(&mut self, account: &str, amount: u128) {
        let id = self.next_deposit;
        self.next_deposit += 1;
        self.s.deposits.insert(
            id,
            PendingDeposit {
                account: account.into(),
                amount,
                created_ns: self.now,
            },
        );
        ledger::credit(&mut self.s, id).unwrap();
    }

    pub fn trader(&mut self, name: &str, amount: u128) -> Trader {
        let t = Trader::new(name, 1);
        self.s.add_key(
            t.key.public(),
            KeyInfo {
                account: name.into(),
                expires_ns: 0,
                can_withdraw: true,
            },
        );
        if amount > 0 {
            self.deposit(name, amount);
        }
        t
    }

    pub fn fund_insurance(&mut self, amount: u128) {
        if amount > 0 {
            self.deposit(INSURANCE, amount);
        }
    }

    pub fn policy(&self) -> [u8; 32] {
        self.s.markets[MKT].policy_hash
    }

    /// A GTC limit order, edited by `f` before signing.
    pub fn order(&mut self, t: &Trader, side: Side, price: u64, qty: u64, f: impl FnOnce(&mut SignedOrder)) -> SignedOrder {
        self.nonce += 1;
        let mut o = SignedOrder {
            chain_id: CHAIN.into(),
            clearing_contract: CONTRACT.into(),
            account: t.name.clone(),
            market: MKT.into(),
            side,
            order_type: OrderType::Limit,
            price,
            qty,
            reduce_only: false,
            post_only: false,
            tif: TimeInForce::Gtc,
            nonce: self.nonce,
            expiry_ns: self.now + 3_600 * NS_PER_SEC,
            client_order_id: self.nonce,
            max_fee_bps: 10,
            policy_hash: self.policy(),
            public_key: t.key.public(),
            signature: Sig64([0; 64]),
        };
        f(&mut o);
        o.signature = Sig64(t.key.sign(&o.message()));
        o
    }

    /// Canonical bytes of a print for the UTC day that ended at the most recent midnight.
    pub fn print_with(&self, ticks: u64, revision: u32, f: impl FnOnce(&mut OraclePrint)) -> Vec<u8> {
        let end = self.now - self.now % NS_PER_DAY;
        let ms = |ns: u64| (ns / 1_000_000) as i64;
        let cur = self.s.markets.get(MKT).and_then(|m| m.print.clone());
        let prev = cur.filter(|c| c.source_end_ms == ms(end)).map(|c| c.hash).unwrap_or([0; 32]);
        let mut p = OraclePrint {
            index_id: INDEX.into(),
            version: FORMULA.into(),
            value_scale: 6,
            value: ticks * 1_000_000,
            source_start_ms: ms(end - NS_PER_DAY),
            source_end_ms: ms(end),
            observed_at_ms: ms(self.now),
            published_at_ms: ms(self.now),
            valid_until_ms: ms(end + 2 * NS_PER_DAY),
            source_hash: [0x11; 32],
            formula_hash: [0x22; 32],
            status: OracleStatus::Fresh,
            data_status: DataStatus::Ok,
            finality: if revision > 1 { Finality::Corrected } else { Finality::Final },
            revision,
            prev_print_hash: prev,
        };
        f(&mut p);
        p.encode().unwrap()
    }

    pub fn print(&self, ticks: u64, revision: u32) -> Vec<u8> {
        self.print_with(ticks, revision, |_| {})
    }

    pub fn reporter_sig(&self, bytes: &[u8]) -> ([u8; 32], Sig64) {
        (self.reporter.key.public(), Sig64(self.reporter.key.sign(bytes)))
    }

    pub fn post_print(&mut self, bytes: &[u8]) -> ledger::Res<Vec<ledger::CEvent>> {
        let sig = vec![self.reporter_sig(bytes)];
        ledger::oracle(&mut self.s, &NativeCrypto, Now::at(self.now), MKT, bytes, &sig)
    }

    /// Move to 08:00 UTC of the next day (after the daily print lands).
    pub fn advance_day(&mut self) {
        self.now += NS_PER_DAY;
    }

    /// Next day, with a usable print at `ticks`.
    pub fn next_print(&mut self, ticks: u64) {
        self.advance_day();
        let p = self.print(ticks, 1);
        self.post_print(&p).unwrap();
    }

    pub fn check_conservation(&self) {
        check_conservation(&self.s).unwrap();
    }
}

/// Ledger invariants:
/// - per market, positions sum to zero and long open interest equals the index;
/// - `Σ free + Σ (margin − funding owed) + fees = total collateral + Σ bad debt` (positions value
///   to zero in aggregate, so this is the whole of everyone's claims).
pub fn check_conservation(s: &MemStore) -> Result<(), String> {
    let mut claims: i128 = s.accounts.values().map(|a| a.free as i128).sum();
    let mut bad: u128 = 0;
    for (id, m) in &s.markets {
        let (mut sum, mut longs) = (0i128, 0u64);
        for (_, p) in s.holders(id) {
            sum += p.size as i128;
            longs += p.size.max(0) as u64;
            claims += p.margin - ledger::funding_owed(m, p);
        }
        if sum != 0 {
            return Err(format!("{id}: positions sum to {sum}"));
        }
        if longs != m.open_interest {
            return Err(format!("{id}: open interest {} but longs {longs}", m.open_interest));
        }
        bad += m.bad_debt;
    }
    claims += s.globals.fee_balance as i128;
    let want = s.globals.total_collateral as i128 + bad as i128;
    if claims != want {
        return Err(format!("claims {claims} != collateral + bad debt {want}"));
    }
    Ok(())
}
