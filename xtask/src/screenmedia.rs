//! H.264 and Opus for the screen recorder (23.11): openh264 and libopus
//! built into one static library, `libscreenrec-media.a`, which `/bin/screenrec`
//! (a `no_std` Rust program) links directly.
//!
//! # Why not `thirdparty`
//!
//! The recorder is not a C program: it has no picolibc, no `crt0`, no TLS of
//! the C library. The library is compiled with the SDK wrappers (headers,
//! flags, PIE), but linked into the Rust program, which supplies the few C
//! functions both libraries need itself (`screenrec/crt.rs`: `malloc`,
//! `pthread_*`, `sem_*`, `log`…). So no `configure`/CMake of the projects —
//! their own source lists (`codec/*/targets.mk` of openh264, `*_sources.mk`
//! of libopus) go into a `build.ninja` written here, plus the glue of
//! `ports/screenrec` (`h264.cpp`, `opus.c`).
//!
//! Build flags: openh264 without assembly (NASM is not part of the toolchain;
//! the C path is fast enough — see ROADMAP), `__Fuchsia__` picks its POSIX
//! branch that needs only `sysconf` for the CPU count; libopus fixed-point
//! (`FIXED_POINT`, 16-bit PCM in, almost no libm), `VAR_ARRAYS` (scratch on
//! the encoder thread's stack, which the recorder makes large).
//!
//! The same sources are built for the host too (Windows, clang): a benchmark
//! (`bench.exe`, `ports/screenrec/bench.cpp`) and the library the host tests
//! of `crates/screencast` link to write real MP4 files for ffprobe.
//!
//! Called by the image build before the user programs; a failure (no
//! network for the first download, no LLVM) is a line in the log — the
//! recorder then builds without H.264 and writes AVI only.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::arch::Arch;
use crate::{cbuild, paths, sdk};

/// A source archive: name on disk, address, SHA-256 (taken by us from the
/// downloaded file 23.11.2026; opus' matches the one published on
/// xiph.org), the directory inside.
struct Archive {
    file: &'static str,
    url: &'static str,
    sha256: &'static str,
    unpacked: &'static str,
}

/// openh264 2.6.0 (BSD-2-Clause).
const OPENH264: Archive = Archive {
    file: "openh264-2.6.0.tar.gz",
    url: "https://github.com/cisco/openh264/archive/refs/tags/v2.6.0.tar.gz",
    sha256: "558544ad358283a7ab2930d69a9ceddf913f4a51ee9bf1bfb9e377322af81a69",
    unpacked: "openh264-2.6.0",
};

/// libopus 1.5.2 (BSD-3-Clause).
const OPUS: Archive = Archive {
    file: "opus-1.5.2.tar.gz",
    url: "https://downloads.xiph.org/releases/opus/opus-1.5.2.tar.gz",
    sha256: "65c1d2f78b9f2fb20082c38cbe47c951ad5839345876e46941612ee87f9a7ce1",
    unpacked: "opus-1.5.2",
};

/// The library's file name.
pub const LIBRARY: &str = "libscreenrec-media.a";

/// Where the host build goes (library and `bench.exe`).
pub fn host_dir() -> PathBuf {
    sdk::root().join("screenrec-media-host")
}

fn thirdparty_dir() -> PathBuf {
    sdk::root().join("thirdparty")
}

fn ports_dir() -> PathBuf {
    paths::workspace_root().join("ports/screenrec")
}

/// Download (once), check and unpack an archive; the unpacked directory.
fn fetch(archive: &Archive) -> Result<PathBuf> {
    let root = thirdparty_dir();
    fs::create_dir_all(&root)?;
    let source = root.join(archive.unpacked);
    if source.is_dir() {
        return Ok(source);
    }
    let file = root.join(archive.file);
    if !file.is_file() {
        say!("> curl {}", archive.url);
        let status = Command::new("curl")
            .args(["--location", "--fail", "--silent", "--show-error", "--output"])
            .arg(&file)
            .arg(archive.url)
            .status()
            .context("curl did not start")?;
        if !status.success() {
            bail!("download failed: {} ({status})", archive.url);
        }
    }
    let bytes = fs::read(&file)?;
    let mut hasher = fpk::Hasher::new();
    hasher.update(&bytes);
    let got: String = hasher.finish().iter().map(|b| format!("{b:02x}")).collect();
    if got != archive.sha256 {
        bail!("{} has SHA-256 {got}, expected {} (file kept)", file.display(), archive.sha256);
    }
    let status = Command::new("tar").current_dir(&root).arg("-xf").arg(archive.file).status().context("tar did not start")?;
    if !status.success() || !source.is_dir() {
        bail!("{} did not unpack into {}", file.display(), source.display());
    }
    Ok(source)
}

