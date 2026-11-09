// Compile-check translation unit: every kernel, instantiated for both
// accumulator element types, compiled for NVIDIA and AMD device targets.
#include "sdk_shim.h"
#include "../../miner/cuda/src/kernels.cuh"
template __global__ void zpu::k_requant<int32_t>(const int32_t*, size_t, uint32_t, zpu::Lut, uint8_t*);
template __global__ void zpu::k_requant<float>(const float*, size_t, uint32_t, zpu::Lut, uint8_t*);
template __global__ void zpu::k_digest<int32_t>(const int32_t*, int, int, zpu::Bytes32, uint64_t, const uint64_t*, zpu::Bytes32, uint64_t*, uint8_t*, unsigned int*, int, uint8_t*);
template __global__ void zpu::k_digest<float>(const float*, int, int, zpu::Bytes32, uint64_t, const uint64_t*, zpu::Bytes32, uint64_t*, uint8_t*, unsigned int*, int, uint8_t*);
