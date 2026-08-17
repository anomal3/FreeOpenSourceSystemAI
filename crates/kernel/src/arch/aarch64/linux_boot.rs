//! Вход в ядро по договору Linux — тот, которым пользуется телефон.
//!
//! # Два входа, а не один
//!
//! Обычный вход ([`crate::kernel_main`]) принимает готовый [`BootInfo`] от
//! нашего UEFI-загрузчика: память описана, таблицы построены, MMU включён.
//! Заводской загрузчик телефона не делает ничего из этого и не будет: у него
//! один договор — MMU выключен, в `x0` дерево устройств, — и другого не бывает.
//!
//! Старый вход при этом не трогается ни на байт. Здесь второй, и вся его работа
//! — довести машину до того состояния, в котором первый её и получает.
//!
//! # Порядок, и почему он такой
//!
//! 1. **Сторожевой таймер.** До всего остального: пока он жив, аппарат
//!    сбрасывается через минуту, и «встало» неотличимо от «перезагрузилось».
//! 2. **Полоса на экране.** До всего долгого: иначе первые полторы минуты
//!    загрузки выглядят как повисшая заставка загрузчика.
//! 3. **MMU.** До первого замка, счётчика и печати: без трансляции не работают
//!    `ldxr`/`stxr`, а на них стоит вся синхронизация ядра. Печать до этого
//!    места ушла бы в вечный цикл внутри лока.
//! 4. **Линия.** Как только можно печатать — найти, куда.
//! 5. **`BootInfo`.** Собрать описание машины из того же дерева.
//! 6. **Обычная загрузка.** Дальше ядро не знает, откуда оно запущено.
//!
//! Каждый из первых трёх пунктов уже был однажды не на своём месте, и каждый раз
//! это стоило захода к аппарату: снаружи все отказы выглядят одинаково.

use boot_info::{Framebuffer, KernelImage, KernelSegment, PixelFormat, SEG_EXEC, SEG_READ, SEG_WRITE};
use fdt::Fdt;

use super::{boot_mmu, fdt_boot, mtk};

// Границы образа, расставленные компоновщиком (`crates/kernel/phone.ld`).
//
// Нужны они ровно для одного: сказать распределителю кадров, что эта память
// занята. Узлы `/memory` описывают всё ОЗУ как свободное, включая тот кусок, в
// котором лежит сам работающий код, и без такой отметки первая же выданная
// страница оказалась бы страницей ядра.
unsafe extern "C" {
    static __image_start: u8;
    static __image_end: u8;
    static __text_start: u8;
    static __text_end: u8;
    static __rodata_start: u8;
    static __rodata_end: u8;
    static __data_start: u8;
    static __data_end: u8;
}

/// Сегменты образа с правами, которых они просят.
///
/// Статический массив, а не `Vec`: кучи в этот момент нет и не будет ещё долго.
/// Он же уезжает в [`BootInfo`] и живёт весь сеанс — `build_kernel_address_space`
/// читает его после переключения стека.
static mut SEGMENTS: [KernelSegment; 3] = [KernelSegment::new(0, 0, 0); 3];

/// Экран, если он найден: адрес буфера и шаг строки в точках.
///
/// Запоминается, как только стал известен, и нужен обработчику отказа: на
/// аппарате без линии залитый цветом экран — единственный способ сказать, что
/// что-то пошло не так.
static mut SCREEN: Option<(u64, u32)> = None;

