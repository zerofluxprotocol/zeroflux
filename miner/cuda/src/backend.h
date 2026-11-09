#pragma once
#include <array>
#include <memory>
#include <string>
#include <vector>
#include "fp8c.h"

namespace zpu {

struct Job {
    Profile profile;
    uint8_t seed[32];
    uint8_t challenge[32];
    uint8_t target[32];
    uint64_t start = 0, count = 0;
    bool stop_on_found = false;
};

struct Found {
    uint64_t nonce;
    uint8_t digest[32];
};

struct Weights {
    Profile profile{};
    uint8_t seed[32] = {0};
    bool valid = false;
    std::vector<std::vector<int8_t>> layers;  // [N][K] row-major
};

void generate_weights(const Profile& p, const uint8_t seed[32], int threads, Weights& w);

class Backend {
   public:
    virtual ~Backend() {}
    virtual std::string name() const = 0;
    // Mine [start, start+count); returns winners; sets attempts done.
    virtual std::vector<Found> run(const Job& job, uint64_t& attempts) = 0;
    // Digests for arbitrary nonces (correctness checks / vectors).
    virtual std::vector<std::array<uint8_t, 32>> digests(const Job& job, const std::vector<uint64_t>& nonces) = 0;
    // Raw GEMM Y[B][N] = X[B][K] * W[N][K]^T on this backend's real GEMM path
    // (operands in D). Returns int64 results; empty if unsupported.
    virtual std::vector<int64_t> raw_gemm(const std::vector<int8_t>&, const std::vector<int8_t>&, int, int, int) { return {}; }
};

std::unique_ptr<Backend> make_cpu_backend(int threads);
#if defined(ZPU_WITH_GPU)
// gemm: "blas-int8" (cuBLAS/hipBLAS int8->int32), "lt-fp8" (cuBLASLt /
// hipBLASLt FP8 E4M3 or E4M3FNUZ -> FP32), "naive" (portable kernel).
std::unique_ptr<Backend> make_gpu_backend(int device, const std::string& gemm, int batch);
#endif

}  // namespace zpu
