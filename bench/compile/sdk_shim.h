// Minimal stand-in for the CUDA/HIP runtime headers, covering only what
// kernels.cuh uses, so device code can be compiled with a bare clang
// (-nocudainc / -nogpuinc). Used for compile checks only; real builds
// use the vendor SDK headers.
#pragma once
#define ZPU_SDK_SHIM 1
#define __host__ __attribute__((host))
#define __device__ __attribute__((device))
#define __global__ __attribute__((global))
#define __constant__ __attribute__((constant))
#define __shared__ __attribute__((shared))
#define __forceinline__ __inline__ __attribute__((always_inline))
#if defined(__HIP__)
#define __HIPCC__ 1
struct __zpu_tid { __device__ __forceinline__ unsigned get_x() const { return __builtin_amdgcn_workitem_id_x(); }
                   __device__ __forceinline__ unsigned get_y() const { return __builtin_amdgcn_workitem_id_y(); }
                   __declspec(property(get = get_x)) unsigned x; __declspec(property(get = get_y)) unsigned y; };
struct __zpu_bid { __device__ __forceinline__ unsigned get_x() const { return __builtin_amdgcn_workgroup_id_x(); }
                   __device__ __forceinline__ unsigned get_y() const { return __builtin_amdgcn_workgroup_id_y(); }
                   __declspec(property(get = get_x)) unsigned x; __declspec(property(get = get_y)) unsigned y; };
struct __zpu_bdim { __device__ __forceinline__ unsigned get_x() const { return __builtin_amdgcn_workgroup_size_x(); }
                    __declspec(property(get = get_x)) unsigned x; };
static __device__ const __zpu_tid threadIdx;
static __device__ const __zpu_bid blockIdx;
static __device__ const __zpu_bdim blockDim;
__device__ __forceinline__ void __syncthreads() { __builtin_amdgcn_fence(__ATOMIC_RELEASE, "workgroup"); __builtin_amdgcn_s_barrier(); __builtin_amdgcn_fence(__ATOMIC_ACQUIRE, "workgroup"); }
__device__ __forceinline__ unsigned atomicAdd(unsigned* p, unsigned v) { return __hip_atomic_fetch_add(p, v, __ATOMIC_RELAXED, __HIP_MEMORY_SCOPE_AGENT); }
__device__ __forceinline__ int __float2int_rn(float x) { return (int)__builtin_rintf(x); }
#else
#define __CUDACC__ 1
struct __zpu_tid { __device__ __forceinline__ unsigned get_x() const { return __nvvm_read_ptx_sreg_tid_x(); }
                   __device__ __forceinline__ unsigned get_y() const { return __nvvm_read_ptx_sreg_tid_y(); }
                   __declspec(property(get = get_x)) unsigned x; __declspec(property(get = get_y)) unsigned y; };
struct __zpu_bid { __device__ __forceinline__ unsigned get_x() const { return __nvvm_read_ptx_sreg_ctaid_x(); }
                   __device__ __forceinline__ unsigned get_y() const { return __nvvm_read_ptx_sreg_ctaid_y(); }
                   __declspec(property(get = get_x)) unsigned x; __declspec(property(get = get_y)) unsigned y; };
struct __zpu_bdim { __device__ __forceinline__ unsigned get_x() const { return __nvvm_read_ptx_sreg_ntid_x(); }
                    __declspec(property(get = get_x)) unsigned x; };
static __device__ const __zpu_tid threadIdx;
static __device__ const __zpu_bid blockIdx;
static __device__ const __zpu_bdim blockDim;
// __syncthreads is a clang builtin on NVPTX
__device__ __forceinline__ unsigned atomicAdd(unsigned* p, unsigned v) { return __nvvm_atom_add_gen_i((int*)p, (int)v); }
__device__ __forceinline__ int __float2int_rn(float x) { return __nvvm_f2i_rn(x); }
#endif
