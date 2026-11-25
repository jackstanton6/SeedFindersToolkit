//! CUDA pre-filter and crack brute force (kernels in gpu.cu). The driver library ships with the NVIDIA
//! driver and is loaded at runtime, so the engine runs without CUDA; no toolkit is needed to run.
//!
//! A query is lowered to clauses the GPU can check from the seed alone (structure placement counts,
//! slime chunks). Lowering only drops or weakens terms, so the GPU keeps a superset of the seeds the
//! CPU pre-check keeps, and the CPU still runs `pre` and the full check on every survivor.

use crate::density::{CSpline, S};
use crate::noise::{NoiseStack, NormalNoise};
use crate::query::{Cond, Metric, Sel};
use crate::world::World;
use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_uint, c_void, CStr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

#[cfg(sft_cuda)]
const PTX: Option<&str> = Some(concat!(include_str!(concat!(env!("OUT_DIR"), "/gpu.ptx")), "\0"));
#[cfg(not(sft_cuda))]
const PTX: Option<&str> = None;

const ATOM: usize = 24;
/// Threads per block in the biome stage (one seed per block).
const BIO_THREADS: u32 = 128;
const PROG_INTS: usize = 4096;
type Atom = [i32; ATOM];
type Ptr = u64;
type R = c_int;

#[allow(non_snake_case)]
struct Api {
    _lib: libloading::Library,
    cuInit: unsafe extern "C" fn(c_uint) -> R,
    cuDeviceGetCount: unsafe extern "C" fn(*mut c_int) -> R,
    cuDeviceGet: unsafe extern "C" fn(*mut c_int, c_int) -> R,
    cuDeviceGetName: unsafe extern "C" fn(*mut c_char, c_int, c_int) -> R,
    cuDeviceGetAttribute: unsafe extern "C" fn(*mut c_int, c_int, c_int) -> R,
    cuDevicePrimaryCtxRetain: unsafe extern "C" fn(*mut *mut c_void, c_int) -> R,
    cuCtxSetCurrent: unsafe extern "C" fn(*mut c_void) -> R,
    cuModuleLoadData: unsafe extern "C" fn(*mut *mut c_void, *const c_void) -> R,
    cuModuleGetFunction: unsafe extern "C" fn(*mut *mut c_void, *mut c_void, *const c_char) -> R,
    cuModuleGetGlobal: unsafe extern "C" fn(*mut Ptr, *mut usize, *mut c_void, *const c_char) -> R,
    cuMemAlloc: unsafe extern "C" fn(*mut Ptr, usize) -> R,
    cuMemFree: unsafe extern "C" fn(Ptr) -> R,
    cuMemcpyHtoD: unsafe extern "C" fn(Ptr, *const c_void, usize) -> R,
    cuMemcpyDtoH: unsafe extern "C" fn(*mut c_void, Ptr, usize) -> R,
    #[allow(clippy::type_complexity)]
    cuLaunchKernel: unsafe extern "C" fn(*mut c_void, c_uint, c_uint, c_uint, c_uint, c_uint, c_uint, c_uint, *mut c_void, *mut *mut c_void, *mut *mut c_void) -> R,
    cuCtxSynchronize: unsafe extern "C" fn() -> R,
    cuCtxSetLimit: unsafe extern "C" fn(c_int, usize) -> R,
    cuGetErrorString: unsafe extern "C" fn(R, *mut *const c_char) -> R,
}

impl Api {
    fn load() -> Result<Api, String> {
        let name = if cfg!(windows) { "nvcuda.dll" } else { "libcuda.so.1" };
        // SAFETY: the NVIDIA driver library; symbols are cast to their documented driver API signatures.
        unsafe {
            let lib = libloading::Library::new(name).map_err(|_| "no NVIDIA driver found".to_string())?;
            macro_rules! sym {
                ($n:literal) => {
                    *lib.get(concat!($n, "\0").as_bytes()).map_err(|e| format!("driver lacks {}: {e}", $n))?
                };
            }
            Ok(Api {
                cuInit: sym!("cuInit"),
                cuDeviceGetCount: sym!("cuDeviceGetCount"),
                cuDeviceGet: sym!("cuDeviceGet"),
                cuDeviceGetName: sym!("cuDeviceGetName"),
                cuDeviceGetAttribute: sym!("cuDeviceGetAttribute"),
                cuDevicePrimaryCtxRetain: sym!("cuDevicePrimaryCtxRetain"),
                cuCtxSetCurrent: sym!("cuCtxSetCurrent"),
                cuModuleLoadData: sym!("cuModuleLoadData"),
                cuModuleGetFunction: sym!("cuModuleGetFunction"),
                cuModuleGetGlobal: sym!("cuModuleGetGlobal_v2"),
                cuMemAlloc: sym!("cuMemAlloc_v2"),
                cuMemFree: sym!("cuMemFree_v2"),
                cuMemcpyHtoD: sym!("cuMemcpyHtoD_v2"),
                cuMemcpyDtoH: sym!("cuMemcpyDtoH_v2"),
                cuLaunchKernel: sym!("cuLaunchKernel"),
                cuCtxSynchronize: sym!("cuCtxSynchronize"),
                cuCtxSetLimit: sym!("cuCtxSetLimit"),
                cuGetErrorString: sym!("cuGetErrorString"),
                _lib: lib,
            })
        }
    }

