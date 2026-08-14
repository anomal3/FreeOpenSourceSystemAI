//! Векторное состояние задачи на x86-64.
//!
//! # Почему этого не было раньше и почему это уже было неверно
//!
//! Ядро собрано без операций с плавающей точкой с Phase 4, а программы шли под
//! `x86_64-unknown-none`, где SSE выключен спецификацией таргета: векторных
//! регистров у компилятора не было, и не сохранять их было безвредно. Первая же
//! программа, собранная с SSE, вскрывает то, что уже неверно, — переключение
//! задач сохраняло целочисленный контекст и не сохраняло ничего больше.
//!
//! # Жадно, а не лениво
//!
//! Классический приём — не сохранять регистры, пока задача их не тронула:
//! `CR0.TS` даёт `#NM` на первой же векторной инструкции, и обработчик
//! подгружает состояние. Здесь выбран **жадный** способ: сохранять и
//! восстанавливать всегда. Причина не в простоте — от ленивого отказались сами
//! современные ядра: на процессоре, где вектор трогает почти каждая программа
//! (а с SSE это буквально каждая, потому что через `xmm` компилятор копирует
//! память), экономия исчезает, а сложность и целый класс гонок остаются.
//!
//! # Откуда берётся размер области
//!
//! Из `CPUID.0D`, и только оттуда. Размер `XSAVE` — не константа, а функция от
//! включённых компонентов: с одним SSE это 576 байт, с AVX — 832, с AVX-512 —
//! за две с половиной тысячи. Зашитое число ломается на первом же процессоре с
//! другим набором расширений, причём ломается записью за конец буфера.
//!
//! # Чего область не гарантирует
//!
//! `xsaveopt` вправе не записывать компоненты, которых программа не трогала, —
//! о том, что в области лежит, говорит её заголовок (`XSTATE_BV`), а не наши
//! ожидания. Поэтому область здесь — непрозрачные байты: ядро её только
//! сохраняет и восстанавливает целиком и никогда не читает как структуру.

use core::arch::asm;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Размер области `FXSAVE` — константа архитектуры.
const FXSAVE_SIZE: usize = 512;

/// Выравнивание области.
///
/// `FXSAVE` требует шестнадцати байт, `XSAVE` — шестидесяти четырёх. Берём
/// большее: невыровненная область — это `#GP` на ровном месте, то есть отказ в
/// кольце ноль на каждом переключении задач.
pub const AREA_ALIGN: usize = 64;

/// Смещение `MXCSR` в области `FXSAVE`. Часть раскладки, заданной
/// архитектурой.
const MXCSR_OFFSET: usize = 24;

/// Значение `MXCSR` у только что запущенной программы: все исключения
/// маскированы. Обнулённый `MXCSR` тоже допустим, но означал бы программу,
/// которую первое же деление на ноль снимает отказом.
const MXCSR_DEFAULT: u32 = 0x1F80;

/// Размер области под состояние. Уточняется в [`init`].
static AREA_SIZE: AtomicUsize = AtomicUsize::new(FXSAVE_SIZE);

/// Есть ли `XSAVE`. Без него остаётся `FXSAVE`, покрывающий x87 и SSE.
static HAVE_XSAVE: AtomicBool = AtomicBool::new(false);

/// Биты `XCR0`.
const XCR0_X87: u64 = 1 << 0;
const XCR0_SSE: u64 = 1 << 1;
const XCR0_AVX: u64 = 1 << 2;
const XCR0_OPMASK: u64 = 1 << 5;
const XCR0_ZMM_HI256: u64 = 1 << 6;
const XCR0_HI16_ZMM: u64 = 1 << 7;

/// Все три бита AVX-512 включаются только вместе: процессор отвергает `XCR0`, в
/// котором есть один из них без остальных.
const XCR0_AVX512: u64 = XCR0_OPMASK | XCR0_ZMM_HI256 | XCR0_HI16_ZMM;

