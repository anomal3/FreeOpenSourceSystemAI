//! `memcpy` и `memset` ядра — способом, выбранным замером (19.11.2026).
//!
//! # Зачем
//!
//! perf-diag (18.11) мерил копию буфера клиента Wayland в окно стола:
//! 8–12 мс на 0,9 Мточки, около 450 МБ/с. Кеши, отображение и замки тут ни
//! при чём: прямое отображение — обычная память с обратной записью, из
//! памяти с объединением записи (экран) никто не читает, копия идёт строкой
//! и не под замком с закрытыми прерываниями. Медленна сама `memcpy`: ядро
//! собрано без SSE, и `compiler_builtins` копирует `rep movsq` (без `ermsb`
//! в целевых свойствах), а заполняет `rep stosq`. На железе это быстро, а в
//! QEMU без ускорения (TCG — так идут все замеры стенда и флешки) каждый шаг
//! `rep` — отдельный круг транслированного кода: те самые сотни мегабайт в
//! секунду. И это не только Wayland: сборка стола, перенос на экран, нули
//! в каждой новой странице программы — всё через те же две функции.
//!
//! # Что здесь
//!
//! `memcpy` и `memset` самого ядра: у `compiler_builtins` они объявлены
//! слабыми именно затем, чтобы их можно было заменить. Способов четыре:
//! `rep movsq`/`stosq` (как у `compiler_builtins`), `rep movsb`/`stosb`,
//! развёрнутый цикл по восемь слов и цикл SSE по 64 байта. Какой быстрее —
//! зависит от машины (на железе с ERMSB — `rep`, в TCG — векторы), поэтому
//! он не угадывается, а **выбирается замером** при загрузке ([`init`],
//! строка `copy: …` в журнале), отдельно для копии и заполнения; команда
//! `copybench` повторяет замер и задаёт способ руками. До замера и для
//! кусков короче [`SMALL`] — `rep movsq`, как было.
//!
//! # Векторы в ядре без векторов
//!
//! Компилятор ядра SSE не трогает, а у задач ядра нет своей области под
//! векторное состояние (`sched` сохраняет его только у задач программ). Цикл
//! SSE поэтому сам сохраняет и возвращает `xmm0`–`xmm3` и идёт с закрытыми
//! прерываниями кусками по [`SSE_CHUNK`]: вытеснить его посреди куска нельзя,
//! прерывание посреди куска его не застанет, а векторы программы, которой
//! принадлежали регистры, остаются нетронутыми. Инструкции — без VEX,
//! верхние половины `ymm` они не трогают. Процессор, на котором SSE ещё не
//! включён (`CR4.OSFXSR`), идёт словами.

#[cfg(target_arch = "x86_64")]
use core::ffi::c_void;
use core::sync::atomic::{AtomicU8, Ordering};

/// Короче этого — `rep movsq`/`stosq`: выбор не окупится.
const SMALL: usize = 256;

/// Кусок цикла SSE под одним закрытием прерываний: в TCG ~50 мкс.
const SSE_CHUNK: usize = 64 * 1024;

/// Способ копии или заполнения.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Method {
    /// `rep movsq` / `rep stosq` — как `compiler_builtins`.
    RepQ = 1,
    /// `rep movsb` / `rep stosb`.
    RepB = 2,
    /// Восемь 64-битных слов за шаг обычными `mov`.
    Words = 3,
    /// Четыре `movdqu` за шаг.
    Sse = 4,
}

const METHODS: [Method; 4] = [Method::RepQ, Method::RepB, Method::Words, Method::Sse];

impl Method {
    fn from_u8(value: u8) -> Option<Self> {
        METHODS.iter().copied().find(|method| *method as u8 == value)
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::RepQ => "rep-q",
            Self::RepB => "rep-b",
            Self::Words => "words",
            Self::Sse => "sse",
        }
    }
}

/// Способ по имени для оболочки (`copybench sse`); `auto` — `None`.
#[must_use]
pub fn parse(name: &str) -> Option<Method> {
    METHODS.iter().copied().find(|method| method.name() == name)
}

/// Выбранные способы копии и заполнения; 0 — ещё не мерили (`rep`).
static COPY: AtomicU8 = AtomicU8::new(0);
static FILL: AtomicU8 = AtomicU8::new(0);

