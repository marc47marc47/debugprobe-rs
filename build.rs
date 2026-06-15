//! 依晶片 feature 把對應的 memory.x 複製到 OUT_DIR，供 cortex-m-rt 的 link.x 引用。
//! 對應 C 版由 Pico SDK / CMake 決定的記憶體配置。
use std::env;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

fn main() {
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());

    let memory_x: &[u8] = if env::var_os("CARGO_FEATURE_RP2350").is_some() {
        include_bytes!("memory_rp2350.x")
    } else {
        include_bytes!("memory_rp2040.x")
    };

    File::create(out.join("memory.x"))
        .unwrap()
        .write_all(memory_x)
        .unwrap();
    println!("cargo:rustc-link-search={}", out.display());

    println!("cargo:rerun-if-changed=memory_rp2040.x");
    println!("cargo:rerun-if-changed=memory_rp2350.x");
    println!("cargo:rerun-if-changed=build.rs");
}
