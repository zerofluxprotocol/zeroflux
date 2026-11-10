// zpu-miner: ZeroFlux ZFX-FP8C miner (C++; CPU, CUDA, HIP backends).
//
//   zpu-miner [--backend cpu|gpu] [--device N] [--gemm blas-int8|lt-fp8|naive]
//             [--batch B] [--threads T]                       ZPU line protocol on stdin/stdout
//   zpu-miner selftest VECTORS.json [backend options]        bit-exactness vs published vectors
//   zpu-miner bench [--profile v01|dev] [--count N] [backend options]
//
// Protocol: see miner/PROTOCOL.md. Driven by `zpu-cli mine --backend exec`.

#include <sys/resource.h>
#include <chrono>
#include <cstring>
#include <fstream>
#include <iostream>
#include <sstream>
#include <thread>
#include "backend.h"
#include "json_mini.h"

using namespace zpu;

static std::string hex(const uint8_t* b, size_t n) {
    static const char* d = "0123456789abcdef";
    std::string s;
    for (size_t i = 0; i < n; i++) { s += d[b[i] >> 4]; s += d[b[i] & 15]; }
    return s;
}
static void unhex32(const std::string& s, uint8_t out[32]) {
    if (s.size() != 64) throw std::runtime_error("expected 32-byte hex");
    for (int i = 0; i < 32; i++) out[i] = (uint8_t)std::stoi(s.substr(2 * i, 2), nullptr, 16);
}

static std::string argv_val(int argc, char** argv, const char* k, const char* def) {
    for (int i = 1; i + 1 < argc; i++) if (!std::strcmp(argv[i], k)) return argv[i + 1];
    return def;
}

static std::unique_ptr<Backend> make(int argc, char** argv) {
    std::string b = argv_val(argc, argv, "--backend", "cpu");
    int threads = std::stoi(argv_val(argc, argv, "--threads", std::to_string(std::thread::hardware_concurrency()).c_str()));
    if (b == "cpu") return make_cpu_backend(threads);
#if defined(ZPU_WITH_GPU)
    if (b == "gpu")
        return make_gpu_backend(std::stoi(argv_val(argc, argv, "--device", "0")), argv_val(argc, argv, "--gemm", "blas-int8"),
                                std::stoi(argv_val(argc, argv, "--batch", "4096")));
#endif
    std::cerr << "backend " << b << " not built into this binary (UNSUPPORTED)\n";
    std::exit(2);
}

static Job job_from(const zj::Value& v) {
    Job j;
    const zj::Value& p = v["profile"];
    j.profile.id = (uint32_t)p["id"].i();
    j.profile.d_in = (int)p["d_in"].i();
    j.profile.hidden = (int)p["hidden"].i();
    j.profile.n_hidden = (int)p["n_hidden"].i();
    j.profile.d_out = (int)p["d_out"].i();
    j.profile.shift_in = (uint32_t)p["shift_in"].i();
    j.profile.shift_hidden = (uint32_t)p["shift_hidden"].i();
    j.profile.prf = prf_for_profile(j.profile.id);
    unhex32(v["epoch_seed"].s(), j.seed);
    unhex32(v["challenge"].s(), j.challenge);
    if (v["target"].kind == zj::Value::Str) unhex32(v["target"].s(), j.target); else std::memset(j.target, 0, 32);
    j.start = v["nonce_start"].kind ? v["nonce_start"].u64() : 0;
    j.count = v["nonce_count"].kind ? v["nonce_count"].u64() : 0;
    j.stop_on_found = v["stop_on_found"].kind == zj::Value::Bool && v["stop_on_found"].b;
    return j;
}

static int serve(Backend& b) {
    std::string line;
    while (std::getline(std::cin, line)) {
        if (line.empty()) continue;
        try {
            zj::Value v = zj::parse(line);
            std::string cmd = v["cmd"].s();
            if (cmd == "info") {
                std::cout << "{\"event\":\"info\",\"backend\":\"" << b.name() << "\",\"impl\":\"zpu-miner-cpp\",\"protocol\":1}" << std::endl;
            } else if (cmd == "job") {
                Job j = job_from(v);
                auto t0 = std::chrono::steady_clock::now();
                uint64_t attempts = 0;
                auto found = b.run(j, attempts);
                double el = std::chrono::duration<double>(std::chrono::steady_clock::now() - t0).count();
                for (auto& f : found)
                    std::cout << "{\"event\":\"found\",\"nonce\":\"" << f.nonce << "\",\"digest\":\"" << hex(f.digest, 32) << "\"}" << std::endl;
                std::cout << "{\"event\":\"done\",\"attempts\":\"" << attempts << "\",\"elapsed_s\":" << el << "}" << std::endl;
            } else if (cmd == "digest") {
                Job j = job_from(v);
                std::vector<uint64_t> ns;
                for (auto& n : v["nonces"].arr) ns.push_back(n.u64());
                auto ds = b.digests(j, ns);
                std::cout << "{\"event\":\"digests\",\"digests\":[";
                for (size_t i = 0; i < ds.size(); i++) std::cout << (i ? "," : "") << "\"" << hex(ds[i].data(), 32) << "\"";
                std::cout << "]}" << std::endl;
            } else if (cmd == "quit") {
                return 0;
            } else {
                std::cout << "{\"event\":\"error\",\"message\":\"unknown cmd\"}" << std::endl;
            }
        } catch (std::exception& e) {
            std::cout << "{\"event\":\"error\",\"message\":\"" << e.what() << "\"}" << std::endl;
        }
    }
    return 0;
}

