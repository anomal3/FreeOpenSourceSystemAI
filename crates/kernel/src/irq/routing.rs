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
//! только прошивка, и говорит она это таблицей `_PRT` в AML — или, на машине
//! без ACPI, свойством `interrupt-map` моста в дереве устройств. Без них остаются
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

use core::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};

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

/// Сколько строк `interrupt-map` вели не в GIC и не были взяты.
static TREE_FOREIGN: AtomicUsize = AtomicUsize::new(0);

/// Принять разводку, прочитанную из дерева устройств.
///
/// Зовётся из разбора дерева, пока оно живо, — задолго до [`init`], который
/// только скажет о ней в журнал. У GIC активный уровень у линий PCI высокий:
/// инвертора на входе нет, и сообщить его контроллеру всё равно нечем.
pub fn set_tree_routes(lines: &[(u8, u8, u32, bool)], foreign: usize) {
    let mut routes = [aml::Route { device: 0, pin: 0, gsi: 0, level: true, active_low: false };
        MAX_ROUTES];
    let mut len = 0;
    for &(device, pin, gsi, level) in lines.iter().take(MAX_ROUTES) {
        routes[len] = aml::Route { device, pin, gsi, level, active_low: false };
        len += 1;
    }
    TREE_FOREIGN.store(foreign, Ordering::Relaxed);
    if len > 0 {
        *TABLE.lock() = Some(Table { routes, len });
    }
}

