//! Ядро FreeOS: приём управления от UEFI-загрузчика и запуск подсистем.
//!
//! Порядок запуска задан жёстко и переставлять его нельзя: serial раньше всего
//! остального (иначе первая же ошибка останется невидимой), проверка хэндоффа
//! раньше обращения к его полям, память раньше кучи, куча раньше прокрутки
//! консоли, и прерывания последними — когда всё, что они могут прервать, уже
//! в согласованном состоянии.
//!
//! # Как ядро сюда попадает
//!
//! Загрузчик читает ELF-образ ядра, выделяет память, копирует `PT_LOAD`
//! сегменты, обнуляет BSS (хвост сегмента, где `memsz > filesz`), применяет
//! relocations из `.rela.dyn`, вызывает `ExitBootServices` и передаёт
//! управление на `e_entry`, положив в первый аргумент указатель на `BootInfo`.
//!
//! # Почему образ position-independent
//!
//! Куда именно UEFI-прошивка даст выделить память, заранее неизвестно:
//! `AllocatePages` возвращает то, что свободно, и на разных машинах это разные
//! адреса. Чтобы ядро не зависело от адреса загрузки, оно собирается как PIE
//! (`ET_DYN`): все внутренние обращения — RIP/PC-относительные, а немногие
//! оставшиеся абсолютные адреса (таблицы указателей, `&'static str` в
//! диагностике) вынесены в `.rela.dyn` как `R_X86_64_RELATIVE` /
//! `R_AARCH64_RELATIVE`, которые загрузчик правит прибавлением базы.
//!
//! Конкретные флаги лежат в `.cargo/config.toml` (секции
//! `[target.x86_64-unknown-none]` и `[target.aarch64-unknown-none]`) и там же
//! прокомментированы. Отдельный linker script не нужен: раскладка по умолчанию
//! от `rust-lld` уже даёт ровно один `PT_LOAD`-набор с корректными `filesz` и
//! `memsz`, а границы BSS загрузчик берёт из разницы между ними — заводить ради
//! этого символы `__bss_start`/`__bss_end` смысла нет.
//!
//! # Точка входа
//!
//! [`kernel_main`] и есть `e_entry` ELF-заголовка: линкеру передан
//! `--entry=kernel_main`. Символ дополнительно экспортируется под своим именем,
//! так что загрузчик может найти его и через таблицу символов.

#![no_std]
#![no_main]

mod acpi;
mod arch;
mod block;
mod config;
mod console;
mod fs;
mod input;
mod irq;
mod mm;
mod net;
mod pci;
mod power;
mod print;
mod random;
mod sched;
mod serial;
mod shell;
mod slot;
mod trust;
mod sync;
mod time;
mod tty;
mod virtio;
mod ui;
mod usb;
mod user;
mod vfs;

extern crate alloc;

use alloc::vec::Vec;
use boot_info::{BOOT_INFO_MAGIC, BOOT_INFO_REVISION, BootInfo, MemoryKind, MemoryMap};
use core::mem::{align_of, size_of};
use core::panic::PanicInfo;
use core::ptr;

use crate::mm::{AddressSpace, VirtAddr};
use crate::vfs::FileSystem;

/// Версия системы: мажор и минор из `[workspace.package]`.
///
/// Ровно то же значение и тем же способом собирает `xtask` для имени файла
/// образа и метки тома (`xtask/src/version.rs`). Патч-версия отброшена там и
/// здесь: две записи одной версии, различающиеся на глаз, — это уже две версии.
/// За машиной никого нет: система запущена стендом.
///
/// Ставится один раз при разборе [`BootInfo`] и читается оболочкой — единственным
/// местом, которому эта разница важна (см. `shell::IDLE_TIMEOUT_SECONDS`).
static UNATTENDED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// За машиной никого нет — см. [`UNATTENDED`].
#[must_use]
pub fn unattended() -> bool {
    UNATTENDED.load(core::sync::atomic::Ordering::Relaxed)
}

const VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION_MAJOR"),
    ".",
    env!("CARGO_PKG_VERSION_MINOR")
);

/// Точка входа ядра на x86-64.
///
/// ABI указан явно и `extern "C"` здесь был бы ошибкой. Загрузчик собран под
/// `x86_64-unknown-uefi`, где `extern "C"` — это Microsoft x64 (первый аргумент
/// в `RCX`), а ядро под `x86_64-unknown-none`, где то же `extern "C"` — System V
/// (первый аргумент в `RDI`). Обе стороны компилируются молча, а в рантайме
/// ядро читает регистр с мусором и сообщает о «повреждённом BootInfo», уводя
/// расследование в сторону. Соглашение фиксирует [`boot_info::KernelEntry`].
///
/// # Safety
///
/// Загрузчик обязан передать либо валидный указатель на инициализированный
/// [`BootInfo`], либо null. Любое другое значение (мусорный, но выровненный и
/// ненулевой адрес) ядро отличить не в состоянии — против этого и работает
/// проверка magic/revision ниже.
#[cfg(target_arch = "x86_64")]
#[unsafe(no_mangle)]
pub extern "sysv64" fn kernel_main(boot_info: *const BootInfo) -> ! {
    start(boot_info)
}

/// Точка входа ядра на AArch64.
///
/// Здесь `extern "C"` корректен: и UEFI-таргет, и freestanding используют
/// AAPCS64, поэтому расхождения, описанного у x86-64 варианта, не возникает.
///
/// # Safety
///
/// Те же требования, что и у x86-64 варианта выше.
#[cfg(target_arch = "aarch64")]
#[unsafe(no_mangle)]
pub extern "C" fn kernel_main(boot_info: *const BootInfo) -> ! {
    start(boot_info)
}

/// Третий вход: договор Linux на AArch64.
///
/// Не «ещё одна точка входа для удобства», а единственная, которую понимает
/// заводской загрузчик телефона: MMU выключен, в `x0` дерево устройств,
/// никакого [`BootInfo`] не существует. Собрать его — работа
/// [`arch::linux_boot`], и после неё дальше идёт ровно тот же [`start`], что и
/// на машине с UEFI. Ядро не знает, откуда его запустили, и знать не должно:
/// ветка «а если мы на телефоне» внутри распределителя памяти была бы началом
/// второй системы внутри первой.
///
/// Символ здесь только ради того, чтобы модуль входа попал в сборку: сам вход —
/// `_phone_start` из `head_fdt.S`, и на него указывает компоновочный сценарий.
#[cfg(all(target_arch = "aarch64", feature = "phone"))]
#[used]
static LINUX_ENTRY: unsafe extern "C" fn(*const u8) -> ! = arch::linux_boot::phone_boot;

/// Общее тело точки входа: всё, что не зависит от соглашения о вызове.
pub(crate) fn start(boot_info: *const BootInfo) -> ! {
    // Первым делом — serial: без него ни одна последующая ошибка не будет видна.
    serial::init();

    let Some(info) = validate(boot_info) else {
        kprintln!();
        kprintln!("FreeOS kernel: refusing to boot, see above. Halting.");
        arch::halt();
    };

    if info.framebuffer.is_present() {
        console::init(&info.framebuffer);
    }

    // Проверка адреса порта — сразу, как только карта памяти прошла проверку.
    // Раньше нельзя (карте ещё нельзя верить), позже незачем: дальше начинается
    // раздача кадров, и первый же выданный кадр по этому адресу превратил бы
    // диагностический вывод в порчу чужих данных.
    if serial_lands_in_ram(&info.memory_map) {
        serial::silence();
    }

    banner(&info, boot_info as usize);
    dump_memory_map(&info.memory_map);

    // Адрес таблиц запоминается до всего остального: он нужен и арх-части при
    // поиске контроллера прерываний, и драйверам, а `BootInfo` до них не
    // доезжает.
    acpi::set_rsdp(info.acpi_rsdp);

    // С этого момента памятью распоряжается ядро, а не прошивка.
    take_over_memory(&info);

    // Переключение на собственный стек — последнее, что делается на стеке
    // загрузчика. Дальше в `info` заглядывать нельзя: это копия, лежащая на
    // покидаемом стеке. Поэтому продолжению уезжает физический адрес исходной
    // структуры, а не ссылка на копию.
    //
    // SAFETY: `STACK_TOP` — вершина области, которую `build_kernel_address_space`
    // только что отобразил на запись, а `resume_on_kernel_stack` — обычная
    // функция ядра, лежащая в исполняемом сегменте.
    unsafe {
        arch::switch_stack(
            VirtAddr::new(mm::STACK_TOP),
            resume_on_kernel_stack,
            boot_info as usize,
        )
    }
}

