// GPU backend for NVIDIA (CUDA: Hopper sm_90, Blackwell sm_100/sm_120) and
// AMD (ROCm/HIP: MI300X gfx942, MI355X gfx950) from one source.
//
// Per batch of B nonces:
//   k_x0       attempt_id + SHAKE256 -> X0[B][d_in]           (consensus code shared with CPU)
//   GEMM       Y[B][N] = X[B][K] * W[N][K]^T                   (vendor BLAS int8, Lt FP8, or naive)
//   k_requant  Y -> X (S-box requantization)                    (shared code)
//   k_digest   SHA-256 over final Y rows, compare with target   (shared code)
//
// Operand storage is a byte LUT over the 32 domain values: int8 two's
// complement, OCP E4M3 (NVIDIA sm_89+, AMD gfx950) or E4M3FNUZ (AMD
// gfx942). Every value in D is exact in all three, and every accumulation
// is < 2^24, so any correct FP32/INT32 accumulator yields the canonical
// integers. Hardware with a reduced-precision accumulator (e.g. FP8 MMA
// without FP32 promotion) fails the max-magnitude self-test and is refused.

#include <algorithm>
#include <array>
#include <chrono>
#include <cstring>
#include <string>
#include <vector>
#include "backend.h"
#include "gpu_compat.h"
#include "kernels.cuh"

namespace zpu {

enum class Gemm { BlasInt8, LtFp8, Naive };
enum class Store { Int8, E4M3, E4M3FNUZ };

static uint8_t encode(Store s, int v) {
    if (s == Store::Int8) return (uint8_t)(int8_t)v;
    int a = v < 0 ? -v : v;  // 1..16
    int e = 0;
    while ((a >> (e + 1)) != 0) e++;
    int m = ((a << 3) >> e) - 8;  // exact for |v| <= 16 (<= 4 significant bits)
    int bias = (s == Store::E4M3) ? 7 : 8;
    return (uint8_t)((v < 0 ? 0x80 : 0) | ((e + bias) << 3) | m);
}

class GpuBackend : public Backend {
    int dev_;
    Gemm gemm_;
    Store store_;
    int batch_;
    std::string name_;
    Profile prof_{};
    uint8_t seed_[32] = {0};
    bool loaded_ = false;
    Lut lut_{};
    std::vector<uint8_t*> dW_;
    uint8_t *dXa_ = nullptr, *dXb_ = nullptr;
    void* dY_ = nullptr;
    uint64_t *dFound_ = nullptr, *dNonces_ = nullptr;
    uint8_t *dFoundDig_ = nullptr, *dAllDig_ = nullptr;
    unsigned int* dNFound_ = nullptr;
    blasHandle_t blas_{};
    ltHandle_t lt_{};
    void* dWork_ = nullptr;
    size_t workBytes_ = 64u << 20;
    static const int MAX_FOUND = 64;

    void alloc(const Profile& p) {
        for (auto w : dW_) GPU_CHECK(gpuFree(w));
        dW_.clear();
        for (void* q : {(void*)dXa_, (void*)dXb_, dY_, (void*)dNonces_, (void*)dAllDig_})
            if (q) GPU_CHECK(gpuFree(q));
        size_t maxw = p.hidden > p.d_in ? p.hidden : p.d_in;
        GPU_CHECK(gpuMalloc(&dXa_, (size_t)batch_ * maxw));
        GPU_CHECK(gpuMalloc(&dXb_, (size_t)batch_ * maxw));
        GPU_CHECK(gpuMalloc(&dY_, (size_t)batch_ * maxw * 4));
        GPU_CHECK(gpuMalloc(&dNonces_, (size_t)batch_ * 8));
        GPU_CHECK(gpuMalloc(&dAllDig_, (size_t)batch_ * 32));
    }

