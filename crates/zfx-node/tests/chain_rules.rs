//! Network-rule tests from spec section 8 ("Network"): reorgs, invalid
//! blocks, target enforcement, timestamp rules, double spends, work expiry,
//! restart/recovery. All run on the CPU-only verifier.

use zfx_node::chain::{AddResult, Chain};
use zfx_node::config::{Allocation, ChainConfig};
use zfx_node::node::{now_secs, Node};
use zfx_verify::block::Block;
use zfx_verify::pow;
use zfx_verify::tx::{address_to_string, P2mrKeys};
use zfx_verify::U256;

fn cfg(fixed: bool) -> ChainConfig {
    let mut c = ChainConfig::devnet();
    c.genesis_timestamp = now_secs() - 100_000;
    c.genesis_target = U256::MAX.shr(4).to_hex();
    c.fixed_difficulty = fixed;
    c.coinbase_maturity = 0;
    c
}

fn mine(mut b: Block, chain: &mut Chain) -> Block {
    let anchor = chain.anchor_for(&b.header.parent, b.header.height).unwrap();
    let w = chain.weights_for(&anchor);
    let c = pow::challenge(&b.header);
    let t = b.header.target_u256();
    for n in 0.. {
        if pow::meets_target(&pow::attempt(&w, &c, n), &t) {
            b.header.nonce = n;
            return b;
        }
    }
    unreachable!()
}

/// Template on an arbitrary parent (for building forks).
fn template_on(chain: &Chain, parent: [u8; 32], miner: [u8; 32], ts: u64) -> Block {
    let st = chain.state_at(&parent).unwrap();
    chain.template_on(parent, &st, miner, &[], &[], ts)
}

#[test]
fn linear_chain_and_rewards() {
    let mut ch = Chain::new(cfg(true)).unwrap();
    let m = [1u8; 32];
    for i in 0..5 {
        let b = ch.template(m, &[], now_secs() - 1000 + i);
        let b = mine(b, &mut ch);
        assert!(matches!(ch.add_block(b, now_secs()).unwrap(), AddResult::NewTip { reorg_depth: 0 }));
    }
    assert_eq!(ch.height(), 5);
    assert_eq!(ch.state.account(&m).balance, 5 * ch.cfg.block_subsidy);
    assert_eq!(ch.state.supply, 5 * ch.cfg.block_subsidy as u128);
    ch.state.audit().unwrap();
}

#[test]
fn reorg_to_more_work_and_side_chain_storage() {
    let mut ch = Chain::new(cfg(true)).unwrap();
    let g = ch.genesis_hash;
    let (ma, mb) = ([0xa; 32], [0xb; 32]);
    let base = now_secs() - 5000;
    let mut pa = g;
    for i in 0..3 {
        let b = mine(template_on(&ch, pa, ma, base + i), &mut ch);
        pa = b.hash();
        ch.add_block(b, now_secs()).unwrap();
    }
    let mut pb = g;
    let mut last = None;
    for i in 0..4 {
        let b = mine(template_on(&ch, pb, mb, base + 100 + i), &mut ch);
        pb = b.hash();
        last = Some(ch.add_block(b, now_secs()).unwrap());
        if i < 3 {
            assert_eq!(last.as_ref().unwrap(), &AddResult::SideChain, "equal or less work stays on side chain (first seen wins)");
        }
    }
    assert_eq!(last.unwrap(), AddResult::NewTip { reorg_depth: 3 });
    assert_eq!(ch.best.last().unwrap(), &pb);
    assert_eq!(ch.state.account(&ma).balance, 0, "reorged-out rewards removed");
    assert_eq!(ch.state.account(&mb).balance, 4 * ch.cfg.block_subsidy);
    assert_eq!(ch.reorg_count, 1);
}