/// Забрать управление памятью у прошивки: пул кадров, собственные таблицы
/// страниц с W^X и куча.
///
/// Порядок шагов здесь не переставляется. Пул кадров нужен раньше таблиц,
/// потому что таблицы из него и строятся. Куча инициализируется последней:
/// её диапазон становится доступным только после того, как процессор
/// переключён на новые таблицы.
fn take_over_memory(info: &BootInfo) {
    kprintln!();
    kprintln!("---- memory subsystem -------------------------------------------");

    // SAFETY: карта памяти приехала от загрузчика и уже проверена вместе с
    // остальным хэндоффом; аллокатор инициализируется ровно один раз.
    let stats = match unsafe { mm::frame::init(info) } {
        Ok(stats) => stats,
        Err(err) => {
            kprintln!("FATAL: frame allocator init failed: {err:?}");
            arch::halt();
        }
    };
    kprintln!(
        "  frames      : {} total, {} free ({} MiB usable)",
        stats.total,
        stats.free,
        stats.free_bytes() / (1024 * 1024)
    );

    let space: Option<Result<arch::KernelSpace, arch::SpaceError>> =
        mm::frame::with(|frames| arch::build_kernel_address_space(info, frames));
    let space = match space {
        Some(Ok(space)) => space,
        Some(Err(err)) => {
            kprintln!("FATAL: building the kernel address space failed: {err:?}");
            arch::halt();
        }
        None => {
            kprintln!("FATAL: frame allocator unavailable while building page tables");
            arch::halt();
        }
    };
    kprintln!("  page tables : root at {:?}", space.root());

    // Точка невозврата номер два за загрузку: со следующей инструкции трансляцию
    // адресов определяют наши таблицы, а не прошивочные. Если в них чего-то не
    // хватает — кода, стека или UART, — отказ произойдёт молча и немедленно.
    //
    // SAFETY: адресное пространство содержит identity-отображение всей
    // физической памяти, поэтому и текущий код, и текущий стек остаются
    // доступными по тем же адресам.
    unsafe { space.activate() };
    kprintln!("  page tables : active, W^X applied to the kernel image");

    probe_framebuffer(info);

    // Физическая память теперь доступна и через прямое отображение; аллокатор
    // переводится на него, чтобы пережить снятие identity в следующей фазе.
    // SAFETY: `build_kernel_address_space` отобразил всю описанную картой
    // физическую память по `PHYS_MAP_BASE`, а таблицы уже активны.
    unsafe { mm::frame::use_direct_map() };

    // SAFETY: диапазон кучи отображён на запись и подкреплён отдельными
    // кадрами; вызывается однократно и только после активации таблиц.
    match unsafe { mm::heap::init() } {
        Ok(stats) => kprintln!(
            "  heap        : {} MiB at {:#018x} ({} bytes free)",
            mm::HEAP_SIZE / (1024 * 1024),
            mm::HEAP_BASE,
            stats.free
        ),
        Err(err) => {
            kprintln!("FATAL: heap init failed: {err:?}");
            arch::halt();
        }
    }
}

/// Убедиться, что фреймбуфер пережил переключение на собственные таблицы.
///
/// Проверка не декоративная: экранная консоль пишет по физическому адресу,
/// который до активации давало отображение прошивки. Если новое отображение
/// уводит этот адрес в другое место, записи будут молча уходить в никуда — на
/// экране останется последний кадр, нарисованный до переключения, и выглядеть
/// это будет как «ядро зависло», хотя serial продолжит работать.
fn probe_framebuffer(info: &BootInfo) {
    let fb = &info.framebuffer;
    if !fb.is_present() {
        return;
    }
    // Проверяются начало, середина и конец: если отображён лишь первый кусок
    // буфера, запись в пиксель (0,0) пройдёт, а нижние строки экрана окажутся
    // недостижимы — ровно тот случай, который выглядит как «консоль замерла».
    let words = (fb.size / 4).max(1);
    let probes = [0usize, (words / 2) as usize, (words - 1) as usize];
    let mut failures = 0;

    for offset in probes {
        let ptr = unsafe { (fb.base as *mut u32).add(offset) };
        // SAFETY: смещение лежит внутри буфера, длину которого сообщил
        // загрузчик, а диапазон только что отображён на запись. Значение
        // восстанавливается, поэтому картинка не портится. `volatile`
        // обязателен: это память устройства.
        let readback = unsafe {
            let saved = ptr.read_volatile();
            ptr.write_volatile(0x00FF_00FF);
            let readback = ptr.read_volatile();
            ptr.write_volatile(saved);
            readback
        };
        if readback != 0x00FF_00FF {
            failures += 1;
            kprintln!("  framebuffer : word {offset} unreachable (read {readback:#010x})");
        }
    }

    if failures == 0 {
        kprintln!("  framebuffer : writable end to end after the switch");
    }
}

