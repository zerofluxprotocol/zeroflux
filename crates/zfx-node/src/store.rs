//! Persistent block store and state snapshots.
//!
//! Block log: one record per line, `zfx2 <sha256(json) hex> <json>\n`,
//! appended and fsynced per block. On load:
//! - a final line without its newline is a torn write from a crash: it is
//!   dropped and the file truncated (normal crash recovery);
//! - any complete line whose checksum or JSON is wrong is corruption: the
//!   node refuses to start and reports the line, unless `--repair` is
//!   given, which truncates the log at the last good record.
//! The node never trusts the log: every block is fully revalidated
//! (PoW, signatures, state transitions, state roots) when loaded.
//!
//! Snapshots: the full consensus state at a height, with the block hash and
//! state root; verified by recomputing the root and comparing it with the
//! header's committed state_root.

use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use zfx_verify::block::Block;
use zfx_verify::hash::sha256;
use zfx_verify::state::{Account, BridgeState, State};

#[derive(Debug)]
pub enum LoadError {
    Corrupt { line: usize, reason: String },
    Io(String),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Corrupt { line, reason } => write!(f, "block log corrupt at line {}: {} (start with --repair to truncate there)", line, reason),
            LoadError::Io(e) => write!(f, "block log I/O error: {}", e),
        }
    }
}

#[derive(Debug, Default, Serialize)]
pub struct LoadReport {
    pub records: usize,
    pub torn_tail_dropped: bool,
    pub repaired_from_line: Option<usize>,
}

pub struct BlockLog {
    path: PathBuf,
    file: std::fs::File,
}

fn record(b: &Block) -> String {
    let j = serde_json::to_string(b).unwrap();
    format!("zfx2 {} {}\n", hex::encode(sha256(&[j.as_bytes()])), j)
}

fn parse(line: &str) -> Result<Block, String> {
    let mut it = line.splitn(3, ' ');
    let (tag, sum, json) = (it.next().unwrap_or(""), it.next().unwrap_or(""), it.next().unwrap_or(""));
    if tag != "zfx2" {
        return Err("bad record tag".into());
    }
    if hex::encode(sha256(&[json.as_bytes()])) != sum {
        return Err("checksum mismatch".into());
    }
    serde_json::from_str(json).map_err(|e| format!("bad JSON: {}", e))
}

impl BlockLog {
    /// Read and validate the log; returns blocks in file order.
    pub fn load(path: &Path, repair: bool) -> Result<(Vec<Block>, LoadReport), LoadError> {
        let mut rep = LoadReport::default();
        let data = match std::fs::read(path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((vec![], rep)),
            Err(e) => return Err(LoadError::Io(e.to_string())),
        };
        let mut blocks = Vec::new();
        let mut good_bytes = 0usize;
        let mut line_no = 0usize;
        let mut rest: &[u8] = &data;
        while !rest.is_empty() {
            line_no += 1;
            let Some(nl) = rest.iter().position(|c| *c == b'\n') else {
                rep.torn_tail_dropped = true; // crash mid-append
                break;
            };
            let line = &rest[..nl];
            match std::str::from_utf8(line).map_err(|e| e.to_string()).and_then(parse) {
                Ok(b) => {
                    blocks.push(b);
                    good_bytes += nl + 1;
                    rest = &rest[nl + 1..];
                }
                Err(reason) => {
                    if !repair {
                        return Err(LoadError::Corrupt { line: line_no, reason });
                    }
                    rep.repaired_from_line = Some(line_no);
                    break;
                }
            }
        }
        if good_bytes != data.len() {
            let f = std::fs::OpenOptions::new().write(true).open(path).map_err(|e| LoadError::Io(e.to_string()))?;
            f.set_len(good_bytes as u64).map_err(|e| LoadError::Io(e.to_string()))?;
            f.sync_all().ok();
        }
        rep.records = blocks.len();
        Ok((blocks, rep))
    }

    pub fn open(path: &Path) -> Result<BlockLog, String> {
        let file = std::fs::OpenOptions::new().create(true).append(true).open(path).map_err(|e| e.to_string())?;
        Ok(BlockLog { path: path.to_path_buf(), file })
    }

