# ZPU miner line protocol (v1)

`zpu-cli` (the client that talks to a node) drives any miner process over stdin/stdout, one JSON object per line. Any language or vendor can implement a miner. Two exist besides the built-in Rust backend: `miner/cuda/build/zpu-miner*` (C++, CPU/CUDA/HIP) and `miner/python/zpu_miner.py` (NumPy, PyTorch, MLX).

Integers that can exceed 2^53 (`nonce_start`, `nonce_count`, `nonce`, `attempts`) are decimal strings.

## Requests (client to miner)

```
{"cmd":"info"}
{"cmd":"job", "profile":{id,d_in,hidden,n_hidden,d_out,shift_in,shift_hidden},
 "epoch_seed":"<32B hex>", "challenge":"<32B hex>", "target":"<32B hex, big-endian>",
 "nonce_start":"<u64>", "nonce_count":"<u64>", "stop_on_found":true}
{"cmd":"digest", ...same fields..., "nonces":["<u64>", ...]}
{"cmd":"quit"}
```

## Responses (miner to client)

```
{"event":"info", "backend":"cuda:NVIDIA H100:sm_90:blas-int8:int8", "impl":"zpu-miner-cpp", "protocol":1}
{"event":"found", "nonce":"<u64>", "digest":"<hex>"}            zero or more per job
{"event":"done", "attempts":"<u64>", "elapsed_s":<float>}        exactly one per job
{"event":"digests", "digests":["<hex>", ...]}                    reply to digest
{"event":"error", "message":"..."}
```

The miner derives the weights itself from `epoch_seed` (CONSENSUS.md 3.4) and caches them until the seed changes. `zpu-cli` re-verifies every `found` digest and `--spot-check N` random digests per job on its own CPU reference. It exits on the first mismatch.