/// Sources of a `targets.mk` of openh264: `$(X_SRCDIR)/path.cpp` lines.
fn openh264_sources(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for (mk, dir) in [("codec/common/targets.mk", "codec/common"), ("codec/encoder/targets.mk", "codec/encoder"), ("codec/processing/targets.mk", "codec/processing")] {
        let text = fs::read_to_string(root.join(mk)).with_context(|| format!("no {mk}"))?;
        let prefix = format!("$({}_SRCDIR)/", mk.split('/').nth(1).unwrap_or_default().to_uppercase());
        let mut on = false;
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.ends_with("_CPP_SRCS=\\") {
                on = true;
                continue;
            }
            if !on {
                continue;
            }
            let Some(rest) = trimmed.strip_prefix(&prefix) else {
                on = false;
                continue;
            };
            let path = rest.trim_end_matches('\\');
            if path.ends_with(".cpp") {
                out.push(root.join(dir).join(path));
            }
        }
    }
    if out.len() < 40 {
        bail!("openh264: only {} sources found in targets.mk", out.len());
    }
    Ok(out)
}

/// Sources of libopus: named lists of its `*_sources.mk`.
fn opus_sources(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for (mk, list) in [("celt_sources.mk", "CELT_SOURCES"), ("silk_sources.mk", "SILK_SOURCES"), ("silk_sources.mk", "SILK_SOURCES_FIXED"), ("opus_sources.mk", "OPUS_SOURCES")] {
        let text = fs::read_to_string(root.join(mk)).with_context(|| format!("no {mk}"))?;
        let mut on = false;
        for line in text.lines() {
            if let Some((name, _)) = line.split_once(" =") {
                on = name.trim() == list;
                continue;
            }
            if on {
                let path = line.trim().trim_end_matches('\\').trim();
                if path.ends_with(".c") {
                    out.push(root.join(path));
                }
                if !line.trim_end().ends_with('\\') {
                    on = false;
                }
            }
        }
    }
    if out.len() < 80 {
        bail!("libopus: only {} sources found in *_sources.mk", out.len());
    }
    Ok(out)
}

/// A path for `build.ninja` (`$:` for the drive colon, forward slashes).
fn ninja_path(path: &Path) -> String {
    path.display().to_string().replace('\\', "/").replace(':', "$:").replace(' ', "$ ")
}

fn plain(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

struct Tools {
    cc: String,
    cxx: String,
    ar: String,
    /// Flags for everything (on top of what the wrapper adds).
    common: String,
    /// For the recorder (not the host): add `crt_stubs.c`.
    guest: bool,
}

fn find_ninja() -> Result<PathBuf> {
    let own = sdk::bin_dir().join(if cfg!(windows) { "ninja.exe" } else { "ninja" });
    if own.is_file() {
        return Ok(own);
    }
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let path = PathBuf::from(local).join("Microsoft/WinGet/Links/ninja.exe");
        if path.is_file() {
            return Ok(path);
        }
    }
    Ok(PathBuf::from("ninja"))
}

/// Write `build.ninja` into `out` and run ninja there.
fn build(out: &Path, tools: &Tools, with_bench: bool) -> Result<PathBuf> {
    let h264 = fetch(&OPENH264)?;
    let opus = fetch(&OPUS)?;
    fs::create_dir_all(out)?;
    let h264_inc: Vec<String> = [
        "codec/api/wels",
        "codec/common/inc",
        "codec/encoder/core/inc",
        "codec/encoder/plus/inc",
        "codec/processing/interface",
        "codec/processing/src/common",
        "codec/processing/src/adaptivequantization",
        "codec/processing/src/downsample",
        "codec/processing/src/scrolldetection",
        "codec/processing/src/vaacalc",
    ]
    .iter()
    .map(|dir| format!("\"-I{}\"", plain(&h264.join(dir))))
    .collect();
    let opus_inc: Vec<String> = ["include", "celt", "silk", "silk/fixed", "src"].iter().map(|dir| format!("\"-I{}\"", plain(&opus.join(dir)))).collect();
    let cxxflags = format!("{} -fno-exceptions -fno-rtti -D__Fuchsia__ {}", tools.common, h264_inc.join(" "));
    let cflags = format!("{} -DOPUS_BUILD -DFIXED_POINT=1 -DDISABLE_FLOAT_API -DVAR_ARRAYS {}", tools.common, opus_inc.join(" "));
    let mut ninja = String::from("# Written by `xtask` (screenmedia.rs); edits are lost.\n");
    ninja += &format!("rule cxx\n  command = {} {cxxflags} -c $in -o $out\n  description = c++ $in\n", tools.cxx);
    ninja += &format!("rule cc\n  command = {} {cflags} -c $in -o $out\n  description = cc $in\n", tools.cc);
    let ar = if cfg!(windows) { format!("cmd /c del /q $out 2>nul & {} rcs $out $in", tools.ar) } else { format!("rm -f $out && {} rcs $out $in", tools.ar) };
    ninja += &format!("rule ar\n  command = {ar}\n  description = ar $out\n");
    ninja += &format!("rule link\n  command = {} -O2 $in -o $out\n", tools.cxx);
    let mut objects = Vec::new();
    let mut add = |rule: &str, source: &Path, tag: &str, ninja: &mut String| {
        let parts: Vec<String> = source.components().rev().take(3).map(|c| c.as_os_str().to_string_lossy().to_string()).collect();
        let name = format!("{tag}_{}_{}_{}.o", parts[2], parts[1], parts[0].trim_end_matches(".cpp").trim_end_matches(".c"));
        *ninja += &format!("build {name}: {rule} {}\n", ninja_path(source));
        objects.push(name);
    };
    for source in openh264_sources(&h264)? {
        add("cxx", &source, "h264", &mut ninja);
    }
    add("cxx", &ports_dir().join("h264.cpp"), "glue", &mut ninja);
    for source in opus_sources(&opus)? {
        add("cc", &source, "opus", &mut ninja);
    }
    add("cc", &ports_dir().join("opus.c"), "glue", &mut ninja);
    if tools.guest {
        add("cc", &ports_dir().join("crt_stubs.c"), "glue", &mut ninja);
    }
    ninja += &format!("build {LIBRARY}: ar {}\n", objects.join(" "));
    if with_bench {
        ninja += &format!("build bench.o: cxx {}\n", ninja_path(&ports_dir().join("bench.cpp")));
        ninja += &format!("build bench.exe: link bench.o {LIBRARY}\n");
    }
    fs::write(out.join("build.ninja"), ninja)?;
    let ninja_exe = find_ninja()?;
    let output = Command::new(&ninja_exe).arg("-C").arg(out).output().with_context(|| format!("{} did not start", ninja_exe.display()))?;
    if !output.status.success() {
        let text = String::from_utf8_lossy(&output.stdout);
        let tail: Vec<&str> = text.lines().rev().take(30).collect();
        bail!("ninja failed in {}:\n{}", out.display(), tail.into_iter().rev().collect::<Vec<_>>().join("\n"));
    }
    Ok(out.join(LIBRARY))
}

