use crate::Error;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use tokio_tungstenite::tungstenite::Message;

/// A sequence gap: a delta arrived out of order. Resubscribe to get a fresh snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gap;

/// A local L2 book rebuilt from a snapshot plus sequenced deltas.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BookState {
    pub seq: u64,
    pub bids: BTreeMap<u64, u64>,
    pub asks: BTreeMap<u64, u64>,
}

impl BookState {
    pub fn best_bid(&self) -> Option<(u64, u64)> {
        self.bids.iter().next_back().map(|(p, q)| (*p, *q))
    }
    pub fn best_ask(&self) -> Option<(u64, u64)> {
        self.asks.iter().next().map(|(p, q)| (*p, *q))
    }

    /// Apply a `book` message. Returns Err(Gap) on a sequence gap; resubscribe to resync.
    pub fn apply(&mut self, v: &Value) -> Result<bool, Gap> {
        let levels = |k: &str| -> Vec<(u64, u64)> { serde_json::from_value(v[k].clone()).unwrap_or_default() };
        let seq = v["seq"].as_u64().unwrap_or(0);
        match v["type"].as_str() {
            Some("snapshot") => {
                self.seq = seq;
                self.bids = levels("bids").into_iter().collect();
                self.asks = levels("asks").into_iter().collect();
                Ok(true)
            }
            Some("delta") => {
                if seq <= self.seq {
                    return Ok(false);
                }
                if seq != self.seq + 1 {
                    return Err(Gap);
                }
                for (side, k) in [(&mut self.bids, "bids"), (&mut self.asks, "asks")] {
                    for (p, q) in levels(k) {
                        if q == 0 {
                            side.remove(&p);
                        } else {
                            side.insert(p, q);
                        }
                    }
                }
                self.seq = seq;
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// A WebSocket connection to a node. `next()` yields every JSON message; `BookFeed` wraps it to
/// keep a gap-checked local book.
pub struct Feed {
    ws: Ws,
}

impl Feed {
    pub async fn connect(base_http: &str) -> Result<Self, Error> {
        let url = format!("{}/v1/ws", base_http.trim_end_matches('/').replacen("http", "ws", 1));
        let (ws, _) = tokio_tungstenite::connect_async(url).await.map_err(|e| Error::Ws(e.to_string()))?;
        Ok(Feed { ws })
    }

    pub async fn send(&mut self, v: Value) -> Result<(), Error> {
        self.ws
            .send(Message::Text(v.to_string().into()))
            .await
            .map_err(|e| Error::Ws(e.to_string()))
    }

    pub async fn subscribe_book(&mut self, market: &str) -> Result<(), Error> {
        self.send(json!({"op": "subscribe", "channel": "book", "market": market})).await
    }

    /// Mark, funding and socialized-loss updates of a perp market.
    pub async fn subscribe_perp(&mut self, market: &str) -> Result<(), Error> {
        self.send(json!({"op": "subscribe", "channel": "perp", "market": market})).await
    }

    pub async fn subscribe_trades(&mut self, market: &str) -> Result<(), Error> {
        self.send(json!({"op": "subscribe", "channel": "trades", "market": market})).await
    }

    pub async fn next(&mut self) -> Result<Value, Error> {
        loop {
            match self.ws.next().await {
                Some(Ok(Message::Text(t))) => return serde_json::from_str(&t).map_err(|e| Error::Decode(e.to_string())),
                Some(Ok(_)) => continue,
                Some(Err(e)) => return Err(Error::Ws(e.to_string())),
                None => return Err(Error::Ws("closed".into())),
            }
        }
    }
}

/// Local order book for one market, kept in sync over WebSocket. Resubscribes on gaps.
pub struct BookFeed {
    feed: Feed,
    market: String,
    pub state: BookState,
    pub resyncs: u64,
}

impl BookFeed {
    pub async fn connect(base_http: &str, market: &str) -> Result<Self, Error> {
        let mut feed = Feed::connect(base_http).await?;
        feed.subscribe_book(market).await?;
        let mut b = BookFeed {
            feed,
            market: market.into(),
            state: BookState::default(),
            resyncs: 0,
        };
        // Wait for the snapshot.
        loop {
            let v = b.feed.next().await?;
            if v["channel"] == "book" && v["type"] == "snapshot" {
                b.state.apply(&v).ok();
                return Ok(b);
            }
        }
    }

    /// Wait for the next book update and apply it. Returns the new sequence number.
    pub async fn update(&mut self) -> Result<u64, Error> {
        loop {
            let v = self.feed.next().await?;
            if v["channel"] != "book" {
                continue;
            }
            match self.state.apply(&v) {
                Ok(true) => return Ok(self.state.seq),
                Ok(false) => continue,
                Err(Gap) => {
                    self.resyncs += 1;
                    self.feed.subscribe_book(&self.market.clone()).await?;
                }
            }
        }
    }

    /// Apply updates until the book reaches at least `seq`.
    pub async fn until_seq(&mut self, seq: u64) -> Result<(), Error> {
        while self.state.seq < seq {
            self.update().await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deltas_apply_in_order_and_gaps_are_detected() {
        let mut b = BookState::default();
        b.apply(&json!({"type": "snapshot", "seq": 5, "bids": [[100, 3]], "asks": [[105, 2]]}))
            .unwrap();
        assert_eq!(
            b.apply(&json!({"type": "delta", "seq": 5, "bids": [[100, 0]], "asks": []})),
            Ok(false)
        );
        b.apply(&json!({"type": "delta", "seq": 6, "bids": [[100, 0], [99, 7]], "asks": []}))
            .unwrap();
        assert_eq!(b.best_bid(), Some((99, 7)));
        assert_eq!(b.apply(&json!({"type": "delta", "seq": 8, "bids": [], "asks": []})), Err(Gap));
    }
}
