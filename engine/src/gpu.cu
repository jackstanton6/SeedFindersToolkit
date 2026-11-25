// Seed-only pre-filter and crack brute force on CUDA. Compiled to PTX by build.rs (nvcc), loaded by
// gpu.rs through the driver API. Literal ports of rng.rs (JavaRandom), structure.rs (grid_chunk,
// frequency_ok) and query.rs (potential, slime_max); the CPU re-checks every survivor exactly.
//
// Program (constant memory): [clauses, then per clause: atoms, atoms * 24 ints]. A seed passes when
// every clause has a passing atom. Atoms:
//   0 COUNT  spacing separation triangular salt reduction freq_bits ox oz at_x at_z within min,
//            [16] 1 when each start's column must allow a biome: [17] column offset, [18..21] biome
//            set, [22..23] quart y range (structure.rs column_ok)
//   1 SLIME  radius at_x at_z within min
//   2 CELL   spacing separation triangular salt gx gz cx cz
//   3 CLUSTER as COUNT (min = need), then pair distance
//   4 BIOME  biome set (4 ints) at_x at_z within y step, min and max (doubles as 2 ints each):
//            query.rs Metric::Biome, the fraction of the disc's grid points in the set
// Biome atoms need the full 64-bit seed (range and random modes only).

typedef long long i64;
typedef unsigned long long u64;

#define MULT 0x5DEECE66DULL
#define MASK48 ((1ULL << 48) - 1)
#define ATOM 24

__constant__ int PROG[4096];

struct JR { u64 s; };

__device__ __forceinline__ void jr_set(JR &r, i64 seed) { r.s = ((u64)seed ^ MULT) & MASK48; }

__device__ __forceinline__ int jr_next(JR &r, int bits) {
    r.s = (r.s * MULT + 0xBULL) & MASK48;
    return (int)(i64)(r.s >> (48 - bits));
}

__device__ int jr_bound(JR &r, int bound) {
    if ((bound & (bound - 1)) == 0) return (int)(((i64)bound * (i64)jr_next(r, 31)) >> 31);
    for (;;) {
        int sample = jr_next(r, 31);
        int mod = sample % bound;
        if ((int)((unsigned)sample - (unsigned)mod + (unsigned)(bound - 1)) >= 0) return mod;
    }
}

__device__ __forceinline__ i64 jr_long(JR &r) {
    i64 hi = jr_next(r, 32), lo = jr_next(r, 32);
    return (i64)(((u64)hi << 32) + (u64)lo);
}

__device__ __forceinline__ float jr_float(JR &r) { return (float)jr_next(r, 24) * 5.9604645e-8f; }

__device__ __forceinline__ double jr_double(JR &r) {
    i64 hi = jr_next(r, 26), lo = jr_next(r, 27);
    return (double)((hi << 27) + lo) * (1.0 / (double)(1ULL << 53));
}

__device__ __forceinline__ void large_feature_with_salt(JR &r, i64 seed, int x, int z, int salt) {
    jr_set(r, (i64)((u64)(i64)x * 341873128712ULL + (u64)(i64)z * 132897987541ULL + (u64)seed + (u64)(i64)salt));
}

__device__ void large_feature(JR &r, i64 seed, int cx, int cz) {
    jr_set(r, seed);
    i64 xs = jr_long(r), zs = jr_long(r);
    jr_set(r, (i64)((u64)(i64)cx * (u64)xs) ^ (i64)((u64)(i64)cz * (u64)zs) ^ seed);
}

__device__ __forceinline__ int floordiv(int a, int b) {
    int q = a / b;
    return (a % b != 0 && ((a < 0) != (b < 0))) ? q - 1 : q;
}

// StructurePlacement grid cell -> chunk (RandomSpreadStructurePlacement.getPotentialStructureChunk)
__device__ void grid_chunk(i64 seed, const int *a, int gx, int gz, int &cx, int &cz) {
    int spacing = a[1], limit = a[1] - a[2], tri = a[3];
    JR r;
    large_feature_with_salt(r, seed, gx, gz, a[4]);
    int dx = tri ? (jr_bound(r, limit) + jr_bound(r, limit)) / 2 : jr_bound(r, limit);
    int dz = tri ? (jr_bound(r, limit) + jr_bound(r, limit)) / 2 : jr_bound(r, limit);
    cx = gx * spacing + dx;
    cz = gz * spacing + dz;
}

__device__ bool frequency_ok(i64 seed, int reduction, float f, int salt, int cx, int cz) {
    JR r;
    switch (reduction) {
    case 0: large_feature_with_salt(r, seed, salt, cx, cz); return jr_float(r) < f;  // (salt, x, z) as vanilla
    case 1: {
        int a = cx >> 4, b = cz >> 4;
        jr_set(r, (i64)(a ^ (int)((unsigned)b << 4)) ^ seed);
        jr_next(r, 32);
        return jr_bound(r, (int)(1.0f / f)) == 0;
    }
    case 2: large_feature_with_salt(r, seed, cx, cz, 10387320); return jr_float(r) < f;
    case 3: large_feature(r, seed, cx, cz); return jr_double(r) < (double)f;
    default: return true;
    }
}


