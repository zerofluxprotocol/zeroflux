//! STARK feasibility prototype for a trust-minimized ZeroFlux -> NEAR bridge.
//!
//! Proves, with Winterfell (hash-based), R parallel
//! GEMV dot-product accumulations of length K over ZFX operands {+-1..+-16}:
//!   trace columns per row r: x_r, w_r, acc_r
//!   transition: acc_r' = acc_r + x_r' * w_r'      (degree 2)
//!   assertions: acc_r[0] = x_r[0]*w_r[0] (via public first products) and
//!               acc_r[K-1] = y_r (public outputs)
//! This covers only the multiply-accumulate core. A full proof of one
//! ZFX-FP8C attempt additionally needs SHAKE256 weight/input generation,
//! the S-box requantization lookups and SHA-256 digests in-circuit, so the
//! extrapolation below is a LOWER BOUND, labeled as a model.
//!
//!   cargo run --release -p zfx-stark-proto -- [R] [K]

use serde_json::json;
use std::time::Instant;
use winterfell::{
    crypto::{hashers::Blake3_256, DefaultRandomCoin, MerkleTree},
    math::{fields::f128::BaseElement, FieldElement, ToElements},
    matrix::ColMatrix,
    Air, AirContext, Assertion, AuxRandElements, BatchingMethod, CompositionPoly, CompositionPolyTrace, DefaultConstraintCommitment,
    DefaultConstraintEvaluator, DefaultTraceLde, EvaluationFrame, FieldExtension, PartitionOptions, ProofOptions, Prover, StarkDomain, Trace,
    TraceInfo, TracePolyTable, TraceTable, TransitionConstraintDegree,
};

fn fe(v: i64) -> BaseElement {
    if v >= 0 {
        BaseElement::new(v as u128)
    } else {
        -BaseElement::new((-v) as u128)
    }
}

#[derive(Clone)]
pub struct Pub {
    first: Vec<BaseElement>,
    outputs: Vec<BaseElement>,
}

impl ToElements<BaseElement> for Pub {
    fn to_elements(&self) -> Vec<BaseElement> {
        let mut v = self.first.clone();
        v.extend(self.outputs.iter().copied());
        v
    }
}

pub struct GemvAir {
    ctx: AirContext<BaseElement>,
    p: Pub,
    rows: usize,
}

impl Air for GemvAir {
    type BaseField = BaseElement;
    type PublicInputs = Pub;
    fn new(ti: TraceInfo, p: Pub, options: ProofOptions) -> Self {
        let rows = ti.width() / 3;
        let degrees = vec![TransitionConstraintDegree::new(2); rows];
        GemvAir { ctx: AirContext::new(ti, degrees, 2 * rows, options), p, rows }
    }
    fn evaluate_transition<E: FieldElement + From<Self::BaseField>>(&self, f: &EvaluationFrame<E>, _p: &[E], out: &mut [E]) {
        let (c, n) = (f.current(), f.next());
        for r in 0..self.rows {
            out[r] = n[3 * r + 2] - c[3 * r + 2] - n[3 * r] * n[3 * r + 1];
        }
    }
    fn get_assertions(&self) -> Vec<Assertion<Self::BaseField>> {
        let last = self.trace_length() - 1;
        let mut a = Vec::new();
        for r in 0..self.rows {
            a.push(Assertion::single(3 * r + 2, 0, self.p.first[r]));
            a.push(Assertion::single(3 * r + 2, last, self.p.outputs[r]));
        }
        a
    }
    fn context(&self) -> &AirContext<Self::BaseField> {
        &self.ctx
    }
}

struct GemvProver {
    options: ProofOptions,
}

impl Prover for GemvProver {
    type BaseField = BaseElement;
    type Air = GemvAir;
    type Trace = TraceTable<BaseElement>;
    type HashFn = Blake3_256<BaseElement>;
    type VC = MerkleTree<Self::HashFn>;
    type RandomCoin = DefaultRandomCoin<Self::HashFn>;
    type TraceLde<E: FieldElement<BaseField = BaseElement>> = DefaultTraceLde<E, Self::HashFn, Self::VC>;
    type ConstraintCommitment<E: FieldElement<BaseField = BaseElement>> = DefaultConstraintCommitment<E, Self::HashFn, Self::VC>;
    type ConstraintEvaluator<'a, E: FieldElement<BaseField = BaseElement>> = DefaultConstraintEvaluator<'a, GemvAir, E>;