/// Включить векторные расширения и выяснить размер области.
///
/// Вызывается один раз при запуске ядра, до планировщика.
pub fn init() {
    // SAFETY: инструкции управляющих регистров исполняются в кольце ноль, а
    // ядро на этом этапе однопоточно.
    unsafe {
        let mut cr0: u64;
        asm!("mov {}, cr0", out(reg) cr0, options(nomem, nostack));
        // EM = 0: векторные инструкции исполняются, а не эмулируются ловушкой.
        // MP = 1 и TS = 0: `#NM` нам не нужен вовсе — сохранение жадное.
        cr0 &= !((1 << 2) | (1 << 3));
        cr0 |= 1 << 1;
        asm!("mov cr0, {}", in(reg) cr0, options(nomem, nostack));

        let mut cr4: u64;
        asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack));
        // OSFXSR: ядро обязуется сохранять состояние SSE — то самое обещание,
        // которое эта фаза наконец выполняет. OSXMMEXCPT: исключения SSE
        // приходят как `#XM`, а не как `#UD`.
        cr4 |= (1 << 9) | (1 << 10);
        asm!("mov cr4, {}", in(reg) cr4, options(nomem, nostack));
    }

    let (_, _, ecx, _) = cpuid(1, 0);
    let have_xsave = ecx & (1 << 26) != 0;
    if !have_xsave {
        // Древний процессор либо гипервизор, спрятавший XSAVE. `FXSAVE` есть
        // везде, где есть SSE, и покрывает ровно то, чем такая машина
        // располагает.
        crate::kprintln!("  fpu         : SSE via FXSAVE, {FXSAVE_SIZE} bytes per task");
        return;
    }

    // SAFETY: `XSAVE` объявлен процессором, значит бит CR4.OSXSAVE существует.
    unsafe {
        let mut cr4: u64;
        asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack));
        cr4 |= 1 << 18;
        asm!("mov cr4, {}", in(reg) cr4, options(nomem, nostack));
    }

    let (eax, _, _, edx) = cpuid(0x0D, 0);
    let supported = u64::from(eax) | (u64::from(edx) << 32);

    // x87 обязателен по архитектуре, SSE есть везде, где есть XSAVE. Дальше —
    // только то, что процессор подтвердил сам.
    let mut wanted = XCR0_X87 | XCR0_SSE;
    if supported & XCR0_AVX != 0 {
        wanted |= XCR0_AVX;
        if supported & XCR0_AVX512 == XCR0_AVX512 {
            wanted |= XCR0_AVX512;
        }
    }

    // SAFETY: маска составлена из битов, о поддержке которых сказал сам
    // процессор, и AVX-512 включается только целиком.
    unsafe { write_xcr0(wanted) };

    // Размер спрашивается **после** установки XCR0: EBX отвечает про текущий
    // набор компонентов, а не про максимально возможный.
    let (_, ebx, _, _) = cpuid(0x0D, 0);
    let size = (ebx as usize).max(FXSAVE_SIZE + 64);
    AREA_SIZE.store(size, Ordering::Relaxed);
    HAVE_XSAVE.store(true, Ordering::Relaxed);

    crate::kprintln!(
        "  fpu         : XSAVE, XCR0 {wanted:#06x}, {size} bytes per task"
    );
}

/// Сколько байт занимает состояние одной задачи.
#[must_use]
pub fn area_size() -> usize {
    AREA_SIZE.load(Ordering::Relaxed)
}

/// Заполнить область состоянием только что запущенной программы.
///
/// # Safety
///
/// `area` — начало блока размером [`area_size`], выровненного на
/// [`AREA_ALIGN`].
pub unsafe fn init_area(area: *mut u8) {
    // SAFETY: контракт функции.
    unsafe {
        core::ptr::write_bytes(area, 0, area_size());
        // Нулевой заголовок `XSAVE` означает «все компоненты в начальном
        // состоянии», и `XRSTOR` их такими и поставит. `MXCSR` — исключение: он
        // грузится из области всегда, поэтому нулём его оставлять нельзя.
        area.add(MXCSR_OFFSET).cast::<u32>().write(MXCSR_DEFAULT);
    }
}

/// Сохранить векторное состояние текущей задачи.
///
/// # Safety
///
/// `area` — начало блока размером [`area_size`], выровненного на
/// [`AREA_ALIGN`], принадлежащего той задаче, которая сейчас исполняется.
pub unsafe fn save(area: *mut u8) {
    if HAVE_XSAVE.load(Ordering::Relaxed) {
        // SAFETY: контракт функции; маска `EDX:EAX = -1` означает «все
        // компоненты, какие включены в XCR0».
        unsafe {
            asm!(
                "xsave [{area}]",
                area = in(reg) area,
                in("eax") u32::MAX,
                in("edx") u32::MAX,
                options(nostack),
            );
        }
    } else {
        // SAFETY: контракт функции.
        unsafe {
            asm!("fxsave [{area}]", area = in(reg) area, options(nostack));
        }
    }
}

/// Восстановить векторное состояние задачи, получающей процессор.
///
/// # Safety
///
/// `area` заполнена либо [`init_area`], либо предыдущим [`save`] на этой же
/// машине.
pub unsafe fn restore(area: *const u8) {
    if HAVE_XSAVE.load(Ordering::Relaxed) {
        // SAFETY: контракт функции.
        unsafe {
            asm!(
                "xrstor [{area}]",
                area = in(reg) area,
                in("eax") u32::MAX,
                in("edx") u32::MAX,
                options(nostack),
            );
        }
    } else {
        // SAFETY: контракт функции.
        unsafe {
            asm!("fxrstor [{area}]", area = in(reg) area, options(nostack));
        }
    }
}

/// Записать `XCR0`.
///
/// # Safety
///
/// Биты обязаны быть из числа поддерживаемых, а зависимые группы — включаться
/// целиком: иначе `#GP`.
unsafe fn write_xcr0(value: u64) {
    // SAFETY: контракт функции; CR4.OSXSAVE к этому моменту установлен.
    unsafe {
        asm!(
            "xsetbv",
            in("ecx") 0,
            in("eax") value as u32,
            in("edx") (value >> 32) as u32,
            options(nomem, nostack),
        );
    }
}

/// Спросить процессор о его возможностях.
fn cpuid(leaf: u32, sub: u32) -> (u32, u32, u32, u32) {
    let (eax, ebx, ecx, edx);
    // SAFETY: `cpuid` не имеет побочных эффектов, кроме записи в четыре
    // регистра. `rbx` LLVM не отдаёт под операнд `asm!` (в некоторых сборках он
    // занят под указатель кадра), поэтому его значение обменивается через
    // временный регистр — стандартный обход, а не хитрость.
    unsafe {
        asm!(
            "mov {tmp:r}, rbx",
            "cpuid",
            "xchg {tmp:r}, rbx",
            tmp = out(reg) ebx,
            inout("eax") leaf => eax,
            inout("ecx") sub => ecx,
            out("edx") edx,
            options(nostack),
        );
    }
    (eax, ebx, ecx, edx)
}
