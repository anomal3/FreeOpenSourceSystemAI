//! vDSO для программ Linux (01.11.2026): время без входа в ядро.
//!
//! Rider при запуске спрашивает часы 1,7 млн раз (Java и .NET), и системный
//! вызов на каждый — около 2 мкс, секунды процессора на запуск. Linux отвечает
//! из памяти программы: рядом с кодом vDSO лежит страница данных времени, и
//! код считает время по `rdtsc` сам. Здесь то же самое:
//!
//! - **код** — `libc/linux/vdso.c`, собран clang под Linux (`xtask`), лежит в
//!   корне как [`IMAGE_PATH`] и читается при первом запуске программы Linux;
//! - **данные** — страница перед кодом ([`Vvar`]): основание счётчика, время
//!   работы в этот момент, множитель и сдвиг, время суток при загрузке; пишет
//!   их ядро ([`refresh`]) при каждой смене часов (калибровка, уточнение
//!   частоты, время суток от прошивки) под счётчиком-замком `seq`;
//! - обе части — одна разделяемая память ([`SharedMem`]): кадры общие у всех
//!   программ, отображение — только на чтение (код — ещё и на исполнение);
//!   адрес кода — в `AT_SYSINFO_EHDR`, по нему glibc находит функции.
//!
//! Только x86-64: счётчик там `rdtsc`, и время ядра идёт по нему же. Файла
//! нет или он не тот — программы живут как раньше, системными вызовами.

use alloc::sync::Arc;

use crate::user::shm::SharedMem;

/// Где лежит код vDSO.
pub const IMAGE_PATH: &str = "/bin/vdso";

/// Больше этого кода не бывает (сейчас — меньше двух килобайт).
const MAX_IMAGE: usize = 4 * 4096;

/// Страница данных времени — ровно та раскладка, что у `struct vvar` в
/// `libc/linux/vdso.c`.
#[repr(C)]
struct Vvar {
    seq: u64,
    valid: u64,
    tsc_base: u64,
    mono_base_ns: u64,
    mult: u64,
    shift: u64,
    real_base_ns: i64,
    real_valid: u64,
}

/// Готовый vDSO: память (страница данных и код за ней) и сколько в ней
/// страниц кода.
#[derive(Clone)]
pub struct Image {
    pub mem: Arc<SharedMem>,
    pub code_pages: usize,
}

/// Состояние: ещё не пробовали, нет, есть.
enum State {
    Untried,
    Missing,
    Ready(Image),
}

static STATE: crate::sync::SpinLock<State> = crate::sync::SpinLock::new(State::Untried);

/// vDSO, если он есть: при первом вызове читается с диска.
#[must_use]
pub fn image() -> Option<Image> {
    if !cfg!(target_arch = "x86_64") {
        return None;
    }
    match &*STATE.lock() {
        State::Ready(image) => return Some(image.clone()),
        State::Missing => return None,
        State::Untried => {}
    }
    // Читается без замка: чтение файла засыпает. Двое прочитавших разом —
    // второй просто возьмёт готовое.
    let loaded = load();
    let mut state = STATE.lock();
    if let State::Ready(image) = &*state {
        return Some(image.clone());
    }
    match loaded {
        Some(image) => {
            *state = State::Ready(image.clone());
            drop(state);
            refresh();
            crate::kprintln!("  linux       : vDSO {} B from {IMAGE_PATH}: time without a system call", image.code_pages * 4096);
            Some(image)
        }
        None => {
            *state = State::Missing;
            crate::kprintln!("  linux       : no vDSO at {IMAGE_PATH}, time goes through system calls");
            None
        }
    }
}

/// Прочитать и проверить код, сложить его за страницей данных.
fn load() -> Option<Image> {
    let node = crate::fs::resolve_as(crate::vfs::perm::Credentials::ROOT, IMAGE_PATH, crate::vfs::perm::Access::NONE)?.ok()?;
    let size = node.metadata().size as usize;
    if size < 64 || size > MAX_IMAGE {
        return None;
    }
    let mut bytes = alloc::vec![0u8; size];
    if node.read_at(0, &mut bytes).ok()? != size {
        return None;
    }
    // ELF64, x86-64, разделяемый объект — иначе glibc на нём споткнётся
    // хуже, чем без vDSO вовсе.
    if &bytes[..4] != b"\x7fELF" || bytes[4] != 2 || u16::from_le_bytes([bytes[16], bytes[17]]) != 3 || u16::from_le_bytes([bytes[18], bytes[19]]) != 0x3e {
        return None;
    }
    let code_pages = size.div_ceil(4096);
    let mem = SharedMem::new(((1 + code_pages) * 4096) as u64);
    for page in 0..=code_pages {
        let frame = mem.frame(page)?;
        // SAFETY: кадр только что выдан этой памяти и виден через прямое
        // отображение; никто его ещё не отображал.
        let out = unsafe { core::slice::from_raw_parts_mut(frame.to_direct_map().as_usize() as *mut u8, 4096) };
        out.fill(0);
        if page > 0 {
            let from = (page - 1) * 4096;
            let take = (size - from).min(4096);
            out[..take].copy_from_slice(&bytes[from..from + take]);
        }
    }
    Some(Image { mem, code_pages })
}

/// Переписать страницу данных по нынешним часам ядра. До первого запуска
/// программы Linux vDSO нет — тогда ничего.
pub fn refresh() {
    let image = match &*STATE.lock() {
        State::Ready(image) => image.clone(),
        _ => return,
    };
    let Some(frame) = image.mem.frame(0) else { return };
    let vvar = frame.to_direct_map().as_usize() as *mut Vvar;
    let clock = crate::time::vdso_clock();
    // SAFETY: кадр страницы данных принадлежит памяти vDSO и жив, пока жива
    // она; программы видят его только на чтение. Запись — под `seq`: нечётный
    // — идёт запись, и читатель в vDSO подождёт.
    unsafe {
        let seq = core::ptr::addr_of_mut!((*vvar).seq);
        let next = seq.read_volatile().wrapping_add(1) | 1;
        seq.write_volatile(next);
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        match clock {
            Some((base, base_ns, hz, real)) => {
                // ns = основание + (такты · mult) >> shift; сдвиг — чтобы
                // множитель влез в 64 бита при любой частоте от мегагерца.
                let mut shift = 62u32;
                while shift > 16 && (1_000_000_000u128 << shift) / u128::from(hz) >= 1u128 << 63 {
                    shift -= 1;
                }
                let mult = ((1_000_000_000u128 << shift) / u128::from(hz)) as u64;
                core::ptr::addr_of_mut!((*vvar).tsc_base).write_volatile(base);
                core::ptr::addr_of_mut!((*vvar).mono_base_ns).write_volatile(base_ns);
                core::ptr::addr_of_mut!((*vvar).mult).write_volatile(mult);
                core::ptr::addr_of_mut!((*vvar).shift).write_volatile(u64::from(shift));
                core::ptr::addr_of_mut!((*vvar).real_base_ns).write_volatile(real.unwrap_or(0));
                core::ptr::addr_of_mut!((*vvar).real_valid).write_volatile(u64::from(real.is_some()));
                core::ptr::addr_of_mut!((*vvar).valid).write_volatile(1);
            }
            None => core::ptr::addr_of_mut!((*vvar).valid).write_volatile(0),
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        seq.write_volatile(next.wrapping_add(1));
    }
}