// ------------------------------------------------------------------------------------------------
// Climate (point path): literal ports of rng.rs Xoroshiro, noise.rs Perlin/NoiseStack, the density.rs
// samplers of the climate router (gpu.rs ClimateProgram) and biome.rs RTree.search.
// ------------------------------------------------------------------------------------------------

#define NODE 14
#define SPL 6
#define MAX_CACHES 32
#define MAX_LAYERS 48

struct Clim {
    const int *node; const double *k; const int *aux; const int *spl;
    int roots[6]; unsigned ydep;
    const int *noise; const u64 *noise_md5; int nnoise;
    const u64 *layer_md5; const double *layer_freq; const float *layer_amp; const int *layer_second; int nlayer;
    const int *tree; const int *kids; const i64 *space; const int *leaf_biome;
};
__constant__ Clim CLIM;

struct XR { u64 lo, hi; };

__device__ __forceinline__ u64 rotl(u64 x, int k) { return (x << k) | (x >> (64 - k)); }
__device__ __forceinline__ u64 mix13(u64 z) {
    z = (z ^ (z >> 30)) * 0xBF58476D1CE4E5B9ULL;
    z = (z ^ (z >> 27)) * 0x94D049BB133111EBULL;
    return z ^ (z >> 31);
}
__device__ __forceinline__ void xr_from(XR &r, u64 lo, u64 hi) {
    if ((lo | hi) == 0) { r.lo = 0x9E3779B97F4A7C15ULL; r.hi = 0x6A09E667F3BCC909ULL; } else { r.lo = lo; r.hi = hi; }
}
__device__ __forceinline__ u64 xr_next(XR &r) {
    u64 s0 = r.lo, s1 = r.hi;
    u64 res = rotl(s0 + s1, 17) + s0;
    s1 ^= s0;
    r.lo = rotl(s0, 49) ^ s1 ^ (s1 << 21);
    r.hi = rotl(s1, 28);
    return res;
}
// XoroshiroRandomSource.nextInt(int): Lemire's multiply-shift
__device__ int xr_bound(XR &r, int bound) {
    u64 b = (u64)bound;
    u64 m = (u64)(unsigned)(int)xr_next(r) * b;
    u64 frac = m & 0xFFFFFFFFULL;
    if (frac < b) {
        u64 thr = (u64)((unsigned)(~bound + 1) % (unsigned)bound);
        while (frac < thr) {
            m = (u64)(unsigned)(int)xr_next(r) * b;
            frac = m & 0xFFFFFFFFULL;
        }
    }
    return (int)(m >> 32);
}
__device__ __forceinline__ double xr_double(XR &r) { return (double)(xr_next(r) >> 11) * (1.0 / (double)(1ULL << 53)); }

struct Perl { double ox, oy, oz; unsigned char p[256]; };

__device__ void perlin_init(Perl &pl, XR &r) {
    pl.ox = xr_double(r) * 256.0;
    pl.oy = xr_double(r) * 256.0;
    pl.oz = xr_double(r) * 256.0;
    for (int i = 0; i < 256; i++) pl.p[i] = (unsigned char)i;
    for (int i = 0; i < 256; i++) {
        int j = xr_bound(r, 256 - i) + i;
        unsigned char t = pl.p[i]; pl.p[i] = pl.p[j]; pl.p[j] = t;
    }
}

__constant__ float GRAD[16][3] = {
    {1, 1, 0}, {-1, 1, 0}, {1, -1, 0}, {-1, -1, 0}, {1, 0, 1}, {-1, 0, 1}, {1, 0, -1}, {-1, 0, -1},
    {0, 1, 1}, {0, -1, 1}, {0, 1, -1}, {0, -1, -1}, {1, 1, 0}, {0, -1, 1}, {-1, 1, 0}, {0, -1, -1},
};

__device__ __forceinline__ int pp(const Perl &pl, int i) { return pl.p[i & 0xFF]; }
__device__ __forceinline__ float pgrad(const Perl &pl, int i, float x, float y, float z) {
    const float *g = GRAD[pp(pl, i) & 15];
    return g[0] * x + g[1] * y + g[2] * z;
}
__device__ __forceinline__ float smooth(float x) { return x * x * x * (x * (x * 6.0f - 15.0f) + 10.0f); }
__device__ __forceinline__ float lerpf(float a, float p0, float p1) { return p0 + a * (p1 - p0); }
__device__ __forceinline__ double wrapd(double x) {
    const double H = 16777215.999999998;
    return (x >= -H && x < H) ? x : x - floor(x / 33554432.0 + 0.5) * 33554432.0;
}