    fn get_pub_inputs(&self, t: &Self::Trace) -> Pub {
        let rows = t.width() / 3;
        let last = t.length() - 1;
        Pub { first: (0..rows).map(|r| t.get(3 * r + 2, 0)).collect(), outputs: (0..rows).map(|r| t.get(3 * r + 2, last)).collect() }
    }
    fn options(&self) -> &ProofOptions {
        &self.options
    }
    fn new_trace_lde<E: FieldElement<BaseField = BaseElement>>(
        &self,
        ti: &TraceInfo,
        m: &ColMatrix<BaseElement>,
        d: &StarkDomain<BaseElement>,
        po: PartitionOptions,
    ) -> (Self::TraceLde<E>, TracePolyTable<E>) {
        DefaultTraceLde::new(ti, m, d, po)
    }
    fn build_constraint_commitment<E: FieldElement<BaseField = BaseElement>>(
        &self,
        c: CompositionPolyTrace<E>,
        n: usize,
        d: &StarkDomain<BaseElement>,
        po: PartitionOptions,
    ) -> (Self::ConstraintCommitment<E>, CompositionPoly<E>) {
        DefaultConstraintCommitment::new(c, n, d, po)
    }
    fn new_evaluator<'a, E: FieldElement<BaseField = BaseElement>>(
        &self,
        air: &'a GemvAir,
        aux: Option<AuxRandElements<E>>,
        cc: winterfell::ConstraintCompositionCoefficients<E>,
    ) -> Self::ConstraintEvaluator<'a, E> {
        DefaultConstraintEvaluator::new(air, aux, cc)
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let rows: usize = args.get(1).and_then(|v| v.parse().ok()).unwrap_or(16);
    let k: usize = args.get(2).and_then(|v| v.parse().ok()).unwrap_or(4096);
    // ZFX operands from the consensus generator (deterministic).
    let w = zfx_verify::pow::Weights::generate(&zfx_verify::pow::FP8C_V01_DEV, &[7; 32]);
    let src: Vec<i64> = w.layers[1].iter().cycle().take(2 * rows * k).map(|v| *v as i64).collect();
    let t0 = Instant::now();
    let mut trace = TraceTable::new(3 * rows, k);
    let mut ys = vec![0i64; rows];
    trace.fill(
        |s| {
            for r in 0..rows {
                let (x, wv) = (src[2 * r * k], src[(2 * r + 1) * k]);
                s[3 * r] = fe(x);
                s[3 * r + 1] = fe(wv);
                s[3 * r + 2] = fe(x * wv);
            }
        },
        |i, s| {
            for r in 0..rows {
                let (x, wv) = (src[2 * r * k + i + 1], src[(2 * r + 1) * k + i + 1]);
                s[3 * r] = fe(x);
                s[3 * r + 1] = fe(wv);
                s[3 * r + 2] += fe(x * wv);
            }
        },
    );
    for r in 0..rows {
        ys[r] = (0..k).map(|i| src[2 * r * k + i] * src[(2 * r + 1) * k + i]).sum();
    }
    let t_trace = t0.elapsed().as_secs_f64();
    let options = ProofOptions::new(32, 8, 0, FieldExtension::None, 8, 31, BatchingMethod::Linear, BatchingMethod::Linear);
    let prover = GemvProver { options };
    let pubs = prover.get_pub_inputs(&trace);
    assert!(pubs.outputs.iter().zip(&ys).all(|(a, b)| *a == fe(*b)), "trace outputs equal exact integer GEMV");
    let t1 = Instant::now();
    let proof = prover.prove(trace).expect("prove");
    let t_prove = t1.elapsed().as_secs_f64();
    let bytes = proof.to_bytes().len();
    let t2 = Instant::now();
    let min = winterfell::AcceptableOptions::MinConjecturedSecurity(95);
    let ok = winterfell::verify::<GemvAir, Blake3_256<BaseElement>, DefaultRandomCoin<Blake3_256<BaseElement>>, MerkleTree<Blake3_256<BaseElement>>>(proof.clone(), pubs.clone(), &min).is_ok();
    let t_verify = t2.elapsed().as_secs_f64();
    // tamper: a wrong output must not verify
    let mut bad = pubs.clone();
    bad.outputs[0] += BaseElement::ONE;
    let rejects = winterfell::verify::<GemvAir, Blake3_256<BaseElement>, DefaultRandomCoin<Blake3_256<BaseElement>>, MerkleTree<Blake3_256<BaseElement>>>(proof, bad, &min).is_err();
    let macs = (rows * k) as f64;
    let attempt_macs = zfx_verify::pow::FP8C_V01.macs_per_attempt() as f64;
    let out = json!({
        "experiment": "stark_gemv_prototype",
        "prover": "winterfell 0.13.1, f128, Blake3-256 Merkle, 32 queries, blowup 8, >= 95-bit conjectured security",
        "rows": rows, "k": k, "macs_proved": macs, "trace_cells": 3 * rows * k,
        "trace_build_s": t_trace, "prove_s": t_prove, "proof_bytes": bytes, "verify_s": t_verify, "verifies": ok, "tampered_output_rejected": rejects,
        "prover_us_per_mac": t_prove * 1e6 / macs,
        "model_lower_bound_one_v01_attempt": {
            "macs": attempt_macs,
            "prover_seconds": t_prove / macs * attempt_macs,
            "note": "MAC core only; SHAKE256 generation of 51.6 M weights + inputs, S-box lookups and SHA-256 are excluded and dominate in practice. Model, not a measurement."
        },
        "host": std::env::consts::ARCH,
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
}
