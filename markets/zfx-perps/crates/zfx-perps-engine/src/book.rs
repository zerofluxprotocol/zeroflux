//! One market's order book.
//!
//! Orders live in a slab (`Vec<Node>` plus a free list) and are linked into a FIFO per price
//! level. Levels sit in a `BTreeMap` per side. Insert, cancel and fill are O(log levels) for the
//! level lookup plus O(1) list surgery; matching walks the best level's list front to back.

use crate::types::{AccountId, OrderId, Side};
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const NIL: u32 = u32::MAX;

/// One side of the book in priority order: (price, [(order id, account, remaining, client id)]).
pub type L3 = Vec<(u64, Vec<(OrderId, AccountId, u64, u64)>)>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Node {
    pub id: OrderId,
    pub account: AccountId,
    pub side: Side,
    pub price: u64,
    /// Remaining quantity in lots.
    pub rem: u64,
    pub client_id: u64,
    pub ts_ns: u64,
    prev: u32,
    next: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Level {
    pub qty: u64,
    pub count: u32,
    head: u32,
    tail: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Book {
    pub bids: BTreeMap<u64, Level>,
    pub asks: BTreeMap<u64, Level>,
    nodes: Vec<Node>,
    free: Vec<u32>,
    index: FxHashMap<OrderId, u32>,
}

impl Book {
    fn levels_mut(&mut self, side: Side) -> &mut BTreeMap<u64, Level> {
        match side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        }
    }

    pub fn levels(&self, side: Side) -> &BTreeMap<u64, Level> {
        match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        }
    }

    pub fn best_bid(&self) -> Option<u64> {
        self.bids.last_key_value().map(|(p, _)| *p)
    }

    pub fn best_ask(&self) -> Option<u64> {
        self.asks.first_key_value().map(|(p, _)| *p)
    }

    /// Best price on `side` (highest bid / lowest ask).
    pub fn best(&self, side: Side) -> Option<u64> {
        match side {
            Side::Buy => self.best_bid(),
            Side::Sell => self.best_ask(),
        }
    }

    pub fn len(&self) -> usize {
        self.index.len()
    }

    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    pub fn slot(&self, id: OrderId) -> Option<u32> {
        self.index.get(&id).copied()
    }

    pub fn node(&self, slot: u32) -> &Node {
        &self.nodes[slot as usize]
    }

    pub fn get(&self, id: OrderId) -> Option<&Node> {
        self.slot(id).map(|s| self.node(s))
    }

    /// First order slot at `price` on `side`, or NIL.
    pub fn head(&self, side: Side, price: u64) -> u32 {
        self.levels(side).get(&price).map_or(NIL, |l| l.head)
    }

    pub fn next_slot(&self, slot: u32) -> u32 {
        self.nodes[slot as usize].next
    }

    /// Append an order at the back of its price level.
    #[allow(clippy::too_many_arguments)]
    pub fn insert(&mut self, id: OrderId, account: AccountId, side: Side, price: u64, rem: u64, client_id: u64, ts_ns: u64) -> u32 {
        let node = Node {
            id,
            account,
            side,
            price,
            rem,
            client_id,
            ts_ns,
            prev: NIL,
            next: NIL,
        };
        let slot = match self.free.pop() {
            Some(s) => {
                self.nodes[s as usize] = node;
                s
            }
            None => {
                self.nodes.push(node);
                (self.nodes.len() - 1) as u32
            }
        };
        let level = self.levels_mut(side).entry(price).or_insert(Level {
            qty: 0,
            count: 0,
            head: NIL,
            tail: NIL,
        });
        let tail = level.tail;
        level.qty += rem;
        level.count += 1;
        level.tail = slot;
        if tail == NIL {
            level.head = slot;
        }
        if tail != NIL {
            self.nodes[tail as usize].next = slot;
            self.nodes[slot as usize].prev = tail;
        }
        self.index.insert(id, slot);
        slot
    }

    /// Unlink an order and free its slot. Returns the removed node and the level's remaining qty.
    pub fn remove(&mut self, slot: u32) -> (Node, u64) {
        let n = self.nodes[slot as usize].clone();
        if n.prev != NIL {
            self.nodes[n.prev as usize].next = n.next;
        }
        if n.next != NIL {
            self.nodes[n.next as usize].prev = n.prev;
        }
        let levels = self.levels_mut(n.side);
        let level = levels.get_mut(&n.price).expect("level of a live order");
        if level.head == slot {
            level.head = n.next;
        }
        if level.tail == slot {
            level.tail = n.prev;
        }
        level.qty -= n.rem;
        level.count -= 1;
        let left = level.qty;
        if level.count == 0 {
            levels.remove(&n.price);
        }
        self.index.remove(&n.id);
        self.free.push(slot);
        (n, left)
    }

    /// Reduce a resting order by `qty` lots (a fill). Returns (order remaining, level remaining).
    /// The caller removes the order when its remaining quantity reaches zero.
    pub fn reduce(&mut self, slot: u32, qty: u64) -> (u64, u64) {
        let n = &mut self.nodes[slot as usize];
        n.rem -= qty;
        let rem = n.rem;
        let (side, price) = (n.side, n.price);
        let level = self.levels_mut(side).get_mut(&price).expect("level of a live order");
        level.qty -= qty;
        (rem, level.qty)
    }

    /// Top `depth` levels as (price, qty), best first.
    pub fn depth(&self, side: Side, depth: usize) -> Vec<(u64, u64)> {
        match side {
            Side::Buy => self.bids.iter().rev().take(depth).map(|(p, l)| (*p, l.qty)).collect(),
            Side::Sell => self.asks.iter().take(depth).map(|(p, l)| (*p, l.qty)).collect(),
        }
    }

    /// Orders on one side in priority order: (price, [(id, account, rem, client_id)]).
    pub fn l3(&self, side: Side) -> L3 {
        let walk = |(p, l): (&u64, &Level)| {
            let mut v = Vec::with_capacity(l.count as usize);
            let mut s = l.head;
            while s != NIL {
                let n = &self.nodes[s as usize];
                v.push((n.id, n.account, n.rem, n.client_id));
                s = n.next;
            }
            (*p, v)
        };
        match side {
            Side::Buy => self.bids.iter().rev().map(walk).collect(),
            Side::Sell => self.asks.iter().map(walk).collect(),
        }
    }

    /// Check list/level/index consistency. Test helper; O(n).
    pub fn check(&self) -> Result<(), String> {
        let mut seen = 0usize;
        for side in [Side::Buy, Side::Sell] {
            for (p, l) in self.levels(side) {
                let (mut qty, mut count, mut s, mut prev) = (0u64, 0u32, l.head, NIL);
                while s != NIL {
                    let n = &self.nodes[s as usize];
                    if n.price != *p || n.side != side || n.prev != prev || n.rem == 0 {
                        return Err(format!("bad node {} at {p}", n.id));
                    }
                    if self.index.get(&n.id) != Some(&s) {
                        return Err(format!("index mismatch {}", n.id));
                    }
                    qty += n.rem;
                    count += 1;
                    prev = s;
                    s = n.next;
                }
                if prev != l.tail || qty != l.qty || count != l.count || count == 0 {
                    return Err(format!("bad level {p}: qty {qty}/{} count {count}/{}", l.qty, l.count));
                }
                seen += count as usize;
            }
        }
        if seen != self.index.len() {
            return Err(format!("index has {} orders, levels {seen}", self.index.len()));
        }
        if let (Some(b), Some(a)) = (self.best_bid(), self.best_ask())
            && b >= a
        {
            return Err(format!("crossed book {b} >= {a}"));
        }
        Ok(())
    }
}