/// Продолжение, исполняемое уже на собственном стеке ядра.
///
/// Стек загрузчика к этому моменту покинут: он лежит в памяти, помеченной
/// `BootloaderReclaimable`, и ядро вправе её переиспользовать.
extern "C" fn resume_on_kernel_stack(boot_info: usize) -> ! {
    // Копия `BootInfo` осталась на старом стеке, поэтому структура читается
    // заново по исходному физическому адресу — он по-прежнему отображён.
    // SAFETY: адрес прошёл полную проверку в `validate` до переключения стека,
    // а память под ним отображена как `BootloaderReclaimable` и пока цела.
    let info = unsafe { ptr::read(boot_info as *const BootInfo) };
    // Запоминается сразу: метку читает оболочка, а до неё ещё вся загрузка, и
    // класть такое «где-нибудь по дороге» — способ однажды прочитать ноль.
    UNATTENDED.store(info.unattended(), core::sync::atomic::Ordering::Relaxed);

    kprintln!();
    kprintln!("---- running on the kernel's own stack --------------------------");
    kprintln!("  stack top   : {:#018x}", mm::STACK_TOP);
    kprintln!("  guard page  : {:#018x} (unmapped)", mm::STACK_TOP - mm::STACK_SIZE - mm::PAGE_SIZE);

    verify_heap();

    // Куча готова — консоль может завести теневой буфер и начать прокручиваться.
    // До этого момента строки за нижним краем экрана просто терялись.
    if console::enable_scroll() {
        kprintln!("  console     : scrollback enabled");
    }

    start_interrupts();

    // Векторные расширения — до планировщика и до первой задачи. Раньше нельзя:
    // область состояния берётся из кучи, а она поднимается выше. Позже нельзя
    // тоже: задача, созданная до этого вызова, получила бы область неизвестного
    // размера — на x86-64 его сообщает `CPUID` только после настройки `XCR0`.
    arch::fpu::init();

    // Источник случайности поднимается сразу после прерываний: с этого момента
    // тики начинают подмешивать в пул своё дрожание, и к моменту, когда
    // случайность понадобится по-настоящему (ключ хоста SSH), пул успеет
    // набрать хоть что-то. Проверка процессора при этом стоит одного `CPUID`.
    random::init();

    // Монотонный счётчик — сразу за таймером и раньше всех, кто меряет время.
    // На x86-64 это измерение длиной в 50 мс, на AArch64 — чтение регистра;
    // до него время работы считается тиками, то есть неточно, поэтому чем
    // раньше, тем меньше отрезок, посчитанный плохо.
    //
    // SAFETY: `acpi_rsdp` приехал в проверенном `BootInfo`; ноль там означает
    // «таблиц нет» и обрабатывается внутри.
    unsafe { arch::monotonic::calibrate(info.acpi_rsdp) };
    time::adopt_monotonic();

    let stats = mm::frame::stats();
    kprintln!();
    kprintln!(
        "  frames used : {} of {} ({} MiB still free)",
        stats.used(),
        stats.total,
        stats.free_bytes() / (1024 * 1024)
    );
    if info.framebuffer.is_present() {
        kprintln!("  framebuffer : still reachable at {:#018x}", info.framebuffer.base);
    }

    // Время суток запоминается сразу после запуска таймера: до этого момента
    // складывать точку отсчёта не с чем, а после — счётчик тиков уже идёт, и
    // задержка попадёт в поправку, а не в ошибку.
    time::adopt_boot_clock(info.wall_clock, info.wall_clock_counter);

    announce_boot_mode(&info);

    mount_initrd(&info);
    mount_disk_root(&info);

    // Часовой пояс лежит в том же файле настроек, что язык и раскладка, и
    // читается там же, где личность сеанса, — как только стало известно, какая
    // ФС корневая.
    time::adopt_timezone();

    // Личность сеанса читается после того, как определился корень: на
    // установленной системе она приходит из `/etc/passwd`, а на загруженной с
    // носителя её взять неоткуда, и сеанс остаётся root. Печатается это в обоих
    // случаях — «проверки прав сегодня никому не откажут» обязано быть видно, а
    // не подразумеваться.
    user::session::adopt_account();

    let have_input = start_input(&info);
    start_graphics(&info);

    // Сеть поднимается до служб, и это не порядок ради порядка: первая же
    // служба, которую система заведёт (клиент DHCP из фазы 35), обращается к
    // карте с первой своей секунды, а карта, поднятая после неё, означала бы
    // службу, которая обязана уметь ждать появления устройства.
    //
    // SAFETY: ядро давно работает на собственных таблицах страниц, а `acpi_rsdp`
    // приехал в проверенном `BootInfo`; ноль там означает «таблиц нет» и
    // обрабатывается внутри.
    unsafe { net::init(info.acpi_rsdp) };

    // Подтверждение загрузки — здесь, и это самое позднее место, до которого
    // ядро вообще доходит перед тем, как отдать управление планировщику. Раньше
    // было бы нельзя: слот с целым ядром и разрушенным корнем стартует
    // прекрасно, и подтверждение сразу после старта означало бы «ядро
    // запустилось», а не «система работает». К этому моменту корень
    // смонтирован, учётная запись прочитана и экран отдан композитору.
    slot::confirm(info.came_back());
    start_services();

    // Планировщик забирает управление насовсем: сюда исполнение уже не вернётся.
    run_session(have_input)
}

/// Поднять композитор и отдать ему экран.
///
/// # Почему экран отдаётся до запуска композитора, а не после
///
/// Так пришлось сделать после того, как обратный порядок дал вполне наглядную
/// картинку: окна нарисованы, а поверх них — весь загрузочный лог целиком.
/// Причина в прокрутке. Консоль хранит теневой буфер экрана и при прокрутке
/// перерисовывает из него **весь** экран (иначе сдвинуть картинку было бы
/// нечем — фреймбуфер не читается). Достаточно одной строки, напечатанной после
/// того, как композитор нарисовал окна, — и если эта строка вызвала прокрутку,
/// консоль возвращает на экран всё, что помнит.
///
/// Поэтому: сначала консоль перестаёт рисовать, потом рисует композитор. Если
/// композитор не поднялся, экран возвращается консоли — с очисткой, потому что
/// восстановить то, что на нём было, нечем.
fn start_graphics(info: &BootInfo) {
    kprintln!();
    kprintln!("---- display ----------------------------------------------------");

    hold_boot_log(info);

    // Безопасный режим — это в первую очередь **меньше**. Композитор занимает
    // мегабайты под поверхности и рисует в память устройства, а ломается на
    // чужой машине чаще всего именно графика: не завестись должно то, что
    // может не завестись.
    if info.safe_mode() {
        kprintln!("  compositor  : not started, safe mode; the shell runs on the boot console");
        return;
    }

    if !info.framebuffer.is_present() {
        kprintln!("  compositor  : no framebuffer; the shell will run on the serial console");
        return;
    }

    console::release_screen();

    if !ui::init(&info.framebuffer) {
        console::reclaim_screen();
        kprintln!("  compositor  : could not take the screen; staying on the boot console");
        return;
    }

    // Дальше `kprintln!` пишет только в serial — экран уже не его.
    let (cols, rows) = ui::shell_size();
    let (_, _, windows) = ui::stats();
    kprintln!(
        "  compositor  : {}x{} screen, {windows} windows, shell {cols}x{rows} characters",
        info.framebuffer.width,
        info.framebuffer.height
    );
    kprintln!("  console     : screen handed over; the boot log continues on serial only");
}

/// Задержать журнал загрузки на экране, прежде чем его закроет рабочий стол.
///
/// # Зачем
///
/// На машине с последовательной линией журнал остаётся в файле, и перечитать
/// его можно когда угодно. На телефоне линия наружу не выведена: экран — и
/// первое, и единственное место, где журнал вообще существует, а живёт он до
/// первого кадра композитора, то есть доли секунды. Разглядеть там что-либо
/// нельзя, и снимок приходится ловить наугад — половина попыток выходит смазанной
/// или не с той строкой.
///
/// Поэтому задержка, и только там, где журналу больше некуда деться: признак —
/// машина, описанная деревом устройств, а не таблицами прошивки.
fn hold_boot_log(info: &BootInfo) {
    if info.device_tree == 0 {
        return;
    }
    const HOLD_SECONDS: u64 = 20;
    kprintln!("  console     : holding this log for {HOLD_SECONDS} s -- the desktop comes next");

    // Ожидание по счётчику тиков, а не по циклу заданной длины: тактовая
    // частота этой машины нам не принадлежит, и «примерно двадцать секунд»
    // превратились бы в две или в двести.
    let until = time::uptime_ms() + HOLD_SECONDS * 1000;
    while time::uptime_ms() < until {
        core::hint::spin_loop();
    }
}

