//! Выключение и перезагрузка машины на x86-64.
//!
//! # Почему это оказалось не одной строчкой
//!
//! Выключить компьютер — операция, которую выполняет не процессор, а чипсет, и
//! команда на неё лежит в таблицах ACPI. Беда в том, что каноническое место
//! этой команды — объект `\_S5` в **DSDT**, а DSDT написан на AML: это
//! байт-код, для чтения которого нужен интерпретатор, а интерпретатор AML —
//! подсистема размером с половину этого ядра.
//!
//! Обходной путь существует и он законный. Начиная с ACPI 5.0 в FADT есть пара
//! регистров `SLEEP_CONTROL_REG`/`SLEEP_STATUS_REG`: в первый пишется тип сна,
//! и никакого AML для этого не нужно. Прошивки, которые их заполняют, — это
//! всё, что рассчитано на «hardware-reduced» платформы, и, к счастью, QEMU в
//! том числе.
//!
//! Если регистров нет, остаётся старый путь: `PM1a_CNT` с типом сна из `\_S5`.
//! Тип оттуда достаётся **разбором байт**, а не исполнением: в потоке AML
//! ищется имя `_S5_`, за ним пакет из четырёх элементов, и первые два — те
//! самые числа. Это ровно тот приём, которым пользуются все ядра, не желающие
//! тащить интерпретатор, и он честно ограничен: пакет, собранный не константами
//! (`Package` со ссылками на методы), так прочитать нельзя. Тогда выключение
//! объявляется недоступным, а не выполняется наугад.
//!
//! # Перезагрузка
//!
//! Тоже из FADT — `RESET_REG` и значение к нему. Там, где его нет, работает
//! исторический путь: импульс в порт `0x64`, тот самый контроллер клавиатуры,
//! который на заре PC умел дёргать линию сброса процессора. Он существует на
//! всём, что называется PC, и это единственная причина, по которой он здесь.

use core::sync::atomic::{AtomicU16, Ordering};

use crate::acpi::{self, read_u16, read_u32, read_u64};
use crate::kprintln;

use super::{inb, inw, outb, outw};

/// Смещения в FADT, все из спецификации ACPI.
const FADT_SCI_INT: usize = 46;
const FADT_SMI_CMD: usize = 48;
const FADT_ACPI_ENABLE: usize = 52;
const FADT_PM1A_EVT_BLK: usize = 56;
const FADT_PM1B_EVT_BLK: usize = 60;
const FADT_PM1A_CNT_BLK: usize = 64;
const FADT_PM1B_CNT_BLK: usize = 68;
const FADT_GPE0_BLK: usize = 80;
const FADT_GPE1_BLK: usize = 84;
const FADT_PM1_EVT_LEN: usize = 88;
const FADT_GPE0_BLK_LEN: usize = 92;
const FADT_GPE1_BLK_LEN: usize = 93;
const FADT_RESET_REG: usize = 116;
const FADT_RESET_VALUE: usize = 128;
const FADT_SLEEP_CONTROL_REG: usize = 244;
const FADT_DSDT64: usize = 140;
const FADT_DSDT32: usize = 40;

/// Обобщённый адрес ACPI: `space, width, offset, size, address`.
const GAS_ADDRESS_SPACE: usize = 0;
const GAS_ADDRESS: usize = 4;

/// Пространство адресов «порты ввода-вывода».
const GAS_SPACE_IO: u8 = 1;
/// Пространство адресов «память».
const GAS_SPACE_MEMORY: u8 = 0;

/// `PM1_CNT.SCI_EN` — «машина в режиме ACPI»: события уходят операционной
/// системе прерыванием, а не прошивке через SMI.
const SCI_EN: u16 = 1 << 0;

/// Кнопка питания в регистрах фиксированных событий: признак в `PM1_STS` и
/// разрешение в `PM1_EN` стоят на одном и том же бите.
const PWRBTN: u16 = 1 << 8;

/// `PM1_CNT.SLP_EN` — «выполнить переход в сон», бит 13.
const SLP_EN: u16 = 1 << 13;
/// Куда в регистре ложится тип сна.
const SLP_TYP_SHIFT: u16 = 10;

/// В `SLEEP_CONTROL_REG` тип сна лежит на три бита выше, а команда — бит 5.
const SLEEP_CONTROL_TYP_SHIFT: u8 = 2;
const SLEEP_CONTROL_ENABLE: u8 = 1 << 5;