/// Замерить и выбрать (загрузка, после запуска всех процессоров).
pub fn init() {
    let (copy, fill) = calibrate(false);
    COPY.store(copy as u8, Ordering::Relaxed);
    FILL.store(fill as u8, Ordering::Relaxed);
}

/// Задать способ руками (`copybench <способ>`) или снова замером (`None`).
pub fn force(method: Option<Method>) {
    match method {
        Some(method) => {
            COPY.store(method as u8, Ordering::Relaxed);
            FILL.store(method as u8, Ordering::Relaxed);
            crate::kprintln!("  copy        : using {} for large copies and fills (set by hand)", method.name());
        }
        None => init(),
    }
}

/// Все способы по порядку — для замеров вне этого модуля.
#[must_use]
pub fn methods() -> [Method; 4] {
    METHODS
}

/// Выполнить `f` с копией способом `method` и вернуть прежний (замеры
/// `copybench`; на это время копирует так вся система).
pub fn with_copy<R>(method: Method, f: impl FnOnce() -> R) -> R {
    let was = COPY.swap(method as u8, Ordering::Relaxed);
    let result = f();
    COPY.store(was, Ordering::Relaxed);
    result
}

/// Способы сейчас: `(копия, заполнение)`.
#[must_use]
pub fn current() -> (Option<Method>, Option<Method>) {
    (Method::from_u8(COPY.load(Ordering::Relaxed)), Method::from_u8(FILL.load(Ordering::Relaxed)))
}

/// Скопировать `len` байт (то же, что `copy_nonoverlapping`, — та и зовёт
/// [`memcpy`]; здесь — чтобы горячие места было видно по имени).
///
/// # Safety
///
/// Как у `core::ptr::copy_nonoverlapping`.
#[inline]
pub unsafe fn copy(dst: *mut u8, src: *const u8, len: usize) {
    // SAFETY: условие делегировано вызывающему.
    unsafe { core::ptr::copy_nonoverlapping(src, dst, len) };
}

/// Замерить все способы на кусках по 256 КиБ (лучшее из трёх): копию и
/// заполнение. `verbose` — каждый способ в журнал. Ответ — лучшие.
pub fn calibrate(verbose: bool) -> (Method, Method) {
    const LEN: usize = 256 * 1024;
    let mut src = alloc::vec::Vec::new();
    let mut dst = alloc::vec::Vec::new();
    if !cfg!(target_arch = "x86_64") || src.try_reserve_exact(LEN).is_err() || dst.try_reserve_exact(LEN).is_err() {
        return (Method::RepQ, Method::RepQ);
    }
    src.resize(LEN, 0u8);
    dst.resize(LEN, 0u8);
    for (index, byte) in src.iter_mut().enumerate() {
        *byte = (index * 7 + index / 251) as u8;
    }
    let mut best_copy = (Method::RepQ, u64::MAX);
    let mut best_fill = (Method::RepQ, u64::MAX);
    let mut line = alloc::string::String::new();
    let mb_s = |ns: u64| LEN as u64 * 1000 / ns.max(1);
    for method in METHODS {
        let mut copy_ns = u64::MAX;
        let mut fill_ns = u64::MAX;
        for round in 0..3u8 {
            let started = crate::time::uptime_ns();
            // SAFETY: два разных буфера по `LEN` байт; смещение на байт —
            // невыровненные края тоже проверяются.
            unsafe { copy_with(method, dst.as_mut_ptr().add(1), src.as_ptr().add(1), LEN - 2) };
            copy_ns = copy_ns.min(crate::time::uptime_ns().saturating_sub(started));
            if dst[1..LEN - 1] != src[1..LEN - 1] {
                crate::kprintln!("  copy        : WARNING: {} copied wrong bytes, not used", method.name());
                copy_ns = u64::MAX;
                break;
            }
            let started = crate::time::uptime_ns();
            // SAFETY: свой буфер, `LEN - 2` байт со смещением на байт.
            unsafe { fill_with(method, dst.as_mut_ptr().add(1), 0x40 + round, LEN - 2) };
            fill_ns = fill_ns.min(crate::time::uptime_ns().saturating_sub(started));
            if dst[1..LEN - 1].iter().any(|byte| *byte != 0x40 + round) {
                crate::kprintln!("  copy        : WARNING: {} filled wrong bytes, not used", method.name());
                fill_ns = u64::MAX;
                break;
            }
        }
        if copy_ns != u64::MAX {
            let _ = core::fmt::Write::write_fmt(
                &mut line,
                format_args!(" {} {}/{}", method.name(), mb_s(copy_ns), if fill_ns == u64::MAX { 0 } else { mb_s(fill_ns) }),
            );
            if copy_ns < best_copy.1 {
                best_copy = (method, copy_ns);
            }
        }
        if fill_ns < best_fill.1 {
            best_fill = (method, fill_ns);
        }
    }
    if verbose {
        crate::kprintln!("  copybench   : MB/s copy/fill:{line}");
    }
    crate::kprintln!(
        "  copy        : memcpy {}, memset {} (MB/s copy/fill:{line})",
        best_copy.0.name(),
        best_fill.0.name()
    );
    (best_copy.0, best_fill.0)
}