/// Поднять устройства ввода.
///
/// Возвращает `true`, если хотя бы один источник событий заработал: приглашение
/// без источника событий — это просто бесконечное ожидание, и запускать его в
/// такой ситуации незачем.
fn start_input(info: &BootInfo) -> bool {
    kprintln!();
    kprintln!("---- input ------------------------------------------------------");

    arch::input::init(info);

    // USB поднимается после арх-специфичного ввода, и порядок здесь имеет
    // значение только один раз: на x86-64 PS/2-клавиатура к этому моменту уже
    // работает, поэтому отказ USB не оставляет машину без ввода и не обязан быть
    // фатальным. На AArch64 наоборот — USB там единственный путь к настоящей
    // клавиатуре, но и там отказ не смертелен: остаётся серийный порт.
    //
    // SAFETY: ядро исполняется на собственных таблицах, прерывания разрешены
    // (`start_interrupts` вызван раньше — ожидания внутри драйвера опираются на
    // таймер), таблицы ACPI не переиспользованы, и ни один лок не удерживается.
    // Драйвер сам дописывает поднятые им источники: клавиатура и мышь приходят
    // с одной шины, и решать за него, что именно нашлось, здесь нечем.
    unsafe { usb::xhci::init(info.acpi_rsdp) };

    // OHCI поднимается **всегда**, а не «если xHCI не нашёлся». Условие
    // выглядело бы разумно и было бы неверным: контроллеры сосуществуют, и на
    // машине с обоими устройство висит на одном из них — на каком именно,
    // заранее не известно никому. Цена безусловной попытки — одна строка в
    // журнале на машине без OHCI (перепись его не нашла — драйвер молчит).
    //
    // SAFETY: те же условия, что у xHCI выше.
    unsafe { usb::ohci::init(info.acpi_rsdp) };

    let sources = input::sources();

    if !sources.any() {
        kprintln!("  input       : no source of key events on this machine");
    }
    sources.any()
}

/// Создать задачи и отдать управление планировщику.
fn run_session(have_input: bool) -> ! {
    kprintln!();
    kprintln!("---- scheduler --------------------------------------------------");

    let spawned = sched::spawn_demo_tasks();
    kprintln!(
        "  spawned    : {spawned} tasks, {} KiB stack + {} KiB guard band each",
        sched::TASK_STACK_SIZE / 1024,
        sched::STACK_GUARD_SIZE / 1024
    );

    // Обслуживание xHCI — отдельная задача: оболочка, научившаяся спать в
    // ожидании ввода, не может обслуживать контроллер, который этот ввод и
    // порождает (см. `usb::xhci::service_task`). Спит она теперь до прерывания,
    // а не до срока.
    if usb::xhci::is_present() {
        if let Err(err) = sched::spawn_daemon("usb", usb::xhci::service_task) {
            kprintln!("  spawn usb service failed: {err}");
        }
    }

    // У OHCI задача своя: контроллеры независимы, и одна задача на двоих
    // означала бы, что отчёт одного ждёт опроса другого. Просыпается она по
    // часам — прерываний у этого драйвера нет, см. шапку `usb::ohci`.
    if usb::ohci::is_present() {
        if let Err(err) = sched::spawn_daemon("ohci", usb::ohci::service_task) {
            kprintln!("  spawn ohci service failed: {err}");
        }
    }

    // Служебные задачи ввода, если архитектуре они нужны. На телефоне это опрос
    // тачскрина: прерывания у него нет, и без этой задачи указатель не двинется
    // ни разу, хотя контроллер найден и отвечает.
    arch::spawn_input_services();

    // Задача, гасящая машину по просьбе кнопки. Заводится всегда, а не только
    // там, где кнопка есть: она же исполняет просьбу от рабочего стола, а её
    // отсутствие означало бы нажатие, о котором некому вспомнить.
    power::start();

    if have_input {
        if let Err(err) = sched::spawn("shell", shell::task) {
            kprintln!("  spawn shell failed: {err}");
        }
    } else {
        // Оболочка без источника событий — это бесконечное ожидание. Сказать об
        // этом надо здесь: иначе загрузка закончится молча, и будет непонятно,
        // почему приглашения нет.
        kprintln!("  shell      : not started, there is no way to type into it");
    }

    // Вытеснение включается здесь, а не при инициализации таймера: до этой
    // строки ядро — один поток управления, которому не с кем делить процессор, и
    // прерывание, снимающее его с самого себя, только удлинило бы загрузку.
    sched::set_preemption(true);
    kprintln!(
        "  preemption : on, {} ms slice at {} Hz",
        sched::SLICE_MS,
        irq::TIMER_HZ
    );

    sched::run()
}

/// Насколько глубоко обходить дерево каталогов.
///
/// Ограничение обязательно, а не на всякий случай: испорченный образ может
/// содержать каталог, ссылающийся на собственный кластер, и отличить это от
/// законного подкаталога драйвер не в состоянии. Без предела обход уходит в
/// бесконечную рекурсию и переполняет стек — то есть повреждённые данные
/// роняют ядро.
const MAX_TREE_DEPTH: usize = 8;

/// Смонтировать образ RAM-диска и показать, что файловая система читается.
fn mount_initrd(info: &BootInfo) {
    kprintln!();
    kprintln!("---- filesystem -------------------------------------------------");

    if !info.initrd.is_present() {
        kprintln!("  initrd      : absent -- booted without a filesystem");
        return;
    }

    // SAFETY: прямое отображение активно (таблицы включены в `take_over_memory`),
    // а память образа помечена загрузчиком как `Reserved`, поэтому ни аллокатор
    // кадров, ни куча её не переиспользуют.
    let disk = match unsafe { vfs::ramdisk::init(&info.initrd) } {
        Ok(disk) => disk,
        Err(err) => {
            kprintln!("  initrd      : unusable image: {err:?}");
            return;
        }
    };
    kprintln!(
        "  initrd      : {} KiB at {:#018x}",
        info.initrd.size / 1024,
        info.initrd.base
    );

    let fs = match fs::Fat32::mount(alloc::boxed::Box::new(disk)) {
        Ok(fs) => fs,
        Err(err) => {
            kprintln!("  mount       : failed: {err}");
            return;
        }
    };
    kprintln!(
        "  mounted     : {} volume '{}'",
        fs.name(),
        fs.label().as_deref().unwrap_or("<unlabelled>")
    );

    match fs.root() {
        Ok(root) => print_tree(&*root, "/", 0),
        Err(err) => kprintln!("  root        : unreadable: {err}"),
    }

    verify_file(&fs, "/data/large-cluster-chain-test.txt");

    // Смонтированная ФС становится корневой: с этого момента её видит оболочка,
    // и `ls` с `cat` работают по тем же путям, что напечатаны выше.
    fs::set_root(alloc::boxed::Box::new(fs));
}