/// Как машина умеет выключаться.
enum Method {
    /// ACPI 5.0: один байт в регистр из FADT, без всякого AML.
    SleepControl { address: u64, space: u8, sleep_type: u8 },
    /// Классика: `PM1a_CNT` (и `PM1b_CNT`, если он есть) с типом из `\_S5`.
    Pm1 { pm1a: u16, pm1b: u16, typ_a: u8, typ_b: u8 },
}

/// Выключить машину.
///
/// Возвращается **только** если выключить не удалось: вызывающий обязан
/// сообщить об этом и остановить процессор, а не считать, что дело сделано.
///
/// # Safety
///
/// Вызывать после того, как всё, что нужно сохранить, сохранено: возврата из
/// удавшегося выключения не бывает.
pub unsafe fn power_off(rsdp: u64) {
    // SAFETY: RSDP пришёл от прошивки через hand-off, таблицы отображены
    // прямым отображением — см. `crate::acpi`.
    let Some(method) = (unsafe { find_method(rsdp) }) else {
        kprintln!("  power       : no way to power off found in ACPI tables");
        return;
    };

    match method {
        Method::SleepControl { address, space, sleep_type } => {
            let value = (sleep_type << SLEEP_CONTROL_TYP_SHIFT) | SLEEP_CONTROL_ENABLE;
            kprintln!("  power       : ACPI sleep control register, S5 type {sleep_type}");
            // SAFETY: адрес и пространство прочитаны из FADT; запись в них и
            // есть команда выключения.
            unsafe { write_gas(address, space, value) };
        }
        Method::Pm1 { pm1a, pm1b, typ_a, typ_b } => {
            kprintln!("  power       : ACPI PM1 control, S5 types {typ_a}/{typ_b}");
            // SAFETY: порты прочитаны из FADT.
            unsafe {
                outw(pm1a, (u16::from(typ_a) << SLP_TYP_SHIFT) | SLP_EN);
                if pm1b != 0 {
                    outw(pm1b, (u16::from(typ_b) << SLP_TYP_SHIFT) | SLP_EN);
                }
            }
        }
    }
}

/// Перезагрузить машину.
///
/// Возвращается только если не получилось ни одним из способов.
///
/// # Safety
///
/// См. [`power_off`].
pub unsafe fn reboot(rsdp: u64) {
    // SAFETY: см. `power_off`.
    if let Some(fadt) = unsafe { acpi::find_table(rsdp, b"FACP") }.ok() {
        if fadt.len() > FADT_RESET_VALUE {
            let space = fadt[FADT_RESET_REG + GAS_ADDRESS_SPACE];
            let address = read_u64(fadt, FADT_RESET_REG + GAS_ADDRESS);
            let value = fadt[FADT_RESET_VALUE];
            if address != 0 && (space == GAS_SPACE_IO || space == GAS_SPACE_MEMORY) {
                kprintln!("  power       : ACPI reset register");
                // SAFETY: адрес из FADT.
                unsafe { write_gas(address, space, value) };
            }
        }
    }

    // Запасной путь: импульс сброса через контроллер клавиатуры. Ждать, пока
    // он освободится, обязательно — команда, посланная в занятый контроллер,
    // теряется, и машина просто продолжит работать.
    kprintln!("  power       : falling back to the keyboard controller reset line");
    // SAFETY: порты i8042 существуют на любой машине, называющей себя PC;
    // запись 0xFE дёргает линию сброса.
    unsafe {
        for _ in 0..0x1_0000 {
            if inb(0x64) & 0x02 == 0 {
                break;
            }
        }
        outb(0x64, 0xFE);
    }
}

/// Записать значение в обобщённый адрес ACPI.
///
/// # Safety
///
/// Адрес должен быть получен из таблицы ACPI и описывать регистр, а не память
/// общего назначения.
unsafe fn write_gas(address: u64, space: u8, value: u8) {
    if space == GAS_SPACE_IO {
        // SAFETY: контракт функции.
        unsafe { outb(address as u16, value) };
    } else {
        // SAFETY: контракт функции; регистры ACPI в памяти доступны через
        // прямое отображение, как и остальные таблицы.
        unsafe {
            let ptr = crate::mm::PhysAddr::new(address).to_direct_map().as_usize() as *mut u8;
            ptr.write_volatile(value);
        }
    }
}

