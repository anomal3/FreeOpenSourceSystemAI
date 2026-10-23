//! Особые файлы программ Linux (фаза 71c): `/dev/null`, `/dev/zero`,
//! `/dev/urandom`, `/proc/self/*`, `/proc/cpuinfo`, `/proc/meminfo` и
//! `/sys/devices/system/cpu/*`.
//!
//! Это не файловая система, а ответы на несколько путей, которые glibc и
//! большие программы читают на каждом запуске: `sysconf(_SC_NPROCESSORS_ONLN)`
//! — `/sys/devices/system/cpu/online`, границы стека главного потока —
//! `/proc/self/maps`. Содержимое `/proc` собирается в момент открытия, как
//! снимок; устройства отвечают на каждое чтение сами.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::vfs::{DirEntry, Metadata, Node, NodeKind, VfsError, VfsResult};

/// Устройство из `/dev`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Device {
    Null,
    Zero,
    Random,
}

struct DeviceNode(Device);

impl Node for DeviceNode {
    fn metadata(&self) -> Metadata {
        Metadata { kind: NodeKind::File, size: 0, mode: 0o666, uid: 0, gid: 0, mtime: 0 }
    }
    fn read_at(&self, _offset: u64, buf: &mut [u8]) -> VfsResult<usize> {
        match self.0 {
            Device::Null => Ok(0),
            Device::Zero => {
                buf.fill(0);
                Ok(buf.len())
            }
            Device::Random => {
                let _ = crate::random::fill(buf);
                Ok(buf.len())
            }
        }
    }
    fn list(&self) -> VfsResult<Vec<DirEntry>> {
        Err(VfsError::WrongKind)
    }
    fn lookup(&self, _name: &str) -> VfsResult<Box<dyn Node>> {
        Err(VfsError::WrongKind)
    }
    /// Запись в любое из трёх принимается и забывается.
    fn write_at(&self, _offset: u64, data: &[u8]) -> VfsResult<usize> {
        Ok(data.len())
    }
}

/// Снимок, собранный при открытии.
struct TextNode(Vec<u8>);

