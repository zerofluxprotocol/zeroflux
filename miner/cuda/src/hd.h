// Host/device annotation shared by the CPU, CUDA and HIP builds. The
// consensus primitives (SHA-256, Keccak/SHAKE256, domain mapping, S-box
// requantization) are written once and compiled for every target, so the
// GPU cannot drift from the CPU reference by construction.
#pragma once
#include <stdint.h>
#include <stddef.h>
// hipcc does not implicitly include the HIP runtime in .cpp sources
// (nvcc does include cuda_runtime.h), so pull in the vendor header here.
// ZPU_SDK_SHIM is set by bench/compile/sdk_shim.h for SDK-free checks.
#if defined(__HIPCC__) && !defined(ZPU_SDK_SHIM)
#include <hip/hip_runtime.h>
#elif defined(__CUDACC__) && !defined(ZPU_SDK_SHIM)
#include <cuda_runtime.h>
#endif
#if defined(__CUDACC__) || defined(__HIPCC__)
#define ZPU_HD __host__ __device__ __forceinline__
#define ZPU_CONST __device__ __constant__
#else
#define ZPU_HD inline
#define ZPU_CONST
#endif
