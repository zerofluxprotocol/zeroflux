// GPU kernels (device code only). Included by backend_gpu.cu for real
// CUDA/HIP builds and by bench/compile/device_check.cu, which compiles
// every kernel for sm_90/sm_100/sm_120/gfx942/gfx950 without vendor SDKs.
#pragma once
#include "fp8c.h"

namespace zpu {

struct Lut {
    uint8_t b[32];
};

struct Bytes32 {
    uint8_t b[32];
};

__global__ void k_x0(Bytes32 ch, uint64_t start, const uint64_t* nonces, int B, int d_in, Lut lut, uint8_t* X) {
    int i = blockIdx.x * blockDim.x + threadIdx.x;
    if (i >= B) return;
    uint64_t n = nonces ? nonces[i] : start + i;
    uint8_t at[32];
    attempt_id(ch.b, n, at);
    int8_t x[256];
    make_x0(at, d_in, x);
    for (int k = 0; k < d_in; k++) {
        int v = x[k];
        X[(size_t)i * d_in + k] = lut.b[v > 0 ? v - 1 : 15 - v];
    }
}

template <typename T>
__device__ __forceinline__ int32_t as_i32(T v);
template <>
__device__ __forceinline__ int32_t as_i32<int32_t>(int32_t v) { return v; }
template <>
__device__ __forceinline__ int32_t as_i32<float>(float v) { return (int32_t)__float2int_rn(v); }

template <typename T>
__global__ void k_requant(const T* Y, size_t n, uint32_t s, Lut lut, uint8_t* X) {
    size_t i = (size_t)blockIdx.x * blockDim.x + threadIdx.x;
    if (i >= n) return;
    X[i] = lut.b[requant_idx(as_i32(Y[i]), s)];
}

template <typename T>
__global__ void k_digest(const T* Y, int B, int d_out, Bytes32 ch, uint64_t start, const uint64_t* nonces, Bytes32 target,
                         uint64_t* found, uint8_t* found_dig, unsigned int* nfound, int max_found, uint8_t* all_dig) {
    int i = blockIdx.x * blockDim.x + threadIdx.x;
    if (i >= B) return;
    uint64_t n = nonces ? nonces[i] : start + i;
    int32_t row[1024];
    for (int j = 0; j < d_out; j++) row[j] = as_i32(Y[(size_t)i * d_out + j]);
    uint8_t d[32];
    result_digest(ch.b, n, row, d_out, d);
    if (all_dig)
        for (int q = 0; q < 32; q++) all_dig[(size_t)i * 32 + q] = d[q];
    if (below_target(d, target.b)) {
        unsigned int slot = atomicAdd(nfound, 1u);
        if ((int)slot < max_found) {
            found[slot] = n;
            for (int q = 0; q < 32; q++) found_dig[slot * 32 + q] = d[q];
        }
    }
}

// Portable tiled int8 GEMM: Y[b][j] = sum_k X[b][k] * W[j][k]. Exact int32.
__global__ void k_gemm_naive(const int8_t* X, const int8_t* W, int32_t* Y, int B, int N, int K) {
    __shared__ int8_t xs[16][32];
    __shared__ int8_t ws[16][32];
    int tb = threadIdx.y, tj = threadIdx.x;
    int b = blockIdx.y * 16 + tb, j = blockIdx.x * 16 + tj;
    int32_t acc = 0;
    for (int k0 = 0; k0 < K; k0 += 32) {
        for (int q = tj; q < 32; q += 16) {
            xs[tb][q] = (b < B && k0 + q < K) ? X[(size_t)b * K + k0 + q] : 0;
            int jj = blockIdx.x * 16 + tb;
            ws[tb][q] = (jj < N && k0 + q < K) ? W[(size_t)jj * K + k0 + q] : 0;
        }
        __syncthreads();
        for (int q = 0; q < 32; q++) acc += (int32_t)xs[tb][q] * (int32_t)ws[tj][q];
        __syncthreads();
    }
    if (b < B && j < N) Y[(size_t)b * N + j] = acc;
}

}  // namespace zpu
