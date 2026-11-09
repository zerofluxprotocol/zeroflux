# zpu-miner builds. CPU build works anywhere (macOS/Linux, any compiler).
#   make cpu                      -> build/zpu-miner        (CPU only)
#   make cuda CUDA_ARCH="90 100"  -> build/zpu-miner-cuda   (Hopper + Blackwell)
#   make hip  HIP_ARCH="gfx942 gfx950" -> build/zpu-miner-hip (MI300X + MI355X)
CXX ?= c++
NVCC ?= nvcc
HIPCC ?= hipcc
CUDA_ARCH ?= 90 100 120
HIP_ARCH ?= gfx942 gfx950
CXXFLAGS ?= -O3 -std=c++17 -Wall -Wextra
SRC = src/main.cpp src/backend_cpu.cpp

cpu: build/zpu-miner
build/zpu-miner: $(SRC) src/*.h
	mkdir -p build
	$(CXX) $(CXXFLAGS) $(SRC) -o $@ -pthread

cuda: build/zpu-miner-cuda
build/zpu-miner-cuda: $(SRC) src/backend_gpu.cu src/*.h
	mkdir -p build
	$(NVCC) -O3 -std=c++17 -DZPU_WITH_GPU $(foreach a,$(CUDA_ARCH),-gencode arch=compute_$(a),code=sm_$(a)) \
	  -x cu src/main.cpp -x cu src/backend_cpu.cpp src/backend_gpu.cu -o $@ -lcublas -lcublasLt

hip: build/zpu-miner-hip
build/zpu-miner-hip: $(SRC) src/backend_gpu.cu src/*.h
	mkdir -p build
	$(HIPCC) -O3 -std=c++17 -DZPU_WITH_GPU -DZPU_HIP $(foreach a,$(HIP_ARCH),--offload-arch=$(a)) \
	  -x hip src/main.cpp src/backend_cpu.cpp src/backend_gpu.cu -o $@ -lhipblas -lhipblaslt

# AddressSanitizer + UndefinedBehaviorSanitizer CPU build; any report aborts.
sanitize: build/zpu-miner-asan
build/zpu-miner-asan: $(SRC) src/*.h
	mkdir -p build
	$(CXX) -O1 -g -std=c++17 -Wall -Wextra -fsanitize=address,undefined -fno-sanitize-recover=all -fno-omit-frame-pointer $(SRC) -o $@ -pthread

sanitize-test: build/zpu-miner-asan
	ASAN_OPTIONS=detect_leaks=0:abort_on_error=1 UBSAN_OPTIONS=print_stacktrace=1:halt_on_error=1 ./build/zpu-miner-asan selftest ../../tests/vectors/fp8c_v0.1.json --threads 4
	ASAN_OPTIONS=detect_leaks=0:abort_on_error=1 UBSAN_OPTIONS=print_stacktrace=1:halt_on_error=1 ./build/zpu-miner-asan bench --profile dev --count 3000 --threads 4

test: cpu
	./build/zpu-miner selftest ../../tests/vectors/fp8c_v0.1.json

clean:
	rm -rf build
.PHONY: cpu cuda hip test clean sanitize sanitize-test
