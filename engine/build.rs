//! Compiles src/gpu.cu to PTX when nvcc is available (CUDA_PATH or PATH). Without it the engine is
//! built without GPU kernels and `gpu::status()` says so; nothing else changes.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=src/gpu.cu");
    println!("cargo:rerun-if-env-changed=CUDA_PATH");
    println!("cargo:rustc-check-cfg=cfg(sft_cuda)");
    let exe = if cfg!(windows) { "nvcc.exe" } else { "nvcc" };
    let nvcc = std::env::var_os("CUDA_PATH").map(|p| PathBuf::from(p).join("bin").join(exe)).filter(|p| p.exists()).unwrap_or_else(|| exe.into());
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("gpu.ptx");
    // compute_75 PTX is JIT-compiled by the driver for any newer GPU; no fused multiply-add (bit-exact floats)
    let ok = Command::new(&nvcc)
        .args(["-ptx", "-arch=compute_75", "-O3", "--fmad=false", "src/gpu.cu", "-o"])
        .arg(&out)
        .status()
        .is_ok_and(|s| s.success());
    if ok {
        println!("cargo:rustc-cfg=sft_cuda");
    } else {
        println!("cargo:warning=nvcc not found or failed: building without CUDA kernels");
    }
}
