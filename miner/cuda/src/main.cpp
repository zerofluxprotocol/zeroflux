// zpu-miner: ZeroFlux ZFX-FP8C miner (C++; CPU, CUDA, HIP backends).
//
//   zpu-miner [--backend cpu|gpu] [--device N] [--gemm blas-int8|lt-fp8|naive]
//             [--batch B] [--threads T]                       ZPU line protocol on stdin/stdout
//   zpu-miner selftest VECTORS.json [backend options]        bit-exactness vs published vectors
//   zpu-miner bench [--profile v01|dev] [--count N] [backend options]
//
// Protocol: see miner/PROTOCOL.md. Driven by `zpu-cli mine --backend exec`.

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
    for (size_t i = 0; i < e["byte_to_val"].arr.size(); i++)
        (byte_to_val((uint8_t)i) == e["byte_to_val"].arr[i].i() ? pass : fail)++;
    for (auto& c : e["requant_cases"].arr) {
        int32_t y = (int32_t)c["y"].i();
        (requant(y, 0) == c["s0"].i() && requant(y, 2) == c["s2"].i() ? pass : fail)++;
    }
    for (const char* key : {"dev", "v01"}) {
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

static int bench(Backend& b, int argc, char** argv) {
    Job j;
    j.profile = argv_val(argc, argv, "--profile", "v01") == std::string("dev") ? FP8C_V01_DEV : FP8C_V01;
    std::memset(j.seed, 0x5e, 32);
    std::memset(j.challenge, 0xc4, 32);
    std::memset(j.target, 0, 32);
    j.count = std::stoull(argv_val(argc, argv, "--count", j.profile.hidden > 1024 ? "2000" : "200000"));
    uint64_t a = 0;
    Job warm = j;
    warm.count = 16;
    b.run(warm, a);
    auto t0 = std::chrono::steady_clock::now();
    b.run(j, a);
    double el = std::chrono::duration<double>(std::chrono::steady_clock::now() - t0).count();
    std::cout << "{\"backend\":\"" << b.name() << "\",\"impl\":\"zpu-miner-cpp\",\"profile\":" << j.profile.id << ",\"attempts\":" << a
              << ",\"elapsed_s\":" << el << ",\"attempts_per_s\":" << a / el << ",\"tera_macs_per_s\":" << a / el * j.profile.macs() / 1e12
              << "}" << std::endl;
    return 0;
}

int main(int argc, char** argv) {
    std::ios::sync_with_stdio(false);
    std::string cmd = argc > 1 ? argv[1] : "";
    auto b = make(argc, argv);
    if (cmd == "selftest") return selftest(*b, argc > 2 ? argv[2] : "tests/vectors/fp8c_v0.1.json");
    if (cmd == "bench") return bench(*b, argc, argv);
    return serve(*b);
}