    fn ok(&self, r: R, what: &str) -> Result<(), String> {
        if r == 0 {
            return Ok(());
        }
        let mut s: *const c_char = std::ptr::null();
        // SAFETY: cuGetErrorString returns a static string or leaves the pointer null.
        let msg = unsafe { (self.cuGetErrorString)(r, &mut s) == 0 && !s.is_null() }.then(|| unsafe { CStr::from_ptr(s) }.to_string_lossy().into_owned());
        Err(format!("{what}: {}", msg.unwrap_or_else(|| format!("CUDA error {r}"))))
    }
}

/// Device buffers, grown on demand.
struct Bufs {
    out: Ptr,
    out_bytes: usize,
    count: Ptr,
    /// Stage-2 results (biome checks over stage-1 survivors).
    out2: Ptr,
    out2_bytes: usize,
}

pub struct Gpu {
    api: Api,
    ctx: *mut c_void,
    filter: *mut c_void,
    filter_bio: *mut c_void,
    crack: *mut c_void,
    biomes: *mut c_void,
    prog: Ptr,
    clim: Ptr,
    /// Whether the per-thread stack was raised for climate programs (a context-wide setting).
    deep_stack: Mutex<bool>,
    bufs: Mutex<Bufs>,
    /// Device name and compute capability.
    pub name: String,
}

// SAFETY: the context is made current on whichever thread uses it, and every use holds `bufs`.
unsafe impl Send for Gpu {}
unsafe impl Sync for Gpu {}

static GPU: OnceLock<Result<Arc<Gpu>, String>> = OnceLock::new();

/// The first CUDA device, initialised once.
pub fn get() -> Result<Arc<Gpu>, String> {
    GPU.get_or_init(|| Gpu::new().map(Arc::new)).clone()
}

/// "NVIDIA GeForce ... (compute 12.0)" when the GPU can be used, else why not.
pub fn status() -> Result<String, String> {
    get().map(|g| g.name.clone())
}

impl Gpu {
    fn new() -> Result<Gpu, String> {
        let api = Api::load()?;
        // SAFETY: plain driver API calls with valid out-pointers; the PTX string is NUL-terminated.
        unsafe {
            api.ok((api.cuInit)(0), "cuInit")?;
            let mut n = 0;
            api.ok((api.cuDeviceGetCount)(&mut n), "cuDeviceGetCount")?;
            if n == 0 {
                return Err("no CUDA device".into());
            }
            let mut dev = 0;
            api.ok((api.cuDeviceGet)(&mut dev, 0), "cuDeviceGet")?;
            let mut buf = [0 as c_char; 256];
            api.ok((api.cuDeviceGetName)(buf.as_mut_ptr(), 256, dev), "cuDeviceGetName")?;
            let (mut major, mut minor) = (0, 0);
            api.ok((api.cuDeviceGetAttribute)(&mut major, 75, dev), "compute capability")?;
            api.ok((api.cuDeviceGetAttribute)(&mut minor, 76, dev), "compute capability")?;
            let name = format!("{} (compute {major}.{minor})", CStr::from_ptr(buf.as_ptr()).to_string_lossy());
            let Some(ptx) = PTX else {
                return Err(format!("{name} found, but this build has no CUDA kernels (nvcc was not found when the engine was built)"));
            };
            if major < 7 || (major == 7 && minor < 5) {
                return Err(format!("{name} is too old (compute 7.5 or newer needed)"));
            }
            let mut ctx = std::ptr::null_mut();
            api.ok((api.cuDevicePrimaryCtxRetain)(&mut ctx, dev), "context")?;
            api.ok((api.cuCtxSetCurrent)(ctx), "context")?;
            let mut module = std::ptr::null_mut();
            api.ok((api.cuModuleLoadData)(&mut module, ptx.as_ptr() as *const c_void), "loading kernels")?;
            let kernel = |name: &CStr| {
                let mut f = std::ptr::null_mut();
                api.ok((api.cuModuleGetFunction)(&mut f, module, name.as_ptr()), &format!("{} kernel", name.to_string_lossy())).map(|_| f)
            };
            let (filter, filter_bio, crack, biomes) = (kernel(c"filter")?, kernel(c"filter_bio")?, kernel(c"crack")?, kernel(c"biomes")?);
            let (mut prog, mut clim, mut size) = (0, 0, 0);
            api.ok((api.cuModuleGetGlobal)(&mut prog, &mut size, module, c"PROG".as_ptr()), "PROG")?;
            api.ok((api.cuModuleGetGlobal)(&mut clim, &mut size, module, c"CLIM".as_ptr()), "CLIM")?;
            if size != std::mem::size_of::<ClimC>() {
                return Err(format!("CLIM is {size} bytes on the device, {} on the host", std::mem::size_of::<ClimC>()));
            }
            let mut count = 0;
            api.ok((api.cuMemAlloc)(&mut count, 4), "cuMemAlloc")?;
            Ok(Gpu { api, ctx, filter, filter_bio, crack, biomes, prog, clim, deep_stack: Mutex::new(false), bufs: Mutex::new(Bufs { out: 0, out_bytes: 0, count, out2: 0, out2_bytes: 0 }), name })
        }
    }

    /// Uploads the program and makes sure `out` holds `bytes`. Caller holds the lock.
    unsafe fn prepare(&self, b: &mut Bufs, prog: &[i32], bytes: usize) -> Result<(), String> {
        let a = &self.api;
        a.ok((a.cuCtxSetCurrent)(self.ctx), "context")?;
        a.ok((a.cuMemcpyHtoD)(self.prog, prog.as_ptr() as *const c_void, prog.len() * 4), "program upload")?;
        if b.out_bytes < bytes {
            if b.out != 0 {
                (a.cuMemFree)(b.out);
            }
            b.out_bytes = 0;
            a.ok((a.cuMemAlloc)(&mut b.out, bytes), "cuMemAlloc")?;
            b.out_bytes = bytes;
        }
        Ok(())
    }

