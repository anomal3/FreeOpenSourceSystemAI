//! Host tests of the MP4 path link the real encoders when they are built
//! (`xtask/src/screenmedia.rs` puts the host library next to the SDK). The
//! cfg `screencast_media` turns those tests on; without the library they are
//! skipped and the rest of the crate does not care.

use std::path::PathBuf;

fn main() {
    println!("cargo:rustc-check-cfg=cfg(screencast_media)");
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    let library = manifest.join("../../build/toolchain/screenrec-media-host/libscreenrec-media.a");
    println!("cargo:rerun-if-changed={}", library.display());
    let host = std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "windows");
    if host && library.is_file() {
        println!("cargo:rustc-cfg=screencast_media");
        println!("cargo:rustc-link-arg={}", library.display());
    }
}