    void ensure(const Job& j) {
        if (loaded_ && prof_.id == j.profile.id && std::memcmp(seed_, j.seed, 32) == 0) return;
        if (j.profile.d_out > 1024 || j.profile.d_in > 256) {
            std::fprintf(stderr, "profile exceeds GPU kernel limits\n");
            std::exit(5);
        }
        if (!loaded_ || prof_.id != j.profile.id) alloc(j.profile);
        Weights w;
        generate_weights(j.profile, j.seed, 16, w);  // CPU gen (shared consensus code), then upload
        for (auto q : dW_) GPU_CHECK(gpuFree(q));
        dW_.clear();
        for (int l = 0; l < j.profile.n_layers(); l++) {
            std::vector<uint8_t> enc(w.layers[l].size());
            for (size_t i = 0; i < enc.size(); i++) enc[i] = encode(store_, w.layers[l][i]);
            uint8_t* d;
            GPU_CHECK(gpuMalloc(&d, enc.size()));
            GPU_CHECK(gpuMemcpy(d, enc.data(), enc.size(), gpuMemcpyHostToDevice));
            dW_.push_back(d);
        }
        prof_ = j.profile;
        std::memcpy(seed_, j.seed, 32);
        loaded_ = true;
    }

    // Y[B][N] (int32 or float) = X[B][K] * W[N][K]^T
    void gemm(const uint8_t* X, const uint8_t* W, int B, int N, int K) {
        if (gemm_ == Gemm::Naive) {
            dim3 blk(16, 16), grd((N + 15) / 16, (B + 15) / 16);
            k_gemm_naive<<<grd, blk>>>((const int8_t*)X, (const int8_t*)W, (int32_t*)dY_, B, N, K);
            GPU_CHECK(gpuGetLastError());
        } else if (gemm_ == Gemm::BlasInt8) {
            // Column-major view: Y^T (N x B) = W' (N x K) * X^T (K x B), with
            // W stored as col-major K x N (op T) and X as col-major K x B.
            int32_t alpha = 1, beta = 0;
            BLAS_CHECK(blasGemmEx(blas_, BLAS_OP_T, BLAS_OP_N, N, B, K, &alpha, W, BLAS_R_8I, K, X, BLAS_R_8I, K, &beta, dY_,
                                  BLAS_R_32I, N, BLAS_COMPUTE_32I, BLAS_GEMM_DEFAULT));
        } else {
            ltDataType_t ft = store_ == Store::E4M3FNUZ ? LT_R_8F_E4M3_FNUZ : LT_R_8F_E4M3;
            ltMatmulDesc_t desc;
            BLAS_CHECK(ltMatmulDescCreate(&desc, BLAS_COMPUTE_32F, BLAS_R_32F));
            ltOperation_t ta = BLAS_OP_T, tb = BLAS_OP_N;
            BLAS_CHECK(ltMatmulDescSetAttribute(desc, LT_DESC_TRANSA, &ta, sizeof(ta)));
            BLAS_CHECK(ltMatmulDescSetAttribute(desc, LT_DESC_TRANSB, &tb, sizeof(tb)));
            ltMatrixLayout_t la, lb, lc;
            BLAS_CHECK(ltMatrixLayoutCreate(&la, ft, K, N, K));
            BLAS_CHECK(ltMatrixLayoutCreate(&lb, ft, K, B, K));
            BLAS_CHECK(ltMatrixLayoutCreate(&lc, BLAS_R_32F, N, B, N));
            ltMatmulPreference_t pref;
            BLAS_CHECK(ltMatmulPreferenceCreate(&pref));
            BLAS_CHECK(ltMatmulPreferenceSetAttribute(pref, LT_PREF_MAX_WORKSPACE, &workBytes_, sizeof(workBytes_)));
            ltMatmulHeuristicResult_t heur;
            int got = 0;
            BLAS_CHECK(ltMatmulAlgoGetHeuristic(lt_, desc, la, lb, lc, lc, pref, 1, &heur, &got));
            if (got == 0) {
                std::fprintf(stderr, "no FP8 GEMM algorithm for this device/layout (UNSUPPORTED)\n");
                std::exit(6);
            }
            float alpha = 1.f, beta = 0.f;
            BLAS_CHECK(ltMatmul(lt_, desc, &alpha, W, la, X, lb, &beta, dY_, lc, dY_, lc, &heur.algo, dWork_, workBytes_, 0));
        }
    }

