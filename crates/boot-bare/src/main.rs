//! Образ, который умеет запустить **чужой** загрузчик.
//!
//! # Что это и зачем отдельно от ядра
//!
//! Система до сих пор запускалась только своим загрузчиком: тот читал ELF,
//! применял релокации, включал MMU, собирал [`boot_info::BootInfo`] и входил в
//! ядро. Телефон так не умеет. Заводской загрузчик MediaTek (LK) знает ровно
//! один договор — линуксовый: он читает образ с 64-байтовым заголовком, кладёт
//! его в память, выключает MMU и кэши, кладёт в `x0` адрес дерева устройств и
//! переходит по началу образа.
//!
//! Проверять этот договор сразу всем ядром — дорого и не нужно. Ядро — это
//! куча, замки, атомарные операции; всё это на выключенном MMU либо не
//! работает вовсе (`ldxr`/`stxr` не действуют на памяти без кэша), либо
//! работает так, что отладка превращается в гадание. Поэтому здесь отдельный
//! маленький образ, который отвечает ровно на четыре вопроса:
//!
//! 1. правильный ли у нас заголовок — то есть согласен ли загрузчик нас
//!    запустить вообще;
//! 2. запускает ли он 64-битный код (у аппарата заводская цепочка 32-битная,
//!    и это единственное, что нельзя выяснить рассуждением);
//! 3. лежит ли в `x0` дерево устройств и то ли в нём, что мы думаем;
//! 4. по тому ли адресу кадровый буфер, который загрузчик уже зажёг.
//!
//! Ответ на первые три виден в серийной линии на QEMU; ответ на четвёртый —
//! цветом экрана на самом аппарате, где серийной линии у нас нет.
//!
//! # Почему статическая сборка, а не позиционно-независимая
//!
//! Ядро собирается PIE, и релокации применяет наш загрузчик. Чужому про них
//! неизвестно ничего, значит либо стуб применяет их сам, либо образ линкуется
//! по тому адресу, куда его положат. Здесь верно второе: проверяется вход, а не
//! размещение, и лишняя неизвестная в опыте, у которого нет обратной связи,
//! стоит дороже гибкости. Адрес — `0x40080000`, см. `bare.ld`.

#![no_std]
#![no_main]

use core::arch::global_asm;
use core::panic::PanicInfo;

use fdt::Fdt;

global_asm!(include_str!("head.S"));

/// Точка входа после стуба: MMU выключен, стек поставлен, `.bss` очищен.
///
/// # Safety
///
/// Вызывается только из `head.S`, который обещает всё перечисленное выше.
/// `dtb` — то, что чужой загрузчик положил в `x0`; правдой это не считается и
/// проверяется разбором.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bare_main(dtb: *const u8) -> ! {
    // ВРЕМЕННО: метки хода. R — дошли до Rust, D — указатель выровнен,
    // F — дерево разобрано, U — линия найдена.
    // SAFETY: см. контракт функции.
    let fdt = unsafe { Fdt::from_ptr(dtb) };

    // Порядок именно такой: сначала линия, потом экран. Линия есть на QEMU и
    // нет на телефоне, экран — наоборот; выполняя оба, один и тот же образ
    // отчитывается на любой из двух машин.
    if let Some(fdt) = fdt {
        report(&fdt);
        paint(&fdt);
    } else {
        // Дерева нет или оно не наше. Сказать об этом некуда — адрес линии мы
        // берём из того же дерева. Остаётся единственный признак, видимый
        // снаружи: машина не перезагружается, а стоит.
    }

    halt()
}

/// Рассказать в серийную линию всё, что удалось прочитать.
///
/// Адрес линии берётся из дерева, а не из константы: у QEMU `virt` это PL011 по
/// `0x09000000`, у MT676x — 16550-подобный по `0x11002000`, и вбитый в код
/// адрес означал бы образ, который печатает ровно на одной из двух машин. Тот
/// самый «угаданный адрес», на котором система уже спотыкалась на чужом
/// гипервизоре.
fn report(fdt: &Fdt<'_>) {
    let Some(uart) = Uart::from_fdt(fdt) else {
        return;
    };
    uart.puts("\r\nFreeOS bare image: the bootloader started us.\r\n");

    if let Some(root) = fdt.nodes().next() {
        if let Some(model) = root.property_str("model") {
            uart.puts("  machine   : ");
            uart.puts(model);
            uart.puts("\r\n");
        }
    }

    // Память: сколько её и с какого адреса. Это первое, что перестаёт сходиться,
    // когда дерево читают неверно, и первое, что стоит увидеть глазами.
    let (address_cells, size_cells) = root_cells(fdt);
    let mut total = 0u64;
    for node in fdt.nodes() {
        if node.property_str("device_type") != Some("memory") {
            continue;
        }
        for region in node.reg(address_cells, size_cells) {
            uart.puts("  memory    : ");
            uart.hex(region.address);
            uart.puts(" + ");
            uart.hex(region.size);
            uart.puts("\r\n");
            total += region.size;
        }
    }
    uart.puts("  memory    : ");
    uart.dec(total / (1024 * 1024));
    uart.puts(" MiB in total\r\n");

    match framebuffer(fdt) {
        Some(base) => {
            uart.puts("  screen    : framebuffer at ");
            uart.hex(base);
            uart.puts("\r\n");
        }
        // На QEMU `virt` кадрового буфера нет вовсе, и это не поломка: там
        // отчитывается линия. Сообщение нужно, чтобы на телефоне отличить «не
        // нашли адрес» от «нашли, но экран не изменился».
        None => uart.puts("  screen    : no framebuffer in /chosen\r\n"),
    }
}