// ImprovedNoise.noise without smear (climate noises are plain PerlinNoise)
__device__ float perlin_get(const Perl &pl, double x, double y, double z) {
    double xs = wrapd(x) + pl.ox, ys = wrapd(y) + pl.oy, zs = wrapd(z) + pl.oz;
    int fx = (int)floor(xs), fy = (int)floor(ys), fz = (int)floor(zs);
    float rx = (float)(xs - (double)fx), ry = (float)(ys - (double)fy), rz = (float)(zs - (double)fz);
    int x0 = pp(pl, fx), x1 = pp(pl, (int)((unsigned)fx + 1u));
    int xy00 = pp(pl, x0 + fy), xy01 = pp(pl, x0 + fy + 1), xy10 = pp(pl, x1 + fy), xy11 = pp(pl, x1 + fy + 1);
    float d000 = pgrad(pl, xy00 + fz, rx, ry, rz);
    float d100 = pgrad(pl, xy10 + fz, rx - 1.0f, ry, rz);
    float d010 = pgrad(pl, xy01 + fz, rx, ry - 1.0f, rz);
    float d110 = pgrad(pl, xy11 + fz, rx - 1.0f, ry - 1.0f, rz);
    float d001 = pgrad(pl, xy00 + fz + 1, rx, ry, rz - 1.0f);
    float d101 = pgrad(pl, xy10 + fz + 1, rx - 1.0f, ry, rz - 1.0f);
    float d011 = pgrad(pl, xy01 + fz + 1, rx, ry - 1.0f, rz - 1.0f);
    float d111 = pgrad(pl, xy11 + fz + 1, rx - 1.0f, ry - 1.0f, rz - 1.0f);
    float ax = smooth(rx), ay = smooth(ry), az = smooth(rz);
    return lerpf(az, lerpf(ay, lerpf(ax, d000, d100), lerpf(ax, d010, d110)),
                     lerpf(ay, lerpf(ax, d001, d101), lerpf(ax, d011, d111)));
}

// Per-seed climate state: every layer's Perlin, and the cache-node memo (value + position per slot)
struct Cl {
    Perl pl[MAX_LAYERS];
    float memo[MAX_CACHES];
    int mx[MAX_CACHES], my[MAX_CACHES], mz[MAX_CACHES];
    unsigned valid;
};

// RandomState for a seed: Xoroshiro(seed).forkPositional(), then per noise fromHashOf(id) and
// NormalNoise.create's two forks, each octave's layer from its name.
__device__ void cl_init(Cl &c, i64 seed) {
    XR r;
    u64 lo = (u64)seed ^ 0x6A09E667F3BCC909ULL;
    xr_from(r, mix13(lo), mix13(lo + 0x9E3779B97F4A7C15ULL));
    u64 plo = xr_next(r), phi = xr_next(r);
    for (int n = 0; n < CLIM.nnoise; n++) {
        XR nr;
        xr_from(nr, CLIM.noise_md5[2 * n] ^ plo, CLIM.noise_md5[2 * n + 1] ^ phi);
        u64 flo = xr_next(nr), fhi = xr_next(nr), slo = xr_next(nr), shi = xr_next(nr);
        for (int l = CLIM.noise[2 * n]; l < CLIM.noise[2 * n] + CLIM.noise[2 * n + 1]; l++) {
            XR lr;
            bool second = CLIM.layer_second[l];
            xr_from(lr, CLIM.layer_md5[2 * l] ^ (second ? slo : flo), CLIM.layer_md5[2 * l + 1] ^ (second ? shi : fhi));
            perlin_init(c.pl[l], lr);
        }
    }
    c.valid = 0;
}

// NoiseStack.get: float accumulation over the noise's layers
__device__ float noise_get(const Cl &c, int n, double x, double y, double z) {
    float v = 0.0f;
    for (int l = CLIM.noise[2 * n]; l < CLIM.noise[2 * n] + CLIM.noise[2 * n + 1]; l++) {
        double f = CLIM.layer_freq[l];
        v += CLIM.layer_amp[l] * perlin_get(c.pl[l], x * f, y * f, z * f);
    }
    return v;
}

__device__ __forceinline__ float jmin(float a, float b) {
    if (isnan(a)) return a;
    if (isnan(b)) return b;
    if (a == 0.0f && b == 0.0f) return signbit(a) ? a : b;
    return a < b ? a : b;
}
__device__ __forceinline__ float jmax(float a, float b) {
    if (isnan(a)) return a;
    if (isnan(b)) return b;
    if (a == 0.0f && b == 0.0f) return signbit(a) ? b : a;
    return a > b ? a : b;
}
__device__ __forceinline__ float clampf(float v, float lo, float hi) { return v < lo ? lo : jmin(v, hi); }
__device__ __forceinline__ int rem_euclid(int a, int b) { int r = a % b; return r < 0 ? r + (b < 0 ? -b : b) : r; }
__device__ __forceinline__ int div_euclid(int a, int b) { int q = a / b; if (a % b < 0) return b > 0 ? q - 1 : q + 1; return q; }