    /// Launches `f` over `n` threads, waits, and returns the survivor count. Caller holds the lock.
    unsafe fn launch(&self, b: &Bufs, f: *mut c_void, n: u64, args: &mut [*mut c_void]) -> Result<u32, String> {
        self.launch_blocks(b, f, n.div_ceil(256), 256, args)
    }

    /// As `launch`, with `blocks` blocks of `threads` threads.
    unsafe fn launch_blocks(&self, b: &Bufs, f: *mut c_void, blocks: u64, threads: u32, args: &mut [*mut c_void]) -> Result<u32, String> {
        let a = &self.api;
        let zero = 0u32;
        a.ok((a.cuMemcpyHtoD)(b.count, &zero as *const u32 as *const c_void, 4), "reset")?;
        a.ok((a.cuLaunchKernel)(f, blocks as c_uint, 1, 1, threads, 1, 1, 0, std::ptr::null_mut(), args.as_mut_ptr(), std::ptr::null_mut()), "launch")?;
        a.ok((a.cuCtxSynchronize)(), "kernel")?;
        let mut count = 0u32;
        a.ok((a.cuMemcpyDtoH)(&mut count as *mut u32 as *mut c_void, b.count, 4), "count")?;
        Ok(count)
    }

    /// Uploads a World's climate program (gpu.cu `Clim`); kept on the device until dropped.
    pub fn upload_climate(self: &Arc<Self>, p: &ClimateProgram) -> Result<ClimDev, String> {
        let _b = self.bufs.lock().unwrap();
        let mut d = ClimDev { gpu: self.clone(), bufs: vec![], c: ClimC::default() };
        // SAFETY: each buffer is allocated for exactly the slice it receives.
        unsafe {
            self.api.ok((self.api.cuCtxSetCurrent)(self.ctx), "context")?;
            d.c.node = d.upload(&p.nodes)?;
            d.c.k = d.upload(&p.k)?;
            d.c.aux = d.upload(&p.aux)?;
            d.c.spl = d.upload(&p.splines)?;
            d.c.roots = p.roots;
            d.c.noise = d.upload(&p.noise)?;
            d.c.noise_md5 = d.upload(&p.noise_md5)?;
            d.c.nnoise = (p.noise.len() / 2) as i32;
            d.c.layer_md5 = d.upload(&p.layer_md5)?;
            d.c.layer_freq = d.upload(&p.layer_freq)?;
            d.c.layer_amp = d.upload(&p.layer_amp)?;
            d.c.layer_second = d.upload(&p.layer_second)?;
            d.c.nlayer = p.layer_freq.len() as i32;
            d.c.tree = d.upload(&p.tree)?;
            d.c.kids = d.upload(&p.kids)?;
            d.c.space = d.upload(&p.spaces)?;
            d.c.leaf_biome = d.upload(&p.leaf_biome)?;
        }
        Ok(d)
    }

    /// Makes `clim` the climate the kernels read, and gives threads the stack its recursion needs.
    /// Caller holds the lock.
    unsafe fn use_climate(&self, clim: &ClimDev) -> Result<(), String> {
        let a = &self.api;
        let mut deep = self.deep_stack.lock().unwrap();
        if !*deep {
            // the sampler, spline and R-tree recursion (gpu.cu `sample`)
            a.ok((a.cuCtxSetLimit)(0, 16 << 10), "stack for biome checks (not enough GPU memory?)")?;
            *deep = true;
        }
        a.ok((a.cuMemcpyHtoD)(self.clim, &clim.c as *const ClimC as *const c_void, std::mem::size_of::<ClimC>()), "climate upload")
    }

    /// Seeded::biome with a fresh context for each (seed, quart x, y, z); -1 where a tie makes the
    /// GPU unsure. For parity tests.
    pub fn biomes(&self, clim: &ClimDev, seeds: &[i64], quarts: &[[i32; 3]]) -> Result<Vec<i32>, String> {
        let mut b = self.bufs.lock().unwrap();
        let n = seeds.len();
        // SAFETY: buffers sized for n entries; argument pointers live until the synchronous launch returns.
        unsafe {
            self.prepare(&mut b, &[0], n * 4)?;
            self.use_climate(clim)?;
            let a = &self.api;
            let (mut sd, mut pd) = (0, 0);
            a.ok((a.cuMemAlloc)(&mut sd, n.max(1) * 8), "cuMemAlloc")?;
            a.ok((a.cuMemAlloc)(&mut pd, n.max(1) * 12), "cuMemAlloc")?;
            let res = (|| {
                a.ok((a.cuMemcpyHtoD)(sd, seeds.as_ptr() as *const c_void, n * 8), "seeds")?;
                a.ok((a.cuMemcpyHtoD)(pd, quarts.as_ptr() as *const c_void, n * 12), "positions")?;
                let (mut s, mut p, mut out) = (sd, pd, b.out);
                let mut args: [*mut c_void; 3] = [&mut s as *mut _ as _, &mut p as *mut _ as _, &mut out as *mut _ as _];
                self.launch_blocks(&b, self.biomes, n as u64, 64, &mut args)?;
                let mut v = vec![0i32; n];
                a.ok((a.cuMemcpyDtoH)(v.as_mut_ptr() as *mut c_void, b.out, n * 4), "results")?;
                Ok(v)
            })();
            (a.cuMemFree)(sd);
            (a.cuMemFree)(pd);
            res
        }
    }

