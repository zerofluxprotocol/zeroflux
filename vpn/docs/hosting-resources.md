# Hosting resources (measured)

Inputs for a hosting cost estimate of the hosted gateway. Measured on 2026-11-10 on the development Mac: Apple M5 Pro, 18 cores, 48 GB RAM, macOS, release build (`cargo run --release -p zfx-vpn-hosted --example measure -- <models dir> <model> 20`). Each row is 20 requests per worker after one warm-up. Raw output: `docs/eval/hosting-2026-11-10/`.

Nothing here was measured on cloud hardware. Apple performance cores are faster per core than typical cloud vCPUs, so treat CPU-seconds as a lower bound until measured on the target instance type.

## What a request costs

Prompt sizes: 40 words (234 characters), 400 words (2.4 KB), 3,000 words (17.7 KB). The prompt contains a name, email, phone, street address and health details, so every detector has work to do.

| Model (disk) | Load time | RSS after load | Peak RSS | 40 words p50 / CPU | 400 words p50 / CPU | 3,000 words p50 / CPU |
|---|---|---|---|---|---|---|
| gliner-pii-base (205 MB) | 1261 ms | 815 MB | 956 MB | 23 ms / 0.09 CPU-s | 197 ms / 0.80 CPU-s | 1632 ms / 6.59 CPU-s |
| gliner-pii-small (86 MB) | 495 ms | 329 MB | 649 MB | 26 ms / 0.10 CPU-s | 224 ms / 0.93 CPU-s | 1844 ms / 7.34 CPU-s |
| gliner-pii-edge (49 MB) | 288 ms | 201 MB | 582 MB | 18 ms / 0.07 CPU-s | 185 ms / 0.71 CPU-s | 1458 ms / 5.67 CPU-s |
| none (deterministic only) | 0 | 33 MB peak (eval run) | | 0.13 ms p50, 1.2 ms p95 (eval corpus) | | |

Latency is detection only (detector called directly). CPU is user + system time of the whole process divided by requests.

## End to end through the hosted gateway

Hosted gateway in process, one tenant, mock upstream on loopback (so model inference time and network are excluded), 400-word prompt unless noted.

| Model | Prompt | Concurrent clients | Throughput | p50 | p95 | CPU per request | RSS |
|---|---|---|---|---|---|---|---|
| gliner-pii-base | 40 words | 1 | 40.5 req/s | 24 ms | 28 ms | 0.10 CPU-s | 585 MB |
| gliner-pii-base | 400 words | 1 | 4.8 req/s | 204 ms | 224 ms | 0.82 CPU-s | 585 MB |
| gliner-pii-base | 400 words | 4 | 5.0 req/s | 772 ms | 949 ms | 0.79 CPU-s | 592 MB |
| gliner-pii-base | 400 words | 8 | 5.0 req/s | 1556 ms | 1858 ms | 0.80 CPU-s | 607 MB |
| gliner-pii-base | 3000 words | 1 | 0.7 req/s | 1519 ms | 1608 ms | 6.08 CPU-s | 609 MB |
| gliner-pii-small | 40 words | 1 | 38.0 req/s | 26 ms | 28 ms | 0.11 CPU-s | 625 MB |
| gliner-pii-small | 400 words | 1 | 4.0 req/s | 253 ms | 271 ms | 0.98 CPU-s | 626 MB |
| gliner-pii-small | 400 words | 4 | 4.1 req/s | 962 ms | 1095 ms | 0.96 CPU-s | 633 MB |
| gliner-pii-small | 400 words | 8 | 4.2 req/s | 1881 ms | 2112 ms | 0.95 CPU-s | 646 MB |
| gliner-pii-small | 3000 words | 1 | 0.5 req/s | 1915 ms | 2245 ms | 7.64 CPU-s | 649 MB |
| gliner-pii-edge | 40 words | 1 | 39.7 req/s | 25 ms | 26 ms | 0.09 CPU-s | 559 MB |
| gliner-pii-edge | 400 words | 1 | 5.4 req/s | 180 ms | 221 ms | 0.72 CPU-s | 559 MB |
| gliner-pii-edge | 400 words | 4 | 5.3 req/s | 737 ms | 877 ms | 0.73 CPU-s | 569 MB |
| gliner-pii-edge | 400 words | 8 | 5.5 req/s | 1450 ms | 1545 ms | 0.72 CPU-s | 580 MB |
| gliner-pii-edge | 3000 words | 1 | 0.7 req/s | 1486 ms | 1601 ms | 5.81 CPU-s | 582 MB |

## What this means for hosting

- **One GLiNER instance runs one request at a time.** The ONNX session sits behind a mutex and uses up to 4 intra-op threads, so throughput stays flat as concurrency rises and latency grows linearly with the queue. CPU per request is about 4x wall time. To serve more traffic, run more instances (processes or sessions), each with its own ~0.6 GB.
- **Cost scales with prompt length, not request count.** Roughly 0.2 CPU-s per 100 words with the base model on this machine. Coding-agent traffic (large tool results and file contents) is at the 3,000-word end or beyond; chat traffic at the low end. A per-token or per-KB price tracks cost better than per request.
- **Model size barely changes the bill.** edge is 10 to 15% cheaper per request than base, small is 15 to 20% more expensive (both are token-level ModernBERT models; base is a span-level DeBERTa model and the most accurate, see docs/evaluation.md). Steady-state RSS after warm-up is 0.56 to 0.65 GB for all three (ONNX Runtime arenas); the smaller models only lower the load-time peak (base 0.96 GB, edge 0.58 GB).
- **Detection adds latency before the first byte.** For a 400-word prompt about 0.2 s here; for long agent prompts 1.5 to 2 s. The local daemon has the same cost on the user's machine.
- **Cloudflare Workers: no.** The gateway is native Rust with ONNX Runtime and a 50 to 200 MB model file, is not compiled to WASM, and needs ~0.6 GB of memory per model instance and CPU-seconds per request. A deterministic-only gateway (no GLiNER) needs little CPU, but this code base does not target WASM (SQLite, Tokio, keyring), so even that would be a port. **Containers or VMs: yes.** Size per model instance: 4 vCPU, 1 GB RAM, plus the model file on local disk. Cloudflare Containers or any VM platform qualifies if its instance size allows that; check the current instance limits before choosing.
- **Without GLiNER** the gateway still blocks critical values (keys, IDs, cards) and tokenizes emails and phone numbers, but misses names, free-text addresses and health details (deterministic accuracy 0.881 vs 0.944 with base on the benchmark).