/// Копия заданным способом.
///
/// # Safety
///
/// См. [`copy`].
unsafe fn copy_with(method: Method, dst: *mut u8, src: *const u8, len: usize) {
    #[cfg(target_arch = "x86_64")]
    // SAFETY: условие делегировано вызывающему.
    unsafe {
        match method {
            Method::RepQ => x86::rep_q(dst, src, len),
            Method::RepB => x86::rep_b(dst, src, len),
            Method::Words => x86::words(dst, src, len),
            Method::Sse => x86::sse(dst, src, len),
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    // SAFETY: условие делегировано вызывающему.
    unsafe {
        let _ = method;
        core::ptr::copy_nonoverlapping(src, dst, len);
    }
}

/// Заполнение заданным способом.
///
/// # Safety
///
/// `dst` доступен на запись на `len` байт.
unsafe fn fill_with(method: Method, dst: *mut u8, byte: u8, len: usize) {
    #[cfg(target_arch = "x86_64")]
    // SAFETY: условие делегировано вызывающему.
    unsafe {
        match method {
            Method::RepQ => x86::fill_rep_q(dst, byte, len),
            Method::RepB => x86::fill_rep_b(dst, byte, len),
            Method::Words => x86::fill_words(dst, byte, len),
            Method::Sse => x86::fill_sse(dst, byte, len),
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    // SAFETY: условие делегировано вызывающему.
    unsafe {
        let _ = method;
        core::ptr::write_bytes(dst, byte, len);
    }
}

/// `memcpy` ядра (вместо слабой из `compiler_builtins`).
///
/// # Safety
///
/// Контракт C: куски не перекрываются и доступны на `n` байт.
#[cfg(target_arch = "x86_64")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memcpy(dst: *mut c_void, src: *const c_void, n: usize) -> *mut c_void {
    let method = if n < SMALL { Method::RepQ } else { Method::from_u8(COPY.load(Ordering::Relaxed)).unwrap_or(Method::RepQ) };
    // SAFETY: контракт `memcpy`; ни один способ не зовёт `memcpy` сам.
    unsafe { copy_with(method, dst.cast(), src.cast(), n) };
    dst
}

/// `memset` ядра (вместо слабой из `compiler_builtins`).
///
/// # Safety
///
/// Контракт C: `dst` доступен на запись на `n` байт.
#[cfg(target_arch = "x86_64")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memset(dst: *mut c_void, value: i32, n: usize) -> *mut c_void {
    let method = if n < SMALL { Method::RepQ } else { Method::from_u8(FILL.load(Ordering::Relaxed)).unwrap_or(Method::RepQ) };
    // SAFETY: контракт `memset`; ни один способ не зовёт `memset` сам.
    unsafe { fill_with(method, dst.cast(), value as u8, n) };
    dst
}

#[cfg(target_arch = "x86_64")]
mod x86 {
    //! Все циклы — ассемблером: на Rust LLVM узнал бы в них копию или
    //! заполнение и позвал бы `memcpy`/`memset` — то есть самих себя.

    use core::arch::asm;

    /// `CR4.OSFXSR`: SSE на этом процессоре включён.
    const CR4_OSFXSR: u64 = 1 << 9;

    /// Как `compiler_builtins` без `ermsb`: байты до выравнивания назначения,
    /// `rep movsq`, хвост байтами.
    pub unsafe fn rep_q(dst: *mut u8, src: *const u8, len: usize) {
        let head = (dst as usize).wrapping_neg() % 8;
        let head = head.min(len);
        let words = (len - head) / 8;
        let tail = len - head - words * 8;
        // SAFETY: `head + 8 * words + tail == len` байт с обеих сторон; флаг
        // направления у ядра сброшен (ABI).
        unsafe {
            asm!(
                "rep movsb",
                "mov rcx, {words}",
                "rep movsq",
                "mov rcx, {tail}",
                "rep movsb",
                words = in(reg) words,
                tail = in(reg) tail,
                inout("rcx") head => _,
                inout("rdi") dst => _,
                inout("rsi") src => _,
                options(nostack, preserves_flags)
            );
        }
    }

    pub unsafe fn rep_b(dst: *mut u8, src: *const u8, len: usize) {
        // SAFETY: см. `rep_q`.
        unsafe {
            asm!(
                "rep movsb",
                inout("rcx") len => _,
                inout("rdi") dst => _,
                inout("rsi") src => _,
                options(nostack, preserves_flags)
            );
        }
    }

    /// Восемь слов за шаг, хвост — `rep movsb`.
    pub unsafe fn words(dst: *mut u8, src: *const u8, len: usize) {
        let blocks = len / 64;
        let done = blocks * 64;
        if blocks > 0 {
            // SAFETY: `blocks * 64 <= len` байт доступны с обеих сторон.
            unsafe {
                asm!(
                    "2:",
                    "mov {a}, [{s}]",
                    "mov {b}, [{s} + 8]",
                    "mov {c}, [{s} + 16]",
                    "mov {d}, [{s} + 24]",
                    "mov [{t}], {a}",
                    "mov [{t} + 8], {b}",
                    "mov [{t} + 16], {c}",
                    "mov [{t} + 24], {d}",
                    "mov {a}, [{s} + 32]",
                    "mov {b}, [{s} + 40]",
                    "mov {c}, [{s} + 48]",
                    "mov {d}, [{s} + 56]",
                    "mov [{t} + 32], {a}",
                    "mov [{t} + 40], {b}",
                    "mov [{t} + 48], {c}",
                    "mov [{t} + 56], {d}",
                    "add {s}, 64",
                    "add {t}, 64",
                    "dec {n}",
                    "jnz 2b",
                    s = inout(reg) src => _,
                    t = inout(reg) dst => _,
                    n = inout(reg) blocks => _,
                    a = out(reg) _,
                    b = out(reg) _,
                    c = out(reg) _,
                    d = out(reg) _,
                    options(nostack)
                );
            }
        }
        // SAFETY: хвост короче 64 байт внутри тех же кусков.
        unsafe { rep_b(dst.add(done), src.add(done), len - done) };
    }

    fn sse_ready() -> bool {
        let cr4: u64;
        // SAFETY: чтение управляющего регистра в нулевом кольце.
        unsafe { asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack, preserves_flags)) };
        cr4 & CR4_OSFXSR != 0
    }

    /// Четыре `movdqu` за шаг, кусками по [`super::SSE_CHUNK`] с закрытыми
    /// прерываниями и своими `xmm0`–`xmm3` (см. описание модуля).
    pub unsafe fn sse(dst: *mut u8, src: *const u8, len: usize) {
        if !sse_ready() {
            // SAFETY: условие делегировано вызывающему.
            return unsafe { words(dst, src, len) };
        }
        let mut done = 0;
        let mut save = [0u64; 8];
        while len - done >= 64 {
            let take = (len - done).min(super::SSE_CHUNK) & !63;
            // SAFETY: `take` байт доступны с обеих сторон; `save` — 64 байта
            // на стеке; регистры и флаг прерываний возвращаются до выхода.
            unsafe {
                asm!(
                    "pushfq",
                    "cli",
                    "movdqu [{v}], xmm0",
                    "movdqu [{v} + 16], xmm1",
                    "movdqu [{v} + 32], xmm2",
                    "movdqu [{v} + 48], xmm3",
                    "2:",
                    "movdqu xmm0, [{s}]",
                    "movdqu xmm1, [{s} + 16]",
                    "movdqu xmm2, [{s} + 32]",
                    "movdqu xmm3, [{s} + 48]",
                    "movdqu [{t}], xmm0",
                    "movdqu [{t} + 16], xmm1",
                    "movdqu [{t} + 32], xmm2",
                    "movdqu [{t} + 48], xmm3",
                    "add {s}, 64",
                    "add {t}, 64",
                    "dec {n}",
                    "jnz 2b",
                    "movdqu xmm0, [{v}]",
                    "movdqu xmm1, [{v} + 16]",
                    "movdqu xmm2, [{v} + 32]",
                    "movdqu xmm3, [{v} + 48]",
                    "popfq",
                    v = in(reg) save.as_mut_ptr(),
                    s = inout(reg) src.add(done) => _,
                    t = inout(reg) dst.add(done) => _,
                    n = inout(reg) take / 64 => _,
                );
            }
            done += take;
        }
        // SAFETY: хвост короче 64 байт внутри тех же кусков.
        unsafe { rep_b(dst.add(done), src.add(done), len - done) };
    }

    /// Байт, размноженный на слово.
    fn pattern(byte: u8) -> u64 {
        u64::from(byte) * 0x0101_0101_0101_0101
    }

    pub unsafe fn fill_rep_q(dst: *mut u8, byte: u8, len: usize) {
        let head = (dst as usize).wrapping_neg() % 8;
        let head = head.min(len);
        let words = (len - head) / 8;
        let tail = len - head - words * 8;
        // SAFETY: `head + 8 * words + tail == len` байт.
        unsafe {
            asm!(
                "rep stosb",
                "mov rcx, {words}",
                "rep stosq",
                "mov rcx, {tail}",
                "rep stosb",
                words = in(reg) words,
                tail = in(reg) tail,
                inout("rcx") head => _,
                inout("rdi") dst => _,
                in("rax") pattern(byte),
                options(nostack, preserves_flags)
            );
        }
    }

    pub unsafe fn fill_rep_b(dst: *mut u8, byte: u8, len: usize) {
        // SAFETY: `len` байт.
        unsafe {
            asm!(
                "rep stosb",
                inout("rcx") len => _,
                inout("rdi") dst => _,
                in("al") byte,
                options(nostack, preserves_flags)
            );
        }
    }

    pub unsafe fn fill_words(dst: *mut u8, byte: u8, len: usize) {
        let blocks = len / 64;
        let done = blocks * 64;
        if blocks > 0 {
            // SAFETY: `blocks * 64 <= len` байт.
            unsafe {
                asm!(
                    "2:",
                    "mov [{t}], {p}",
                    "mov [{t} + 8], {p}",
                    "mov [{t} + 16], {p}",
                    "mov [{t} + 24], {p}",
                    "mov [{t} + 32], {p}",
                    "mov [{t} + 40], {p}",
                    "mov [{t} + 48], {p}",
                    "mov [{t} + 56], {p}",
                    "add {t}, 64",
                    "dec {n}",
                    "jnz 2b",
                    t = inout(reg) dst => _,
                    n = inout(reg) blocks => _,
                    p = in(reg) pattern(byte),
                    options(nostack)
                );
            }
        }
        // SAFETY: хвост короче 64 байт.
        unsafe { fill_rep_b(dst.add(done), byte, len - done) };
    }

    pub unsafe fn fill_sse(dst: *mut u8, byte: u8, len: usize) {
        if !sse_ready() {
            // SAFETY: условие делегировано вызывающему.
            return unsafe { fill_words(dst, byte, len) };
        }
        let mut done = 0;
        let mut save = [0u64; 2];
        let fill = [pattern(byte); 2];
        while len - done >= 64 {
            let take = (len - done).min(super::SSE_CHUNK) & !63;
            // SAFETY: `take` байт; `save` и `fill` — по 16 байт на стеке;
            // регистр и флаг прерываний возвращаются до выхода.
            unsafe {
                asm!(
                    "pushfq",
                    "cli",
                    "movdqu [{v}], xmm0",
                    "movdqu xmm0, [{f}]",
                    "2:",
                    "movdqu [{t}], xmm0",
                    "movdqu [{t} + 16], xmm0",
                    "movdqu [{t} + 32], xmm0",
                    "movdqu [{t} + 48], xmm0",
                    "add {t}, 64",
                    "dec {n}",
                    "jnz 2b",
                    "movdqu xmm0, [{v}]",
                    "popfq",
                    v = in(reg) save.as_mut_ptr(),
                    f = in(reg) fill.as_ptr(),
                    t = inout(reg) dst.add(done) => _,
                    n = inout(reg) take / 64 => _,
                );
            }
            done += take;
        }
        // SAFETY: хвост короче 64 байт.
        unsafe { fill_rep_b(dst.add(done), byte, len - done) };
    }
}