__device__ float unary(int op, float v) {
    switch (op) {
    case 0: return fabsf(v);
    case 1: return v * v;
    case 2: return v * v * v;
    case 3: return (float)sqrt((double)v);
    case 4: return v > 0.0f ? v : v * 0.5f;
    case 5: return v > 0.0f ? v : v * 0.25f;
    case 6: return 1.0f / v;
    case 7: return -v;
    case 8: { float c = clampf(v, -1.0f, 1.0f); return c / 2.0f - c * c * c / 24.0f; }
    case 9: return (float)log((double)v);
    default: return (v == 0.0f || isnan(v)) ? v : (v > 0.0f ? 1.0f : -1.0f);
    }
}

__device__ float grad_compute(const int *n, int c, float fv, float factor) {
    int tiling = n[5], from = n[6], lo = n[7], hi = n[8], range = n[9];
    if (tiling == 0) return fv + (float)(min(max(c, lo), hi) - from) * factor;
    if (tiling == 1) return fv + (float)rem_euclid(c - from, range) * factor;
    int rel = c - from, tile = div_euclid(rel, range), local = rel - tile * range;
    return (tile & 1) == 0 ? fv + (float)local * factor : fv + (float)(range - local) * factor;
}

#define K(i) ((float)CLIM.k[i])

__device__ float sample(Cl &c, int id, int x, int y, int z);

// density.rs spline_eval; coordinates are evaluated at most once per sample (PointSplineInput)
__device__ float spline_eval(Cl &c, int sp, const int *coords, float *cache, int x, int y, int z) {
    const int *s = CLIM.spl + sp * SPL;
    if (s[0] == 0) return K(s[1]);
    int ci = s[1], n = s[2];
    if (isnan(cache[ci])) cache[ci] = sample(c, coords[ci], x, y, z);
    float input = cache[ci];
    const double *loc = CLIM.k + s[3], *der = CLIM.k + s[4];
    const int *val = CLIM.aux + s[5];
    int i = 0;
    while (i < n && !(input < (float)loc[i])) i++;
    int start = i - 1, last = n - 1;
    if (start < 0) {
        float v = spline_eval(c, val[0], coords, cache, x, y, z);
        return (float)der[0] == 0.0f ? v : v + (float)der[0] * (input - (float)loc[0]);
    }
    if (start == last) {
        float v = spline_eval(c, val[last], coords, cache, x, y, z);
        return (float)der[last] == 0.0f ? v : v + (float)der[last] * (input - (float)loc[last]);
    }
    float x1 = (float)loc[start], x2 = (float)loc[start + 1];
    float t = (input - x1) / (x2 - x1);
    float d1 = (float)der[start], d2 = (float)der[start + 1];
    float y1 = spline_eval(c, val[start], coords, cache, x, y, z);
    float y2 = spline_eval(c, val[start + 1], coords, cache, x, y, z);
    float a = d1 * (x2 - x1) - (y2 - y1);
    float b = -d2 * (x2 - x1) + (y2 - y1);
    return lerpf(t, y1, y2) + t * (1.0f - t) * lerpf(t, a, b);
}