/// Найти способ выключения в таблицах.
///
/// # Safety
///
/// См. [`power_off`].
unsafe fn find_method(rsdp: u64) -> Option<Method> {
    // SAFETY: контракт функции.
    let fadt = unsafe { acpi::find_table(rsdp, b"FACP") }.ok()?;

    // Сначала ACPI 5.0: если регистр есть, тип сна для S5 равен пяти по
    // определению, и DSDT читать не нужно вовсе.
    if fadt.len() > FADT_SLEEP_CONTROL_REG + 12 {
        let space = fadt[FADT_SLEEP_CONTROL_REG + GAS_ADDRESS_SPACE];
        let address = read_u64(fadt, FADT_SLEEP_CONTROL_REG + GAS_ADDRESS);
        if address != 0 && (space == GAS_SPACE_IO || space == GAS_SPACE_MEMORY) {
            return Some(Method::SleepControl { address, space, sleep_type: 5 });
        }
    }

    // Иначе — PM1 плюс тип сна из DSDT.
    if fadt.len() <= FADT_PM1B_CNT_BLK + 4 {
        return None;
    }
    let pm1a = u16::try_from(read_u32(fadt, FADT_PM1A_CNT_BLK)).ok()?;
    let pm1b = u16::try_from(read_u32(fadt, FADT_PM1B_CNT_BLK)).unwrap_or(0);
    if pm1a == 0 {
        return None;
    }

    // SAFETY: контракт функции.
    let (typ_a, typ_b) = unsafe { sleep_type_from_dsdt(fadt) }?;
    Some(Method::Pm1 { pm1a, pm1b, typ_a, typ_b })
}

/// Достать типы сна S5 из DSDT, разбирая байты, а не исполняя их.
///
/// В потоке AML ищется имя `_S5_`, за которым идёт `PackageOp` (0x12), длина
/// пакета, число элементов и сами элементы. Нужны первые два, и каждый из них
/// — либо `ZeroOp`/`OneOp` (константы 0 и 1), либо `BytePrefix` (0x0A) с
/// байтом следом. Всё остальное означает пакет, собранный не константами, и
/// тогда честный ответ — «не умею», а не догадка.
///
/// # Safety
///
/// См. [`power_off`].
unsafe fn sleep_type_from_dsdt(fadt: &[u8]) -> Option<(u8, u8)> {
    let address = if fadt.len() > FADT_DSDT64 + 8 && read_u64(fadt, FADT_DSDT64) != 0 {
        read_u64(fadt, FADT_DSDT64)
    } else {
        u64::from(read_u32(fadt, FADT_DSDT32))
    };
    if address == 0 {
        return None;
    }

    // SAFETY: контракт функции; таблица читается тем же способом, что и
    // остальные — через прямое отображение, с проверкой длины из заголовка.
    let dsdt = unsafe { acpi::table_at(address, b"DSDT") }.ok()?;

    let mut at = acpi::SDT_HEADER_LEN;
    while at + 8 < dsdt.len() {
        if &dsdt[at..at + 4] == b"_S5_" {
            let mut cursor = at + 4;
            // Между именем и пакетом стоит `PackageOp`; спецификация допускает
            // здесь `NameOp`, если имя встретилось внутри объявления.
            if dsdt.get(cursor) == Some(&0x12) {
                cursor += 1;
                // Байт длины пакета: старшие два бита говорят, сколько байт
                // занимает сама длина. Нам она не нужна — нужно её пропустить.
                let lead = *dsdt.get(cursor)?;
                cursor += 1 + usize::from(lead >> 6);
                // Число элементов.
                cursor += 1;
                let first = read_aml_byte(dsdt, &mut cursor)?;
                let second = read_aml_byte(dsdt, &mut cursor).unwrap_or(first);
                return Some((first, second));
            }
        }
        at += 1;
    }
    None
}

// --- кнопка питания -----------------------------------------------------------
//
// Кнопка на корпусе — «фиксированное событие» ACPI: единственное событие, о
// котором чипсет умеет сообщать сам, без единой строчки AML. Механика простая:
// в `PM1_EN` разрешается бит `PWRBTN`, чипсет при нажатии выставляет тот же бит
// в `PM1_STS` и дёргает линию SCI, а её номер (`SCI_INT`) записан в FADT.
//
// Два условия, без которых ничего этого не произойдёт:
//
// * **Режим ACPI должен быть включён.** Пока `SCI_EN` сброшен, кнопка идёт в
//   прошивку через SMI, и до нас не доходит вовсе. Включается он записью
//   `ACPI_ENABLE` в порт `SMI_CMD` — и на некоторых машинах не мгновенно,
//   поэтому его ждут, а не предполагают.
// * **Чужие источники SCI должны молчать.** Линия заведена по уровню: событие,
//   которое некому снять, вернётся прерыванием немедленно и навсегда. Всё, что
//   мы снимать не умеем, — то есть GPE, описанные в AML, — поэтому запрещается
//   явно, а не оставляется на усмотрение прошивки.