    /// Offsets in [0, n) of the search units lo..lo+n passing the program (sorted). `mode`: 0 range
    /// (seed = base + unit), 1 structure seeds, 2 random (seed = splitmix(base + unit)). `shuffle`:
    /// (total units, key) as in search.rs `permute`. Programs with biome atoms need `clim`.
    #[allow(clippy::too_many_arguments)]
    pub fn filter(&self, prog: &[i32], clim: Option<&ClimDev>, mode: i32, base: u64, lo: u64, n: u64, shuffle: Option<(u64, u64)>) -> Result<Vec<u32>, String> {
        let mut b = self.bufs.lock().unwrap();
        let (mut total, mut key) = shuffle.unwrap_or((0, 0));
        // SAFETY: argument pointers live until the synchronous launch returns; buffers hold n entries.
        unsafe {
            self.prepare(&mut b, prog, n as usize * 4)?;
            if let Some(c) = clim {
                self.use_climate(c)?;
            }
            let (mut lo, mut base, mut n, mut mode, mut out, mut count, mut cap) = (lo, base, n, mode, b.out, b.count, n as u32);
            let mut args: [*mut c_void; 9] = [
                &mut lo as *mut _ as _, &mut n as *mut _ as _, &mut mode as *mut _ as _, &mut base as *mut _ as _,
                &mut total as *mut _ as _, &mut key as *mut _ as _, &mut out as *mut _ as _, &mut count as *mut _ as _, &mut cap as *mut _ as _,
            ];
            let mut k = self.launch(&b, self.filter, n, &mut args)?.min(cap) as usize;
            let mut res = b.out;
            if clim.is_some() && k > 0 {
                // stage 2: a block per survivor checks the biomes
                let a = &self.api;
                if b.out2_bytes < k * 4 {
                    if b.out2 != 0 {
                        (a.cuMemFree)(b.out2);
                    }
                    b.out2_bytes = 0;
                    a.ok((a.cuMemAlloc)(&mut b.out2, b.out_bytes), "cuMemAlloc")?;
                    b.out2_bytes = b.out_bytes;
                }
                let (mut surv, mut out2) = (b.out, b.out2);
                let mut args: [*mut c_void; 9] = [
                    &mut surv as *mut _ as _, &mut lo as *mut _ as _, &mut mode as *mut _ as _, &mut base as *mut _ as _,
                    &mut total as *mut _ as _, &mut key as *mut _ as _, &mut out2 as *mut _ as _, &mut count as *mut _ as _, &mut cap as *mut _ as _,
                ];
                k = self.launch_blocks(&b, self.filter_bio, k as u64, BIO_THREADS, &mut args)?.min(cap) as usize;
                res = b.out2;
            }
            let mut v = vec![0u32; k];
            self.api.ok((self.api.cuMemcpyDtoH)(v.as_mut_ptr() as *mut c_void, res, k * 4), "results")?;
            v.sort_unstable();
            Ok(v)
        }
    }

    /// Crack brute force: every seed (i << bits) | lows[j] passing the program. `done` counts seeds tried.
    pub fn crack(&self, prog: &[i32], lows: &[i64], bits: u32, stop: &AtomicBool, done: &AtomicU64) -> Result<Vec<i64>, String> {
        const CAP: usize = 1 << 20;
        let total = (lows.len() as u64) << (48 - bits);
        let mut b = self.bufs.lock().unwrap();
        let a = &self.api;
        let mut found = vec![];
        // SAFETY: as in `filter`; `lows_dev` holds lows.len() entries and is freed before returning.
        unsafe {
            self.prepare(&mut b, prog, CAP * 8)?;
            let mut lows_dev = 0;
            a.ok((a.cuMemAlloc)(&mut lows_dev, lows.len().max(1) * 8), "cuMemAlloc")?;
            let res = (|| {
                a.ok((a.cuMemcpyHtoD)(lows_dev, lows.as_ptr() as *const c_void, lows.len() * 8), "lows")?;
                let (mut lo, mut batch) = (0u64, 1u64 << 20);
                while lo < total && !stop.load(Relaxed) {
                    let n = batch.min(total - lo);
                    let t = Instant::now();
                    let (mut lp, mut nl, mut bits, mut base, mut nn, mut out, mut count, mut cap) = (lows_dev, lows.len() as u64, bits as c_int, lo, n, b.out, b.count, CAP as u32);
                    let mut args: [*mut c_void; 8] = [
                        &mut lp as *mut _ as _, &mut nl as *mut _ as _, &mut bits as *mut _ as _, &mut base as *mut _ as _,
                        &mut nn as *mut _ as _, &mut out as *mut _ as _, &mut count as *mut _ as _, &mut cap as *mut _ as _,
                    ];
                    let k = self.launch(&b, self.crack, n, &mut args)? as usize;
                    if k > CAP {
                        return Err("too many candidate seeds; add observations".to_string());
                    }
                    let mut v = vec![0i64; k];
                    a.ok((a.cuMemcpyDtoH)(v.as_mut_ptr() as *mut c_void, b.out, k * 8), "results")?;
                    found.extend(v);
                    done.fetch_add(n, Relaxed);
                    lo += n;
                    batch = next_batch(batch, t, 1024, 1 << 34);
                }
                Ok(())
            })();
            (a.cuMemFree)(lows_dev);
            res?;
        }
        Ok(found)
    }
}