    pub fn append(&mut self, b: &Block) -> Result<(), String> {
        self.file.write_all(record(b).as_bytes()).map_err(|e| e.to_string())?;
        self.file.sync_data().map_err(|e| e.to_string())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Serializable consensus state (tuple-keyed maps flattened).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct StateSnap {
    pub accounts: Vec<(String, u64, u64)>,
    pub immature: Vec<(u64, String, u64)>,
    pub supply: String,
    pub bridge: Option<BridgeSnap>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct BridgeSnap {
    pub epoch: u32,
    pub members: Vec<String>,
    pub pending: Option<(Vec<String>, u64)>,
    pub paused: bool,
    pub admin_nonce: u64,
    pub locked_total: String,
    pub released_total: String,
    pub deposits: u64,
    pub releases: u64,
    pub used_withdrawals: Vec<String>,
}

fn addr_hex(a: &[u8; 32]) -> String {
    hex::encode(a)
}

fn addr_from(h: &str) -> Result<[u8; 32], String> {
    hex::decode(h).ok().and_then(|v| v.try_into().ok()).ok_or_else(|| "bad address".to_string())
}

impl StateSnap {
    pub fn from_state(s: &State) -> StateSnap {
        StateSnap {
            accounts: s.accounts.iter().map(|(a, acc)| (addr_hex(a), acc.balance, acc.nonce)).collect(),
            immature: s.immature.iter().map(|((h, a), v)| (*h, addr_hex(a), *v)).collect(),
            supply: s.supply.to_string(),
            bridge: s.bridge.as_ref().map(|b| BridgeSnap {
                epoch: b.epoch,
                members: b.members.iter().map(hex::encode).collect(),
                pending: b.pending.as_ref().map(|(k, h)| (k.iter().map(hex::encode).collect(), *h)),
                paused: b.paused,
                admin_nonce: b.admin_nonce,
                locked_total: b.locked_total.to_string(),
                released_total: b.released_total.to_string(),
                deposits: b.deposits,
                releases: b.releases,
                used_withdrawals: b.used_withdrawals.iter().map(hex::encode).collect(),
            }),
        }
    }

    pub fn to_state(&self) -> Result<State, String> {
        let mut s = State::default();
        for (a, bal, n) in &self.accounts {
            s.accounts.insert(addr_from(a)?, Account { balance: *bal, nonce: *n });
        }
        for (h, a, v) in &self.immature {
            s.immature.insert((*h, addr_from(a)?), *v);
        }
        s.supply = self.supply.parse().map_err(|_| "bad supply")?;
        if let Some(b) = &self.bridge {
            let dec = |v: &Vec<String>| -> Result<Vec<Vec<u8>>, String> { v.iter().map(|h| hex::decode(h).map_err(|e| e.to_string())).collect() };
            let pending = match &b.pending {
                Some((k, h)) => Some((dec(k)?, *h)),
                None => None,
            };
            s.bridge = Some(BridgeState {
                epoch: b.epoch,
                members: dec(&b.members)?,
                pending_activation: pending.as_ref().map(|p| p.1),
                pending,
                paused: b.paused,
                admin_nonce: b.admin_nonce,
                locked_total: b.locked_total.parse().map_err(|_| "bad locked_total")?,
                released_total: b.released_total.parse().map_err(|_| "bad released_total")?,
                deposits: b.deposits,
                releases: b.releases,
                used_withdrawals: b.used_withdrawals.iter().map(|h| addr_from(h)).collect::<Result<_, _>>()?,
            });
        }
        Ok(s)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Snapshot {
    pub format: String,
    pub chain_commitment: String,
    pub height: u64,
    pub block_hash: String,
    pub state_root: String,
    pub state: StateSnap,
    /// SHA256 over the canonical JSON of the fields above.
    pub checksum: String,
}

impl Snapshot {
    pub fn body_hash(&self) -> String {
        let mut c = self.clone();
        c.checksum = String::new();
        hex::encode(sha256(&[serde_json::to_string(&c).unwrap().as_bytes()]))
    }

    pub fn new(chain_commitment: &[u8; 32], height: u64, block_hash: &[u8; 32], st: &State) -> Snapshot {
        let mut s = Snapshot {
            format: "zfx-state-snapshot-v1".into(),
            chain_commitment: hex::encode(chain_commitment),
            height,
            block_hash: hex::encode(block_hash),
            state_root: hex::encode(st.root()),
            state: StateSnap::from_state(st),
            checksum: String::new(),
        };
        s.checksum = s.body_hash();
        s
    }

    /// Integrity: file checksum, recomputed state root, and (if given) the
    /// state_root committed in the block header at `height`.
    pub fn verify(&self, header_state_root: Option<&[u8; 32]>) -> Result<State, String> {
        if self.body_hash() != self.checksum {
            return Err("snapshot checksum mismatch (file corrupted)".into());
        }
        let st = self.state.to_state()?;
        if hex::encode(st.root()) != self.state_root {
            return Err("snapshot state does not hash to its recorded state_root".into());
        }
        if let Some(r) = header_state_root {
            if hex::encode(r) != self.state_root {
                return Err("snapshot state_root differs from the chain's committed state_root".into());
            }
        }
        st.audit()?;
        Ok(st)
    }

    pub fn write_atomic(&self, path: &Path) -> Result<(), String> {
        let tmp = path.with_extension("tmp");
        let mut f = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
        f.write_all(serde_json::to_string_pretty(self).unwrap().as_bytes()).map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())
    }

    pub fn read(path: &Path) -> Result<Snapshot, String> {
        let f = std::fs::File::open(path).map_err(|e| e.to_string())?;
        serde_json::from_reader(BufReader::new(f)).map_err(|e| e.to_string())
    }
}

/// Count lines (for diagnostics).
pub fn count_records(path: &Path) -> usize {
    std::fs::File::open(path).map(|f| BufReader::new(f).lines().count()).unwrap_or(0)
}