/// Порты, из которых читается и в которые пишется признак события. Ноль
/// означает «блока нет»: `PM1b` есть далеко не на всякой машине.
///
/// Атомики, а не замок: читает их обработчик прерывания, которому ждать нельзя.
static PM1A_STS_PORT: AtomicU16 = AtomicU16::new(0);
static PM1B_STS_PORT: AtomicU16 = AtomicU16::new(0);

/// Подготовить кнопку питания и вернуть номер входа (GSI), в который она
/// приходит.
///
/// Прерывание после возврата ещё **не** размаскировано — этим занимается тот,
/// кто владеет I/O APIC. Порядок обязателен: разреши мы событие после
/// размаскирования, первое же нажатие пришло бы в вектор, для которого ещё не
/// записаны порты, и обработчик не смог бы снять признак.
///
/// # Safety
///
/// Вызывать один раз, до размаскирования входа, с действующим RSDP.
pub unsafe fn prepare_button(rsdp: u64) -> Option<u32> {
    // SAFETY: контракт функции.
    let fadt = unsafe { acpi::find_table(rsdp, b"FACP") }.ok()?;
    if fadt.len() <= FADT_PM1_EVT_LEN {
        return None;
    }

    let evt_len = usize::from(fadt[FADT_PM1_EVT_LEN]);
    let pm1a_evt = u16::try_from(read_u32(fadt, FADT_PM1A_EVT_BLK)).unwrap_or(0);
    let pm1b_evt = u16::try_from(read_u32(fadt, FADT_PM1B_EVT_BLK)).unwrap_or(0);
    // Блок событий делится пополам: первая половина — признаки, вторая —
    // разрешения. Длина меньше четырёх байт означает блок, в котором такой пары
    // просто нет, — то есть машину, которую мы не понимаем.
    if pm1a_evt == 0 || evt_len < 4 {
        kprintln!("  power       : FADT declares no PM1 event block, no power button");
        return None;
    }
    let half = (evt_len / 2) as u16;

    // SAFETY: порты прочитаны из FADT, режим ACPI включается предписанным
    // спецификацией способом.
    if !unsafe { enable_acpi_mode(fadt) } {
        return None;
    }

    // SAFETY: см. выше.
    unsafe {
        silence_gpes(fadt);

        // Признак сбрасывается **до** разрешения: кнопку могли нажать до нашей
        // загрузки, и прошивка могла оставить признак выставленным. Разреши мы
        // событие первым — машина выключилась бы сразу после загрузки, и
        // причину этого пришлось бы искать долго.
        for (evt, port) in [(pm1a_evt, &PM1A_STS_PORT), (pm1b_evt, &PM1B_STS_PORT)] {
            if evt == 0 {
                continue;
            }
            outw(evt, inw(evt));
            port.store(evt, Ordering::Relaxed);
            let enable = evt + half;
            outw(enable, inw(enable) | PWRBTN);
        }
    }

    Some(u32::from(read_u16(fadt, FADT_SCI_INT)))
}

