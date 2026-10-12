// Seed-only pre-filter and crack brute force on CUDA. Compiled to PTX by build.rs (nvcc), loaded by
// gpu.rs through the driver API. Literal ports of rng.rs (JavaRandom), structure.rs (grid_chunk,
// frequency_ok) and query.rs (potential, slime_max); the CPU re-checks every survivor exactly.
//
// Program (constant memory): [clauses, then per clause: atoms, atoms * 16 ints]. A seed passes when
// every clause has a passing atom. Atoms:
//   0 COUNT  spacing separation triangular salt reduction freq_bits ox oz at_x at_z within min
//   1 SLIME  radius at_x at_z within min
//   2 CELL   spacing separation triangular salt gx gz cx cz

typedef long long i64;
typedef unsigned long long u64;

#define MULT 0x5DEECE66DULL
#define MASK48 ((1ULL << 48) - 1)
#define ATOM 16

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

// query::potential count (no exclusion zones), stopping at `min`
__device__ bool count_atom(i64 seed, const int *a) {
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
            if (dx * dx + dz * dz <= r2 && frequency_ok(seed, a[5], f, a[4], cx, cz) && ++n >= min) return true;
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

__device__ bool cell_atom(i64 seed, const int *a) {
    int cx, cz;
    grid_chunk(seed, a, a[5], a[6], cx, cz);
    return cx == a[7] && cz == a[8];
}

__device__ bool eval(i64 seed) {
    int k = 1;
    for (int c = 0; c < PROG[0]; c++) {
        int na = PROG[k++];
        bool ok = false;
        for (int i = 0; i < na && !ok; i++) {
            const int *a = PROG + k + i * ATOM;
            ok = a[0] == 0 ? count_atom(seed, a) : a[0] == 1 ? slime_atom(seed, a) : cell_atom(seed, a);
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

// Units base..base+n of a search; passing offsets (unordered) go to out. mode: 0 range (seed = base + i),
// 1 structure seeds (low 48 bits of base + i), 2 random (splitmix(rng + base + i)).
extern "C" __global__ void filter(u64 base, u64 n, int mode, u64 rng, unsigned *out, unsigned *count, unsigned cap) {
    u64 i = (u64)blockIdx.x * blockDim.x + threadIdx.x;
    if (i >= n) return;
    u64 u = base + i;
    i64 seed = mode == 2 ? (i64)splitmix(rng + u) : mode == 1 ? (i64)(u & MASK48) : (i64)u;
    if (eval(seed)) {
        unsigned k = atomicAdd(count, 1u);
        if (k < cap) out[k] = (unsigned)i;
    }
}

// Crack brute force: index idx -> seed (idx / nlows) << bits | lows[idx % nlows], for idx in base..base+n.
extern "C" __global__ void crack(const i64 *lows, u64 nlows, int bits, u64 base, u64 n, i64 *out, unsigned *count, unsigned cap) {
    u64 i = (u64)blockIdx.x * blockDim.x + threadIdx.x;
    if (i >= n) return;
    u64 idx = base + i;
    i64 seed = (i64)(((idx / nlows) << bits) | (u64)lows[idx % nlows]);
    if (eval(seed)) {
        unsigned k = atomicAdd(count, 1u);
        if (k < cap) out[k] = seed;
    }
}