/// Build (or bring up to date) the library for `arch` and put it into the
/// sysroot (`lib/libscreenrec-media.a`) — where `user-progs/build.rs` looks.
pub fn ensure(arch: Arch) -> Result<PathBuf> {
    let sysroot = cbuild::sysroot(arch);
    if !sysroot.join("include/stdio.h").is_file() {
        bail!("no SDK for {} (cargo xtask sdk)", arch.name());
    }
    let quote = |path: PathBuf| format!("\"{}\"", plain(&path));
    let tools = Tools {
        cc: quote(sdk::tool_path(arch, "cc")),
        cxx: quote(sdk::tool_path(arch, "c++")),
        ar: quote(sdk::tool_path(arch, "ar")),
        common: String::from("-O3 -DNDEBUG -fno-strict-aliasing -w"),
        guest: true,
    };
    let out = thirdparty_dir().join(format!("screenrec-media-{}", arch.name()));
    let library = build(&out, &tools, false)?;
    let target = sysroot.join("lib").join(LIBRARY);
    let fresh = fs::metadata(&target).and_then(|t| Ok((t.modified()?, fs::metadata(&library)?.modified()?))).is_ok_and(|(t, l)| t >= l);
    if !fresh {
        fs::copy(&library, &target).with_context(|| format!("cannot copy into {}", target.display()))?;
    }
    // The host build is a convenience (benchmark, host tests): its failure is
    // only a line.
    if cfg!(windows) {
        if let Err(err) = ensure_host() {
            say!("screenrec-media for the host skipped: {err:#}");
        }
    }
    Ok(target)
}

/// The host library and `bench.exe` (Windows, clang of LLVM).
pub fn ensure_host() -> Result<PathBuf> {
    let cxx = cbuild::llvm_tool("clang++")?;
    let cc = cbuild::llvm_tool("clang")?;
    let ar = cbuild::llvm_tool("llvm-ar")?;
    let quote = |path: PathBuf| format!("\"{}\"", plain(&path));
    let tools = Tools { cc: quote(cc), cxx: quote(cxx), ar: quote(ar), common: String::from("-O3 -DNDEBUG -D_CRT_SECURE_NO_WARNINGS -w"), guest: false };
    build(&host_dir(), &tools, true)
}

#[cfg(test)]
mod tests {
    /// `cargo test -p xtask screenmedia -- --ignored`: build the library for
    /// x86-64 and the host without the whole image.
    #[test]
    #[ignore = "downloads and builds openh264 and libopus"]
    fn build_screenmedia() {
        let library = super::ensure(crate::arch::Arch::X86_64).expect("x86_64 build");
        assert!(library.is_file());
        if crate::cbuild::sysroot(crate::arch::Arch::Aarch64).join("include/stdio.h").is_file() {
            assert!(super::ensure(crate::arch::Arch::Aarch64).expect("aarch64 build").is_file());
        }
        if cfg!(windows) {
            assert!(super::ensure_host().expect("host build").is_file());
        }
    }
}