/// Включить режим ACPI, если он ещё не включён.
///
/// Возвращает `false`, если включить не удалось: молча продолжать нельзя —
/// разрешённое событие в этом случае никуда не придёт, а система будет считать,
/// что кнопка работает.
///
/// # Safety
///
/// `fadt` — настоящая таблица; запись в `SMI_CMD` предписана спецификацией.
unsafe fn enable_acpi_mode(fadt: &[u8]) -> bool {
    let pm1a_cnt = u16::try_from(read_u32(fadt, FADT_PM1A_CNT_BLK)).unwrap_or(0);
    if pm1a_cnt == 0 {
        return false;
    }
    // SAFETY: порт из FADT.
    if unsafe { inw(pm1a_cnt) } & SCI_EN != 0 {
        return true;
    }

    let smi_cmd = read_u32(fadt, FADT_SMI_CMD);
    let acpi_enable = fadt[FADT_ACPI_ENABLE];
    // Ноль в обоих полях — законное «переключать нечего»: так объявляют себя
    // машины, у которых режима SMI нет вовсе. Но `SCI_EN` при этом обязан быть
    // уже взведён, а он не взведён — значит перед нами машина, чью схему
    // событий мы не понимаем.
    let Ok(port) = u16::try_from(smi_cmd) else {
        return false;
    };
    if port == 0 || acpi_enable == 0 {
        kprintln!("  power       : ACPI mode is off and the firmware offers no way to turn it on");
        return false;
    }

    // SAFETY: порт и значение — из FADT, это и есть предписанный способ.
    unsafe { outb(port, acpi_enable) };

    // Переход занимает у чипсета время; спецификация не называет предела, но
    // говорит ждать. Ждём по часам, а не по числу оборотов: цикл, откалиброванный
    // на эмуляторе, на живой машине означал бы совсем другой срок.
    let deadline = crate::time::uptime_ms() + 1000;
    while crate::time::uptime_ms() < deadline {
        // SAFETY: порт из FADT.
        if unsafe { inw(pm1a_cnt) } & SCI_EN != 0 {
            return true;
        }
        core::hint::spin_loop();
    }
    kprintln!("  power       : ACPI mode did not come up, the power button would not arrive");
    false
}

/// Запретить и погасить все GPE.
///
/// Не «на всякий случай»: событие GPE описано в AML, снять его признак
/// правильно мы не умеем, а линия SCI заведена по уровню. Одно разрешённое
/// прошивкой событие превратило бы машину в бесконечный поток прерываний.
///
/// # Safety
///
/// `fadt` — настоящая таблица; порты берутся из неё.
unsafe fn silence_gpes(fadt: &[u8]) {
    if fadt.len() <= FADT_GPE1_BLK_LEN {
        return;
    }
    for (block, len) in [
        (FADT_GPE0_BLK, FADT_GPE0_BLK_LEN),
        (FADT_GPE1_BLK, FADT_GPE1_BLK_LEN),
    ] {
        let Ok(base) = u16::try_from(read_u32(fadt, block)) else {
            continue;
        };
        let bytes = u16::from(fadt[len]);
        if base == 0 || bytes < 2 {
            continue;
        }
        // Блок GPE устроен как блок PM1: половина признаков, половина
        // разрешений. Сначала запрещаем, потом гасим — обратный порядок
        // оставил бы окно, в котором событие успело бы прийти.
        let half = bytes / 2;
        for offset in 0..half {
            // SAFETY: порты из FADT, запись единиц в регистр признаков — это
            // предписанный способ их снять.
            unsafe {
                outb(base + half + offset, 0);
                outb(base + offset, 0xFF);
            }
        }
    }
}

/// Обработчик события ACPI.
///
/// Вызывается из обработчика прерывания, поэтому не делает ничего, кроме двух
/// вещей: снимает признак у чипсета и поднимает просьбу выключиться. Сброс
/// файловой системы и снятие питания — работа задачи (см. [`crate::power`]):
/// здесь она означала бы захват замков в обработчике.
pub fn on_event() {
    let mut pressed = false;
    for port in [&PM1A_STS_PORT, &PM1B_STS_PORT] {
        let port = port.load(Ordering::Relaxed);
        if port == 0 {
            continue;
        }
        // SAFETY: порт записан `prepare_button` из FADT и с тех пор не менялся.
        let status = unsafe { inw(port) };
        if status == 0 {
            continue;
        }
        if status & PWRBTN != 0 {
            pressed = true;
        }
        // Снимаются **все** увиденные признаки, а не только наш. Оставленный
        // чужой признак на линии по уровню — это прерывание, которое повторится
        // сразу же и уже не прекратится.
        // SAFETY: см. выше; запись единицы в бит признака — предписанный способ
        // его снять.
        unsafe { outw(port, status) };
    }

    if pressed {
        crate::power::request(false, crate::power::Source::PowerButton);
    }
}

/// Прочитать одну константу AML: `Zero`, `One` или байт с префиксом.
fn read_aml_byte(bytes: &[u8], cursor: &mut usize) -> Option<u8> {
    match bytes.get(*cursor)? {
        0x00 => {
            *cursor += 1;
            Some(0)
        }
        0x01 => {
            *cursor += 1;
            Some(1)
        }
        0x0A => {
            let value = *bytes.get(*cursor + 1)?;
            *cursor += 2;
            Some(value)
        }
        _ => None,
    }
}