    void forward(int B) {
        const Profile& p = prof_;
        // FP8 Lt needs dimensions that are multiples of 16; padded rows are
        // computed and ignored (rows are independent).
        if (gemm_ == Gemm::LtFp8) B = (B + 15) / 16 * 16;
        uint8_t* x = dXa_;
        uint8_t* nx = dXb_;
        for (int l = 0; l < p.n_layers(); l++) {
            int k = p.layer_k(l), n = p.layer_n(l);
            gemm(x, dW_[l], B, n, k);
            if (l != p.n_layers() - 1) {
                size_t tot = (size_t)B * n;
                int th = 256;
                unsigned blocks = (unsigned)std::min<size_t>((tot + th * 8 - 1) / (th * 8), 65535);
                if (gemm_ == Gemm::LtFp8)
                    k_requant<float><<<blocks, th>>>((const float*)dY_, tot, p.layer_shift(l), lut_, nx);
                else
                    k_requant<int32_t><<<blocks, th>>>((const int32_t*)dY_, tot, p.layer_shift(l), lut_, nx);
                GPU_CHECK(gpuGetLastError());
                std::swap(x, nx);
            }
        }
    }

    void launch_digest(int B, const Job& j, const uint64_t* nonces, uint8_t* all) {
        Bytes32 ch, tg;
        std::memcpy(ch.b, j.challenge, 32);
        std::memcpy(tg.b, j.target, 32);
        int th = 64;
        if (gemm_ == Gemm::LtFp8)
            k_digest<float><<<(B + th - 1) / th, th>>>((const float*)dY_, B, prof_.d_out, ch, j.start, nonces, tg, dFound_, dFoundDig_, dNFound_, MAX_FOUND, all);
        else
            k_digest<int32_t><<<(B + th - 1) / th, th>>>((const int32_t*)dY_, B, prof_.d_out, ch, j.start, nonces, tg, dFound_, dFoundDig_, dNFound_, MAX_FOUND, all);
        GPU_CHECK(gpuGetLastError());
    }

    void launch_x0(int B, const Job& j, uint64_t start, const uint64_t* nonces) {
        Bytes32 ch;
        std::memcpy(ch.b, j.challenge, 32);
        int th = 64;
        k_x0<<<(B + th - 1) / th, th>>>(ch, start, nonces, B, prof_.d_in, lut_, dXa_);
        GPU_CHECK(gpuGetLastError());
    }

   public:
    GpuBackend(int dev, const std::string& g, int batch) : dev_(dev), batch_((batch + 15) / 16 * 16) {
        GPU_CHECK(gpuSetDevice(dev));
        gpuDeviceProp prop;
        GPU_CHECK(gpuGetDeviceProperties(&prop, dev));
        std::string arch;
#if defined(ZPU_HIP)
        arch = prop.gcnArchName;
#else
        arch = "sm_" + std::to_string(prop.major) + std::to_string(prop.minor);
#endif
        gemm_ = g == "naive" ? Gemm::Naive : g == "lt-fp8" ? Gemm::LtFp8 : Gemm::BlasInt8;
        store_ = Store::Int8;
        if (gemm_ == Gemm::LtFp8) store_ = arch.find("gfx94") != std::string::npos ? Store::E4M3FNUZ : Store::E4M3;
        for (int u = 0; u < 32; u++) lut_.b[u] = encode(store_, u < 16 ? u + 1 : 15 - u);
        name_ = std::string(ZPU_VENDOR) + ":" + prop.name + ":" + arch + ":" + g +
                (store_ == Store::E4M3FNUZ ? ":e4m3fnuz" : store_ == Store::E4M3 ? ":e4m3" : ":int8");
        BLAS_CHECK(blasCreate(&blas_));
        BLAS_CHECK(ltCreate(&lt_));
        GPU_CHECK(gpuMalloc(&dWork_, workBytes_));
        GPU_CHECK(gpuMalloc(&dFound_, MAX_FOUND * 8));
        GPU_CHECK(gpuMalloc(&dFoundDig_, MAX_FOUND * 32));
        GPU_CHECK(gpuMalloc(&dNFound_, 4));
    }
    std::string name() const override { return name_; }

