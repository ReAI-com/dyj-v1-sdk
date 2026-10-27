//! 编译 vendored opencore-amrwb 解码器（Apache-2.0，见 vendor/opencore-amr/LICENSE）。
//! 源取自 BelledonneCommunications/opencore-amr 镜像（amrwb/wrapper.cpp + dec 树），
//! 仅保留 AMR-WB 解码所需文件。

use std::fs;
use std::path::PathBuf;

fn main() {
    let vendor = PathBuf::from("vendor/opencore-amr");
    println!("cargo:rerun-if-changed={}", vendor.join("amrwb").display());
    println!("cargo:rerun-if-changed={}", vendor.join("dec").display());

    let mut build = cc::Build::new();
    build
        .cpp(true)
        .include(vendor.join("oscl"))
        .include(vendor.join("dec/src"))
        .include(vendor.join("dec/include"))
        .include(vendor.join("common/dec/include"))
        .include(vendor.join("amrwb"))
        .file(vendor.join("amrwb/wrapper.cpp"));

    // Makefile.am 的清单：不含 decoder_amr_wb.cpp（独立入口，未使用）
    let src = vendor.join("dec/src");
    let mut files: Vec<PathBuf> = fs::read_dir(&src)
        .expect("vendor dec/src missing")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "cpp"))
        .filter(|p| p.file_name().is_none_or(|n| n != "decoder_amr_wb.cpp"))
        .collect();
    files.sort();
    for f in files {
        build.file(f);
    }

    build.compile("opencore_amrwb_dec");
}