// density.rs S::sample
__device__ float sample(Cl &c, int id, int x, int y, int z) {
    const int *n = CLIM.node + id * NODE;
    switch (n[0]) {
    case 0: return K(n[10]);
    case 1: { double xz = CLIM.k[n[10]], ys = CLIM.k[n[11]]; return noise_get(c, n[4], (double)x * xz, (double)y * ys, (double)z * xz); }
    case 2: {
        double xz = CLIM.k[n[10]], ys = CLIM.k[n[11]];
        double nx = (double)x * xz + (double)sample(c, n[1], x, y, z);
        double ny = (double)y * ys;
        double nz = (double)z * xz + (double)sample(c, n[2], x, y, z);
        return noise_get(c, n[4], nx, ny, nz);
    }
    case 3: {
        double xz = CLIM.k[n[10]], ys = CLIM.k[n[11]];
        double nx = (double)x * xz + (double)sample(c, n[1], x, y, z);
        double ny = (double)y * ys + (double)sample(c, n[2], x, y, z);
        double nz = (double)z * xz + (double)sample(c, n[3], x, y, z);
        return noise_get(c, n[4], nx, ny, nz);
    }
    case 4: return noise_get(c, n[4], (double)z * 0.25, (double)x * 0.25, 0.0) * 4.0f;
    case 5: return sample(c, n[1], x, y, z) + K(n[10]);
    case 6: return K(n[10]) - sample(c, n[1], x, y, z);
    case 7: return sample(c, n[1], x, y, z) * K(n[10]);
    case 8: return K(n[10]) / sample(c, n[1], x, y, z);
    case 9: return jmin(sample(c, n[1], x, y, z), K(n[10]));
    case 10: return jmax(sample(c, n[1], x, y, z), K(n[10]));
    case 11: { float a = sample(c, n[1], x, y, z); return a + sample(c, n[2], x, y, z); }
    case 12: { float a = sample(c, n[1], x, y, z); return a - sample(c, n[2], x, y, z); }
    case 13: { float a = sample(c, n[1], x, y, z); return a == 0.0f ? 0.0f : a * sample(c, n[2], x, y, z); }
    case 14: { float a = sample(c, n[1], x, y, z); return a == 0.0f ? 0.0f : a / sample(c, n[2], x, y, z); }
    case 15: { float a = sample(c, n[1], x, y, z); return a <= K(n[10]) ? a : jmin(a, sample(c, n[2], x, y, z)); }
    case 16: { float a = sample(c, n[1], x, y, z); return a >= K(n[10]) ? a : jmax(a, sample(c, n[2], x, y, z)); }
    case 17: return grad_compute(n, n[4] == 1 ? x : n[4] == 2 ? y : z, K(n[10]), K(n[11]));
    case 18: return unary(n[4], sample(c, n[1], x, y, z));
    case 19: return clampf(sample(c, n[1], x, y, z), K(n[10]), K(n[11]));
    case 20: {
        float al = sample(c, n[1], x, y, z);
        if (al == 0.0f) return sample(c, n[2], x, y, z);
        if (al == 1.0f) return sample(c, n[3], x, y, z);
        float fv = sample(c, n[2], x, y, z);
        return lerpf(al, fv, sample(c, n[3], x, y, z));
    }
    case 21: {
        float al = sample(c, n[1], x, y, z);
        if (al == 0.0f) return K(n[10]);
        if (al == 1.0f) return sample(c, n[2], x, y, z);
        return lerpf(al, K(n[10]), sample(c, n[2], x, y, z));
    }
    case 22: {
        float al = sample(c, n[1], x, y, z);
        if (al == 0.0f) return sample(c, n[2], x, y, z);
        if (al == 1.0f) return K(n[10]);
        return lerpf(al, sample(c, n[2], x, y, z), K(n[10]));
    }
    case 23: { float v = sample(c, n[1], x, y, z); return (v >= K(n[10]) && v < K(n[11])) ? K(n[12]) : K(n[13]); }
    case 24: { float v = sample(c, n[1], x, y, z); return (v >= K(n[10]) && v < K(n[11])) ? sample(c, n[2], x, y, z) : sample(c, n[3], x, y, z); }
    case 25: return sample(c, n[1], x, y, z) < K(n[10]) ? sample(c, n[2], x, y, z) : sample(c, n[3], x, y, z);
    case 26: {
        float v = sample(c, n[1], x, y, z);
        int k = n[5] - 1;
        for (int j = 0; j < n[7]; j++) if (v < K(n[6] + j)) { k = j; break; }
        return sample(c, CLIM.aux[n[4] + k], x, y, z);
    }
    case 27: {
        float cache[32];
        for (int j = 0; j < 32; j++) cache[j] = __int_as_float(0x7fc00000);
        return spline_eval(c, n[4], CLIM.aux + n[5], cache, x, y, z);
    }
    case 28: {
        // a cache node: the same value for the same position (y ignored when it does not depend on y)
        int slot = n[4];
        bool ydep = n[5] != 0;
        if ((c.valid >> slot & 1) && c.mx[slot] == x && c.mz[slot] == z && (!ydep || c.my[slot] == y)) return c.memo[slot];
        float v = sample(c, n[1], x, y, z);
        c.memo[slot] = v; c.mx[slot] = x; c.my[slot] = y; c.mz[slot] = z;
        c.valid |= 1u << slot;
        return v;
    }
    case 29: return sample(c, n[1], n[4], y, z);
    case 30: return sample(c, n[1], x, n[4], z);
    case 31: return sample(c, n[1], x, y, n[4]);
    default: return sample(c, n[1], n[4], y, n[5]);
    }
}

// Climate.quantizeCoord: (long)(v * 10000.0F), saturating as Java's cast
__device__ __forceinline__ i64 quantize(float v) {
    float p = v * 10000.0f;
    if (isnan(p)) return 0;
    if (p >= 9.223372e18f) return 0x7FFFFFFFFFFFFFFFLL;
    if (p <= -9.223372e18f) return (i64)0x8000000000000000ULL;
    return (i64)p;
}

__device__ i64 space_dist(int sp, const i64 *t) {
    const i64 *s = CLIM.space + sp * 14;
    i64 d = 0;
    for (int k = 0; k < 7; k++) {
        i64 above = t[k] - s[2 * k + 1], below = s[2 * k] - t[k];
        i64 e = above > 0 ? above : (below > 0 ? below : 0);
        d += e * e;
    }
    return d;
}

// biome.rs search_node (RTree.search from `cand`, -1 for none)
__device__ int search_node(int id, const i64 *t, int cand) {
    const int *n = CLIM.tree + id * 4;
    if (n[0] >= 0) return n[0];
    i64 mn = cand < 0 ? 0x7FFFFFFFFFFFFFFFLL : space_dist(cand, t);
    int best = cand;
    for (int j = 0; j < n[2]; j++) {
        int k = CLIM.kids[n[1] + j];
        const int *kn = CLIM.tree + k * 4;
        i64 cd = space_dist(kn[3], t);
        if (mn > cd) {
            int leaf = search_node(k, t, best);
            i64 ld = kn[0] >= 0 ? cd : space_dist(leaf, t);
            if (mn > ld) { mn = ld; best = leaf; }
        }
    }
    return best;
}