    std::vector<Found> run(const Job& job, uint64_t& attempts) override {
        ensure(job);
        std::vector<Found> out;
        attempts = 0;
        for (uint64_t off = 0; off < job.count; off += batch_) {
            int B = (int)std::min<uint64_t>(batch_, job.count - off);
            GPU_CHECK(gpuMemset(dNFound_, 0, 4));
            Job jj = job;
            jj.start = job.start + off;
            launch_x0(B, jj, jj.start, nullptr);
            forward(B);
            launch_digest(B, jj, nullptr, nullptr);
            unsigned int nf = 0;
            GPU_CHECK(gpuMemcpy(&nf, dNFound_, 4, gpuMemcpyDeviceToHost));
            nf = nf > MAX_FOUND ? MAX_FOUND : nf;
            if (nf) {
                std::vector<uint64_t> ns(nf);
                std::vector<uint8_t> ds(nf * 32);
                GPU_CHECK(gpuMemcpy(ns.data(), dFound_, nf * 8, gpuMemcpyDeviceToHost));
                GPU_CHECK(gpuMemcpy(ds.data(), dFoundDig_, nf * 32, gpuMemcpyDeviceToHost));
                for (unsigned i = 0; i < nf; i++) {
                    Found f{ns[i], {}};
                    std::memcpy(f.digest, &ds[i * 32], 32);
                    out.push_back(f);
                }
            }
            attempts += B;
            if (job.stop_on_found && !out.empty()) break;
        }
        GPU_CHECK(gpuDeviceSynchronize());
        return out;
    }

    std::string profile(const Job& job, int B) override {
        ensure(job);
        B = std::min(B, batch_);
        auto now = [] { GPU_CHECK(gpuDeviceSynchronize()); return std::chrono::steady_clock::now(); };
        auto ms = [](std::chrono::steady_clock::time_point a, std::chrono::steady_clock::time_point b) { return std::chrono::duration<double, std::milli>(b - a).count(); };
        std::string out = "{\"B\":" + std::to_string(B);
        for (int rep = 0; rep < 2; rep++) {  // rep 0 warms caches and heuristics
            GPU_CHECK(gpuMemset(dNFound_, 0, 4));
            auto t0 = now();
            launch_x0(B, job, job.start, nullptr);
            auto t1 = now();
            double g = 0, rq = 0;
            uint8_t* x = dXa_;
            uint8_t* nx = dXb_;
            int Bg = gemm_ == Gemm::LtFp8 ? (B + 15) / 16 * 16 : B;
            for (int l = 0; l < prof_.n_layers(); l++) {
                auto a = now();
                gemm(x, dW_[l], Bg, prof_.layer_n(l), prof_.layer_k(l));
                auto b = now();
                g += ms(a, b);
                if (l != prof_.n_layers() - 1) {
                    size_t tot = (size_t)B * prof_.layer_n(l);
                    unsigned blocks = (unsigned)std::min<size_t>((tot + 2047) / 2048, 65535);
                    if (gemm_ == Gemm::LtFp8) k_requant<float><<<blocks, 256>>>((const float*)dY_, tot, prof_.layer_shift(l), lut_, nx);
                    else k_requant<int32_t><<<blocks, 256>>>((const int32_t*)dY_, tot, prof_.layer_shift(l), lut_, nx);
                    auto c = now();
                    rq += ms(b, c);
                    std::swap(x, nx);
                }
            }
            auto t2 = now();
            launch_digest(B, job, nullptr, nullptr);
            auto t3 = now();
            if (rep == 1) {
                double tot = ms(t0, t3);
                out += ",\"x0_ms\":" + std::to_string(ms(t0, t1)) + ",\"gemm_ms\":" + std::to_string(g) + ",\"requant_ms\":" + std::to_string(rq) +
                       ",\"digest_ms\":" + std::to_string(ms(t2, t3)) + ",\"total_ms\":" + std::to_string(tot) +
                       ",\"gemm_tmac_s\":" + std::to_string(B * (double)prof_.macs() / (g / 1e3) / 1e12) +
                       ",\"attempts_per_s_stage_sum\":" + std::to_string(B / (tot / 1e3));
            }
        }
        return out + ",\"backend\":\"" + name_ + "\"}";
    }