/// Launch sizes follow the measured speed (~50 ms per launch) so slow programs stay responsive and
/// clear of the display driver watchdog (2 s on Windows).
pub fn next_batch(batch: u64, started: Instant, min: u64, max: u64) -> u64 {
    let ms = started.elapsed().as_secs_f64() * 1000.0;
    if ms < 25.0 { (batch * 2).min(max) } else if ms > 100.0 { (batch / 2).max(min) } else { batch }
}

/// gpu.cu `Clim`: device pointers to a ClimateProgram's arrays.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ClimC {
    node: Ptr,
    k: Ptr,
    aux: Ptr,
    spl: Ptr,
    roots: [i32; 6],
    ydep: u32,
    noise: Ptr,
    noise_md5: Ptr,
    nnoise: i32,
    layer_md5: Ptr,
    layer_freq: Ptr,
    layer_amp: Ptr,
    layer_second: Ptr,
    nlayer: i32,
    tree: Ptr,
    kids: Ptr,
    space: Ptr,
    leaf_biome: Ptr,
}

/// A climate program on the device (freed on drop).
pub struct ClimDev {
    gpu: Arc<Gpu>,
    bufs: Vec<Ptr>,
    c: ClimC,
}

// SAFETY: device pointers only; every use goes through the Gpu lock.
unsafe impl Send for ClimDev {}
unsafe impl Sync for ClimDev {}

impl ClimDev {
    unsafe fn upload<T: Copy>(&mut self, v: &[T]) -> Result<Ptr, String> {
        let a = &self.gpu.api;
        let bytes = std::mem::size_of_val(v).max(8);
        let mut p = 0;
        a.ok((a.cuMemAlloc)(&mut p, bytes), "cuMemAlloc")?;
        self.bufs.push(p);
        a.ok((a.cuMemcpyHtoD)(p, v.as_ptr() as *const c_void, std::mem::size_of_val(v)), "climate upload")?;
        Ok(p)
    }
}

impl Drop for ClimDev {
    fn drop(&mut self) {
        let _b = self.gpu.bufs.lock().unwrap();
        for &p in &self.bufs {
            // SAFETY: allocated by `upload` and not used after the owner drops.
            unsafe { (self.gpu.api.cuMemFree)(p) };
        }
    }
}

// ------------------------------------------------------------------------------------------------
// Climate: the compiled climate samplers, their noises and the biome R-tree, flattened for gpu.cu
// ------------------------------------------------------------------------------------------------

/// Ints per sampler node: [op, a, b, c, i0..i5, k0..k3] (a/b/c child nodes, k = constant indices).
pub const NODE: usize = 14;
/// Ints per spline node: [0, constant] or [1, coord, points, first location, first derivative, first value].
pub const SPL: usize = 6;
/// Memo slots (cache nodes) and noise layers the device keeps per thread (gpu.cu has the same).
pub const MAX_CACHES: usize = 32;
pub const MAX_LAYERS: usize = 64;

/// gpu.cu `Clim`, host side. Built once per World from seed 0's samplers (their shape does not
/// depend on the seed; only the noises do, and the device creates those per seed).
#[derive(Default)]
pub struct ClimateProgram {
    pub nodes: Vec<i32>,
    pub k: Vec<f64>,
    pub aux: Vec<i32>,
    pub splines: Vec<i32>,
    pub roots: [i32; 6],
    pub caches: i32,
    /// Per noise: first layer and layer count; its id hash is in `noise_md5` (lo, hi).
    pub noise: Vec<i32>,
    pub noise_md5: Vec<u64>,
    /// Per layer: octave name hash (lo, hi), frequency, amplitude, and which fork (0 first, 1 second).
    pub layer_md5: Vec<u64>,
    pub layer_freq: Vec<f64>,
    pub layer_amp: Vec<f32>,
    pub layer_second: Vec<i32>,
    /// R-tree (see `Climate::flat`), root = node 0.
    pub tree: Vec<i32>,
    pub kids: Vec<i32>,
    pub spaces: Vec<i64>,
    pub leaf_biome: Vec<i32>,
}

struct Enc<'a> {
    p: ClimateProgram,
    noise_ids: HashMap<*const NoiseStack, i32>,
    names: &'a HashMap<String, Arc<NoiseStack>>,
    params: &'a HashMap<String, NormalNoise>,
    slots: HashMap<usize, i32>,
    shared: HashMap<*const S, (i32, bool)>,
}

