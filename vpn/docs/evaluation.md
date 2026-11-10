# Evaluation

Reproduce:
```sh
python3 -I scripts/gen_eval_corpus.py > tests/fixtures/eval_corpus.jsonl
promptvpn model install edge && promptvpn model install small && promptvpn model install base
promptvpn eval --configs all --out docs/eval/results.json
```
Raw results: `docs/eval/results-2026-11-08.json`. The corpus file is generated (not committed) because its fake keys trip secret scanners.

## Setup

- Corpus: 270 synthetic cases (seed 7), all values fake. 103 critical labels (SSNs incl. invalid ranges, undelimited and dotted, 8 API key formats, cards, PEM/OpenSSH keys, passwords, JWT, URL credentials, passports, driver's licenses), structured PII (emails in prose/code/markdown/JSON, phones, addresses, DOB, health, IBAN, accounts), **free-text PII** (lowercase names, names without cue words, addresses without street numbers, conditions in casual prose, spelled-out dates and numbers), registered exact values, **custom entity types** ("insurance policy number", "employee id"), encoding and Unicode evasion, tool arguments, tool results, logs, prompt injection, a ~20 KB context, 25 hard negatives, 15 "hard" variants.
- Policy: defaults (names allowed, PII tokenized, critical blocked), destination `EXTERNAL_UNVERIFIED`, fallback block-and-review; custom entities tokenized.
- Hardware: Apple M-series Mac, CPU only (ONNX Runtime, 4 threads).

## Results (2026-11-08)

| config | critical escapes | decision accuracy | false-block rate | utility kept | p50 ms | p95 ms | model download |
|---|---|---|---|---|---|---|---|
| deterministic only | 1/103 | 0.881 | 0.000 | 0.891 | 0.2 | 1.7 | 0 |
| + GLiNER edge | 1/103 | 0.922 | 0.000 | 0.908 | 10.6 | 17.1 | 49 MB |
| + GLiNER small | 1/103 | 0.922 | 0.006 | 0.891 | 19.5 | 27.8 | 86 MB |
| **+ GLiNER base (default)** | **1/103** | **0.944** | **0.006** | **0.912** | **20.9** | **28.1** | **205 MB** |

Recall / precision for the categories the model is there for:

| category | deterministic | + GLiNER base |
|---|---|---|
| PERSON | 0.26 / 1.00 | 0.89 / 0.69 |
| HEALTH_INFORMATION | 0.52 / 1.00 | 0.86 / 0.90 |
| ADDRESS | 0.65 / 1.00 | 0.71 / 1.00 |
| DOB | 0.90 / 1.00 | 1.00 / 0.91 |
| custom: employee id | 0 | 1.00 / 0.67 |
| custom: insurance policy number | 0 | 0.33 / 1.00 |

Critical categories (government IDs, cards, keys, private keys, passwords, passports, licenses) are 0.98–1.00 recall at 1.00 precision in both configurations: the deterministic layer carries them.

## What the numbers say

- **Rules carry the critical protection.** The one escape is an SSN spelled out in words. No configuration catches it.
- **GLiNER is what makes free-text PII work.** Names go from 26% to 89% recall, health details from 52% to 86%. Base beats small and edge on accuracy, and it is not slower than small (its DeBERTa encoder is span-level and efficient on short prompts).
- **Precision cost is in names.** 31% of GLiNER's PERSON spans are not people in the label set (product or place names). Names are allowed by default, so this does not block anything; it only matters if you set names to tokenize.
- **Custom entity types work unevenly.** "employee id" was found every time; "insurance policy number" only a third of the time, because GLiNER often prefers a built-in label for ID-like strings. Model-only critical labels below 0.8 confidence go to review instead of hard-blocking, so such mislabels cost a review, not a broken request.
- **Free-form addresses stay hard**: 4 of 6 addresses without a street number were missed by every configuration.

## Removed configurations

An earlier run on the same corpus measured two generative classifiers through Ollama: Qwen3Guard-Gen-0.6B (0 of 8 custom topics, 0.9% false blocks, 82 ms) and LFM3-1.7B (7 of 8 topics, 13% false blocks, 0.8–3 s). Neither returned reliable spans, so both were removed; see `docs/deviations.md`.

## Caveats

- The corpus was written alongside the recognizers; deterministic recall is optimistic. Zero escapes on a finite corpus is not a bound.
- Latency is detection time in-process. Policy evaluation adds well under a millisecond; HTTP and the upstream model dominate end to end.
- Required for each release: rerun all configurations with `sh scripts/check.sh --models` (fails on more than one critical escape or above 2% false blocks).
