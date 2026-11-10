//! cargo run -p pv-detectors --features gliner --example gliner_smoke -- <dir>
use pv_detectors::span::SpanModel;
fn main() {
    let dir = std::env::args().nth(1).expect("dir");
    let m = pv_detectors::span::gliner_impl::GlinerModel::load(&format!("{dir}/tokenizer.json"), &format!("{dir}/model_quint8.onnx"), 0.5, "gliner", &std::env::args().nth(2).unwrap_or_else(|| "token_level".into())).expect("load");
    for t in [
        "Please update the account for Maria Gonzalez (SSN 123-45-6789) and email maria.g@example.com, phone 415-555-0134.",
        "Patient John Smith was diagnosed with type 2 diabetes and prescribed metformin. He lives at 42 Oak Street, Springfield.",
        "The quarterly revenue grew 12% at Acme Corp.",
        "What is the capital of France?",
        "Our unannounced Orion-7 accelerator hits 3.2 PFLOPS FP8 with 192GB HBM4, tape-out is March 2027. Summarize for the board.",
    ] {
        let t0 = std::time::Instant::now();
        let f = m.detect(t, &[]).expect("detect");
        println!("--- {:?}", t0.elapsed());
        for x in f {
            println!("{:<18} {:.2} {:?}", x.category, x.confidence, &t[x.start_byte..x.end_byte]);
        }
    }
}
