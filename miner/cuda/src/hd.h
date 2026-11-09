// Host/device annotation shared by the CPU, CUDA and HIP builds. The
// consensus primitives (SHA-256, Keccak/SHAKE256, domain mapping, S-box
// requantization) are written once and compiled for every target, so the
// GPU cannot drift from the CPU reference by construction.
#pragma once
#if defined(__CUDACC__) || defined(__HIPCC__)
#define ZPU_HD __host__ __device__ __forceinline__
#define ZPU_CONST __device__ __constant__
#else
#define ZPU_HD inline
#define ZPU_CONST
#endif
#include <stdint.h>
#include <stddef.h>
