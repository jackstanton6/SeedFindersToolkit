//! CUDA pre-filter and crack brute force (kernels in gpu.cu). The driver library ships with the NVIDIA
//! driver and is loaded at runtime, so the engine runs without CUDA; no toolkit is needed to run.
//!
//! A query is lowered to clauses the GPU can check from the seed alone (structure placement counts,
//! slime chunks). Lowering only drops or weakens terms, so the GPU keeps a superset of the seeds the
//! CPU pre-check keeps, and the CPU still runs `pre` and the full check on every survivor.

use crate::query::{Cond, Metric, Sel};
use crate::world::World;
use std::ffi::{c_char, c_int, c_uint, c_void, CStr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

#[cfg(sft_cuda)]
const PTX: Option<&str> = Some(concat!(include_str!(concat!(env!("OUT_DIR"), "/gpu.ptx")), "\0"));
#[cfg(not(sft_cuda))]
const PTX: Option<&str> = None;

const ATOM: usize = 16;
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
}

pub struct Gpu {
    api: Api,
    ctx: *mut c_void,
    filter: *mut c_void,
    crack: *mut c_void,
    prog: Ptr,
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
            let (mut filter, mut crack) = (std::ptr::null_mut(), std::ptr::null_mut());
            api.ok((api.cuModuleGetFunction)(&mut filter, module, c"filter".as_ptr()), "filter kernel")?;
            api.ok((api.cuModuleGetFunction)(&mut crack, module, c"crack".as_ptr()), "crack kernel")?;
            let (mut prog, mut size) = (0, 0);
            api.ok((api.cuModuleGetGlobal)(&mut prog, &mut size, module, c"PROG".as_ptr()), "PROG")?;
            let mut count = 0;
            api.ok((api.cuMemAlloc)(&mut count, 4), "cuMemAlloc")?;
            Ok(Gpu { api, ctx, filter, crack, prog, bufs: Mutex::new(Bufs { out: 0, out_bytes: 0, count }), name })
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
        let a = &self.api;
        let zero = 0u32;
        a.ok((a.cuMemcpyHtoD)(b.count, &zero as *const u32 as *const c_void, 4), "reset")?;
        let blocks = n.div_ceil(256) as c_uint;
        a.ok((a.cuLaunchKernel)(f, blocks, 1, 1, 256, 1, 1, 0, std::ptr::null_mut(), args.as_mut_ptr(), std::ptr::null_mut()), "launch")?;
        a.ok((a.cuCtxSynchronize)(), "kernel")?;
        let mut count = 0u32;
        a.ok((a.cuMemcpyDtoH)(&mut count as *mut u32 as *mut c_void, b.count, 4), "count")?;
        Ok(count)
    }

    /// Offsets in [0, n) of the search units lo..lo+n passing the program (sorted). `mode`: 0 range
    /// (seed = base + unit), 1 structure seeds, 2 random (seed = splitmix(base + unit)). `shuffle`:
    /// (total units, key) as in search.rs `permute`.
    pub fn filter(&self, prog: &[i32], mode: i32, base: u64, lo: u64, n: u64, shuffle: Option<(u64, u64)>) -> Result<Vec<u32>, String> {
        let mut b = self.bufs.lock().unwrap();
        let (mut total, mut key) = shuffle.unwrap_or((0, 0));
        // SAFETY: argument pointers live until the synchronous launch returns; buffers hold n entries.
        unsafe {
            self.prepare(&mut b, prog, n as usize * 4)?;
            let (mut lo, mut base, mut n, mut mode, mut out, mut count, mut cap) = (lo, base, n, mode, b.out, b.count, n as u32);
            let mut args: [*mut c_void; 9] = [
                &mut lo as *mut _ as _, &mut n as *mut _ as _, &mut mode as *mut _ as _, &mut base as *mut _ as _,
                &mut total as *mut _ as _, &mut key as *mut _ as _, &mut out as *mut _ as _, &mut count as *mut _ as _, &mut cap as *mut _ as _,
            ];
            let k = self.launch(&b, self.filter, n, &mut args)?.min(cap) as usize;
            let mut v = vec![0u32; k];
            self.api.ok((self.api.cuMemcpyDtoH)(v.as_mut_ptr() as *mut c_void, b.out, k * 4), "results")?;
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
                    batch = next_batch(batch, t, 1 << 34);
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
pub fn next_batch(batch: u64, started: Instant, max: u64) -> u64 {
    let ms = started.elapsed().as_secs_f64() * 1000.0;
    if ms < 25.0 { (batch * 2).min(max) } else if ms > 100.0 { (batch / 2).max(1024) } else { batch }
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

/// Clauses (each an OR of atoms) that every seed passing `c` satisfies.
fn clauses(w: &World, c: &Cond) -> Vec<Vec<Atom>> {
    let one = |a: Option<Atom>| a.map(|a| vec![a]).into_iter().collect::<Vec<_>>();
    match c {
        Cond::And(v) => v.iter().flat_map(|c| clauses(w, c)).collect(),
        // (a1 and a2) or b implies a1 or b: one clause per branch, if every branch has one
        Cond::Or(v) => v.iter().map(|c| clauses(w, c).into_iter().next()).collect::<Option<Vec<_>>>().map(|f| vec![f.concat()]).unwrap_or_default(),
        Cond::Metric { m, min, .. } if *min >= 1.0 => match m {
            Metric::Count { s, at, within } => one(count_atom(w, s, *at, *within, *min)),
            Metric::Loot { s, at, within, .. } => one(count_atom(w, s, *at, *within, 1.0)),
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
    cl.sort_by_key(|c| c.iter().any(|a| a[0] == 1 || a[0] == 3)); // slime and cluster scans are the expensive ones: last
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

/// The GPU program for a query; empty when nothing in it is seed-only.
pub fn program(w: &World, c: &Cond) -> Vec<i32> {
    encode(clauses(w, c))
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