/// Залить экран цветом — единственный отчёт, доступный на телефоне.
///
/// Заливка, а не надпись: шрифт потребовал бы знать формат точки и шаг строки,
/// а они в дереве не описаны. Ровный цвет во весь экран виден и при неверном
/// шаге, и при перепутанном порядке каналов — а именно это и надо выяснить
/// первым опытом.
fn paint(fdt: &Fdt<'_>) {
    let Some(base) = framebuffer(fdt) else {
        return;
    };
    let Some(bytes) = vram(fdt) else {
        return;
    };
    // Цвет выбран так, чтобы его нельзя было спутать ни с заставкой
    // загрузчика, ни с чёрным экраном, ни с погасшей панелью: насыщенный
    // зелёный не встречается ни в одном из этих состояний. Порядок каналов
    // здесь неизвестен, и это не важно — при любом из двух получится либо
    // зелёный, либо синий, но никак не чёрный.
    const COLOUR: u32 = 0x0000_FF00;
    let pixels = (bytes / 4) as usize;
    let buffer = base as *mut u32;
    for index in 0..pixels {
        // SAFETY: адрес и объём взяты из дерева, которое составил загрузчик,
        // уже показавший на этой памяти свою заставку. Писать в неё безопасно
        // ровно в той мере, в какой можно верить дереву, — а других источников
        // об этой машине у нас нет.
        unsafe { buffer.add(index).write_volatile(COLOUR) };
    }
}

/// Физический адрес кадрового буфера из `/chosen`.
///
/// Читаются оба способа, которыми LK его передаёт: новый — двумя половинами по
/// 32 бита, старый — одной структурой. Версия загрузчика у аппарата своя, а
/// разница видна только на нём, и приехать на телефон, умея читать один из
/// двух, значит потратить попытку на выяснение того, какой именно.
fn framebuffer(fdt: &Fdt<'_>) -> Option<u64> {
    let chosen = fdt.find("/chosen")?;
    // Панель, которую загрузчик не нашёл, ничего не сканирует: писать по
    // адресу можно сколько угодно, на экране не появится ничего.
    if chosen.property_u64("atag,videolfb-islcmfound") == Some(0) {
        return None;
    }
    if let (Some(high), Some(low)) = (
        chosen.property_u64("atag,videolfb-fb_base_h"),
        chosen.property_u64("atag,videolfb-fb_base_l"),
    ) {
        let base = (high << 32) | (low & 0xffff_ffff);
        return (base != 0).then_some(base);
    }
    let blob = chosen.property("atag,videolfb")?;
    if blob.len() < 20 {
        return None;
    }
    let base = u64::from_be_bytes(blob[0..8].try_into().ok()?);
    (base != 0).then_some(base)
}

/// Сколько байт видеопамяти обещал загрузчик.
fn vram(fdt: &Fdt<'_>) -> Option<u64> {
    let chosen = fdt.find("/chosen")?;
    if let Some(size) = chosen.property_u64("atag,videolfb-vramSize") {
        return (size != 0).then_some(size);
    }
    let blob = chosen.property("atag,videolfb")?;
    if blob.len() < 20 {
        return None;
    }
    let size = u64::from(u32::from_be_bytes(blob[16..20].try_into().ok()?));
    (size != 0).then_some(size)
}

/// Размеры ячеек корня — ими читаются адреса во всём дереве.
fn root_cells(fdt: &Fdt<'_>) -> (usize, usize) {
    match fdt.nodes().next() {
        Some(root) => (
            root.property_u64("#address-cells").unwrap_or(2) as usize,
            root.property_u64("#size-cells").unwrap_or(1) as usize,
        ),
        None => (2, 1),
    }
}

