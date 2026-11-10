//! JSON shapes of the HTTP API. Amounts that can exceed 2^53 are decimal strings. Batches use
//! exactly the settlement contract's `settle_batch` argument format so the adapter can submit
//! them unchanged; chain events use the contract's event format plus `seq`.

use base64::Engine as _;
use obkit_engine::{Batch, ChainEvent, Key};
use serde::{Deserialize, Serialize};

pub fn key_to_str(k: &Key) -> String {
    format!("ed25519:{}", bs58::encode(k).into_string())
}

pub fn key_from_str(s: &str) -> Result<Key, String> {
    let b58 = s.strip_prefix("ed25519:").ok_or("public key must be ed25519:<base58>")?;
    let v = bs58::decode(b58).into_vec().map_err(|e| format!("public key: {e}"))?;
    v.try_into().map_err(|_| "public key must be 32 bytes".to_string())
}

pub fn sig_from_str(s: &str) -> Result<[u8; 64], String> {
    let v = base64::engine::general_purpose::STANDARD
        .decode(s)
        .map_err(|e| format!("signature: {e}"))?;
    v.try_into().map_err(|_| "signature must be 64 bytes (base64)".to_string())
}

pub fn b64(b: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(b)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TradeDto {
    pub trade_id: u64,
    pub price: u64,
    pub qty: u64,
    pub taker_side: &'static str,
    pub ts_ns: u64,
}

// ---------- batches (contract settle_batch format) ----------

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DeltaDto {
    pub account: String,
    pub token: String,
    pub delta: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct WithdrawDto {
    pub account: String,
    pub token: String,
    pub amount: String,
    pub nonce: String,
    pub public_key: String,
    pub signature: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ForcedDto {
    pub request_id: String,
    pub account: String,
    pub token: String,
    pub amount: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct BatchDto {
    pub id: String,
    pub entries: Vec<DeltaDto>,
    pub withdrawals: Vec<WithdrawDto>,
    pub forced: Vec<ForcedDto>,
    pub seal: bool,
    pub journal_seq: String,
    pub journal_hash: String,
}

impl From<&Batch> for BatchDto {
    fn from(b: &Batch) -> Self {
        BatchDto {
            id: b.id.to_string(),
            entries: b
                .entries
                .iter()
                .map(|e| DeltaDto {
                    account: e.account.clone(),
                    token: e.token.clone(),
                    delta: e.delta.to_string(),
                })
                .collect(),
            withdrawals: b
                .withdrawals
                .iter()
                .map(|w| WithdrawDto {
                    account: w.account.clone(),
                    token: w.token.clone(),
                    amount: w.amount.to_string(),
                    nonce: w.nonce.to_string(),
                    public_key: key_to_str(&w.public_key),
                    signature: b64(&w.signature),
                })
                .collect(),
            forced: b
                .forced
                .iter()
                .map(|f| ForcedDto {
                    request_id: f.request_id.to_string(),
                    account: f.account.clone(),
                    token: f.token.clone(),
                    amount: f.amount.to_string(),
                })
                .collect(),
            seal: b.seal,
            journal_seq: b.journal_seq.to_string(),
            journal_hash: hex::encode(b.journal_hash),
        }
    }
}

// ---------- chain events (contract event format + seq) ----------

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChainEventDto {
    Deposit {
        seq: String,
        account: String,
        token: String,
        amount: String,
    },
    KeyAdded {
        seq: String,
        account: String,
        public_key: String,
    },
    KeyRemoved {
        seq: String,
        account: String,
        public_key: String,
    },
    ForcedWithdrawal {
        seq: String,
        request_id: String,
        account: String,
        token: String,
        amount: String,
    },
    WithdrawalReverted {
        seq: String,
        account: String,
        token: String,
        amount: String,
        #[serde(default)]
        reason: String,
    },
    BatchSettled {
        seq: String,
        batch_id: String,
    },
}

fn num<T: std::str::FromStr>(s: &str, what: &str) -> Result<T, String> {
    s.parse().map_err(|_| format!("{what}: not a number: {s}"))
}

impl TryFrom<&ChainEventDto> for ChainEvent {
    type Error = String;
    fn try_from(e: &ChainEventDto) -> Result<Self, String> {
        Ok(match e {
            ChainEventDto::Deposit {
                seq,
                account,
                token,
                amount,
            } => ChainEvent::Deposit {
                seq: num(seq, "seq")?,
                account: account.clone(),
                token: token.clone(),
                amount: num(amount, "amount")?,
            },
            ChainEventDto::KeyAdded { seq, account, public_key } => ChainEvent::KeyAdded {
                seq: num(seq, "seq")?,
                account: account.clone(),
                public_key: key_from_str(public_key)?,
            },
            ChainEventDto::KeyRemoved { seq, account, public_key } => ChainEvent::KeyRemoved {
                seq: num(seq, "seq")?,
                account: account.clone(),
                public_key: key_from_str(public_key)?,
            },
            ChainEventDto::ForcedWithdrawal {
                seq,
                request_id,
                account,
                token,
                amount,
            } => ChainEvent::ForcedWithdrawal {
                seq: num(seq, "seq")?,
                request_id: num(request_id, "request_id")?,
                account: account.clone(),
                token: token.clone(),
                amount: num(amount, "amount")?,
            },
            ChainEventDto::WithdrawalReverted {
                seq,
                account,
                token,
                amount,
                ..
            } => ChainEvent::WithdrawalReverted {
                seq: num(seq, "seq")?,
                account: account.clone(),
                token: token.clone(),
                amount: num(amount, "amount")?,
            },
            ChainEventDto::BatchSettled { seq, batch_id } => ChainEvent::BatchSettled {
                seq: num(seq, "seq")?,
                batch_id: num(batch_id, "batch_id")?,
            },
        })
    }
}

// ---------- requests ----------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OrderReq {
    pub account: String,
    pub market: String,
    pub side: String,
    pub price: u64,
    pub qty: u64,
    #[serde(default = "gtc")]
    pub tif: String,
    #[serde(default = "cancel_taker")]
    pub stp: String,
    #[serde(default)]
    pub client_id: u64,
    pub ts: u64,
    pub public_key: String,
    pub signature: String,
}

fn gtc() -> String {
    "gtc".into()
}
fn cancel_taker() -> String {
    "cancel_taker".into()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CancelReq {
    pub account: String,
    pub market: String,
    #[serde(default)]
    pub order_id: u64,
    #[serde(default)]
    pub client_id: u64,
    pub ts: u64,
    pub public_key: String,
    pub signature: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CancelAllReq {
    pub account: String,
    #[serde(default)]
    pub market: Option<String>,
    pub ts: u64,
    pub public_key: String,
    pub signature: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WithdrawReq {
    pub account: String,
    pub token: String,
    pub amount: String,
    pub nonce: u64,
    pub public_key: String,
    pub signature: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChainEventsReq {
    pub events: Vec<ChainEventDto>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_roundtrip() {
        let k = [7u8; 32];
        assert_eq!(key_from_str(&key_to_str(&k)).unwrap(), k);
        assert!(key_from_str("secp256k1:abc").is_err());
    }

    #[test]
    fn chain_event_json_matches_contract_format() {
        let j = r#"{"type":"deposit","seq":"3","account":"a.near","token":"usdc.near","amount":"1000000000000000000000000000"}"#;
        let e: ChainEventDto = serde_json::from_str(j).unwrap();
        let c = ChainEvent::try_from(&e).unwrap();
        assert_eq!(
            c,
            ChainEvent::Deposit {
                seq: 3,
                account: "a.near".into(),
                token: "usdc.near".into(),
                amount: 10u128.pow(27)
            }
        );
        let j = r#"{"type":"withdrawal_reverted","seq":"4","account":"a.near","token":"t.near","amount":"5","reason":"bad signature"}"#;
        assert!(matches!(
            ChainEvent::try_from(&serde_json::from_str::<ChainEventDto>(j).unwrap()).unwrap(),
            ChainEvent::WithdrawalReverted { .. }
        ));
    }
}