impl Node for TextNode {
    fn metadata(&self) -> Metadata {
        // Размер — ноль, как у файлов `/proc` в Linux: читают их до конца, а не
        // по `st_size`.
        Metadata { kind: NodeKind::File, size: 0, mode: 0o444, uid: 0, gid: 0, mtime: 0 }
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> VfsResult<usize> {
        let start = (offset as usize).min(self.0.len());
        let len = buf.len().min(self.0.len() - start);
        buf[..len].copy_from_slice(&self.0[start..start + len]);
        Ok(len)
    }
    fn list(&self) -> VfsResult<Vec<DirEntry>> {
        Err(VfsError::WrongKind)
    }
    fn lookup(&self, _name: &str) -> VfsResult<Box<dyn Node>> {
        Err(VfsError::WrongKind)
    }
}

/// `/proc/<свой номер>/…` — то же, что `/proc/self/…`.
fn self_relative(path: &str) -> Option<&str> {
    if let Some(rest) = path.strip_prefix("/proc/self/") {
        return Some(rest);
    }
    if let Some(rest) = path.strip_prefix("/proc/thread-self/") {
        return Some(rest);
    }
    let rest = path.strip_prefix("/proc/")?;
    let (number, rest) = rest.split_once('/')?;
    let pid = crate::user::with_current(|program| program.linux_pid)?;
    (number.parse::<u32>().ok()? == pid).then_some(rest)
}

/// Узел для пути Linux `path`, если он особый. `None` — путь обычный.
pub fn node(path: &str) -> Option<(Arc<dyn Node>, bool)> {
    let device = match path {
        "/dev/null" => Some(Device::Null),
        "/dev/zero" => Some(Device::Zero),
        "/dev/urandom" | "/dev/random" => Some(Device::Random),
        _ => None,
    };
    if let Some(device) = device {
        return Some((Arc::new(DeviceNode(device)), true));
    }
    if let Some(names) = directory(path) {
        return Some((Arc::new(DirNode(names)), false));
    }
    let text = text(path)?;
    Some((Arc::new(TextNode(text.into_bytes())), false))
}

/// Каталог в памяти: имена — снимок в момент открытия (фаза 71h).
struct DirNode(Vec<(String, NodeKind)>);

impl Node for DirNode {
    fn metadata(&self) -> Metadata {
        Metadata { kind: NodeKind::Directory, size: 0, mode: 0o555, uid: 0, gid: 0, mtime: 0 }
    }
    fn read_at(&self, _offset: u64, _buf: &mut [u8]) -> VfsResult<usize> {
        Err(VfsError::WrongKind)
    }
    fn list(&self) -> VfsResult<Vec<DirEntry>> {
        Ok(self
            .0
            .iter()
            .map(|(name, kind)| DirEntry { name: name.clone(), kind: *kind, size: 0, mode: 0o555, uid: 0, gid: 0, mtime: 0 })
            .collect())
    }
    fn lookup(&self, _name: &str) -> VfsResult<Box<dyn Node>> {
        Err(VfsError::NotFound)
    }
}

/// Есть ли такой особый каталог (`stat`, фаза 71h).
pub fn is_directory(path: &str) -> bool {
    directory(path).is_some()
}

/// Каталоги `/proc` (фаза 71h): Chromium открывает `/proc` и
/// `/proc/self/task` дескриптором каталога, а `/proc/self/fd` перечисляет,
/// чтобы закрыть лишнее в ребёнке.
fn directory(path: &str) -> Option<Vec<(String, NodeKind)>> {
    let file = |name: &str| (String::from(name), NodeKind::File);
    let dir = |name: &str| (String::from(name), NodeKind::Directory);
    match path {
        "/proc" => Some(alloc::vec![dir("self"), file("cpuinfo"), file("meminfo"), file("version")]),
        "/proc/self" | "/proc/thread-self" => Some(alloc::vec![
            file("maps"),
            file("status"),
            file("stat"),
            file("cmdline"),
            file("comm"),
            file("exe"),
            dir("fd"),
            dir("task")
        ]),
        "/proc/self/fd" => {
            let fds = crate::user::with_current(|program| {
                (0..1024 + 3).filter(|fd| program.files.is_open(*fd)).collect::<Vec<usize>>()
            })?;
            Some(fds.into_iter().map(|fd| (format!("{fd}"), NodeKind::File)).collect())
        }
        "/proc/self/task" => {
            let tid = crate::sched::current().as_u32();
            Some(alloc::vec![(format!("{tid}"), NodeKind::Directory)])
        }
        _ => None,
    }
}

/// Есть ли такой особый путь — для `stat` и `access`.
pub fn exists(path: &str) -> bool {
    matches!(path, "/dev/null" | "/dev/zero" | "/dev/urandom" | "/dev/random") || text(path).is_some()
}

/// Содержимое файла `/proc` или `/sys`.
fn text(path: &str) -> Option<String> {
    // Настройки сети для резолвера glibc (фаза 71f).
    if let Some(bytes) = super::net::special_file(path) {
        return String::from_utf8(bytes).ok();
    }
    let cpus = crate::smp::online().max(1);
    if let Some(rest) = self_relative(path) {
        let (pid, name, maps, uid, gid) = crate::user::with_current(|program| {
            let shown = String::from(crate::linux_path::shown(&program.path));
            (program.linux_pid, shown, program.linux_maps(), program_cred(program).0, program_cred(program).1)
        })?;
        let short = name.rsplit('/').next().unwrap_or(&name).chars().take(15).collect::<String>();
        return match rest {
            "maps" => Some(maps),
            "status" => Some(format!(
                "Name:\t{short}\nState:\tR (running)\nTgid:\t{pid}\nPid:\t{pid}\nPPid:\t1\n\
                 Uid:\t{uid}\t{uid}\t{uid}\t{uid}\nGid:\t{gid}\t{gid}\t{gid}\t{gid}\n\
                 Threads:\t1\nVmRSS:\t0 kB\n"
            )),
            "stat" => {
                let mut line = format!("{pid} ({short}) R 1 {pid} {pid} 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 0 0 0");
                // Остальные поля до 52-го — нули: читающие берут нужные по
                // номеру.
                for _ in 24..=52 {
                    line.push_str(" 0");
                }
                line.push('\n');
                Some(line)
            }
            "cmdline" => Some(format!("{name}\0")),
            "comm" => Some(format!("{short}\n")),
            _ => None,
        };
    }
    match path {
        "/proc/cpuinfo" => {
            let mut out = String::new();
            for cpu in 0..cpus {
                out.push_str(&format!(
                    "processor\t: {cpu}\nvendor_id\t: FossAi\nmodel name\t: FossAi virtual CPU\n\
                     cpu MHz\t\t: 0.000\ncpu cores\t: {cpus}\nflags\t\t: {}\n\n",
                    cpu_flags()
                ));
            }
            Some(out)
        }
        "/proc/meminfo" => {
            let frames = crate::mm::frame::stats();
            let kib = |frames: usize| frames * crate::mm::PAGE_SIZE / 1024;
            Some(format!(
                "MemTotal:       {} kB\nMemFree:        {} kB\nMemAvailable:   {} kB\nBuffers:        0 kB\n\
                 Cached:         0 kB\nSwapTotal:      0 kB\nSwapFree:       0 kB\n",
                kib(frames.total),
                kib(frames.free),
                kib(frames.free)
            ))
        }
        "/proc/version" => Some(String::from("Linux version 6.1.0-fossai (FossAi) #1 SMP\n")),
        "/proc/sys/kernel/osrelease" => Some(String::from("6.1.0-fossai\n")),
        "/sys/devices/system/cpu/online" | "/sys/devices/system/cpu/possible" | "/sys/devices/system/cpu/present" => {
            Some(if cpus == 1 { String::from("0\n") } else { format!("0-{}\n", cpus - 1) })
        }
        _ => None,
    }
}

fn program_cred(program: &crate::user::Program) -> (u32, u32) {
    let cred = program.credentials();
    (cred.uid, cred.gid)
}

/// Флаги процессора для `/proc/cpuinfo` — по CPUID.
fn cpu_flags() -> String {
    let leaf1 = core::arch::x86_64::__cpuid(1);
    let leaf7 = core::arch::x86_64::__cpuid_count(7, 0);
    let mut flags = String::from("fpu tsc cx8 cmov mmx fxsr sse sse2");
    for (present, name) in [
        (leaf1.ecx & 1 != 0, "pni"),
        (leaf1.ecx & (1 << 9) != 0, "ssse3"),
        (leaf1.ecx & (1 << 12) != 0, "fma"),
        (leaf1.ecx & (1 << 19) != 0, "sse4_1"),
        (leaf1.ecx & (1 << 20) != 0, "sse4_2"),
        (leaf1.ecx & (1 << 23) != 0, "popcnt"),
        (leaf1.ecx & (1 << 25) != 0, "aes"),
        (leaf1.ecx & (1 << 28) != 0, "avx"),
        (leaf1.ecx & (1 << 29) != 0, "f16c"),
        (leaf7.ebx & (1 << 5) != 0, "avx2"),
        (leaf7.ebx & (1 << 8) != 0, "bmi2"),
    ] {
        if present {
            flags.push(' ');
            flags.push_str(name);
        }
    }
    flags
}