/// Серийная линия, найденная в дереве.
///
/// Два вида регистров, потому что машин две. У PL011 данные лежат по смещению
/// ноль, а «готов принять» — бит 5 регистра флагов по `0x18`, взведённый
/// означает «занят». У 16550 данные тоже по нулю, а «готов» — бит 5 регистра
/// состояния по `0x14`, но взведённый означает **обратное**: свободен. Перепутать
/// их — значит либо печатать в занятый порт и терять байты, либо ждать вечно.
struct Uart {
    base: *mut u8,
    kind: Kind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Pl011,
    Ns16550,
}

impl Uart {
    /// Найти линию: сначала ту, что загрузчик назвал своей, потом любую знакомую.
    ///
    /// `stdout-path` — это выбор загрузчика, и уважать его важнее, чем найти
    /// первый попавшийся порт: у машины их несколько, и наружу выведен обычно
    /// один.
    fn from_fdt(fdt: &Fdt<'_>) -> Option<Self> {
        let node = Self::from_stdout(fdt).or_else(|| {
            fdt.find_compatible("arm,pl011")
                .or_else(|| fdt.find_compatible("mediatek,mt6577-uart"))
                .or_else(|| fdt.find_compatible("ns16550a"))
        })?;
        let kind = if node.is_compatible("arm,pl011") {
            Kind::Pl011
        } else {
            Kind::Ns16550
        };
        let (address_cells, size_cells) = root_cells(fdt);
        let region = node.reg(address_cells, size_cells).next()?;
        (region.address != 0).then(|| Self { base: region.address as *mut u8, kind })
    }

    /// Узел, названный в `/chosen/stdout-path`.
    fn from_stdout<'a>(fdt: &Fdt<'a>) -> Option<fdt::Node<'a>> {
        let path = fdt.find("/chosen")?.property_str("stdout-path")?;
        // Путь бывает с параметрами линии через двоеточие: `/pl011@9000000:115200n8`.
        let path = path.split(':').next().unwrap_or(path);
        fdt.find(path)
    }

    fn putc(&self, byte: u8) {
        // SAFETY: адрес взят из дерева машины; запись в регистр устройства
        // обязана быть volatile — компилятор вправе выбросить или объединить
        // обычную, и тогда в линию уйдёт часть строки или ничего.
        unsafe {
            for _ in 0..100_000 {
                let ready = match self.kind {
                    // У PL011 бит 5 регистра флагов — «передатчик полон».
                    Kind::Pl011 => self.base.add(0x18).read_volatile() & (1 << 5) == 0,
                    // У 16550 бит 5 регистра состояния — «передатчик пуст».
                    Kind::Ns16550 => self.base.add(0x14).read_volatile() & (1 << 5) != 0,
                };
                if ready {
                    break;
                }
            }
            self.base.write_volatile(byte);
        }
    }

    fn puts(&self, text: &str) {
        for byte in text.bytes() {
            self.putc(byte);
        }
    }

    /// Число шестнадцатеричным, с приставкой. Без выделения памяти — её нет.
    fn hex(&self, mut value: u64) {
        self.puts("0x");
        let mut digits = [0u8; 16];
        for slot in digits.iter_mut().rev() {
            *slot = b"0123456789abcdef"[(value & 0xf) as usize];
            value >>= 4;
        }
        // Ведущие нули убираются, но не все: `0x0` обязан остаться числом.
        let first = digits.iter().position(|d| *d != b'0').unwrap_or(15);
        for digit in &digits[first..] {
            self.putc(*digit);
        }
    }

    /// Число десятичным.
    fn dec(&self, mut value: u64) {
        if value == 0 {
            self.putc(b'0');
            return;
        }
        let mut digits = [0u8; 20];
        let mut len = 0;
        while value > 0 {
            digits[len] = b'0' + (value % 10) as u8;
            value /= 10;
            len += 1;
        }
        for index in (0..len).rev() {
            self.putc(digits[index]);
        }
    }
}

/// Остановиться навсегда.
///
/// Именно остановиться, а не перезагрузиться: машина, вставшая после нашего
/// кода, — это отчёт. Сторожевой таймер телефона всё равно перезапустит её
/// через несколько секунд, и по тому, успел ли экран стать зелёным, видно,
/// дошли мы до заливки или нет.
fn halt() -> ! {
    loop {
        // SAFETY: `wfe` не трогает памяти и не меняет состояния, кроме
        // ожидания события.
        unsafe { core::arch::asm!("wfe", options(nomem, nostack)) };
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    halt()
}
