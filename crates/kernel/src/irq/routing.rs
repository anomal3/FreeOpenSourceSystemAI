// Copyright (C) 2026 Виталий Ардашов, Роман Кощеев
// Этот файл является частью FreeOpenSourceSystemAI.
// Программа распространяется на условиях GNU General Public License v3.

//! На какую линию контроллера прерываний выведено устройство PCI.
//!
//! # Зачем это нужно
//!
//! Устройство, объявляющее MSI или MSI-X, ни в какой маршрутизации не
//! нуждается: оно прерывает процессор обычной записью в память, и адрес этой
//! записи назначаем мы сами. Так подключены сеть virtio, диски virtio, AHCI,
//! NVMe и xHCI — всё, что переведено на прерывания в фазе 52.
//!
//! Остальные объявляют только `INTx` — четыре вывода, разведённые по плате к
//! входам контроллера прерываний. Какой вывод к какому входу приходит, знает
//! только прошивка, и говорит она это таблицей `_PRT` в AML. Без неё остаются
//! два пути: опрашивать (как сейчас) или угадать номер линии — а угаданный
//! адрес в этом проекте уже стоил одной поездки к чужой машине.
//!
//! По переписи устройств в QEMU так подключены контроллеры EHCI и OHCI и
//! сетевая карта e1000; на настоящем ноутбуке таких будет больше.
//!
//! # Что здесь есть и чего нет
//!
//! Есть таблица: устройство и вывод — линия. Читается один раз на загрузке,
//! потому что описывает разводку платы, а она не меняется.
//!
//! Нет самой доставки прерывания: разрешить линию у контроллера — дело
//! арх-части, и у двух архитектур оно разное (вход I/O APIC против SPI у GIC).
//! Здесь только ответ на вопрос «какая линия», один на обе.

use crate::sync::SpinLock;
use crate::{acpi, kprintln};

/// Сколько записей маршрутизации помещается.
///
/// Прошивка QEMU для Q35 объявляет ровно 128: тридцать два устройства по четыре
/// вывода. Больше на одной шине не бывает по устройству самой шины, поэтому
/// число не «с запасом», а точное.
const MAX_ROUTES: usize = 128;

/// Куда выведена одна линия.
#[derive(Debug, Clone, Copy)]
pub struct Line {
    /// Номер линии у контроллера прерываний.
    pub gsi: u32,
    /// Срабатывание по уровню, а не по фронту.
    pub level: bool,
    /// Активный уровень — низкий.
    pub active_low: bool,
}

/// Разобранная таблица.
struct Table {
    routes: [aml::Route; MAX_ROUTES],
    len: usize,
}

static TABLE: SpinLock<Option<Table>> = SpinLock::new(None);

/// Прочитать таблицу маршрутизации из DSDT.
///
/// Отсутствие таблицы — не отказ: машина без `_PRT`, объявленного данными, —
/// это машина, на которой устройства с одним только `INTx` останутся на опросе.
/// Так было до этой фазы со всеми, и так останется с этими; важно, чтобы об
/// этом было сказано вслух, а не выяснялось по молчащему устройству.
pub fn init() {
    let rsdp = acpi::rsdp();
    if rsdp == 0 {
        kprintln!("  routing     : no ACPI tables; PCI interrupt lines are unknown");
        return;
    }
    // SAFETY: прямое отображение активно, таблицы ACPI ещё не переиспользованы —
    // то же условие, при котором читаются MADT и FADT рядом.
    let dsdt = match unsafe { acpi::dsdt(rsdp) } {
        Ok(bytes) => bytes,
        Err(err) => {
            kprintln!("  routing     : no DSDT ({err}); PCI interrupt lines are unknown");
            return;
        }
    };

    let mut routes = [aml::Route { device: 0, pin: 0, gsi: 0, level: true, active_low: true };
        MAX_ROUTES];
    match aml::routing(dsdt, &mut routes) {
        Ok((len, dropped)) => {
            if dropped > 0 {
                // Молча обрезанная таблица выглядит как «у этого устройства нет
                // прерывания», и причину пришлось бы искать долго.
                kprintln!("  routing     : {dropped} route(s) did not fit and were ignored");
            }
            let lines = summarise(&routes[..len]);
            kprintln!("  routing     : {len} PCI route(s) from _PRT, {lines}");
            *TABLE.lock() = Some(Table { routes, len });
        }
        Err(err) => {
            kprintln!("  routing     : cannot read _PRT ({err}); PCI interrupt lines are unknown");
        }
    }
}

/// Перечислить линии, на которые ведёт таблица, — одной строкой.
///
/// Именно линии, а не записи: записей 128, а разных линий четыре, и в журнале
/// полезно именно это число. Оно же сразу показывает беду: одна линия на всё —
/// значит прошивка их не развела, и такую таблицу лучше увидеть на загрузке,
/// чем при разборе потерянного прерывания.
fn summarise(routes: &[aml::Route]) -> alloc::string::String {
    let mut seen: alloc::vec::Vec<u32> = alloc::vec::Vec::new();
    for route in routes {
        if !seen.contains(&route.gsi) {
            seen.push(route.gsi);
        }
    }
    seen.sort_unstable();
    let mut out = alloc::format!("{} line(s):", seen.len());
    for gsi in seen.iter().take(8) {
        out.push_str(&alloc::format!(" {gsi}"));
    }
    if seen.len() > 8 {
        out.push_str(" ...");
    }
    out
}

/// На какую линию выведен вывод `pin` устройства номер `device`.
///
/// `pin` — от нуля (INTA), как его отдаёт [`crate::pci::Device::interrupt_pin`].
#[must_use]
pub fn line_for(device: u8, pin: u8) -> Option<Line> {
    let guard = TABLE.lock();
    let table = guard.as_ref()?;
    table.routes[..table.len]
        .iter()
        .find(|route| route.device == device && route.pin == pin)
        .map(|route| Line { gsi: route.gsi, level: route.level, active_low: route.active_low })
}

/// Есть ли таблица вообще — для диагностики.
#[must_use]
pub fn known() -> bool {
    TABLE.lock().is_some()
}
