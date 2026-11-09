// One source, two vendors. ZPU_HIP selects ROCm (hipcc), otherwise CUDA.
#pragma once
#if defined(ZPU_HIP)
#include <hip/hip_runtime.h>
#ifndef HIPBLAS_V2
#define HIPBLAS_V2
#endif
#include <hipblas/hipblas.h>
#include <hipblaslt/hipblaslt.h>
#define gpuError_t hipError_t
#define gpuSuccess hipSuccess
#define gpuGetErrorString hipGetErrorString
#define gpuMalloc hipMalloc
#define gpuFree hipFree
#define gpuMemcpy hipMemcpy
#define gpuMemcpyAsync hipMemcpyAsync
#define gpuMemset hipMemset
#define gpuMemcpyHostToDevice hipMemcpyHostToDevice
#define gpuMemcpyDeviceToHost hipMemcpyDeviceToHost
#define gpuDeviceSynchronize hipDeviceSynchronize
#define gpuSetDevice hipSetDevice
#define gpuGetDeviceProperties hipGetDeviceProperties
#define gpuMemGetInfo hipMemGetInfo
#define gpuDeviceProp hipDeviceProp_t
#define gpuGetLastError hipGetLastError
#define gpuStream_t hipStream_t
// BLAS (int8 -> int32)
#define blasHandle_t hipblasHandle_t
#define blasCreate hipblasCreate
#define blasDestroy hipblasDestroy
#define blasStatus_t hipblasStatus_t
#define BLAS_STATUS_SUCCESS HIPBLAS_STATUS_SUCCESS
#define BLAS_OP_T HIPBLAS_OP_T
#define BLAS_OP_N HIPBLAS_OP_N
#define BLAS_R_8I HIP_R_8I
#define BLAS_R_32I HIP_R_32I
#define BLAS_R_32F HIP_R_32F
#define BLAS_COMPUTE_32I HIPBLAS_COMPUTE_32I
#define BLAS_COMPUTE_32F HIPBLAS_COMPUTE_32F
#define BLAS_GEMM_DEFAULT HIPBLAS_GEMM_DEFAULT
#define blasGemmEx hipblasGemmEx
// Lt (FP8)
#define ltHandle_t hipblasLtHandle_t
#define ltCreate hipblasLtCreate
#define ltMatmulDesc_t hipblasLtMatmulDesc_t
#define ltMatmulDescCreate hipblasLtMatmulDescCreate
#define ltMatmulDescSetAttribute hipblasLtMatmulDescSetAttribute
#define LT_DESC_TRANSA HIPBLASLT_MATMUL_DESC_TRANSA
#define LT_DESC_TRANSB HIPBLASLT_MATMUL_DESC_TRANSB
#define ltMatrixLayout_t hipblasLtMatrixLayout_t
#define ltMatrixLayoutCreate hipblasLtMatrixLayoutCreate
#define ltMatmulPreference_t hipblasLtMatmulPreference_t
#define ltMatmulPreferenceCreate hipblasLtMatmulPreferenceCreate
#define ltMatmulPreferenceSetAttribute hipblasLtMatmulPreferenceSetAttribute
#define LT_PREF_MAX_WORKSPACE HIPBLASLT_MATMUL_PREF_MAX_WORKSPACE_BYTES
#define ltMatmulHeuristicResult_t hipblasLtMatmulHeuristicResult_t
#define ltMatmulAlgoGetHeuristic hipblasLtMatmulAlgoGetHeuristic
#define ltMatmul hipblasLtMatmul
#define ltOperation_t hipblasOperation_t
#define ltDataType_t hipDataType
#define LT_R_8F_E4M3 HIP_R_8F_E4M3
#define LT_R_8F_E4M3_FNUZ HIP_R_8F_E4M3_FNUZ
#define ZPU_VENDOR "rocm"
#else
#include <cublasLt.h>
#include <cublas_v2.h>
#include <cuda_runtime.h>
#define gpuError_t cudaError_t
#define gpuSuccess cudaSuccess
#define gpuGetErrorString cudaGetErrorString
#define gpuMalloc cudaMalloc
#define gpuFree cudaFree
#define gpuMemcpy cudaMemcpy
#define gpuMemcpyAsync cudaMemcpyAsync
#define gpuMemset cudaMemset
#define gpuMemcpyHostToDevice cudaMemcpyHostToDevice
#define gpuMemcpyDeviceToHost cudaMemcpyDeviceToHost
#define gpuDeviceSynchronize cudaDeviceSynchronize
#define gpuSetDevice cudaSetDevice
#define gpuGetDeviceProperties cudaGetDeviceProperties
#define gpuMemGetInfo cudaMemGetInfo
#define gpuDeviceProp cudaDeviceProp
#define gpuGetLastError cudaGetLastError
#define gpuStream_t cudaStream_t
#define blasHandle_t cublasHandle_t
#define blasCreate cublasCreate
#define blasDestroy cublasDestroy
#define blasStatus_t cublasStatus_t
#define BLAS_STATUS_SUCCESS CUBLAS_STATUS_SUCCESS
#define BLAS_OP_T CUBLAS_OP_T
#define BLAS_OP_N CUBLAS_OP_N
#define BLAS_R_8I CUDA_R_8I
#define BLAS_R_32I CUDA_R_32I
#define BLAS_R_32F CUDA_R_32F
#define BLAS_COMPUTE_32I CUBLAS_COMPUTE_32I
#define BLAS_COMPUTE_32F CUBLAS_COMPUTE_32F
#define BLAS_GEMM_DEFAULT CUBLAS_GEMM_DEFAULT
#define blasGemmEx cublasGemmEx
#define ltHandle_t cublasLtHandle_t
#define ltCreate cublasLtCreate
#define ltMatmulDesc_t cublasLtMatmulDesc_t
#define ltMatmulDescCreate cublasLtMatmulDescCreate
#define ltMatmulDescSetAttribute cublasLtMatmulDescSetAttribute
#define LT_DESC_TRANSA CUBLASLT_MATMUL_DESC_TRANSA
#define LT_DESC_TRANSB CUBLASLT_MATMUL_DESC_TRANSB
#define ltMatrixLayout_t cublasLtMatrixLayout_t
#define ltMatrixLayoutCreate cublasLtMatrixLayoutCreate
#define ltMatmulPreference_t cublasLtMatmulPreference_t
#define ltMatmulPreferenceCreate cublasLtMatmulPreferenceCreate
#define ltMatmulPreferenceSetAttribute cublasLtMatmulPreferenceSetAttribute
#define LT_PREF_MAX_WORKSPACE CUBLASLT_MATMUL_PREF_MAX_WORKSPACE_BYTES
#define ltMatmulHeuristicResult_t cublasLtMatmulHeuristicResult_t
#define ltMatmulAlgoGetHeuristic cublasLtMatmulAlgoGetHeuristic
#define ltMatmul cublasLtMatmul
#define ltOperation_t cublasOperation_t
#define ltDataType_t cudaDataType_t
#define LT_R_8F_E4M3 CUDA_R_8F_E4M3
#define LT_R_8F_E4M3_FNUZ CUDA_R_8F_E4M3 /* no FNUZ on NVIDIA */
#define ZPU_VENDOR "cuda"
#endif

#include <cstdio>
#include <cstdlib>
#define GPU_CHECK(x)                                                                         \
    do {                                                                                     \
        gpuError_t e_ = (x);                                                                 \
        if (e_ != gpuSuccess) {                                                              \
            std::fprintf(stderr, "GPU error %s at %s:%d\n", gpuGetErrorString(e_), __FILE__, __LINE__); \
            std::exit(3);                                                                    \
        }                                                                                    \
    } while (0)
#define BLAS_CHECK(x)                                                                        \
    do {                                                                                     \
        int s_ = (int)(x);                                                                   \
        if (s_ != 0) {                                                                       \
            std::fprintf(stderr, "BLAS error %d at %s:%d\n", s_, __FILE__, __LINE__);        \
            std::exit(4);                                                                    \
        }                                                                                    \
    } while (0)
