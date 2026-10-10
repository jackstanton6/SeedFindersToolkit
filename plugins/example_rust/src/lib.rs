//! Example native plugin. Build with `cargo build --release` in this folder; the app loads
//! target/release/example_rust.{dll,so,dylib} (see plugin.toml). Use in a query:
//!     native("example_rust", "spawn_island", radius=96)
//!     native("example_rust", "tall_spawn", min=160)

use std::ffi::{c_char, c_void, CStr};

/// Host API, ABI version 1. Must match `SftHost` in engine/src/search.rs.
#[repr(C)]
pub struct SftHost {
    pub abi: u32,
    pub ctx: *mut c_void,
    pub seed: i64,
    pub biome_at: extern "C" fn(*mut c_void, i32, i32, i32) -> i32,
    pub biome_id: extern "C" fn(*mut c_void, *const c_char) -> i32,
    pub surface_height: extern "C" fn(*mut c_void, i32, i32) -> i32,
    pub count_structures: extern "C" fn(*mut c_void, *const c_char, i32, i32, i32) -> i32,
}

#[no_mangle]
pub extern "C" fn sft_abi_version() -> u32 {
    1
}

/// 1 = match, 0 = no match, negative = error (counts as no match).
///
/// # Safety
/// Called by the host with a valid `host` and NUL-terminated `name`/`params`.
#[no_mangle]
pub unsafe extern "C" fn sft_filter(host: *const SftHost, name: *const c_char, params: *const c_char) -> i32 {
    let h = &*host;
    let params = CStr::from_ptr(params).to_str().unwrap_or("{}");
    match CStr::from_ptr(name).to_str().unwrap_or("") {
        "spawn_island" => spawn_island(h, param(params, "radius").unwrap_or(96.0) as i32) as i32,
        "tall_spawn" => ((h.surface_height)(h.ctx, 0, 0) >= param(params, "min").unwrap_or(160.0) as i32) as i32,
        _ => -1,
    }
}

const OCEANS: [&CStr; 9] = [
    c"ocean", c"deep_ocean", c"warm_ocean", c"lukewarm_ocean", c"deep_lukewarm_ocean",
    c"cold_ocean", c"deep_cold_ocean", c"frozen_ocean", c"deep_frozen_ocean",
];

/// Land at the origin and ocean on all eight compass points `r` blocks out: a spawn island.
fn spawn_island(h: &SftHost, r: i32) -> bool {
    let ocean: Vec<i32> = OCEANS.iter().map(|n| (h.biome_id)(h.ctx, n.as_ptr())).collect();
    let is_ocean = |x: i32, z: i32| ocean.contains(&(h.biome_at)(h.ctx, x, 63, z));
    let d = r * 7 / 10;
    !is_ocean(0, 0) && [(r, 0), (-r, 0), (0, r), (0, -r), (d, d), (-d, d), (d, -d), (-d, -d)].iter().all(|&(x, z)| is_ocean(x, z))
}

/// `"key": number` lookup in the params JSON; keeps the example dependency-free.
fn param(json: &str, key: &str) -> Option<f64> {
    let rest = &json[json.find(&format!("\"{key}\""))? + key.len() + 2..];
    let rest = rest.trim_start().strip_prefix(':')?.trim_start();
    let end = rest.find(|c: char| !(c.is_ascii_digit() || "+-.eE".contains(c))).unwrap_or(rest.len());
    rest[..end].parse().ok()
}

#[cfg(test)]
mod tests {
    #[test]
    fn param_lookup() {
        assert_eq!(super::param(r#"{"radius": 96, "min":-3.5}"#, "radius"), Some(96.0));
        assert_eq!(super::param(r#"{"radius": 96, "min":-3.5}"#, "min"), Some(-3.5));
        assert_eq!(super::param("{}", "radius"), None);
    }
}