#[test]
fn invalid_blocks_rejected() {
    let mut ch = Chain::new(cfg(true)).unwrap();
    let m = [2u8; 32];
    let good = mine(ch.template(m, &[], now_secs() - 50), &mut ch);
    // 1. PoW above target: find a nonce that fails.
    let mut bad = good.clone();
    let anchor = ch.anchor_for(&bad.header.parent, 1).unwrap();
    let w = ch.weights_for(&anchor);
    let c = pow::challenge(&bad.header);
    bad.header.nonce = (0..).find(|n| !pow::meets_target(&pow::attempt(&w, &c, *n), &bad.header.target_u256())).unwrap();
    assert!(ch.add_block(bad, now_secs()).unwrap_err().contains("AboveTarget"));
    // 2. Easier target than required (target enforcement).
    let mut easy = ch.template(m, &[], now_secs() - 50);
    easy.header.target = U256::MAX.shr(1).to_be_bytes();
    let easy = mine(easy, &mut ch);
    assert!(ch.add_block(easy, now_secs()).unwrap_err().contains("target"));
    // 3. Wrong version / profile.
    let mut v = good.clone();
    v.header.version = 1;
    assert!(ch.add_block(v, now_secs()).is_err());
    // 4. Timestamp in the future.
    let fut = mine(ch.template(m, &[], now_secs() + 10_000), &mut ch);
    assert!(ch.add_block(fut, now_secs()).unwrap_err().contains("future"));
    // 5. tx_root mismatch (coinbase changed after mining).
    let mut tr = good.clone();
    tr.coinbase.amount += 1;
    assert!(ch.add_block(tr, now_secs()).unwrap_err().contains("tx_root"));
    // 6. Coinbase overpay with consistent tx_root -> state-invalid.
    let mut over = ch.template(m, &[], now_secs() - 40);
    over.coinbase.amount += 1;
    over.header.tx_root = over.compute_tx_root();
    // (state_root stays the honest one: the block fails on the coinbase rule first)
    let over = mine(over, &mut ch);
    ch.add_block(over.clone(), now_secs()).unwrap();
    assert_eq!(ch.height(), 0, "state-invalid block not connected");
    assert_eq!(ch.entries[&over.hash()].status, zfx_node::chain::Status::Invalid);
    // The honest block still connects.
    ch.add_block(good, now_secs()).unwrap();
    assert_eq!(ch.height(), 1);
    // 7. Timestamp <= median time past.
    let mut old = ch.template(m, &[], 0);
    old.header.timestamp = ch.cfg.genesis_timestamp;
    let old = mine(old, &mut ch);
    assert!(ch.add_block(old, now_secs()).unwrap_err().contains("median"));
}

#[test]
fn difficulty_retarget_is_enforced() {
    let mut c = cfg(false);
    c.target_spacing_secs = 60;
    let mut ch = Chain::new(c).unwrap();
    let m = [3u8; 32];
    let base = now_secs() - 10_000;
    // Blocks arrive every 10 s instead of 60 s: difficulty must rise.
    for i in 0..40u64 {
        let b = mine(template_on(&ch, *ch.best.last().unwrap(), m, base + 10 * i), &mut ch);
        ch.add_block(b, now_secs()).unwrap();
    }
    let t = ch.tip().block.header.target_u256();
    assert!(t < ch.cfg.genesis_target_u256().div_u64(3), "target must tighten under fast blocks");
}

#[test]
fn transactions_double_spend_and_reorg_return() {
    let alice = P2mrKeys::from_seed(&[0xa1; 32]);
    let bob = P2mrKeys::from_seed(&[0xb0; 32]);
    let mut c = cfg(true);
    c.allocations = vec![Allocation { address: address_to_string(&alice.address()), amount: 1_000_000 }];
    let node = Node::new(c, None, Some([9u8; 32]), 0).unwrap();
    let cid = node.lock().chain.cfg.chain_id;
    let t1 = alice.sign_tx(cid, bob.address(), 1000, 10, 0, 0).unwrap();
    let t1b = alice.sign_tx(cid, [7u8; 32], 999_000, 10, 0, 0).unwrap(); // same nonce: double spend
    node.submit_tx(t1.clone(), None).unwrap();
    assert!(node.submit_tx(t1b.clone(), None).unwrap_err().contains("double spend"));
    // Mine t1 via the miner API.
    let w = node.get_work(None).unwrap();
    assert_eq!(w["txs"], 1);
    let wid = w["work_id"].as_str().unwrap().to_string();
    let block = node.lock().works[&wid].clone();
    let mined = {
        let mut g = node.lock();
        mine(block, &mut g.chain)
    };
    let r = node.submit_work(None, &wid, mined.header.nonce);
    assert_eq!(r["accepted"], true, "{}", r);
    {
        let g = node.lock();
        assert_eq!(g.chain.state.account(&bob.address()).balance, 1000);
        assert_eq!(g.chain.state.account(&alice.address()).nonce, 1);
        assert!(g.mempool.txs.is_empty());
    }
    // Block containing the conflicting spend on top is state-invalid.
    let mut dbl = {
        let g = node.lock();
        g.chain.template([9u8; 32], &[], now_secs())
    };
    dbl.txs = vec![t1b.clone()];
    dbl.coinbase.amount += 10;
    dbl.header.tx_root = dbl.compute_tx_root();
    // state_root left as the empty-template root: the block is rejected at connect
    let dbl = {
        let mut g = node.lock();
        mine(dbl, &mut g.chain)
    };
    node.process_block(dbl, None).unwrap();
    assert_eq!(node.lock().chain.height(), 1, "double-spend block rejected at connect");
    // Stale work: tip changed after this get_work.
    let w_old = node.get_work(None).unwrap();
    let g_b = {
        let g = node.lock();
        g.chain.template([9u8; 32], &[], now_secs())
    };
    let g_b = {
        let mut g = node.lock();
        mine(g_b, &mut g.chain)
    };
    node.process_block(g_b, None).unwrap();
    let r = node.submit_work(None, w_old["work_id"].as_str().unwrap(), 0);
    assert!(r["error"].as_str().unwrap().contains("stale"), "{}", r);
    // Reorg from genesis with 3 empty blocks: t1 must return to the mempool.
    let genesis = node.lock().chain.genesis_hash;
    let mut p = genesis;
    for i in 0..3 {
        let b = {
            let g = node.lock();
            template_on(&g.chain, p, [5u8; 32], now_secs() - 900 + i)
        };
        let b = {
            let mut g = node.lock();
            mine(b, &mut g.chain)
        };
        p = b.hash();
        node.process_block(b, None).unwrap();
    }
    let g = node.lock();
    assert_eq!(g.chain.best.last().unwrap(), &p);
    assert_eq!(g.chain.state.account(&bob.address()).balance, 0);
    assert!(g.mempool.txs.contains_key(&t1.txid()), "disconnected tx re-admitted to mempool");
}

