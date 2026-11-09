// ZFX-FP8C v0.1 consensus arithmetic (see docs/CONSENSUS.md section 3).
// Independent C++ implementation; checked against tests/vectors.
#pragma once
#include "hd.h"
#include "keccak.h"
#include "sha256.h"

namespace zpu {

#if defined(__CUDA_ARCH__) || defined(__HIP_DEVICE_COMPILE__)
__device__ __constant__ static const uint8_t AES_SBOX[256] = {
#else
static const uint8_t AES_SBOX[256] = {
#endif
    0x63,0x7c,0x77,0x7b,0xf2,0x6b,0x6f,0xc5,0x30,0x01,0x67,0x2b,0xfe,0xd7,0xab,0x76,0xca,0x82,0xc9,0x7d,0xfa,0x59,0x47,0xf0,0xad,0xd4,0xa2,0xaf,0x9c,0xa4,0x72,0xc0,
    0xb7,0xfd,0x93,0x26,0x36,0x3f,0xf7,0xcc,0x34,0xa5,0xe5,0xf1,0x71,0xd8,0x31,0x15,0x04,0xc7,0x23,0xc3,0x18,0x96,0x05,0x9a,0x07,0x12,0x80,0xe2,0xeb,0x27,0xb2,0x75,
    0x09,0x83,0x2c,0x1a,0x1b,0x6e,0x5a,0xa0,0x52,0x3b,0xd6,0xb3,0x29,0xe3,0x2f,0x84,0x53,0xd1,0x00,0xed,0x20,0xfc,0xb1,0x5b,0x6a,0xcb,0xbe,0x39,0x4a,0x4c,0x58,0xcf,
    0xd0,0xef,0xaa,0xfb,0x43,0x4d,0x33,0x85,0x45,0xf9,0x02,0x7f,0x50,0x3c,0x9f,0xa8,0x51,0xa3,0x40,0x8f,0x92,0x9d,0x38,0xf5,0xbc,0xb6,0xda,0x21,0x10,0xff,0xf3,0xd2,
    0xcd,0x0c,0x13,0xec,0x5f,0x97,0x44,0x17,0xc4,0xa7,0x7e,0x3d,0x64,0x5d,0x19,0x73,0x60,0x81,0x4f,0xdc,0x22,0x2a,0x90,0x88,0x46,0xee,0xb8,0x14,0xde,0x5e,0x0b,0xdb,
    0xe0,0x32,0x3a,0x0a,0x49,0x06,0x24,0x5c,0xc2,0xd3,0xac,0x62,0x91,0x95,0xe4,0x79,0xe7,0xc8,0x37,0x6d,0x8d,0xd5,0x4e,0xa9,0x6c,0x56,0xf4,0xea,0x65,0x7a,0xae,0x08,
    0xba,0x78,0x25,0x2e,0x1c,0xa6,0xb4,0xc6,0xe8,0xdd,0x74,0x1f,0x4b,0xbd,0x8b,0x8a,0x70,0x3e,0xb5,0x66,0x48,0x03,0xf6,0x0e,0x61,0x35,0x57,0xb9,0x86,0xc1,0x1d,0x9e,
    0xe1,0xf8,0x98,0x11,0x69,0xd9,0x8e,0x94,0x9b,0x1e,0x87,0xe9,0xce,0x55,0x28,0xdf,0x8c,0xa1,0x89,0x0d,0xbf,0xe6,0x42,0x68,0x41,0x99,0x2d,0x0f,0xb0,0x54,0xbb,0x16};

struct Profile {
    uint32_t id;
    int d_in, hidden, n_hidden, d_out;
    uint32_t shift_in, shift_hidden;
    int n_layers() const { return n_hidden + 2; }
    int layer_k(int l) const { return l == 0 ? d_in : hidden; }
    int layer_n(int l) const { return l == n_layers() - 1 ? d_out : hidden; }
    uint32_t layer_shift(int l) const { return l == 0 ? shift_in : shift_hidden; }
    uint64_t macs() const {
        uint64_t m = 0;
        for (int l = 0; l < n_layers(); l++) m += (uint64_t)layer_k(l) * layer_n(l);
        return m;
    }
};

static const Profile FP8C_V01 = {1, 64, 4096, 3, 256, 0, 2};
static const Profile FP8C_V01_DEV = {2, 64, 256, 3, 64, 0, 0};

// 5-bit index -> operand value in D = {+-1..+-16}.
ZPU_HD int8_t idx_to_val(uint8_t u) {
    u &= 31;
    return u < 16 ? (int8_t)(u + 1) : (int8_t)(15 - (int)u);
}
ZPU_HD int8_t byte_to_val(uint8_t b) { return idx_to_val(b & 31); }
// Arithmetic shift on two's complement int32, then S-box, top 5 bits.
ZPU_HD uint8_t requant_idx(int32_t y, uint32_t s) { return AES_SBOX[(uint8_t)((y >> s) & 0xff)] >> 3; }
ZPU_HD int8_t requant(int32_t y, uint32_t s) { return idx_to_val(requant_idx(y, s)); }

static const char ATTEMPT_DOMAIN[] = "ZFX-attempt-v0.1";
static const char RESULT_DOMAIN[] = "ZFX-result-v0.1";
static const char X0_DOMAIN[] = "ZFX-FP8C-X0-v0.1";
static const char W_DOMAIN[] = "ZFX-FP8C-W-v0.1";

template <size_t N>
ZPU_HD void absorb_str(Sha256& h, const char (&s)[N]) { h.update((const uint8_t*)s, N - 1); }
template <size_t N>
ZPU_HD void absorb_str(Shake256& h, const char (&s)[N]) { h.absorb((const uint8_t*)s, N - 1); }

ZPU_HD void le64(uint64_t v, uint8_t o[8]) { for (int i = 0; i < 8; i++) o[i] = (uint8_t)(v >> (8 * i)); }
ZPU_HD void le32(uint32_t v, uint8_t o[4]) { for (int i = 0; i < 4; i++) o[i] = (uint8_t)(v >> (8 * i)); }

ZPU_HD void attempt_id(const uint8_t challenge[32], uint64_t nonce, uint8_t out[32]) {
    Sha256 h; h.init();
    absorb_str(h, ATTEMPT_DOMAIN);
    h.update(challenge, 32);
    uint8_t n[8]; le64(nonce, n);
    h.update(n, 8);
    h.final(out);
}

// x0 as operand values (d_in <= 256).
ZPU_HD void make_x0(const uint8_t attempt[32], int d_in, int8_t* out) {
    Shake256 x; x.init();
    absorb_str(x, X0_DOMAIN);
    x.absorb(attempt, 32);
    uint8_t buf[256];
    x.squeeze(buf, d_in);
    for (int i = 0; i < d_in; i++) out[i] = byte_to_val(buf[i]);
}

inline void weight_row(const uint8_t seed[32], uint32_t layer, uint32_t row, int k, int8_t* out) {
    Shake256 x; x.init();
    absorb_str(x, W_DOMAIN);
    x.absorb(seed, 32);
    uint8_t b[4];
    le32(layer, b); x.absorb(b, 4);
    le32(row, b); x.absorb(b, 4);
    uint8_t chunk[136];
    for (int o = 0; o < k; o += 136) {
        int n = k - o < 136 ? k - o : 136;
        x.squeeze(chunk, n);
        for (int i = 0; i < n; i++) out[o + i] = byte_to_val(chunk[i]);
    }
}

// Digest over the final-layer int32 outputs (little-endian).
ZPU_HD void result_digest(const uint8_t challenge[32], uint64_t nonce, const int32_t* out, int d_out, uint8_t dig[32]) {
    Sha256 h; h.init();
    absorb_str(h, RESULT_DOMAIN);
    h.update(challenge, 32);
    uint8_t n[8]; le64(nonce, n);
    h.update(n, 8);
    for (int i = 0; i < d_out; i++) {
        uint8_t b[4]; le32((uint32_t)out[i], b);
        h.update(b, 4);
    }
    h.final(dig);
}

// digest < target, both big-endian 256-bit.
ZPU_HD bool below_target(const uint8_t d[32], const uint8_t t[32]) {
    for (int i = 0; i < 32; i++) {
        if (d[i] < t[i]) return true;
        if (d[i] > t[i]) return false;
    }
    return false;
}

}  // namespace zpu