/// Прочитать таблицу маршрутизации из DSDT.
///
/// Отсутствие таблицы — не отказ: машина без `_PRT`, объявленного данными, —
/// это машина, на которой устройства с одним только `INTx` останутся на опросе.
/// Так было до этой фазы со всеми, и так останется с этими; важно, чтобы об
/// этом было сказано вслух, а не выяснялось по молчащему устройству.
pub fn init() {
    let rsdp = acpi::rsdp();
    if rsdp == 0 {
        let foreign = TREE_FOREIGN.load(Ordering::Relaxed);
        if foreign > 0 {
            // Строка, ведущая в чужой контроллер (каскад, приставка), — это
            // линии, которых у нас не будет. Узнать об этом лучше здесь.
            kprintln!("  routing     : {foreign} interrupt-map entr(ies) lead past the GIC and were ignored");
        }
        let routes = TABLE.lock().as_ref().map(|table| (table.routes, table.len));
        match routes {
            Some((routes, len)) => {
                let lines = summarise(&routes[..len]);
                kprintln!("  routing     : {len} PCI route(s) from the device tree's interrupt-map, {lines}");
            }
            None => kprintln!("  routing     : no ACPI tables and no interrupt-map; PCI interrupt lines are unknown"),
        }
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

/// На какую линию выведен вывод `pin` функции по адресу `address`.
///
/// `pin` — от нуля (INTA), как его отдаёт [`crate::pci::Device::interrupt_pin`].
/// Функция за мостом спрашивается с пересчётом (см. [`root_slot`]) — здесь, а не
/// у вызывающего: перепись шины и драйвер обязаны получить **одну** линию, и
/// двух мест, где её считают, быть не должно.
#[must_use]
pub fn line_for(address: crate::pci::Address, pin: u8) -> Option<Line> {
    let (device, pin) = root_slot(address, pin);
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

// --- общая линия -------------------------------------------------------------

/// Сколько разных линий можем обслуживать.
///
/// Линий у шины четыре на сегмент, и все четыре — это уже больше, чем устройств
/// без MSI на любой из наших машин. Восемь взято тем же числом, что у MSI, и по
/// той же причине: таблица перебирается в обработчике, и перебор должен быть
/// заведомо коротким.
const MAX_LINES: usize = 8;

/// Сколько устройств может сидеть на одной линии.
///
/// `INTx` разделяемый по устройству самой шины: четыре вывода на тридцать два
/// устройства, и совпадения неизбежны. Драйвер, написанный так, будто линия его
/// одного, работает ровно до второго устройства на ней.
const MAX_SHARERS: usize = 4;

/// Свободная ячейка линии. Ноль — законный номер линии на некоторых машинах,
/// поэтому «свободно» обозначено заведомо невозможным номером.
const LINE_FREE: u32 = u32::MAX;

static LINE_GSI: [AtomicU32; MAX_LINES] = [const { AtomicU32::new(LINE_FREE) }; MAX_LINES];
static LINE_HANDLERS: [[AtomicUsize; MAX_SHARERS]; MAX_LINES] =
    [const { [const { AtomicUsize::new(0) }; MAX_SHARERS] }; MAX_LINES];

/// Сколько раз линия срабатывала — считая и те разы, когда никто из сидящих на
/// ней не признал сигнал своим.
///
/// Это не то же, что счётчик пробуждений: тот считает признанные сигналы. Два
/// числа расходятся ровно в одном случае, зато в самом опасном — линия поднята
/// причиной, которой никто не снимает. У уровневого прерывания это не «лишний
/// вызов», а машина, занятая одним и тем же сигналом бесконечно.
static LINE_CALLS: [AtomicU64; MAX_LINES] = [const { AtomicU64::new(0) }; MAX_LINES];

/// Переходники: у арх-части обработчик без аргументов, а какая именно линия
/// сработала, знать надо. По одному переходнику на ячейку — единственный способ
/// передать номер, не заводя обработчику аргумент, которого у прерывания нет.
static TRAMPOLINES: [fn(); MAX_LINES] = [
    || fire(0),
    || fire(1),
    || fire(2),
    || fire(3),
    || fire(4),
    || fire(5),
    || fire(6),
    || fire(7),
];

/// Позвать всех, кто сидит на этой линии.
///
/// Зовутся **все**, а не один: разделяемая линия не говорит, кто её поднял.
/// Каждый обработчик обязан посмотреть свой регистр состояния и промолчать,
/// если это не он. Иначе — и это главная ловушка уровневого прерывания —
/// устройство, чей признак никто не снял, будет поднимать линию снова и снова,
/// и машина встанет не от ошибки, а от занятости.
fn fire(line: usize) {
    LINE_CALLS[line].fetch_add(1, Ordering::Relaxed);
    for slot in &LINE_HANDLERS[line] {
        let handler = slot.load(Ordering::Acquire);
        if handler == 0 {
            continue;
        }
        // SAFETY: в таблице лежат только указатели на `fn()`, положенные
        // `request`; ноль означает пустую ячейку и отсеян выше.
        let handler: fn() = unsafe { core::mem::transmute(handler) };
        handler();
    }
}

/// Подписать обработчик на линию устройства PCI.
///
/// Возвращает номер линии, если получилось. `None` означает одно из трёх:
/// устройство не пользуется выводом прерывания, таблица маршрутизации не
/// прочитана, линию не удалось разрешить у контроллера. Во всех трёх случаях
/// драйвер обязан остаться на опросе, а не отказаться работать, — и сказать об
/// этом вслух.
///
/// `handler` вызывается из обработчика прерывания, с запрещёнными прерываниями.
/// От него требуются две вещи: снять признак у **своего** устройства и уйти.
/// Разбор колец и всё, что занимает время, — дело разбуженной задачи.
#[must_use]
pub fn request(device: &crate::pci::Device, handler: fn()) -> Option<u32> {
    let pin = device.interrupt_pin()?;
    let line = line_for(device.address, pin)?;

    // Линия, уже разрешённая у контроллера, второй раз не разрешается: у неё
    // просто прибавляется обработчик. Повторный вызов `route_line` переписал бы
    // вход контроллера на новый вектор, и первый драйвер остался бы с линией,
    // которая больше никуда не ведёт.
    if let Some(slot) = find_line(line.gsi) {
        if !add_handler(slot, handler) {
            return None;
        }
        // SAFETY: обработчик уже стоит в таблице этой линии.
        unsafe { device.enable_intx() };
        return Some(line.gsi);
    }

    for (slot, taken) in LINE_GSI.iter().enumerate() {
        // Обмен, а не проверка с последующей записью: два драйвера могут
        // подниматься одновременно.
        if taken
            .compare_exchange(LINE_FREE, line.gsi, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            // Соседний вызов мог занять ячейку **этой же** линией, пока мы
            // сюда шли. Тогда подписываемся к нему, а не заводим вторую.
            if taken.load(Ordering::Acquire) == line.gsi {
                return add_handler(slot, handler).then_some(line.gsi);
            }
            continue;
        }
        if !add_handler(slot, handler) {
            taken.store(LINE_FREE, Ordering::Release);
            return None;
        }
        // Обработчик стоит до разрешения линии: прерывание может прийти сразу,
        // и пустая ячейка означала бы уровневый сигнал, который некому снять.
        if crate::arch::interrupts::route_line(line.gsi, line.level, line.active_low, TRAMPOLINES[slot])
        {
            // Последним — разрешение самому устройству поднимать линию. Его
            // ставит не драйвер, а мы: бит `Interrupt Disable` выставлен для
            // всех при включении bus master, и снимать его имеет право только
            // тот, кто уже завёл обработчик. Порядок обязателен — устройство
            // вправе поднять линию в ту же секунду.
            //
            // SAFETY: обработчик стоит, вход контроллера размаскирован.
            unsafe { device.enable_intx() };
            return Some(line.gsi);
        }
        LINE_HANDLERS[slot][0].store(0, Ordering::Release);
        taken.store(LINE_FREE, Ordering::Release);
        return None;
    }
    None
}

/// Устройство корневой шины и его вывод, к которым приходит вывод `pin`
/// функции `address`.
///
/// Таблица — и `_PRT`, и `interrupt-map` — описывает только корневую шину. Мост
/// переставляет выводы тех, кто за ним: INTA устройства номер `d` выходит из
/// моста как `(INTA + d) % 4`. Правило из спецификации мостов PCI, одно на
/// всех, и идёт оно вверх до корневой шины, где мост — уже обычное устройство
/// со своей строкой в таблице. Без пересчёта устройство за мостом получило бы
/// линию того, кто на корневой шине носит тот же номер.
///
/// У моста на ACPI бывает собственный `_PRT`; мы его не читаем, и перестановка —
/// то же, что делает Linux, когда `_PRT` у моста нет.
fn root_slot(address: crate::pci::Address, pin: u8) -> (u8, u8) {
    let (mut at, mut pin) = (address, pin);
    // Предел — страховка от испорченной таблицы мостов, а не ожидаемая глубина.
    for _ in 0..16 {
        let Some(bridge) = crate::pci::upstream_bridge(at.bus) else {
            break;
        };
        pin = (pin + at.device) % 4;
        at = bridge;
    }
    (at.device, pin)
}

fn find_line(gsi: u32) -> Option<usize> {
    LINE_GSI.iter().position(|taken| taken.load(Ordering::Acquire) == gsi)
}

fn add_handler(line: usize, handler: fn()) -> bool {
    LINE_HANDLERS[line]
        .iter()
        .any(|slot| slot.compare_exchange(0, handler as usize, Ordering::AcqRel, Ordering::Acquire).is_ok())
}

/// Сколько устройств сидит на каждой заведённой линии — для диагностики.
#[must_use]
pub fn shared_lines() -> alloc::vec::Vec<(u32, usize, u64)> {
    let mut out = alloc::vec::Vec::new();
    for (slot, taken) in LINE_GSI.iter().enumerate() {
        let gsi = taken.load(Ordering::Acquire);
        if gsi == LINE_FREE {
            continue;
        }
        let users = LINE_HANDLERS[slot]
            .iter()
            .filter(|handler| handler.load(Ordering::Acquire) != 0)
            .count();
        out.push((gsi, users, LINE_CALLS[slot].load(Ordering::Relaxed)));
    }
    out
}