/// Точка входа с EL1, вызывается из `head_fdt.S`.
///
/// # Safety
///
/// Вызывается ровно один раз, из ассемблерного входа, который уже поставил
/// стек, обнулил `.bss`, установил таблицу векторов и спустился на EL1.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phone_boot(dtb: *const u8) -> ! {
    // SAFETY: указатель пришёл в `x0` от загрузчика; всё остальное проверяет
    // сам разбор, а невалидное дерево даёт `None`, а не чтение мимо.
    let Some(fdt) = (unsafe { Fdt::from_ptr(dtb) }) else {
        // Дерева нет — значит нет ни карты памяти, ни адреса экрана, ни адреса
        // линии. Сказать об этом нечем и некому: стоим.
        stop();
    };

    // 1. Сторожевой таймер. Раньше всего остального.
    // SAFETY: MMU выключен, адреса физические.
    unsafe { mtk::disable_watchdog(&fdt) };

    // 2. Признак жизни на экране. Тонкая полоса, а не кадр: с выключенным MMU
    // каждая запись — отдельная посылка на шину, и целый экран занял бы десятки
    // секунд, которые видно глазом.
    // SAFETY: та же причина — тождественные адреса.
    let screen = unsafe { mtk::scanned_buffer(&fdt) };
    if let Some((base, stride)) = screen {
        // SAFETY: ядро однопоточно, это первые инструкции после входа.
        unsafe { SCREEN = Some((base, stride)) };
        // SAFETY: адрес и шаг прочитаны у самого контроллера дисплея.
        unsafe { mtk::progress(base, stride, 0) };
    }

    let framebuffer = screen.map(|(base, stride)| Framebuffer {
        base,
        size: u64::from(stride) * u64::from(fdt_boot::DEFAULT_SCREEN.1) * 4,
        width: fdt_boot::DEFAULT_SCREEN.0,
        height: fdt_boot::DEFAULT_SCREEN.1,
        stride,
        // BGRA — то, что оставляет LK на MediaTek: синий в младшем байте.
        format: PixelFormat::Bgr,
    });

    // 3. MMU. С этой строки работают атомарные операции, а значит замки, куча и
    // печать. До неё — ничего из этого.
    //
    // SAFETY: исполнение на EL1 (за это отвечает `head_fdt.S`), MMU выключен,
    // вызов единственный. Кадровый буфер отдаётся отдельно, чтобы он попал в
    // раскладку некэшируемым: панель читает его мимо кэша.
    unsafe {
        boot_mmu::enable(
            &fdt,
            framebuffer.map(|fb| boot_mmu::Span { start: fb.base, len: fb.size }),
        );
    }
    if let Some((base, stride)) = screen {
        // SAFETY: буфер отображён, адрес прежний — отображение тождественное.
        unsafe { mtk::progress(base, stride, 1) };
    }

    // 4. Линия. Дерево запоминается заодно: по нему потом ищется контроллер
    // прерываний, и другого источника этих адресов на этой машине нет.
    super::set_device_tree(dtb as u64);
    // SAFETY: окно линии отображено ранней раскладкой как память устройства.
    unsafe { crate::serial::preset(super::serial_from_tree(&fdt)) };

    // 5. Описание машины.
    // SAFETY: дерево разобрано выше, а границы образа расставил компоновщик.
    let info = unsafe { fdt_boot::describe(dtb, framebuffer, image()) };
    let Some(info) = info else {
        crate::kprintln!("FATAL: the device tree describes no memory; nothing to boot on");
        paint(0xFFFF_0000);
        stop();
    };

    if let Some((base, stride)) = screen {
        // SAFETY: см. выше.
        unsafe { mtk::progress(base, stride, 2) };
    }

    // 6. Дальше — обычная загрузка, та же самая, что на всякой другой машине.
    crate::start(info as *const _)
}

/// Отказ на раннем этапе: до того, как ядро поставило свои векторы.
///
/// Печатать пробуем, но рассчитывать на печать нельзя: у аппарата линии нет
/// вовсе, а отказ мог случиться и до того, как нашёлся экран. Поэтому весь
/// отчёт — красный экран, и это ровно тот минимум, который отличает «упало» от
/// «зависло».
///
/// # Safety
///
/// Вызывается только из таблицы векторов `head_fdt.S`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phone_fault(vector: u64, esr: u64) -> ! {
    // Класс исключения — старшие шесть бит синдрома; по нему отличают отказ
    // доступа к памяти от запрещённой инструкции, и это первое, что спрашивают.
    crate::kprintln!(
        "FATAL: early exception, vector {vector}, ESR {esr:#018x} (class {:#04x})",
        (esr >> 26) & 0x3f
    );
    paint(0xFFFF_0000);
    stop();
}

/// Залить экран одним цветом — весь отчёт, который есть у аппарата без линии.
fn paint(colour: u32) {
    // SAFETY: ядро в этой точке уже не продолжится, гонки быть не с чем.
    let Some((base, stride)) = (unsafe { SCREEN }) else {
        return;
    };
    let words = (stride * fdt_boot::DEFAULT_SCREEN.1) as usize;
    for offset in 0..words {
        // SAFETY: буфер найден у контроллера дисплея, длина посчитана по его
        // же шагу строки; запись обязана быть volatile — это память устройства.
        unsafe { (base as *mut u32).add(offset).write_volatile(colour) };
    }
}

/// Остановиться навсегда, не маскируя ничего лишнего.
fn stop() -> ! {
    super::halt()
}

/// Где лежит образ и какими правами просят его сегменты.
fn image() -> KernelImage {
    let start = (&raw const __image_start) as u64;
    let end = (&raw const __image_end) as u64;

    let segments = [
        (
            (&raw const __text_start) as u64,
            (&raw const __text_end) as u64,
            SEG_READ | SEG_EXEC,
        ),
        (
            (&raw const __rodata_start) as u64,
            (&raw const __rodata_end) as u64,
            SEG_READ,
        ),
        // Данные, `.bss` и стек — одним куском: они и лежат подряд, и права у
        // них одни. Стек попадает сюда намеренно: без него `image_size` в
        // заголовке описывал бы меньше, чем образ занимает на самом деле.
        (
            (&raw const __data_start) as u64,
            end,
            SEG_READ | SEG_WRITE,
        ),
    ];

    for (slot, (base, limit, flags)) in segments.iter().enumerate() {
        // SAFETY: ядро однопоточно, индекс меньше длины массива.
        unsafe {
            (&raw mut SEGMENTS)
                .cast::<KernelSegment>()
                .add(slot)
                .write(KernelSegment::new(*base, limit.saturating_sub(*base), *flags));
        }
    }

    KernelImage {
        base: start,
        size: end - start,
        segments_ptr: (&raw const SEGMENTS) as u64,
        segments_len: segments.len() as u64,
    }
}

// Ассемблерный вход. Лежит рядом и включается вместе с этим модулем: без него
// у образа не было бы ни заголовка, ни таблицы векторов.
core::arch::global_asm!(include_str!("head_fdt.S"));