static int selftest(Backend& b, const std::string& path) {
    std::ifstream f(path);
    if (!f) { std::cerr << "cannot open " << path << "\n"; return 2; }
    std::stringstream ss;
    ss << f.rdbuf();
    zj::Value v = zj::parse(ss.str());
    int pass = 0, fail = 0;
    // Edge cases: byte map and requantization table (pure arithmetic).
    const zj::Value& e = v["edge"];
    for (size_t i = 0; e.kind && i < e["byte_to_val"].arr.size(); i++)
        (byte_to_val((uint8_t)i) == e["byte_to_val"].arr[i].i() ? pass : fail)++;
    for (auto& c : (e.kind ? e["requant_cases"].arr : std::vector<zj::Value>{})) {
        int32_t y = (int32_t)c["y"].i();
        (requant(y, 0) == c["s0"].i() && requant(y, 2) == c["s2"].i() ? pass : fail)++;
    }
    for (const char* key : {"dev", "v01", "near_v1"}) {
        if (!v[key].kind) continue;
        const zj::Value& pv = v[key];
        zj::Value jv;
        jv.kind = zj::Value::Obj;
        jv.obj["profile"] = pv["profile"];
        jv.obj["epoch_seed"] = pv["epoch_seed"];
        jv.obj["challenge"] = pv["challenge"];
        Job j = job_from(jv);
        std::vector<uint64_t> ns;
        for (auto& a : pv["attempts"].arr) ns.push_back(a["nonce"].u64());
        auto ds = b.digests(j, ns);
        for (size_t i = 0; i < ns.size(); i++) {
            bool ok = hex(ds[i].data(), 32) == pv["attempts"].arr[i]["digest"].s();
            (ok ? pass : fail)++;
            std::cout << (ok ? "PASS " : "FAIL ") << key << " nonce=" << ns[i] << " " << b.name() << "\n";
        }
    }
    std::cout << pass << "/" << (pass + fail) << " checks bit-exact on " << b.name() << "\n";
    return fail ? 1 : 0;
}

static long peak_rss_kb() {
    struct rusage ru;
    getrusage(RUSAGE_SELF, &ru);
#if defined(__APPLE__)
    return ru.ru_maxrss / 1024;  // bytes on macOS
#else
    return ru.ru_maxrss;  // KiB on Linux
#endif
}

static std::string compiler_id() {
#if defined(__clang__)
    return std::string("clang ") + __clang_version__;
#elif defined(__GNUC__)
    return "gcc " + std::to_string(__GNUC__) + "." + std::to_string(__GNUC_MINOR__);
#else
    return "unknown";
#endif
}