/// Напечатать дерево каталогов, не глубже [`MAX_TREE_DEPTH`].
/// Найти диск, разобрать на нём таблицу разделов и смонтировать корень.
///
/// # Что здесь происходит и почему именно так
///
/// Ядро не получает от загрузчика никакого указания, с какого носителя оно
/// пришло, и добавлять такое поле в hand-off не потребовалось: раздел
/// **опознаётся по своему типу в GPT**. Тип `FREEOS_ROOT_TYPE` придуман нами и
/// записан установщиком — этого достаточно, чтобы отличить свой корень от
/// чужих разделов, и не нужно ни нового контракта, ни угадывания по порядку.
///
/// Отсутствие диска, таблицы разделов или нужного раздела — не отказ. Запуск
/// без установки (`xtask run`) — обычный режим работы: там корнем остаётся
/// образ RAM-диска, и система обязана в нём работать.
fn mount_disk_root(info: &BootInfo) {
    use disk::gpt;

    kprintln!();
    kprintln!("---- root filesystem --------------------------------------------");

    if info.acpi_rsdp == 0 {
        kprintln!("  disk        : no ACPI tables, so no PCI: keeping the initrd as root");
        return;
    }

    // SAFETY: ядро давно работает на собственных таблицах страниц (см.
    // `take_over_memory` выше по ходу загрузки), а RSDP пришёл от прошивки
    // через hand-off.
    let root = match unsafe { pci::Root::discover(info.acpi_rsdp) } {
        Ok(root) => root,
        Err(err) => {
            kprintln!("  disk        : no PCI at all ({err}): keeping the initrd as root");
            return;
        }
    };

    // SAFETY: см. выше.
    let disks = unsafe { block::probe_all(&root) };
    if disks.is_empty() {
        kprintln!("  disk        : no block device at all: keeping the initrd as root");
        return;
    }
    for found in &disks {
        kprintln!(
            "  disk        : {} #{}, {} sectors ({} MiB)",
            found.kind.name(),
            found.unit,
            found.sectors(),
            found.sectors() / 2048,
        );
    }

    // Разделы опознаются по типу GUID и ищутся на **всех** носителях сразу.
    // Порядок дисков при этом ничего не значит, и это важнее, чем кажется: на
    // чужой машине наш диск не обязан быть первым, а машина с двумя системами —
    // обычное дело.
    let found = block::scan(disks);

    // Какой слот грузился, сказал загрузчик. Спрашивать об этом диск было бы
    // нельзя: на диске оба слота выглядят одинаково пригодными, а знает, какой
    // из них выбран, только тот, кто выбирал.
    let slot = slots::slot_from_code(info.boot_slot);
    let root_type = match slot {
        Some(slots::Slot::B) => gpt::FREEOS_ROOT_B_TYPE,
        // Система без слотов ищет тот же тип, что и слот A: диск, размеченный
        // прежним установщиком, обязан продолжать грузиться.
        Some(slots::Slot::A) | None => gpt::FREEOS_ROOT_TYPE,
    };

    let Some(partition) = block::take(&found, root_type) else {
        kprintln!("  root        : no FreeOS root partition: keeping the initrd as root");
        return;
    };
    let first_lba = partition.first_lba;
    kprintln!(
        "  root        : slot {} on {} #{} at LBA {first_lba}",
        slot.map_or("none", slots::Slot::name),
        partition.source,
        partition.unit,
    );

    // Проверка идёт **до** монтирования и в одиночку — иначе она чинила бы том
    // из-под себя: редактор держит счётчики свободного в памяти, и починка под
    // ним оставила бы его с числами, которых на диске уже нет.
    let mut device = partition.device;
    check_volume("root", &mut device, first_lba, info.check_disk());

    // Корень системы со слотами монтируется **только на чтение**, и это не
    // осторожность, а условие, при котором обновление слотами вообще имеет
    // смысл: система, которая пишет в свой корень, отличается от образа,
    // который в него положили, — и откат к предыдущему слоту перестаёт быть
    // возвратом к известному состоянию. Всё, что пишется, живёт на разделе
    // состояния (ниже).
    //
    // Безопасный режим делает то же самое по другой причине — см. фазу 28b.
    let writable = slot.is_none() && !info.safe_mode();
    let mount = match fs::Ext2Fs::mount(alloc::boxed::Box::new(device), first_lba, writable) {
        Ok(mount) => mount,
        Err(err) => {
            kprintln!("  root        : cannot mount ext2 at LBA {first_lba}: {err}");
            return;
        }
    };

    let (blocks, block_size, groups, requests) = mount.stats();
    kprintln!(
        "  root        : ext2 at LBA {first_lba}, {blocks} blocks of {block_size} B in {groups} group(s)"
    );
    // Состояние тома из суперблока — первое, что стоит сказать о найденном
    // корне. «Грязный» том не мешает загрузке и не должен мешать: система,
    // отказывающаяся включаться после пропажи питания, хуже той, что честно
    // говорит о том, чего не знает.
    if mount.was_clean() {
        kprintln!("  root        : volume was unmounted cleanly");
    } else {
        kprintln!("  root        : volume was NOT unmounted cleanly, counters may be stale");
    }
    if !writable {
        kprintln!("  root        : mounted read-only, nothing will be written to it");
    }
    kprintln!("  root        : replacing the initrd as /, {requests} disk request(s) so far");
    fs::set_root(alloc::boxed::Box::new(mount));

    mount_state(&found, info);
    // Разметка запоминается целиком: подтверждение загрузки и `sysupdate`
    // спросят о ней позже и из другого места.
    slot::remember(&found, slot);
    verify_root();
}

/// Имя файла с описанием служб.
///
/// Не путь: с фазы 39 настройка ищется сначала в `/etc`, а потом в эталоне,
/// приехавшем с образом (см. [`config`]). Именно этот файл и был причиной, по
/// которой механизм понадобился: `/etc` живёт на разделе состояния, обновление
/// до него не дотягивается — и служба, дописанная в новой версии, не
/// запускалась бы ни на одной обновившейся машине.
const SERVICES: &str = "services";

/// Запустить супервизор служб, если в системе есть что запускать.
///
/// # Почему это делает ядро, а не оболочка
///
/// Потому что службы обязаны работать и там, где оболочки нет вовсе: на машине
/// без единого устройства ввода приглашение не запускается (см.
/// [`run_session`]), а сеть, SSH и всё, что придёт следом, работать обязаны —
/// иначе к такой машине нельзя даже подключиться, чтобы это исправить.
///
/// Супервизор объявляется **служебной** задачей, и его дети наследуют это
/// свойство. Без этого первая же служба сломала бы две вещи сразу: оболочка
/// ждёт, пока договорят остальные задачи, прежде чем напечатать приглашение, а
/// `exit` останавливает машину, когда живых задач не осталось.
fn start_services() {
    kprintln!();
    kprintln!("---- services ---------------------------------------------------");

    // Файла нет — значит служб не заказывали. Это обычное состояние живого
    // носителя, а не поломка.
    let Some(source) = config::exists(SERVICES) else {
        kprintln!("  services    : no service file anywhere, nothing to supervise");
        return;
    };
    // Откуда взято, говорится вслух: «служба не запустилась» и «список служб
    // заморожен правкой в /etc» — разные неисправности, и различает их ровно
    // эта строка.
    kprintln!("  services    : described by {}", config::path(SERVICES, source));

    // Супервизор исполняется от root — иначе он не смог бы запустить службу от
    // чужого имени. От чьего имени работает сама служба, решает её описание.
    match user::spawn_with(
        "/bin/init",
        crate::vfs::perm::Credentials::ROOT,
        true,
    ) {
        Ok(id) => kprintln!("  services    : /bin/init started as {id}, reading {SERVICES}"),
        Err(err) => kprintln!("  services    : /bin/init did not start: {err}"),
    }
}

/// Ветки, которые обслуживает раздел состояния.
///
/// Именно эти пять, и ни одной больше. `/etc` — настройки, `/home` — данные
/// человека, `/root` — то же самое для суперпользователя, `/var` — то, что
/// система пишет о себе, `/opt` — пакеты, поставленные поверх системы. Всё
/// остальное принадлежит образу и заменяется вместе с ним.
///
/// Список общий с установщиком: он создаёт эти каталоги и на разделе состояния,
/// и пустыми на корне — точками монтирования. Расхождение между двумя списками
/// выглядело бы как пропавший каталог, поэтому оба короткие и оба на виду.
const STATE_BRANCHES: [&str; 5] = ["/etc", "/home", "/root", "/var", "/opt"];

