//! `/bin/screenrec` links H.264 and Opus (23.11) when the library is built:
//! `xtask/src/screenmedia.rs` puts `libscreenrec-media.a` into the SDK
//! sysroot of the architecture before the programs are built. Without it the
//! recorder builds without them (cfg `screenrec_media` off) and writes AVI.

use std::path::PathBuf;

fn main() {
    println!("cargo:rustc-check-cfg=cfg(screenrec_media)");
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if os != "none" {
        return;
    }
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    let library = manifest.join(format!("../../build/toolchain/sysroot/{arch}/lib/libscreenrec-media.a"));
    println!("cargo:rerun-if-changed={}", library.display());
    if library.is_file() {
        println!("cargo:rustc-cfg=screenrec_media");
        println!("cargo:rustc-link-arg-bin=screenrec={}", library.display());
    }
}