// Benchmark JSON (spec A4): throughput, launch overhead (first small batch,
// includes weight upload / plan creation), host + device memory, toolchain,
// and an acceptance check: digests for sample nonces recomputed on the CPU
// reference backend must match bit for bit.
static int bench(Backend& b, int argc, char** argv) {
    Job j;
    std::string pn = argv_val(argc, argv, "--profile", "v01");
    j.profile = pn == "dev" ? FP8C_V01_DEV : pn == "near" ? NEAR_V1 : FP8C_V01;
    std::memset(j.seed, 0x5e, 32);
    std::memset(j.challenge, 0xc4, 32);
    std::memset(j.target, 0, 32);
    j.count = std::stoull(argv_val(argc, argv, "--count", j.profile.hidden > 1024 ? "2000" : "200000"));
    uint64_t a = 0;
    Job warm = j;
    warm.count = 16;
    auto tw = std::chrono::steady_clock::now();
    b.run(warm, a);
    double launch = std::chrono::duration<double>(std::chrono::steady_clock::now() - tw).count();
    auto t0 = std::chrono::steady_clock::now();
    b.run(j, a);
    double el = std::chrono::duration<double>(std::chrono::steady_clock::now() - t0).count();
    std::vector<uint64_t> sample = {0, 1, 7, 1000003, 0xfffffffffULL};
    auto got = b.digests(j, sample);
    auto ref = make_cpu_backend((int)std::thread::hardware_concurrency());
    auto want = ref->digests(j, sample);
    bool accept = got.size() == want.size();
    for (size_t i = 0; accept && i < got.size(); i++) accept = got[i] == want[i];
    std::cout << "{\"backend\":\"" << b.name() << "\",\"impl\":\"zpu-miner-cpp\",\"profile\":" << j.profile.id << ",\"attempts\":" << a
              << ",\"elapsed_s\":" << el << ",\"attempts_per_s\":" << a / el << ",\"tera_macs_per_s\":" << a / el * j.profile.macs() / 1e12
              << ",\"launch_overhead_s\":" << launch << ",\"host_peak_rss_kb\":" << peak_rss_kb() << ",\"device\":" << b.info()
              << ",\"hardware_threads\":" << std::thread::hardware_concurrency() << ",\"compiler\":\"" << compiler_id()
              << "\",\"acceptance\":{\"sample_nonces\":" << sample.size() << ",\"matches_cpu_reference\":" << (accept ? "true" : "false")
              << "}}" << std::endl;
    return accept ? 0 : 1;
}

// Max-magnitude accumulator stress on the backend's real GEMM path:
// rows with sum = +-2^20, cancellation, big-then-tiny terms, plus random D.
static int stress(Backend& b, int B, int K, int N) {
    std::vector<int8_t> X((size_t)B * K), W((size_t)N * K);
    uint64_t s = 0x9e3779b97f4a7c15ULL;
    auto rnd = [&]() { s ^= s << 13; s ^= s >> 7; s ^= s << 17; return idx_to_val((uint8_t)(s & 31)); };
    for (auto& v : X) v = rnd();
    for (auto& v : W) v = rnd();
    for (int k = 0; k < K; k++) {
        X[k] = 16; W[k] = 16;                                  // row0 x col0 = +2^20
        X[(size_t)K + k] = 16; W[(size_t)K + k] = -16;          // row1 x col1 = -2^20
        X[2 * (size_t)K + k] = (k & 1) ? -16 : 16;               // cancellation
        X[3 * (size_t)K + k] = k < K / 2 ? 16 : 1;               // big then tiny
        W[2 * (size_t)K + k] = k < K / 2 ? 16 : 1;
    }
    auto t0 = std::chrono::steady_clock::now();
    auto got = b.raw_gemm(X, W, B, N, K);
    double el = std::chrono::duration<double>(std::chrono::steady_clock::now() - t0).count();
    if (got.empty()) { std::cout << "{\"backend\":\"" << b.name() << "\",\"verdict\":\"UNSUPPORTED\"}" << std::endl; return 2; }
    long long bad = 0, maxabs = 0;
    for (int i = 0; i < B; i++)
        for (int j = 0; j < N; j++) {
            long long r = 0;
            for (int k = 0; k < K; k++) r += (long long)X[(size_t)i * K + k] * W[(size_t)j * K + k];
            if (r < 0 ? -r > maxabs : r > maxabs) maxabs = r < 0 ? -r : r;
            if (got[(size_t)i * N + j] != r) bad++;
        }
    std::cout << "{\"backend\":\"" << b.name() << "\",\"impl\":\"zpu-miner-cpp\",\"rows\":" << B << ",\"k\":" << K << ",\"n\":" << N
              << ",\"max_abs_ref\":" << maxabs << ",\"mismatches\":" << bad << ",\"gemm_s\":" << el
              << ",\"verdict\":\"" << (bad ? "FAIL" : "PASS") << "\"}" << std::endl;
    return bad ? 1 : 0;
}

int main(int argc, char** argv) {
    std::ios::sync_with_stdio(false);
    std::string cmd = argc > 1 ? argv[1] : "";
    auto b = make(argc, argv);
    if (cmd == "selftest") return selftest(*b, argc > 2 ? argv[2] : "tests/vectors/fp8c_v0.1.json");
    if (cmd == "bench") return bench(*b, argc, argv);
    if (cmd == "stress") return stress(*b, 512, 4096, 4096);
    if (cmd == "profile") {
        Job j;
        j.profile = FP8C_V01;
        std::memset(j.seed, 0x5e, 32);
        std::memset(j.challenge, 0xc4, 32);
        std::memset(j.target, 0, 32);
        std::string r = b->profile(j, std::stoi(argv_val(argc, argv, "--batch", "8192")));
        std::cout << (r.empty() ? "{\"verdict\":\"UNSUPPORTED\"}" : r) << std::endl;
        return r.empty() ? 2 : 0;
    }
    return serve(*b);
}