impl Enc<'_> {
    fn k(&mut self, v: f64) -> i32 {
        self.p.k.push(v);
        (self.p.k.len() - 1) as i32
    }

    fn noise(&mut self, n: &Arc<NoiseStack>) -> Result<i32, String> {
        let key = Arc::as_ptr(n);
        if let Some(&i) = self.noise_ids.get(&key) {
            return Ok(i);
        }
        let name = self.names.iter().find(|(_, v)| Arc::ptr_eq(v, n)).map(|(k, _)| k.clone()).ok_or("noise not created by the compiler")?;
        let params = &self.params[&name];
        let first = self.p.layer_freq.len() as i32;
        for (h, freq, amp) in params.octave_layers() {
            for (second, f) in [(0, freq), (1, freq * 1.0181268882175227)] {
                self.p.layer_md5.extend([h.md5().0, h.md5().1]);
                self.p.layer_freq.push(f);
                self.p.layer_amp.push(amp);
                self.p.layer_second.push(second);
            }
        }
        if self.p.layer_freq.len() > MAX_LAYERS {
            return Err(format!("climate noises have more than {MAX_LAYERS} layers"));
        }
        let i = (self.p.noise.len() / 2) as i32;
        self.p.noise.extend([first, self.p.layer_freq.len() as i32 - first]);
        self.p.noise_md5.extend([params.id.md5().0, params.id.md5().1]);
        self.noise_ids.insert(key, i);
        Ok(i)
    }

    fn spline(&mut self, sp: &CSpline) -> i32 {
        let id = (self.p.splines.len() / SPL) as i32;
        match sp {
            CSpline::Const(v) => {
                let k = self.k(*v as f64);
                self.p.splines.extend([0, k, 0, 0, 0, 0]);
            }
            CSpline::Multi { coord, loc, val, der } => {
                self.p.splines.extend([1, *coord as i32, loc.len() as i32, 0, 0, 0]);
                let lk = self.p.k.len() as i32;
                self.p.k.extend(loc.iter().map(|&v| v as f64));
                let dk = self.p.k.len() as i32;
                self.p.k.extend(der.iter().map(|&v| v as f64));
                let kids: Vec<i32> = val.iter().map(|v| self.spline(v)).collect();
                let va = self.p.aux.len() as i32;
                self.p.aux.extend(kids);
                let at = id as usize * SPL;
                self.p.splines[at + 3..at + 6].copy_from_slice(&[lk, dk, va]);
            }
        }
        id
    }

    /// Encodes children into n[1..], returns whether any depends on y.
    fn kids(&mut self, n: &mut [i32; NODE], list: &[&S]) -> Result<bool, String> {
        let mut y = false;
        for (i, c) in list.iter().enumerate() {
            let (id, cy) = self.node(c)?;
            n[1 + i] = id;
            y |= cy;
        }
        Ok(y)
    }

    /// Node id of `s` and whether its value depends on y.
    fn node(&mut self, s: &S) -> Result<(i32, bool), String> {
        let mut n = [0i32; NODE];
        let ydep = match s {
            S::Const(v) => {
                n[10] = self.k(*v as f64);
                false
            }
            S::Noise(nz, xz, ys) => {
                n[0] = 1;
                n[4] = self.noise(nz)?;
                n[10] = self.k(*xz);
                n[11] = self.k(*ys);
                *ys != 0.0
            }
            S::ShiftedXz(sx, sz, nz, xz, ys) => {
                let y = self.kids(&mut n, &[sx, sz])?;
                n[0] = 2;
                n[4] = self.noise(nz)?;
                n[10] = self.k(*xz);
                n[11] = self.k(*ys);
                y || *ys != 0.0
            }
            S::ShiftedXyz(sx, sy, sz, nz, xz, ys) => {
                let y = self.kids(&mut n, &[sx, sy, sz])?;
                n[0] = 3;
                n[4] = self.noise(nz)?;
                n[10] = self.k(*xz);
                n[11] = self.k(*ys);
                y || *ys != 0.0
            }
            S::ShiftB(nz) => {
                n[0] = 4;
                n[4] = self.noise(nz)?;
                false
            }
            S::ConstAdd(l, c) | S::ConstMul(l, c) | S::ConstMin(l, c) | S::ConstMax(l, c) => {
                let y = self.kids(&mut n, &[l])?;
                n[0] = match s {
                    S::ConstAdd(..) => 5,
                    S::ConstMul(..) => 7,
                    S::ConstMin(..) => 9,
                    _ => 10,
                };
                n[10] = self.k(*c as f64);
                y
            }
            S::ConstSub(c, r) | S::ConstDiv(c, r) => {
                let y = self.kids(&mut n, &[r])?;
                n[0] = if matches!(s, S::ConstSub(..)) { 6 } else { 8 };
                n[10] = self.k(*c as f64);
                y
            }
            S::Add(l, r) | S::Sub(l, r) | S::Mul(l, r) | S::Div(l, r) => {
                let y = self.kids(&mut n, &[l, r])?;
                n[0] = match s {
                    S::Add(..) => 11,
                    S::Sub(..) => 12,
                    S::Mul(..) => 13,
                    _ => 14,
                };
                y
            }
            S::Min(l, r, b) | S::Max(l, r, b) => {
                let y = self.kids(&mut n, &[l, r])?;
                n[0] = if matches!(s, S::Min(..)) { 15 } else { 16 };
                n[10] = self.k(*b as f64);
                y
            }
            S::Grad { axis, tiling, from, lo, hi, range, fv, factor } => {
                n[..10].copy_from_slice(&[17, 0, 0, 0, *axis as i32, *tiling as i32, *from, *lo, *hi, *range]);
                n[10] = self.k(*fv as f64);
                n[11] = self.k(*factor as f64);
                *axis == 2
            }
            S::Unary(op, i) => {
                let y = self.kids(&mut n, &[i])?;
                n[0] = 18;
                n[4] = *op as i32;
                y
            }
            S::Clamp(i, lo, hi) => {
                let y = self.kids(&mut n, &[i])?;
                n[0] = 19;
                n[10] = self.k(*lo as f64);
                n[11] = self.k(*hi as f64);
                y
            }
            S::Lerp(a, f, t) => {
                let y = self.kids(&mut n, &[a, f, t])?;
                n[0] = 20;
                y
            }
            S::LerpCF(a, f, t) => {
                let y = self.kids(&mut n, &[a, t])?;
                n[0] = 21;
                n[10] = self.k(*f as f64);
                y
            }
            S::LerpCS(a, f, t) => {
                let y = self.kids(&mut n, &[a, f])?;
                n[0] = 22;
                n[10] = self.k(*t as f64);
                y
            }
            S::RangeConst(i, lo, hi, x, z) => {
                let y = self.kids(&mut n, &[i])?;
                n[0] = 23;
                for (j, v) in [lo, hi, x, z].into_iter().enumerate() {
                    n[10 + j] = self.k(*v as f64);
                }
                y
            }
            S::Range(i, lo, hi, a, b) => {
                let y = self.kids(&mut n, &[i, a, b])?;
                n[0] = 24;
                n[10] = self.k(*lo as f64);
                n[11] = self.k(*hi as f64);
                y
            }
            S::Sel1(i, t, lo, hi) => {
                let y = self.kids(&mut n, &[i, lo, hi])?;
                n[0] = 25;
                n[10] = self.k(*t as f64);
                y
            }
            S::Sel(i, ts, fs) => {
                let mut y = self.kids(&mut n, &[i])?;
                let mut ids = vec![];
                for f in fs {
                    let (id, fy) = self.node(f)?;
                    ids.push(id);
                    y |= fy;
                }
                n[0] = 26;
                n[4] = self.p.aux.len() as i32;
                n[5] = ids.len() as i32;
                self.p.aux.extend(ids);
                n[6] = self.p.k.len() as i32;
                n[7] = ts.len() as i32;
                self.p.k.extend(ts.iter().map(|&t| t as f64));
                y
            }
            S::Spline(sp, coords) => {
                let mut y = false;
                let mut ids = vec![];
                for c in coords {
                    let (id, cy) = self.node(c)?;
                    ids.push(id);
                    y |= cy;
                }
                n[0] = 27;
                n[4] = self.spline(sp);
                n[5] = self.p.aux.len() as i32;
                n[6] = ids.len() as i32;
                self.p.aux.extend(ids);
                y
            }
            S::Cache(id, i) => {
                let (inner, y) = self.shared_node(i)?;
                let next = self.slots.len() as i32;
                let slot = *self.slots.entry(*id).or_insert(next);
                if slot as usize >= MAX_CACHES {
                    return Err(format!("climate samplers have more than {MAX_CACHES} caches"));
                }
                n[..6].copy_from_slice(&[28, inner, 0, 0, slot, y as i32]);
                y
            }
            S::Shared(i) => return self.shared_node(i),
            S::SliceX(i, c) | S::SliceZ(i, c) => {
                let y = self.kids(&mut n, &[i])?;
                n[0] = if matches!(s, S::SliceX(..)) { 29 } else { 31 };
                n[4] = *c;
                y
            }
            S::SliceY(i, c) => {
                self.kids(&mut n, &[i])?;
                n[0] = 30;
                n[4] = *c;
                false
            }
            S::SliceXz(i, cx, cz) => {
                let y = self.kids(&mut n, &[i])?;
                n[0] = 32;
                n[4] = *cx;
                n[5] = *cz;
                y
            }
            S::Interp(..) | S::FindTop(..) => return Err("climate samplers use volume-only functions".into()),
        };
        self.p.nodes.extend(n);
        Ok(((self.p.nodes.len() / NODE - 1) as i32, ydep))
    }

    /// Arc'd subtrees (caches shared between climate parameters) are encoded once.
    fn shared_node(&mut self, s: &Arc<S>) -> Result<(i32, bool), String> {
        let key = Arc::as_ptr(s);
        if let Some(&r) = self.shared.get(&key) {
            return Ok(r);
        }
        let r = self.node(s)?;
        self.shared.insert(key, r);
        Ok(r)
    }
}

