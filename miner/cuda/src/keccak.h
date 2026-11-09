// Keccak-f[1600] and SHAKE256 (FIPS 202), absorb-then-squeeze, host+device.
#pragma once
#include "hd.h"

namespace zpu {

#if defined(__CUDA_ARCH__) || defined(__HIP_DEVICE_COMPILE__)
__device__ __constant__ static const uint64_t KECCAK_RC[24] = {
#else
static const uint64_t KECCAK_RC[24] = {
#endif
    0x0000000000000001ULL, 0x0000000000008082ULL, 0x800000000000808aULL, 0x8000000080008000ULL,
    0x000000000000808bULL, 0x0000000080000001ULL, 0x8000000080008081ULL, 0x8000000000008009ULL,
    0x000000000000008aULL, 0x0000000000000088ULL, 0x0000000080008009ULL, 0x000000008000000aULL,
    0x000000008000808bULL, 0x800000000000008bULL, 0x8000000000008089ULL, 0x8000000000008003ULL,
    0x8000000000008002ULL, 0x8000000000000080ULL, 0x000000000000800aULL, 0x800000008000000aULL,
    0x8000000080008081ULL, 0x8000000000008080ULL, 0x0000000080000001ULL, 0x8000000080008008ULL};

ZPU_HD uint64_t rotl64(uint64_t x, int n) { return n == 0 ? x : (x << n) | (x >> (64 - n)); }

ZPU_HD void keccak_f(uint64_t s[25]) {
    const int rho[25] = {0, 1, 62, 28, 27, 36, 44, 6, 55, 20, 3, 10, 43, 25, 39, 41, 45, 15, 21, 8, 18, 2, 61, 56, 14};
    for (int round = 0; round < 24; round++) {
        uint64_t c[5], d[5], b[25];
        for (int x = 0; x < 5; x++) c[x] = s[x] ^ s[x + 5] ^ s[x + 10] ^ s[x + 15] ^ s[x + 20];
        for (int x = 0; x < 5; x++) d[x] = c[(x + 4) % 5] ^ rotl64(c[(x + 1) % 5], 1);
        for (int i = 0; i < 25; i++) s[i] ^= d[i % 5];
        for (int x = 0; x < 5; x++)
            for (int y = 0; y < 5; y++) b[y + 5 * ((2 * x + 3 * y) % 5)] = rotl64(s[x + 5 * y], rho[x + 5 * y]);
        for (int x = 0; x < 5; x++)
            for (int y = 0; y < 5; y++) s[x + 5 * y] = b[x + 5 * y] ^ (~b[(x + 1) % 5 + 5 * y] & b[(x + 2) % 5 + 5 * y]);
        s[0] ^= KECCAK_RC[round];
    }
}

struct Shake256 {
    uint64_t s[25];
    uint32_t pos;  // byte position within the rate
    bool squeezing;
    static const uint32_t RATE = 136;

    ZPU_HD void init() {
        for (int i = 0; i < 25; i++) s[i] = 0;
        pos = 0;
        squeezing = false;
    }
    ZPU_HD void xor_byte(uint32_t i, uint8_t v) { s[i / 8] ^= (uint64_t)v << (8 * (i % 8)); }
    ZPU_HD uint8_t get_byte(uint32_t i) const { return (uint8_t)(s[i / 8] >> (8 * (i % 8))); }
    ZPU_HD void absorb(const uint8_t* p, size_t n) {
        for (size_t i = 0; i < n; i++) {
            xor_byte(pos++, p[i]);
            if (pos == RATE) { keccak_f(s); pos = 0; }
        }
    }
    ZPU_HD void finish() {
        xor_byte(pos, 0x1f);
        xor_byte(RATE - 1, 0x80);
        keccak_f(s);
        pos = 0;
        squeezing = true;
    }
    ZPU_HD void squeeze(uint8_t* out, size_t n) {
        if (!squeezing) finish();
        for (size_t i = 0; i < n; i++) {
            if (pos == RATE) { keccak_f(s); pos = 0; }
            out[i] = get_byte(pos++);
        }
    }
};

}  // namespace zpu
