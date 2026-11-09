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

// Requantization. The S-box index is data-dependent, so a __constant__
// lookup serializes across a warp on NVIDIA (measured: 79% of batch time on
// H100). Each block stages the fused 256-entry table (S-box -> 5-bit index ->
// storage byte) in shared memory and walks the tensor with a grid-stride loop.
template <typename T>
__global__ void k_requant(const T* Y, size_t n, uint32_t s, Lut lut, uint8_t* X) {
    __shared__ uint8_t tab[256];
    for (unsigned i = threadIdx.x; i < 256; i += blockDim.x) tab[i] = lut.b[AES_SBOX[i] >> 3];
    __syncthreads();
    size_t stride = (size_t)gridDim.x * blockDim.x;
    for (size_t i = (size_t)blockIdx.x * blockDim.x + threadIdx.x; i < n; i += stride)
        X[i] = tab[(uint8_t)((as_i32(Y[i]) >> s) & 0xff)];
}

template <typename T>
__global__ void k_digest(const T* Y, int B, int d_out, Bytes32 ch, uint64_t start, const uint64_t* nonces, Bytes32 target,
                         uint64_t* found, uint8_t* found_dig, unsigned int* nfound, int max_found, uint8_t* all_dig) {
    int i = blockIdx.x * blockDim.x + threadIdx.x;
    if (i >= B) return;
    uint64_t n = nonces ? nonces[i] : start + i;
    // Stream the output row into SHA-256 (same bytes as result_digest, no
    // per-thread 4 KiB scratch copy).
    Sha256 h;
    h.init();
    absorb_str(h, RESULT_DOMAIN);
    h.update(ch.b, 32);
    uint8_t nb[8];
    le64(n, nb);
    h.update(nb, 8);
    const T* yr = Y + (size_t)i * d_out;
    for (int j = 0; j < d_out; j++) {
        uint8_t b4[4];
        le32((uint32_t)as_i32(yr[j]), b4);
        h.update(b4, 4);
    }
    uint8_t d[32];
    h.final(d);
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
