// Multi-threaded CPU backend (reference speed path for the C++ miner).
#include <atomic>
#include <mutex>
#include <thread>
#include "backend.h"

namespace zpu {

void generate_weights(const Profile& p, const uint8_t seed[32], int threads, Weights& w) {
    w.profile = p;
    for (int i = 0; i < 32; i++) w.seed[i] = seed[i];
    w.layers.assign(p.n_layers(), {});
    for (int l = 0; l < p.n_layers(); l++) {
        int k = p.layer_k(l), n = p.layer_n(l);
        w.layers[l].assign((size_t)k * n, 0);
        std::vector<std::thread> ts;
        for (int t = 0; t < threads; t++)
            ts.emplace_back([&, t] {
                for (int r = t; r < n; r += threads) weight_row(seed, l, r, k, &w.layers[l][(size_t)r * k]);
            });
        for (auto& t : ts) t.join();
    }
    w.valid = true;
}

static inline int32_t dot(const int8_t* a, const int8_t* b, int k) {
    int32_t acc = 0;
    for (int i = 0; i < k; i++) acc += (int32_t)a[i] * (int32_t)b[i];
    return acc;
}

static void attempt(const Weights& w, const uint8_t ch[32], uint64_t nonce, uint8_t dig[32]) {
    const Profile& p = w.profile;
    uint8_t at[32];
    attempt_id(ch, nonce, at);
    std::vector<int8_t> x(p.d_in), nx;
    make_x0(at, p.d_in, x.data());
    std::vector<int32_t> y;
    for (int l = 0; l < p.n_layers(); l++) {
        int k = p.layer_k(l), n = p.layer_n(l);
        y.assign(n, 0);
        for (int j = 0; j < n; j++) y[j] = dot(x.data(), &w.layers[l][(size_t)j * k], k);
        if (l != p.n_layers() - 1) {
            nx.resize(n);
            for (int j = 0; j < n; j++) nx[j] = requant(y[j], p.layer_shift(l));
            x.swap(nx);
        }
    }
    result_digest(ch, nonce, y.data(), p.d_out, dig);
}

class CpuBackend : public Backend {
    int threads_;
    Weights w_;
    void ensure(const Job& j) {
        bool same = w_.valid && w_.profile.id == j.profile.id;
        for (int i = 0; same && i < 32; i++) same = w_.seed[i] == j.seed[i];
        if (!same) generate_weights(j.profile, j.seed, threads_, w_);
    }

   public:
    explicit CpuBackend(int t) : threads_(t > 0 ? t : 1) {}
    std::string name() const override { return "cpu-cpp x" + std::to_string(threads_); }
    std::vector<Found> run(const Job& job, uint64_t& attempts) override {
        ensure(job);
        std::vector<Found> found;
        std::mutex m;
        std::atomic<uint64_t> done{0};
        std::vector<std::thread> ts;
        for (int t = 0; t < threads_; t++)
            ts.emplace_back([&, t] {
                uint8_t d[32];
                for (uint64_t i = t; i < job.count; i += threads_) {
                    attempt(w_, job.challenge, job.start + i, d);
                    if (below_target(d, job.target)) {
                        std::lock_guard<std::mutex> g(m);
                        Found f{job.start + i, {}};
                        for (int q = 0; q < 32; q++) f.digest[q] = d[q];
                        found.push_back(f);
                    }
                    done++;
                }
            });
        for (auto& t : ts) t.join();
        attempts = done;
        return found;
    }
    std::vector<std::array<uint8_t, 32>> digests(const Job& job, const std::vector<uint64_t>& nonces) override {
        ensure(job);
        std::vector<std::array<uint8_t, 32>> out(nonces.size());
        for (size_t i = 0; i < nonces.size(); i++) attempt(w_, job.challenge, nonces[i], out[i].data());
        return out;
    }
};

std::unique_ptr<Backend> make_cpu_backend(int threads) { return std::unique_ptr<Backend>(new CpuBackend(threads)); }

}  // namespace zpu