/// Найти и смонтировать раздел состояния.
///
/// Его отсутствие — не отказ: система, установленная прежним установщиком, живёт
/// одним корнем, и объявлять её сломанной незачем. Сказать об этом надо, потому
/// что дальше `/home` окажется на корне, а не там, где его ищут.
fn mount_state(found: &[block::Partition], info: &BootInfo) {
    use disk::gpt;

    let Some(partition) = block::take(found, gpt::FREEOS_STATE_TYPE) else {
        kprintln!("  state       : no state partition; /etc and /home stay on the root volume");
        return;
    };
    let first_lba = partition.first_lba;
    let mut device = partition.device;
    check_volume("state", &mut device, first_lba, info.check_disk());

    // Раздел состояния — единственный, куда система пишет. В безопасном режиме
    // он тоже открывается только на чтение: смысл режима в том, чтобы у системы
    // было как можно меньше возможностей что-нибудь испортить.
    let writable = !info.safe_mode();
    let mount = match fs::Ext2Fs::mount(alloc::boxed::Box::new(device), first_lba, writable) {
        Ok(mount) => mount,
        Err(err) => {
            kprintln!("  state       : cannot mount ext2 at LBA {first_lba}: {err}");
            kprintln!("  state       : /etc and /home stay on the root volume");
            return;
        }
    };
    let (blocks, block_size, _, _) = mount.stats();
    kprintln!(
        "  state       : ext2 at LBA {first_lba}, {blocks} blocks of {block_size} B"
    );
    if mount.was_clean() {
        kprintln!("  state       : volume was unmounted cleanly");
    } else {
        kprintln!("  state       : volume was NOT unmounted cleanly, counters may be stale");
    }

    // Одна файловая система на пять веток, и **одна ссылка** на неё: клонируется
    // `Arc`, а не том. Пять отдельных объектов поверх одного диска означали бы
    // пять редакторов со своими счётчиками свободного — и пять «разных» томов
    // для всякого, кто их пересчитывает: `fsck` проверял бы один том пятикратно.
    let shared: alloc::sync::Arc<dyn crate::vfs::FileSystem> = alloc::sync::Arc::new(mount);
    for branch in STATE_BRANCHES {
        fs::mount_at(branch, alloc::sync::Arc::clone(&shared));
        kprintln!("  state       : {branch} comes from the state partition");
    }
    if !writable {
        kprintln!("  state       : mounted read-only, nothing will be written to it");
    }
}

/// Сказать вслух, каким режимом грузимся.
///
/// Строка есть всегда, а не только в безопасном режиме. Человек, приславший
/// журнал, не должен доказывать, что грузился обычным образом; а система, не
/// сказавшая о безопасном режиме, объяснила бы «пропавший» рабочий стол
/// поломкой.
fn announce_boot_mode(info: &BootInfo) {
    kprintln!();
    if info.safe_mode() {
        kprintln!("  boot        : safe mode, no desktop, root read-only");
    } else {
        kprintln!("  boot        : normal mode");
    }
    if info.check_disk() {
        kprintln!("  boot        : the root volume will be checked before mounting");
    }
}

/// Проверить корневой том, если прошлый сеанс закрыл его не по-человечески.
///
/// Проверка не запускается на томе, закрытом чисто, и это не экономия: полный
/// обход стоит чтения всех таблиц inode и всех каталогов, то есть секунд на
/// каждой загрузке. Признак чистого размонтирования (фаза 27) существует ровно
/// затем, чтобы знать, когда это оправдано.
fn check_volume(
    what: &str,
    device: &mut dyn disk::BlockDevice,
    first_lba: u64,
    forced: bool,
) {
    let clean = match ext2::Ext2::is_clean(device, first_lba) {
        // Чистый том проверяется только по просьбе из меню загрузчика: цена
        // полного обхода — секунды, и платить их на каждой загрузке незачем.
        Ok(true) if !forced => return,
        Ok(clean) => clean,
        Err(err) => {
            // Суперблок не читается вовсе. Проверка тут бессильна, а
            // монтирование ниже скажет о том же своими словами.
            kprintln!("  fsck        : {what}: cannot read the superblock: {err}");
            return;
        }
    };

    // Причина названа в той же строке, что и сам факт проверки: «почему она
    // идёт» — первый вопрос человека, увидевшего задержку на загрузке, и
    // ответы на него разные.
    if forced && clean {
        kprintln!("  fsck        : checking the {what} volume (asked for in the boot menu)");
    } else if forced {
        kprintln!("  fsck        : checking the {what} volume (asked for, and it is dirty too)");
    } else {
        kprintln!("  fsck        : checking the {what} volume (it was not closed cleanly)");
    }
    let report = match ext2::check(device, first_lba, ext2::Fix::Safe) {
        Ok(report) => report,
        Err(err) => {
            kprintln!("  fsck        : {what}: the check itself failed: {err}");
            return;
        }
    };

    // Печатается не всё: на разбитом вдребезги томе список находок длиннее
    // экрана, а первые несколько всё равно объясняют, что произошло.
    const SHOWN: usize = 8;
    for problem in report.problems.iter().take(SHOWN) {
        kprintln!("  fsck        : {what}: {problem}");
    }
    let hidden = report.problems.len().saturating_sub(SHOWN) + report.dropped;
    if hidden > 0 {
        kprintln!("  fsck        : and {hidden} more");
    }

    if report.is_clean() {
        kprintln!("  fsck        : {what}: nothing to repair, the volume is consistent");
    } else {
        kprintln!(
            "  fsck        : {} problem(s) found, {} repaired, {} file(s) moved to /lost+found",
            report.problems.len() + report.dropped,
            report.fixed,
            report.rescued
        );
    }
    if report.needs_attention() {
        // Честность важнее бодрости: то, что чинится только решением человека,
        // так и остаётся, и система обязана это сказать, а не отчитаться
        // «починено» и замолчать.
        kprintln!("  fsck        : some of it needs a decision and was left alone");
    }
}

/// Показать, что корень действительно читается, и что права на нём настоящие.
///
/// Не украшение вывода: это единственное место, где видно, что путь
/// «virtio-blk → GPT → ext2 → VFS» работает целиком. Файл выбран тот, который
/// записал установщик, — совпадение содержимого доказывает всю цепочку разом.
fn verify_root() {
    /// Имя файла учётных записей; ищется он как всякая настройка (см. [`config`]).
    const PASSWD: &str = "passwd";

    match fs::list("/") {
        Some(Ok(entries)) => {
            for entry in entries {
                kprintln!(
                    "    /{:<12} {:04o} {}:{} {} bytes",
                    entry.name,
                    entry.mode,
                    entry.uid,
                    entry.gid,
                    entry.size
                );
            }
        }
        Some(Err(err)) => kprintln!("  root        : cannot list /: {err}"),
        None => {}
    }

    match config::read(PASSWD, 512) {
        Some((data, source)) => {
            kprintln!(
                "  account     : {}, {} bytes",
                config::path(PASSWD, source),
                data.len()
            );
            // Показывается последняя содержательная строка: первые в файле —
            // комментарии, а интересна сама запись.
            let text = alloc::string::String::from_utf8_lossy(&data);
            if let Some(line) = text.lines().filter(|line| !line.starts_with('#')).next_back() {
                kprintln!("    {line}");
            }
        }
        None => {}
    }
}

fn print_tree(node: &dyn vfs::Node, name: &str, depth: usize) {
    let pad = depth * 2;
    match node.metadata().kind {
        vfs::NodeKind::Directory => {
            kprintln!("  {:pad$}{}/", "", name, pad = pad + 2);
            if depth >= MAX_TREE_DEPTH {
                kprintln!("  {:pad$}... depth limit reached", "", pad = pad + 4);
                return;
            }
            let Ok(entries) = node.list() else {
                kprintln!("  {:pad$}... unreadable", "", pad = pad + 4);
                return;
            };
            for entry in entries {
                match node.lookup(&entry.name) {
                    Ok(child) => print_tree(&*child, &entry.name, depth + 1),
                    Err(err) => kprintln!("  {:pad$}{}: {err}", "", entry.name, pad = pad + 4),
                }
            }
        }
        vfs::NodeKind::File => {
            kprintln!("  {:pad$}{} ({} bytes)", "", name, node.metadata().size, pad = pad + 2);
        }
    }
}

