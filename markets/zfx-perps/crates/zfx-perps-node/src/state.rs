//! Read model and publisher.
//!
//! The publisher thread consumes durable events from the sequencer, in order, and maintains what
//! the gateway reads without touching the engine thread: L2 books with a per-market sequence
//! number, recent trades, the account/key index used for authentication, and the settlement
//! batches waiting for the chain. It also fans out WebSocket messages.

use crate::dto::*;
use crate::sequencer::Published;
use crossbeam_channel::Receiver;
use zfx_perps_engine::{AccountId, AssetSpec, Batch, Engine, Event, MarketSpec, Side};
use parking_lot::RwLock;
use rustc_hash::FxHashMap;
use serde_json::json;
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::broadcast;

pub const RECENT_TRADES: usize = 500;

/// Per-market (bid, ask) level updates of one command.
type Deltas = BTreeMap<u16, (Vec<(u64, u64)>, Vec<(u64, u64)>)>;

#[derive(Default)]
pub struct L2 {
    pub seq: u64,
    pub bids: BTreeMap<u64, u64>,
    pub asks: BTreeMap<u64, u64>,
    pub trades: VecDeque<TradeDto>,
}

#[derive(Default)]
pub struct Directory {
    pub assets: Vec<AssetSpec>,
    pub markets: Vec<MarketSpec>,
    pub market_by_symbol: FxHashMap<String, u16>,
    pub asset_by_token: FxHashMap<String, u16>,
    pub names: Vec<String>,
    pub by_name: FxHashMap<String, AccountId>,
    pub key_owner: FxHashMap<[u8; 32], AccountId>,
}

/// One outbound WebSocket message. `market`/`account` route it to subscribers.
#[derive(Clone, Debug)]
pub struct Out {
    pub channel: Channel,
    pub market: Option<u16>,
    pub account: Option<AccountId>,
    /// Book sequence number for book deltas, 0 otherwise.
    pub seq: u64,
    pub text: Arc<str>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Book,
    Trades,
    Account,
    /// Mark, funding and socialized-loss updates of a perp market.
    Perp,
}

pub struct Shared {
    pub dir: RwLock<Directory>,
    pub books: RwLock<Vec<Arc<RwLock<L2>>>>,
    /// Settlement batches cut but not yet confirmed on-chain, oldest first.
    pub batches: RwLock<VecDeque<Batch>>,
    pub chain_cursor: AtomicU64,
    pub last_settled: AtomicU64,
    pub published_seq: AtomicU64,
    pub out: broadcast::Sender<Out>,
    /// Latest oracle round per perp market (price and per-source results).
    pub oracle: RwLock<BTreeMap<u16, serde_json::Value>>,
}

impl Shared {
    pub fn from_engine(e: &Engine) -> Arc<Self> {
        let mut d = Directory::default();
        for a in &e.assets {
            d.asset_by_token.insert(a.chain_ref.clone(), a.id);
            d.assets.push(a.clone());
        }
        let mut books = vec![];
        for m in &e.markets {
            d.market_by_symbol.insert(m.spec.symbol.clone(), m.spec.id);
            d.markets.push(m.spec.clone());
            let mut l2 = L2::default();
            for (p, l) in &m.book.bids {
                l2.bids.insert(*p, l.qty);
            }
            for (p, l) in &m.book.asks {
                l2.asks.insert(*p, l.qty);
            }
            books.push(Arc::new(RwLock::new(l2)));
        }
        for (i, a) in e.accounts.iter().enumerate() {
            d.names.push(a.name.clone());
            d.by_name.insert(a.name.clone(), i as AccountId);
            for k in &a.keys {
                d.key_owner.insert(*k, i as AccountId);
            }
        }
        let (out, _) = broadcast::channel(1 << 16);
        Arc::new(Shared {
            dir: RwLock::new(d),
            books: RwLock::new(books),
            batches: RwLock::new(e.settle.unconfirmed.iter().cloned().collect()),
            chain_cursor: AtomicU64::new(e.chain_cursor),
            last_settled: AtomicU64::new(e.settle.last_settled),
            published_seq: AtomicU64::new(0),
            out,
            oracle: RwLock::new(BTreeMap::new()),
        })
    }

    pub fn book(&self, m: u16) -> Option<Arc<RwLock<L2>>> {
        self.books.read().get(m as usize).cloned()
    }
}

/// Run the publisher on the current thread until the sequencer stops.
pub fn run_publisher(shared: Arc<Shared>, rx: Receiver<Published>) {
    while let Ok((seq, events)) = rx.recv() {
        apply(&shared, &events);
        shared.published_seq.store(seq, Ordering::Relaxed);
    }
}