impl ClimateProgram {
    pub fn new(w: &World) -> Result<ClimateProgram, String> {
        let seeded = crate::world::Seeded::new(w, 0);
        if !seeded.xoroshiro() {
            return Err("legacy (JavaRandom) worlds are not ported to the GPU".into());
        }
        let (roots, comp) = seeded.climate_samplers();
        let mut e = Enc { p: ClimateProgram::default(), noise_ids: HashMap::new(), names: comp.noises(), params: comp.noise_params, slots: HashMap::new(), shared: HashMap::new() };
        for (i, r) in roots.iter().enumerate() {
            e.p.roots[i] = e.shared_node(r)?.0;
        }
        e.p.caches = e.slots.len() as i32;
        let (tree, kids, spaces, biomes) = w.climate.flat();
        e.p.tree = tree.into_iter().flatten().collect();
        e.p.kids = kids;
        e.p.spaces = spaces;
        e.p.leaf_biome = biomes.into_iter().map(i32::from).collect();
        Ok(e.p)
    }
}

// ------------------------------------------------------------------------------------------------
// Lowering
// ------------------------------------------------------------------------------------------------

fn count_atom(w: &World, s: &Sel, at: (i32, i32), within: i32, min: f64) -> Option<Atom> {
    let p = w.structures.gpu_spread(s.set)?;
    let mut a = [0; ATOM];
    a[1..9].copy_from_slice(&p);
    a[9..13].copy_from_slice(&[at.0, at.1, within, min.ceil().clamp(1.0, 1e6) as i32]);
    Some(a)
}