/// Прочитать файл целиком и убедиться, что дочитан именно до конца.
///
/// Файлы в образе заканчиваются известной строкой, поэтому обрыв цепочки
/// кластеров виден сразу и не выглядит как «просто короткий файл».
fn verify_file(fs: &dyn vfs::FileSystem, path: &str) {
    let node = match fs.resolve(path) {
        Ok(node) => node,
        Err(err) => {
            kprintln!("  read        : {path}: {err}");
            return;
        }
    };

    let size = node.metadata().size as usize;
    let mut buf = alloc::vec![0u8; size];
    match node.read_at(0, &mut buf) {
        Ok(read) if read == size => {
            let tail = core::str::from_utf8(&buf[size.saturating_sub(64)..]).unwrap_or("<not utf-8>");
            let marker = tail.lines().rev().find(|line| !line.is_empty()).unwrap_or("");
            kprintln!("  read        : {path}, {read} bytes");
            kprintln!("  last line   : {marker}");
        }
        Ok(read) => kprintln!("  read        : {path}: short read, {read} of {size}"),
        Err(err) => kprintln!("  read        : {path}: {err}"),
    }
}

/// Поднять контроллер прерываний с таймером и убедиться, что тики доходят.
///
/// Установка обработчиков и разрешение прерываний — намеренно два разных шага.
/// Между ними ядро уже способно объяснить отказ, но ещё не может быть прервано:
/// если что-то в настройке контроллера пойдёт не так, диагностика об этом
/// успеет напечататься.
fn start_interrupts() {
    kprintln!();
    kprintln!("---- interrupts -------------------------------------------------");

    arch::interrupts::init();
    arch::interrupts::enable();
    kprintln!("  interrupts  : enabled, timer at {} Hz", irq::TIMER_HZ);

    // Ожидание тиков — единственное прямое доказательство, что прерывания
    // действительно доходят до процессора. Настроенный, но молчащий контроллер
    // выглядит снаружи ровно так же, как работающий.
    let start = irq::ticks();
    let target = start + u64::from(irq::TIMER_HZ) / 2;
    let mut spins: u64 = 0;
    while irq::ticks() < target {
        core::hint::spin_loop();
        spins += 1;
        // Страховка от вечного ожидания: если тики не идут, ядро обязано
        // сказать об этом, а не выглядеть зависшим.
        if spins > 2_000_000_000 {
            kprintln!("  timer       : NO TICKS -- interrupts are not reaching the CPU");
            return;
        }
    }
    kprintln!(
        "  timer       : {} ticks in {} ms of uptime",
        irq::ticks(),
        time::uptime_ms()
    );
}

/// Проверить, что куча действительно работает.
///
/// Аллокации здесь не декоративные: до этого момента ни одна строчка ядра не
/// пользовалась `alloc`, и первая же настоящая аллокация — самый дешёвый способ
/// убедиться, что диапазон кучи отображён на разные физические кадры, а не на
/// один и тот же.
fn verify_heap() {
    let mut values: Vec<u64> = Vec::new();
    for i in 0..1024 {
        values.push(i * i);
    }
    let sum: u64 = values.iter().sum();

    let text = alloc::format!("{} values, checksum {:#x}", values.len(), sum);
    kprintln!("  heap check  : {text}");
}

/// Проверить хэндофф и вернуть **копию** [`BootInfo`].
///
/// Копия, а не ссылка: оригинал лежит в памяти типа
/// [`MemoryKind::BootloaderReclaimable`], которую ядро вправе переиспользовать,
/// как только заберёт из неё всё нужное.
///
/// Возвращает `None`, объяснив причину в serial, если хэндофф невалиден.
fn validate(raw: *const BootInfo) -> Option<BootInfo> {
    if raw.is_null() {
        kprintln!("FATAL: bootloader passed a null BootInfo pointer");
        return None;
    }
    if !raw.is_aligned() {
        kprintln!(
            "FATAL: BootInfo pointer {:#018x} is not {}-byte aligned",
            raw as usize,
            align_of::<BootInfo>()
        );
        return None;
    }

    // Сначала читаются только два скалярных поля, и лишь потом — структура
    // целиком. Причина тонкая: `BootInfo` содержит поля-`enum` (`Arch`,
    // `PixelFormat`), и чтение структуры из мусорной памяти создало бы значение
    // enum вне списка вариантов, то есть UB — ещё до того, как мы успели бы
    // сообщить о несовпадении magic. Скалярные `u64`/`u32` валидны при любом
    // битовом узоре, поэтому их читать безопасно всегда.
    //
    // SAFETY: указатель ненулевой и выровнен; загрузчик обязался передать
    // читаемую память (см. контракт `kernel_main`). `&raw const` не создаёт
    // промежуточной ссылки, поэтому требования к валидности `&BootInfo` не
    // возникает.
    let (magic, revision) = unsafe {
        (ptr::read(&raw const (*raw).magic), ptr::read(&raw const (*raw).revision))
    };

    if magic != BOOT_INFO_MAGIC || revision != BOOT_INFO_REVISION {
        kprintln!("FATAL: BootInfo handoff mismatch at {:#018x}", raw as usize);
        kprintln!("  expected magic {BOOT_INFO_MAGIC:#018x} revision {BOOT_INFO_REVISION}");
        kprintln!("  observed magic {magic:#018x} revision {revision}");
        kprintln!("  bootloader and kernel were built from different boot-info versions");
        return None;
    }

    // SAFETY: magic и revision совпали, значит структуру писал загрузчик,
    // собранный с тем же самым `boot-info`, — а он записывает в enum-поля
    // только объявленные варианты. Дополнительно: `BootInfo: Copy`, поэтому
    // `ptr::read` не создаёт двойного владения.
    let info = unsafe { ptr::read(raw) };

    if info.arch != arch::ARCH_ID {
        kprintln!(
            "WARNING: bootloader reports arch {:?}, kernel was built for {}",
            info.arch,
            arch::ARCH_NAME
        );
    }
    Some(info)
}

/// Баннер: кто стартовал и что именно приехало в `BootInfo`.
fn banner(info: &BootInfo, addr: usize) {
    kprintln!("================================================================");
    // Версия совпадает с той, что стоит в имени файла образа и в метке тома:
    // строка на экране — единственный способ узнать, что за образ подключён к
    // машине, когда имя файла уже не видно. «Phase 8 bring-up» здесь стояло с
    // восьмой фазы и врало все следующие тринадцать — номер фазы держится в
    // README и в истории, а не в баннере.
    kprintln!(" FreeOS {} kernel", VERSION);
    kprintln!(" architecture : {}", arch::ARCH_NAME);
    kprintln!("================================================================");
    kprintln!("BootInfo @ {addr:#018x}");
    kprintln!("  magic       : {:#018x} (valid)", info.magic);
    kprintln!("  revision    : {}", info.revision);

    let fb = &info.framebuffer;
    if fb.is_present() {
        kprintln!(
            "  framebuffer : {}x{} stride {} {:?}",
            fb.width,
            fb.height,
            fb.stride,
            fb.format
        );
        kprintln!("                base {:#018x}, {} KiB", fb.base, fb.size / 1024);
    } else {
        kprintln!("  framebuffer : absent (headless boot, serial only)");
    }

    if info.acpi_rsdp != 0 {
        kprintln!("  ACPI RSDP   : {:#018x}", info.acpi_rsdp);
    } else {
        kprintln!("  ACPI RSDP   : none");
    }
    if info.device_tree != 0 {
        kprintln!("  device tree : {:#018x}", info.device_tree);
    } else {
        kprintln!("  device tree : none");
    }
    kprintln!("  mem regions : {}", info.memory_map.len);
}