    std::vector<int64_t> raw_gemm(const std::vector<int8_t>& X, const std::vector<int8_t>& W, int B, int N, int K) override {
        int Bp = gemm_ == Gemm::LtFp8 ? (B + 15) / 16 * 16 : B;
        std::vector<uint8_t> ex((size_t)Bp * K, encode(store_, 1)), ew(W.size());
        for (size_t i = 0; i < X.size(); i++) ex[i] = encode(store_, X[i]);
        for (size_t i = 0; i < W.size(); i++) ew[i] = encode(store_, W[i]);
        uint8_t *dx, *dw;
        void* dy;
        GPU_CHECK(gpuMalloc(&dx, ex.size()));
        GPU_CHECK(gpuMalloc(&dw, ew.size()));
        GPU_CHECK(gpuMalloc(&dy, (size_t)Bp * N * 4));
        GPU_CHECK(gpuMemcpy(dx, ex.data(), ex.size(), gpuMemcpyHostToDevice));
        GPU_CHECK(gpuMemcpy(dw, ew.data(), ew.size(), gpuMemcpyHostToDevice));
        void* saveY = dY_;
        dY_ = dy;
        gemm(dx, dw, Bp, N, K);
        dY_ = saveY;
        GPU_CHECK(gpuDeviceSynchronize());
        std::vector<int64_t> out((size_t)B * N);
        if (gemm_ == Gemm::LtFp8) {
            std::vector<float> y((size_t)Bp * N);
            GPU_CHECK(gpuMemcpy(y.data(), dy, y.size() * 4, gpuMemcpyDeviceToHost));
            for (size_t i = 0; i < out.size(); i++) out[i] = (int64_t)y[i];
        } else {
            std::vector<int32_t> y((size_t)Bp * N);
            GPU_CHECK(gpuMemcpy(y.data(), dy, y.size() * 4, gpuMemcpyDeviceToHost));
            for (size_t i = 0; i < out.size(); i++) out[i] = y[i];
        }
        GPU_CHECK(gpuFree(dx));
        GPU_CHECK(gpuFree(dw));
        GPU_CHECK(gpuFree(dy));
        return out;
    }

    std::vector<std::array<uint8_t, 32>> digests(const Job& job, const std::vector<uint64_t>& nonces) override {
        ensure(job);
        std::vector<std::array<uint8_t, 32>> out(nonces.size());
        for (size_t off = 0; off < nonces.size(); off += batch_) {
            int B = (int)std::min<size_t>(batch_, nonces.size() - off);
            GPU_CHECK(gpuMemcpy(dNonces_, &nonces[off], (size_t)B * 8, gpuMemcpyHostToDevice));
            GPU_CHECK(gpuMemset(dNFound_, 0, 4));
            launch_x0(B, job, 0, dNonces_);
            forward(B);
            Job jj = job;
            std::memset(jj.target, 0, 32);
            launch_digest(B, jj, dNonces_, dAllDig_);
            std::vector<uint8_t> d((size_t)B * 32);
            GPU_CHECK(gpuMemcpy(d.data(), dAllDig_, d.size(), gpuMemcpyDeviceToHost));
            for (int i = 0; i < B; i++) std::memcpy(out[off + i].data(), &d[(size_t)i * 32], 32);
        }
        return out;
    }
};

std::unique_ptr<Backend> make_gpu_backend(int device, const std::string& gemm, int batch) {
    return std::unique_ptr<Backend>(new GpuBackend(device, gemm, batch));
}

}  // namespace zpu