#[test]
fn restart_recovery_from_disk() {
    let dir = std::env::temp_dir().join(format!("zfx-restart-{}", rand::random::<u32>()));
    let d = dir.to_str().unwrap().to_string();
    let c = cfg(true);
    let best = {
        let node = Node::new(c.clone(), Some(&d), Some([4u8; 32]), 0).unwrap();
        for i in 0..6 {
            let b = {
                let g = node.lock();
                g.chain.template([4u8; 32], &[], now_secs() - 500 + i)
            };
            let b = {
                let mut g = node.lock();
                mine(b, &mut g.chain)
            };
            node.process_block(b, None).unwrap();
        }
        let g = node.lock();
        *g.chain.best.last().unwrap()
    };
    // Simulate a crash mid-write: append a truncated line.
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().append(true).open(dir.join("blocks.zfxlog")).unwrap();
    f.write_all(b"zfx2 00 {\"header\":{\"vers").unwrap();
    drop(f);
    let node = Node::new(c.clone(), Some(&d), None, 0).unwrap();
    let g = node.lock();
    assert_eq!(g.chain.height(), 6);
    assert_eq!(g.chain.best.last().unwrap(), &best);
    drop(g);
    // A different chain config must refuse the data dir.
    let mut c2 = c.clone();
    c2.chain_id ^= 1;
    assert!(Node::new(c2, Some(&d), None, 0).is_err());
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn mainnet_activation_is_gated_and_schedules_apply() {
    let mut c = cfg(true);
    c.network = "mainnet".into();
    assert!(Chain::new(c).err().unwrap().contains("gated"));
    // A halving schedule is enforced by consensus (coinbase must match it).
    let mut c = cfg(true);
    c.subsidy_schedule = Some(zfx_verify::monetary::SubsidySchedule::Halving { initial: 1000, interval: 2, tail: 1 });
    let mut ch = Chain::new(c).unwrap();
    let m = [8u8; 32];
    for i in 0..5 {
        let b = mine(ch.template(m, &[], now_secs() - 1000 + i), &mut ch);
        ch.add_block(b, now_secs()).unwrap();
    }
    // heights 1..5: 1000,1000,500,500,250
    assert_eq!(ch.state.account(&m).balance, 3250);
    let mut bad = ch.template(m, &[], now_secs());
    bad.coinbase.amount = 250; // height 6 pays 250 under the schedule: valid
    assert_eq!(bad.coinbase.amount, 250);
    bad.coinbase.amount = 500; // overpay
    bad.header.tx_root = bad.compute_tx_root();
    let bad = mine(bad, &mut ch);
    ch.add_block(bad, now_secs()).unwrap();
    assert_eq!(ch.height(), 5, "coinbase above the schedule is rejected at connect");
}