fn send(s: &Shared, channel: Channel, market: Option<u16>, account: Option<AccountId>, v: serde_json::Value) {
    let seq = if channel == Channel::Book {
        v["seq"].as_u64().unwrap_or(0)
    } else {
        0
    };
    let _ = s.out.send(Out {
        channel,
        market,
        account,
        seq,
        text: Arc::from(v.to_string()),
    });
}

pub fn apply(s: &Shared, events: &[Event]) {
    // Level updates of one command are published as one book delta per market.
    let mut deltas: Deltas = BTreeMap::new();
    for ev in events {
        match ev {
            Event::AssetAdded { asset } => {
                let mut d = s.dir.write();
                d.asset_by_token.insert(asset.chain_ref.clone(), asset.id);
                d.assets.push(asset.clone());
            }
            Event::MarketAdded { market } => {
                let mut d = s.dir.write();
                d.market_by_symbol.insert(market.symbol.clone(), market.id);
                d.markets.push(market.clone());
                s.books.write().push(Arc::new(RwLock::new(L2::default())));
            }
            Event::AccountCreated { account, name } => {
                let mut d = s.dir.write();
                debug_assert_eq!(*account as usize, d.names.len());
                d.names.push(name.clone());
                d.by_name.insert(name.clone(), *account);
            }
            Event::KeyAdded { account, public_key } => {
                s.dir.write().key_owner.insert(*public_key, *account);
            }
            Event::KeyRemoved { public_key, .. } => {
                s.dir.write().key_owner.remove(public_key);
            }
            Event::Level { market, side, price, qty } => {
                let e = deltas.entry(*market).or_default();
                match side {
                    Side::Buy => e.0.push((*price, *qty)),
                    Side::Sell => e.1.push((*price, *qty)),
                }
            }
            Event::Fill {
                trade_id,
                market,
                price,
                qty,
                taker_side,
                maker,
                maker_order,
                maker_fee,
                maker_remaining,
                taker,
                taker_order,
                taker_fee,
                ts_ns,
            } => {
                let t = TradeDto {
                    trade_id: *trade_id,
                    price: *price,
                    qty: *qty,
                    taker_side: taker_side.as_str(),
                    ts_ns: *ts_ns,
                };
                let symbol = s
                    .dir
                    .read()
                    .markets
                    .get(*market as usize)
                    .map(|m| m.symbol.clone())
                    .unwrap_or_default();
                if let Some(b) = s.book(*market) {
                    let mut b = b.write();
                    b.trades.push_back(t.clone());
                    if b.trades.len() > RECENT_TRADES {
                        b.trades.pop_front();
                    }
                }
                send(
                    s,
                    Channel::Trades,
                    Some(*market),
                    None,
                    json!({"channel": "trades", "market": symbol, "trade": t}),
                );
                for (acct, order, role, side, fee, remaining) in [
                    (
                        *maker,
                        *maker_order,
                        "maker",
                        taker_side.opposite(),
                        *maker_fee,
                        Some(*maker_remaining),
                    ),
                    (*taker, *taker_order, "taker", *taker_side, *taker_fee, None),
                ] {
                    send(
                        s,
                        Channel::Account,
                        None,
                        Some(acct),
                        json!({"channel": "account", "type": "fill", "market": symbol, "order_id": order, "trade_id": trade_id, "role": role,
                               "side": side.as_str(), "price": price, "qty": qty, "fee": fee.to_string(), "remaining": remaining, "ts_ns": ts_ns}),
                    );
                }
            }
            Event::Accepted {
                order_id,
                account,
                market,
                side,
                price,
                qty,
                client_id,
                tif,
            } => {
                let symbol = market_symbol(s, *market);
                send(
                    s,
                    Channel::Account,
                    None,
                    Some(*account),
                    json!({"channel": "account", "type": "accepted", "order_id": order_id, "market": symbol,
                    "side": side.as_str(), "price": price, "qty": qty, "client_id": client_id, "tif": tif.as_str()}),
                );
            }
            Event::Canceled {
                order_id,
                account,
                market,
                remaining,
                reason,
            } => {
                let symbol = market_symbol(s, *market);
                send(
                    s,
                    Channel::Account,
                    None,
                    Some(*account),
                    json!({"channel": "account", "type": "canceled", "order_id": order_id, "market": symbol,
                    "remaining": remaining, "reason": reason}),
                );
            }
            Event::Rejected {
                account,
                client_id,
                reason,
            } => {
                send(
                    s,
                    Channel::Account,
                    None,
                    Some(*account),
                    json!({"channel": "account", "type": "rejected", "client_id": client_id, "reason": reason}),
                );
            }
            Event::Deposited { account, asset, amount } | Event::Credited { account, asset, amount } => {
                let kind = if matches!(ev, Event::Deposited { .. }) {
                    "deposited"
                } else {
                    "credited"
                };
                let token = asset_token(s, *asset);
                send(
                    s,
                    Channel::Account,
                    None,
                    Some(*account),
                    json!({"channel": "account", "type": kind, "token": token, "amount": amount.to_string()}),
                );
            }
            Event::WithdrawQueued {
                account,
                asset,
                amount,
                nonce,
            } => {
                let token = asset_token(s, *asset);
                send(
                    s,
                    Channel::Account,
                    None,
                    Some(*account),
                    json!({"channel": "account", "type": "withdraw_queued", "token": token,
                    "amount": amount.to_string(), "nonce": nonce}),
                );
            }
            Event::ForcedQueued {
                request_id,
                account,
                asset,
                amount,
            } => {
                let token = asset_token(s, *asset);
                send(
                    s,
                    Channel::Account,
                    None,
                    Some(*account),
                    json!({"channel": "account", "type": "forced_withdrawal_queued", "request_id": request_id,
                    "token": token, "amount": amount.to_string()}),
                );
            }
            Event::BatchCut { batch } => s.batches.write().push_back(batch.clone()),
            Event::BatchSettled { batch_id } => {
                let mut q = s.batches.write();
                while q.front().is_some_and(|b| b.id <= *batch_id) {
                    q.pop_front();
                }
                s.last_settled.fetch_max(*batch_id, Ordering::Relaxed);
            }
            Event::MarketHalted { .. } => {}
            Event::Mark { market, mark, index } => {
                let symbol = market_symbol(s, *market);
                send(
                    s,
                    Channel::Perp,
                    Some(*market),
                    None,
                    json!({"channel": "perp", "type": "mark", "market": symbol, "mark": mark, "index": index}),
                );
            }
            Event::Funding {
                market,
                rate_ppm,
                payment_per_lot,
            } => {
                let symbol = market_symbol(s, *market);
                send(
                    s,
                    Channel::Perp,
                    Some(*market),
                    None,
                    json!({"channel": "perp", "type": "funding", "market": symbol, "rate_ppm": rate_ppm, "payment_per_lot": payment_per_lot.to_string()}),
                );
            }
            Event::Socialized { amount, from_accounts } => {
                // Market-wide notice; affected accounts see it in their equity.
                let markets: Vec<u16> = s.dir.read().markets.iter().filter(|m| m.perp.is_some()).map(|m| m.id).collect();
                for m in markets {
                    send(
                        s,
                        Channel::Perp,
                        Some(m),
                        None,
                        json!({"channel": "perp", "type": "socialized_loss", "amount": amount.to_string(), "accounts": from_accounts}),
                    );
                }
            }
            Event::Position {
                account,
                market,
                size,
                entry_notional,
            } => {
                let symbol = market_symbol(s, *market);
                send(
                    s,
                    Channel::Account,
                    None,
                    Some(*account),
                    json!({"channel": "account", "type": "position", "market": symbol, "size": size, "entry_notional": entry_notional.to_string()}),
                );
            }
            Event::Liquidated {
                account,
                equity,
                maintenance,
            } => {
                send(
                    s,
                    Channel::Account,
                    None,
                    Some(*account),
                    json!({"channel": "account", "type": "liquidated", "equity": equity.to_string(), "maintenance": maintenance.to_string()}),
                );
            }
            Event::InsuranceTakeover {
                account,
                market,
                size,
                price,
            } => {
                let symbol = market_symbol(s, *market);
                send(
                    s,
                    Channel::Account,
                    None,
                    Some(*account),
                    json!({"channel": "account", "type": "insurance_takeover", "market": symbol, "size": size, "price": price}),
                );
            }
            Event::Bankruptcy { account, deficit } => {
                send(
                    s,
                    Channel::Account,
                    None,
                    Some(*account),
                    json!({"channel": "account", "type": "bankruptcy", "deficit": deficit.to_string()}),
                );
            }
        }
    }
    for (m, (bids, asks)) in deltas {
        let Some(b) = s.book(m) else { continue };
        let seq = {
            let mut b = b.write();
            for (p, q) in &bids {
                if *q == 0 {
                    b.bids.remove(p);
                } else {
                    b.bids.insert(*p, *q);
                }
            }
            for (p, q) in &asks {
                if *q == 0 {
                    b.asks.remove(p);
                } else {
                    b.asks.insert(*p, *q);
                }
            }
            b.seq += 1;
            b.seq
        };
        let symbol = market_symbol(s, m);
        send(
            s,
            Channel::Book,
            Some(m),
            None,
            json!({"channel": "book", "type": "delta", "market": symbol, "seq": seq, "bids": bids, "asks": asks}),
        );
    }
}

fn market_symbol(s: &Shared, m: u16) -> String {
    s.dir.read().markets.get(m as usize).map(|x| x.symbol.clone()).unwrap_or_default()
}

fn asset_token(s: &Shared, a: u16) -> String {
    s.dir.read().assets.get(a as usize).map(|x| x.chain_ref.clone()).unwrap_or_default()
}