/// A count atom whose starts must also have a column allowing their biomes (full seeds only).
fn with_column(w: &World, s: &Sel, mut a: Atom) -> Atom {
    if let Some((off, biomes)) = w.structures.gpu_column(s.set, s.only) {
        let (min_y, max_y) = (w.min_y, w.min_y + w.height - 1);
        a[16] = 1;
        a[17] = off;
        a[18..22].copy_from_slice(&set_ints(biomes));
        a[22] = (min_y - 1) >> 2;
        a[23] = max_y >> 2;
    }
    a
}

fn set_ints(b: u128) -> [i32; 4] {
    std::array::from_fn(|i| (b >> (32 * i)) as u32 as i32)
}

fn f64_ints(v: f64) -> [i32; 2] {
    [v.to_bits() as u32 as i32, (v.to_bits() >> 32) as u32 as i32]
}

/// Clauses (each an OR of atoms) that every seed passing `c` satisfies. `full`: the GPU sees whole
/// 64-bit seeds (not structure seeds), so biome atoms can be used.
fn clauses(w: &World, c: &Cond, full: bool) -> Vec<Vec<Atom>> {
    let one = |a: Option<Atom>| a.map(|a| vec![a]).into_iter().collect::<Vec<_>>();
    match c {
        Cond::And(v) => v.iter().flat_map(|c| clauses(w, c, full)).collect(),
        // (a1 and a2) or b implies a1 or b: one clause per branch, if every branch has one
        Cond::Or(v) => v.iter().map(|c| clauses(w, c, full).into_iter().next()).collect::<Option<Vec<_>>>().map(|f| vec![f.concat()]).unwrap_or_default(),
        Cond::Metric { m: Metric::Biome { biomes, at, within, y, step }, min, max } if full && *step > 0 && *within >= 0 => {
            let mut a = [0; ATOM];
            a[0] = 4;
            a[1..5].copy_from_slice(&set_ints(*biomes));
            a[5..10].copy_from_slice(&[at.0, at.1, *within, *y, *step]);
            a[10..12].copy_from_slice(&f64_ints(*min));
            a[12..14].copy_from_slice(&f64_ints(*max));
            vec![vec![a]]
        }
        Cond::Metric { m, min, .. } if *min >= 1.0 => match m {
            Metric::Count { s, at, within } => one(count_atom(w, s, *at, *within, *min).map(|a| if full { with_column(w, s, a) } else { a })),
            Metric::Loot { s, at, within, .. } => one(count_atom(w, s, *at, *within, 1.0).map(|a| if full { with_column(w, s, a) } else { a })),
            Metric::Portal { set, at, within, .. } => one(count_atom(w, &Sel { set: *set, only: None, biomes: None, tags: vec![] }, *at, *within, 1.0)),
            // as in Eval::pre: starts sit up to ~128 blocks from their locate position
            Metric::Near { a, b, dist, at, within } => {
                let others = if a.set == b.set { 2.0 } else { 1.0 };
                [count_atom(w, a, *at, *within, *min), count_atom(w, b, *at, within + dist + 128, others)].into_iter().flat_map(one).collect()
            }
            Metric::Cluster { sels, dist, at, within, .. } if sels.len() == 1 => {
                let slack = w.structures.reach(sels[0].set);
                one(count_atom(w, &sels[0], *at, within + dist + slack, *min).map(|mut a| {
                    if *min >= 2.0 {
                        a[0] = 3;
                        a[13] = 2 * (dist + slack);
                    }
                    a
                }))
            }
            Metric::Slime { radius, at, within } => {
                let mut a = [0; ATOM];
                a[..6].copy_from_slice(&[1, *radius, at.0, at.1, *within, min.ceil().min(1e6) as i32]);
                vec![vec![a]]
            }
            _ => vec![],
        },
        _ => vec![],
    }
}

fn encode(mut cl: Vec<Vec<Atom>>) -> Vec<i32> {
    // cheapest first: placement, then slime and cluster scans, then anything reading biomes
    cl.sort_by_key(|c| c.iter().map(|a| if a[0] == 4 || a[16] != 0 { 2 } else if a[0] == 1 || a[0] == 3 { 1 } else { 0 }).max());
    let mut p = vec![0];
    for c in cl {
        if p.len() + 1 + c.len() * ATOM > PROG_INTS {
            break; // dropping a clause only weakens the filter
        }
        p[0] += 1;
        p.push(c.len() as i32);
        p.extend(c.iter().flatten());
    }
    if p[0] == 0 { vec![] } else { p }
}

/// The GPU program for a query; empty when nothing in it can be checked on the GPU. `full`: the
/// units are whole seeds, so biome checks can be lowered too (not in structure-seed mode).
pub fn program(w: &World, c: &Cond, full: bool) -> Vec<i32> {
    encode(clauses(w, c, full))
}

/// Whether a program reads biomes (it then needs a ClimDev and the filter_bio kernel).
pub fn uses_climate(prog: &[i32]) -> bool {
    if prog.is_empty() {
        return false;
    }
    let mut k = 1;
    for _ in 0..prog[0] {
        let na = prog[k] as usize;
        k += 1;
        for a in prog[k..k + na * ATOM].chunks(ATOM) {
            if a[0] == 4 || (a[0] == 0 && a[16] != 0) {
                return true;
            }
        }
        k += na * ATOM;
    }
    false
}

/// Crack program: every observed start's grid cell lands on its chunk ([spacing, sep, tri, salt, gx, gz, cx, cz]).
pub fn crack_program(cells: &[[i32; 8]]) -> Vec<i32> {
    encode(cells.iter().map(|c| {
        let mut a = [0; ATOM];
        a[0] = 2;
        a[1..9].copy_from_slice(c);
        vec![a]
    }).collect())
}