// Another leaf at distance `d` with a different biome: then the warm start (which the CPU keeps
// per query order) can change the answer, so the caller must treat the biome as unknown.
__device__ bool tie(int id, const i64 *t, i64 d, int biome) {
    const int *n = CLIM.tree + id * 4;
    if (n[0] >= 0) return space_dist(n[3], t) == d && CLIM.leaf_biome[n[0]] != biome;
    for (int j = 0; j < n[2]; j++) {
        int k = CLIM.kids[n[1] + j];
        if (space_dist(CLIM.tree[k * 4 + 3], t) <= d && tie(k, t, d, biome)) return true;
    }
    return false;
}

// world.rs Seeded::biome with a fresh context: the biome at a quart, or -1 when a tie makes it unsure
__device__ int biome_at(Cl &c, int qx, int qy, int qz) {
    i64 t[7];
    for (int k = 0; k < 6; k++) t[k] = quantize(sample(c, CLIM.roots[k], qx << 2, qy << 2, qz << 2));
    t[6] = 0;
    int leaf = search_node(0, t, -1);
    int b = CLIM.leaf_biome[leaf];
    return tie(0, t, space_dist(leaf, t), b) ? -1 : b;
}

__device__ __forceinline__ bool in_set(const int *set, int b) { return b < 0 || ((unsigned)set[b >> 5] >> (b & 31) & 1); }

struct Lazy { Cl *c; i64 seed; bool ready; };
__device__ __forceinline__ Cl &climate(Lazy &l) {
    if (!l.ready) { cl_init(*l.c, l.seed); l.ready = true; }
    return *l.c;
}

// structure.rs column_ok: some quart of the start column allows the structure (unsure counts as yes)
__device__ bool column_ok(Lazy &l, const int *a, int cx, int cz) {
    int x = cx * 16 + a[17], z = cz * 16 + a[17];
    Cl &c = climate(l);
    for (int qy = a[23]; qy >= a[22]; qy--) {
        if (in_set(a + 18, biome_at(c, x >> 2, qy, z >> 2))) return true;
    }
    return false;
}

__device__ __forceinline__ double dbl(const int *a) { return __hiloint2double(a[1], a[0]); }

// query.rs Metric::Biome within [min, max]: bounds on the fraction (unsure points count as either)
__device__ bool biome_atom(Lazy &l, const int *a) {
    int ax = a[5], az = a[6], within = a[7], y = a[8], step = a[9];
    double mn = dbl(a + 10), mx = dbl(a + 12);
    int n = 2 * (within / step) + 1;
    int x0 = ax - (n / 2) * step, z0 = az - (n / 2) * step;
    i64 w2 = (i64)within * within;
    int len = 0;
    for (int j = 0; j < n; j++)
        for (int i = 0; i < n; i++) {
            i64 dx = x0 + i * step - ax, dz = z0 + j * step - az;
            len += dx * dx + dz * dz <= w2;
        }
    double total = (double)(len > 1 ? len : 1);
    Cl &c = climate(l);
    int lo = 0, hi = 0, left = len;
    for (int j = 0; j < n; j++)
        for (int i = 0; i < n; i++) {
            i64 dx = x0 + i * step - ax, dz = z0 + j * step - az;
            if (dx * dx + dz * dz > w2) continue;
            int b = biome_at(c, (x0 + i * step) >> 2, y >> 2, (z0 + j * step) >> 2);
            left--;
            if (b < 0) hi++;
            else if (in_set(a + 1, b)) { lo++; hi++; }
            // decided early: the upper bound already reaches min and the lower can no longer pass max
            if ((double)hi / total >= mn && (double)(lo + 0) / total <= mx && (double)(lo + left) / total <= mx) return true;
            if ((double)(hi + left) / total < mn || (double)lo / total > mx) return false;
        }
    return (double)hi / total >= mn && (double)lo / total <= mx;
}

// query::potential count (no exclusion zones), stopping at `min`; with a[16], each start's column
// must also allow the structure's biomes
__device__ bool count_atom(i64 seed, const int *a, Lazy &l) {
    int spacing = a[1], ox = a[7], oz = a[8], ax = a[9], az = a[10], r = a[11], min = a[12];
    float f = __int_as_float(a[6]);
    int gx0 = floordiv(floordiv(ax - r - ox, 16), spacing), gx1 = floordiv(floordiv(ax + r - ox, 16), spacing);
    int gz0 = floordiv(floordiv(az - r - oz, 16), spacing), gz1 = floordiv(floordiv(az + r - oz, 16), spacing);
    i64 r2 = (i64)r * r;
    int n = 0;
    for (int gx = gx0; gx <= gx1; gx++) {
        for (int gz = gz0; gz <= gz1; gz++) {
            int cx, cz;
            grid_chunk(seed, a, gx, gz, cx, cz);
            i64 dx = (i64)(cx * 16 + ox - ax), dz = (i64)(cz * 16 + oz - az);
            if (dx * dx + dz * dz <= r2 && frequency_ok(seed, a[5], f, a[4], cx, cz) && (!a[16] || column_ok(l, a, cx, cz)) && ++n >= min) return true;
        }
    }
    return n >= min;
}

