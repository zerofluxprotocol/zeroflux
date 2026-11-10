# Local patch to gline-rs 1.1.0 (Apache-2.0)

Upstream: https://crates.io/crates/gline-rs 1.1.0, source copied unchanged except
`src/model/output/decoded/token.rs`.

Why: `knowledgator/gliner-pii-edge-v1.0` (token-level GLiNER) exports logits as
`(batch, words, classes, 3)`. gline-rs 1.1.0 only accepts
`(3, batch, words, classes)` and fails with "unexpected logits shape".
The patch detects the position-last layout and permutes axes to the expected
layout; the older layout is unchanged.

Second fix (`src/text/tokenizer.rs`, `src/model/input/encoded.rs`): gline-rs
hard-coded DeBERTa's sequence start/end token IDs (1 and 2). ModernBERT-based
GLiNER models use `[CLS]`=50281 and `[SEP]`=50282, so every score came out
negative. The tokenizer now reports its own start/end IDs (defaults unchanged).