/// Зеркало [`boot_info::MemoryRegion`] со всеми полями скалярного типа.
///
/// Нужно ровно по той же причине, что и посегментное чтение в [`validate`]:
/// массив регионов лежит по адресу, пришедшему из-за границы доверия, и
/// восстанавливать из него `MemoryRegion` (у которого поле `kind` — `enum`)
/// значило бы рисковать невалидным дискриминантом. Совпадение раскладки
/// проверяется статически ниже.
#[repr(C)]
#[derive(Clone, Copy)]
struct RawRegion {
    start: u64,
    len: u64,
    kind: u32,
    _reserved: u32,
}

const _: () = assert!(size_of::<RawRegion>() == size_of::<boot_info::MemoryRegion>());
const _: () = assert!(align_of::<RawRegion>() == align_of::<boot_info::MemoryRegion>());

const KIND_USABLE: u32 = MemoryKind::Usable as u32;
const KIND_RESERVED: u32 = MemoryKind::Reserved as u32;
const KIND_ACPI_RECLAIM: u32 = MemoryKind::AcpiReclaimable as u32;
const KIND_ACPI_NVS: u32 = MemoryKind::AcpiNvs as u32;
const KIND_BOOTLOADER: u32 = MemoryKind::BootloaderReclaimable as u32;
const KIND_KERNEL: u32 = MemoryKind::Kernel as u32;
const KIND_FRAMEBUFFER: u32 = MemoryKind::Framebuffer as u32;

fn kind_name(kind: u32) -> &'static str {
    match kind {
        KIND_USABLE => "usable",
        KIND_RESERVED => "reserved",
        KIND_ACPI_RECLAIM => "acpi-reclaim",
        KIND_ACPI_NVS => "acpi-nvs",
        KIND_BOOTLOADER => "boot-reclaim",
        KIND_KERNEL => "kernel",
        KIND_FRAMEBUFFER => "framebuffer",
        _ => "unknown",
    }
}

/// Верхняя граница на число регионов, которые ядро согласно обойти.
///
/// Реальные карты UEFI — это десятки, изредка сотни записей. Если `len`
/// окажется абсурдным (повреждённый хэндофф, переполнение у загрузчика), лучше
/// напечатать усечённую сводку, чем уйти читать чужую память на гигабайты.
const MAX_REGIONS: u64 = 1024;

/// Сколько регионов показать подробно — остальные сворачиваются в счётчик.
const REGIONS_SHOWN: usize = 8;

/// Попадает ли предполагаемый адрес последовательного порта в оперативную
/// память.
///
/// Если да — порта там нет. Прошивка описывает MMIO как `Reserved` либо не
/// описывает вовсе, а всё, из чего ядро потом раздаёт кадры, — это память, и
/// регистров устройства в ней быть не может.
///
/// Стоило это дорого: на VirtualBox 7.2.14 (Apple Silicon) PL011 не существует,
/// а память начинается с `0x08000000` и покрывает `0x09000000`, куда ядро
/// писало каждый печатаемый символ. Порча памяти от вывода диагностики — отказ,
/// который ищут не там, где он происходит.
///
/// Ноль в [`arch::SERIAL_MMIO`] означает «порт не адресуется памятью» (x86-64 с
/// его пространством ввода-вывода) — там проверять нечего.
fn serial_lands_in_ram(map: &MemoryMap) -> bool {
    let probe = arch::SERIAL_MMIO;
    if probe == 0 || map.ptr == 0 || map.len == 0 {
        return false;
    }
    if map.ptr % align_of::<RawRegion>() as u64 != 0 {
        // Карта нечитаема — и отнимать единственный канал диагностики на этом
        // основании нельзя: непригодная карта как раз тот случай, когда о ней
        // надо суметь рассказать.
        return false;
    }

    let count = map.len.min(MAX_REGIONS);
    let base = map.ptr as *const RawRegion;
    for index in 0..count {
        // SAFETY: те же соображения, что и в [`dump_memory_map`]: массив
        // построен загрузчиком, ещё не переиспользован, индекс в пределах
        // заявленной длины, выравнивание проверено выше.
        let region = unsafe { ptr::read(base.add(index as usize)) };
        let holds_memory = matches!(
            region.kind,
            KIND_USABLE | KIND_ACPI_RECLAIM | KIND_ACPI_NVS | KIND_BOOTLOADER | KIND_KERNEL
        );
        if holds_memory && region.start <= probe && probe < region.start.saturating_add(region.len)
        {
            return true;
        }
    }
    false
}

fn dump_memory_map(map: &MemoryMap) {
    kprintln!();
    kprintln!("Memory map:");
    if map.ptr == 0 || map.len == 0 {
        kprintln!("  bootloader passed an empty memory map");
        return;
    }
    if map.ptr % align_of::<RawRegion>() as u64 != 0 {
        kprintln!("  region array at {:#018x} is misaligned, skipping", map.ptr);
        return;
    }

    let count = map.len.min(MAX_REGIONS);
    if count != map.len {
        kprintln!("  len={} looks implausible, inspecting first {}", map.len, count);
    }

    let base = map.ptr as *const RawRegion;
    let mut usable_bytes: u64 = 0;
    let mut total_bytes: u64 = 0;
    let mut usable_regions: u64 = 0;

    for index in 0..count {
        // SAFETY: массив построен загрузчиком, который заявил `len` записей по
        // адресу `ptr`, и лежит в BootloaderReclaimable-памяти, ещё никем не
        // переиспользованной — на Phase 1 ядро вообще ничего не выделяет.
        // Индекс ограничен `count <= len`, выравнивание проверено выше.
        // `read` (а не ссылка на массив) — чтобы не строить `&[MemoryRegion]`
        // с потенциально невалидными дискриминантами `kind`.
        let region = unsafe { ptr::read(base.add(index as usize)) };
        // saturating, а не обычное сложение: длины приходят снаружи, и на
        // повреждённой карте переполнение u64 уронило бы ядро паникой прямо
        // внутри диагностики — то есть ровно там, где она нужнее всего.
        total_bytes = total_bytes.saturating_add(region.len);
        if region.kind == KIND_USABLE {
            usable_bytes = usable_bytes.saturating_add(region.len);
            usable_regions += 1;
        }
        if (index as usize) < REGIONS_SHOWN {
            kprintln!(
                "  [{:02}] {:#014x}-{:#014x} {:>7} KiB  {}",
                index,
                region.start,
                region.start.wrapping_add(region.len),
                region.len / 1024,
                kind_name(region.kind)
            );
        }
    }

    if count as usize > REGIONS_SHOWN {
        kprintln!("  ... {} more regions", count as usize - REGIONS_SHOWN);
    }
    kprintln!(
        "  usable: {} MiB in {} regions; described total: {} MiB",
        usable_bytes / (1024 * 1024),
        usable_regions,
        total_bytes / (1024 * 1024)
    );
}

/// Обработчик паники: рассказать всё, что знаем, и остановиться.
///
/// `kprintln!` сам разбирается, куда писать: в serial всегда, на экран — если
/// консоль уже поднята. Так паника до инициализации фреймбуфера всё равно
/// оказывается видимой.
#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    // Если экран отдан композитору, забираем его обратно и очищаем. Диагностика
    // поверх окон уехала бы туда, где кончился загрузочный лог, и смешалась бы с
    // рамками; а сообщение о панике — последнее, что покажет машина, и важнее,
    // чтобы его было видно, чем сохранить картинку, которая больше не изменится.
    console::reclaim_screen();

    kprintln!();
    kprintln!("*** KERNEL PANIC ***");
    if let Some(location) = info.location() {
        kprintln!("at {}:{}:{}", location.file(), location.line(), location.column());
    }
    kprintln!("{}", info.message());
    arch::halt();
}