__device__ bool is_slime(i64 seed, int x, int z) {
    i64 s = (i64)((u64)seed + (u64)(i64)(int)((unsigned)x * (unsigned)x * 4987142u) + (u64)(i64)(int)((unsigned)x * 5947611u)
                  + (u64)(i64)(int)((unsigned)z * (unsigned)z) * 4392871ULL + (u64)(i64)(int)((unsigned)z * 389711u)) ^ 987234911LL;
    JR r;
    jr_set(r, s);
    return jr_bound(r, 10) == 0;
}

__device__ int slime_count(i64 seed, int cx, int cz, int radius) {
    i64 r2 = (i64)radius * radius;
    int n = 0;
    for (int x = (cx - radius) >> 4; x <= (cx + radius) >> 4; x++) {
        for (int z = (cz - radius) >> 4; z <= (cz + radius) >> 4; z++) {
            i64 dx = x * 16 + 8 - cx, dz = z * 16 + 8 - cz;
            if (dx * dx + dz * dz <= r2 && is_slime(seed, x, z)) n++;
        }
    }
    return n;
}

// query::Eval::slime_max >= min
__device__ bool slime_atom(i64 seed, const int *a) {
    int radius = a[1], ax = a[2], az = a[3], within = a[4], min = a[5];
    if (within <= 0) return slime_count(seed, ax, az, radius) >= min;
    i64 w2 = (i64)within * within;
    for (int x = (ax - within) >> 4; x <= (ax + within) >> 4; x++) {
        for (int z = (az - within) >> 4; z <= (az + within) >> 4; z++) {
            i64 dx = x * 16 + 8 - ax, dz = z * 16 + 8 - az;
            if (dx * dx + dz * dz <= w2 && slime_count(seed, x * 16 + 8, z * 16 + 8, radius) >= min) return true;
        }
    }
    return false;
}

#define NEAR_MAX 48

// query::Eval::pre for cluster, weakened: some chunk within `r` of the point has need - 1 others
// whose locate positions are within `d` of its own and (for need <= 4) of each other. Every circle
// of radius d / 2 covering need chunks has such a chunk.
__device__ bool cluster_atom(i64 seed, const int *a) {
    int spacing = a[1], ox = a[7], oz = a[8], ax = a[9], az = a[10], r = a[11], need = a[12], d = a[13];
    float f = __int_as_float(a[6]);
    int gx0 = floordiv(floordiv(ax - r - ox, 16), spacing), gx1 = floordiv(floordiv(ax + r - ox, 16), spacing);
    int gz0 = floordiv(floordiv(az - r - oz, 16), spacing), gz1 = floordiv(floordiv(az + r - oz, 16), spacing);
    int k = d / (16 * spacing) + 1;  // neighbour cells that can hold a chunk within d
    i64 r2 = (i64)r * r, d2 = (i64)d * d;
    for (int gx = gx0; gx <= gx1; gx++) {
        for (int gz = gz0; gz <= gz1; gz++) {
            int cx, cz;
            grid_chunk(seed, a, gx, gz, cx, cz);
            i64 dx = (i64)(cx * 16 + ox - ax), dz = (i64)(cz * 16 + oz - az);
            if (dx * dx + dz * dz > r2 || !frequency_ok(seed, a[5], f, a[4], cx, cz)) continue;
            int m = 0, nx[NEAR_MAX], nz[NEAR_MAX];
            for (int i = -k; i <= k; i++) {
                for (int j = -k; j <= k; j++) {
                    if (i == 0 && j == 0) continue;
                    int ux, uz;
                    grid_chunk(seed, a, gx + i, gz + j, ux, uz);
                    i64 ex = (i64)(ux - cx) * 16, ez = (i64)(uz - cz) * 16;
                    if (ex * ex + ez * ez <= d2 && frequency_ok(seed, a[5], f, a[4], ux, uz)) {
                        if (m < NEAR_MAX) { nx[m] = ux; nz[m] = uz; }
                        m++;
                    }
                }
            }
            if (m + 1 < need) continue;
            if (need <= 2 || need > 4 || m > NEAR_MAX) return true;
            // need - 1 of the neighbours within d of each other
            for (int p = 0; p < m; p++) {
                for (int q = p + 1; q < m; q++) {
                    i64 ex = (i64)(nx[q] - nx[p]) * 16, ez = (i64)(nz[q] - nz[p]) * 16;
                    if (ex * ex + ez * ez > d2) continue;
                    if (need == 3) return true;
                    for (int s = q + 1; s < m; s++) {
                        i64 fx = (i64)(nx[s] - nx[p]) * 16, fz = (i64)(nz[s] - nz[p]) * 16;
                        i64 gx2 = (i64)(nx[s] - nx[q]) * 16, gz2 = (i64)(nz[s] - nz[q]) * 16;
                        if (fx * fx + fz * fz <= d2 && gx2 * gx2 + gz2 * gz2 <= d2) return true;
                    }
                }
            }
        }
    }
    return false;
}

__device__ bool cell_atom(i64 seed, const int *a) {
    int cx, cz;
    grid_chunk(seed, a, a[5], a[6], cx, cz);
    return cx == a[7] && cz == a[8];
}

__device__ bool eval(i64 seed, Lazy &l) {
    int k = 1;
    for (int c = 0; c < PROG[0]; c++) {
        int na = PROG[k++];
        bool ok = false;
        for (int i = 0; i < na && !ok; i++) {
            const int *a = PROG + k + i * ATOM;
            switch (a[0]) {
            case 0: ok = count_atom(seed, a, l); break;
            case 1: ok = slime_atom(seed, a); break;
            case 2: ok = cell_atom(seed, a); break;
            case 3: ok = cluster_atom(seed, a); break;
            default: ok = biome_atom(l, a);
            }
        }
        if (!ok) return false;
        k += na * ATOM;
    }
    return true;
}

__device__ __forceinline__ u64 splitmix(u64 z) {
    z += 0x9E3779B97F4A7C15ULL;
    z = (z ^ (z >> 30)) * 0xBF58476D1CE4E5B9ULL;
    z = (z ^ (z >> 27)) * 0x94D049BB133111EBULL;
    return z ^ (z >> 31);
}

// search.rs permute
__device__ u64 permute(u64 i, u64 n, u64 key) {
    if (n <= 1) return i;
    int k = 64 - __clzll((long long)(n - 1));
    u64 mask = k == 64 ? ~0ULL : (1ULL << k) - 1;
    int s = (k + 1) / 2;
    u64 x = i;
    for (;;) {
        x = (x ^ key) & mask;
        x = (x * 0xBF58476D1CE4E5B9ULL) & mask;
        x ^= x >> s;
        x = (x ^ (key >> 32)) & mask;
        x = (x * 0x94D049BB133111EBULL) & mask;
        x ^= x >> s;
        if (x < n) return x;
    }
}

// Units lo..lo+n of a search (shuffled by permute(unit, total, key) when total > 0); passing offsets
// (unordered) go to out. mode: 0 range (seed = base + unit), 1 structure seeds (low 48 bits of
// base + unit), 2 random (splitmix(base + unit), base = generator seed).
__device__ __forceinline__ void filter_one(u64 lo, u64 n, int mode, u64 base, u64 total, u64 key, unsigned *out, unsigned *count, unsigned cap, Cl *c) {
    u64 i = (u64)blockIdx.x * blockDim.x + threadIdx.x;
    if (i >= n) return;
    u64 u = (total ? permute(lo + i, total, key) : lo + i) + base;
    i64 seed = mode == 2 ? (i64)splitmix(u) : mode == 1 ? (i64)(u & MASK48) : (i64)u;
    Lazy l = {c, seed, false};
    if (eval(seed, l)) {
        unsigned k = atomicAdd(count, 1u);
        if (k < cap) out[k] = (unsigned)i;
    }
}

extern "C" __global__ void filter(u64 lo, u64 n, int mode, u64 base, u64 total, u64 key, unsigned *out, unsigned *count, unsigned cap) {
    filter_one(lo, n, mode, base, total, key, out, count, cap, nullptr);
}

// filter for programs with biome atoms: each thread holds a seed's climate state (~14 KB of stack)
extern "C" __global__ void filter_bio(u64 lo, u64 n, int mode, u64 base, u64 total, u64 key, unsigned *out, unsigned *count, unsigned cap) {
    Cl c;
    filter_one(lo, n, mode, base, total, key, out, count, cap, &c);
}

// Crack brute force: index idx -> seed (idx / nlows) << bits | lows[idx % nlows], for idx in base..base+n.
extern "C" __global__ void crack(const i64 *lows, u64 nlows, int bits, u64 base, u64 n, i64 *out, unsigned *count, unsigned cap) {
    u64 i = (u64)blockIdx.x * blockDim.x + threadIdx.x;
    if (i >= n) return;
    u64 idx = base + i;
    i64 seed = (i64)(((idx / nlows) << bits) | (u64)lows[idx % nlows]);
    Lazy l = {nullptr, seed, false};  // crack programs have no biome atoms
    if (eval(seed, l)) {
        unsigned k = atomicAdd(count, 1u);
        if (k < cap) out[k] = seed;
    }
}

// Parity: Seeded::biome (fresh context) for n (seed, quart) pairs; out = biome, or -1 when unsure.
extern "C" __global__ void biomes(const i64 *seeds, const int *pos, u64 n, int *out) {
    u64 i = (u64)blockIdx.x * blockDim.x + threadIdx.x;
    if (i >= n) return;
    Cl c;
    cl_init(c, seeds[i]);
    out[i] = biome_at(c, pos[3 * i], pos[3 * i + 1], pos[3 * i + 2]);
}
