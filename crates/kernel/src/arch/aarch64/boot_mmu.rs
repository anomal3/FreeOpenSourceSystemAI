//! Ранняя трансляция адресов: включить MMU до того, как заработает ядро.
//!
//! # Зачем это вообще
//!
//! Договор Linux передаёт управление с **выключенным** MMU, и в таком виде ядро
//! не поедет ни на шаг. Дело не в удобстве: на памяти без кэша не работают
//! `ldxr`/`stxr` — монитор исключительного доступа их просто не отслеживает.
//! Значит, не работает ни один атомарный обмен, а на них стоит всё: замки
//! (`sync::SpinLock`), счётчики, куча. Первая же попытка что-нибудь напечатать
//! ушла бы в вечный цикл внутри лока — то есть в зависание без единого
//! сообщения.
//!
//! Поэтому здесь строится минимальное тождественное отображение — ровно
//! настолько, чтобы включить MMU и дожить до того момента, когда ядро построит
//! своё настоящее пространство ([`super::paging::build_kernel_address_space`])
//! и переключится на него.
//!
//! # Что здесь можно, а чего нельзя
//!
//! Нельзя ничего, что берёт замок: ни `kprintln!`, ни куча, ни атомарные
//! операции. Всё состояние — статические таблицы в `.bss`, обнулённые входом.
//! Разбор дерева устройств можно: он не выделяет памяти и не синхронизируется.
//!
//! # Раскладка
//!
//! Тождественное отображение первых четырёх гигабайт блоками по 2 МиБ. Четырёх
//! хватает с запасом: у MT676x всё, к чему обращается ранний код, лежит ниже —
//! регистры (0x10000000, 0x11000000, 0x14000000), ОЗУ (с 0x40000000), образ,
//! стек, дерево и кадровый буфер. То, что выше, отобразит уже настоящее
//! пространство ядра, которому доступны все 48 бит.
//!
//! Тип памяти выбирается по дереву, а не по догадке:
//!
//! | что | как отображается | почему |
//! |---|---|---|
//! | узлы `/memory` | Normal, write-back, inner shareable | обычное ОЗУ |
//! | кадровый буфер | Normal **non-cacheable** | панель читает его мимо кэша |
//! | всё остальное | Device-nGnRnE | регистры, спекулятивно читать нельзя |
//!
//! Блоки по 2 МиБ, а не по гигабайту, нужны ровно ради средней строки этой
//! таблицы: кадровый буфер живёт **внутри** ОЗУ, и гигабайтный блок описал бы
//! его тем же типом, что и всю остальную память. Записанное в кэш панель не
//! увидит, а вытесненная позже строка кэша затрёт то, что нарисовано после
//! переключения на таблицы ядра. Два разных типа памяти на один физический
//! адрес архитектура объявляет непредсказуемым поведением, и это как раз тот
//! случай, когда «непредсказуемо» означает «картинка портится через минуту».
//!
//! Non-cacheable здесь ещё и **быстрее** Device: у Device-nGnRnE каждая запись
//! — отдельная посылка на шину без объединения, и заливка экрана на аппарате
//! занимала десятки секунд, которые было видно глазом. Normal-NC разрешает
//! процессору собирать записи в пакеты.

use core::arch::asm;

use fdt::Fdt;

use super::paging::{
    AP_EL1_RW, ATTR_IDX_DEVICE_NGNRNE, ATTR_IDX_NORMAL, ATTR_IDX_NORMAL_NC, DESC_AF,
    DESC_AP_SHIFT, DESC_ATTR_INDX_SHIFT, DESC_PXN, DESC_SH_SHIFT, DESC_TABLE, DESC_UXN,
    DESC_VALID, ENTRIES_PER_TABLE, MAIR_EL1_VALUE, SCTLR_C, SCTLR_I, SCTLR_M, SCTLR_SPAN,
    SH_INNER_SHAREABLE, SH_NON_SHAREABLE, supported_ips, tcr_el1_value,
};

/// Размер блока нижнего уровня этой раскладки.
const BLOCK: u64 = 2 * 1024 * 1024;

/// Сколько гигабайт отображается тождественно. См. заголовок модуля.
const GIB_COVERED: usize = 4;

/// Таблица трансляции: 512 записей, выровнена по странице — этого требует
/// формат дескриптора, где младшие 12 бит адреса таблицы не хранятся вовсе.
#[repr(C, align(4096))]
struct Table([u64; ENTRIES_PER_TABLE]);

impl Table {
    const EMPTY: Self = Self([0; ENTRIES_PER_TABLE]);
}

/// Корень: одна запись на 512 ГиБ. Занята только нулевая.
static mut LEVEL0: Table = Table::EMPTY;
/// Гигабайтные записи. Заняты первые [`GIB_COVERED`].
static mut LEVEL1: Table = Table::EMPTY;
/// Блоки по 2 МиБ, по таблице на гигабайт.
static mut LEVEL2: [Table; GIB_COVERED] = [Table::EMPTY, Table::EMPTY, Table::EMPTY, Table::EMPTY];

/// Область, которую надо описать не так, как соседей.
#[derive(Clone, Copy)]
pub struct Span {
    pub start: u64,
    pub len: u64,
}

impl Span {
    fn covers(&self, address: u64) -> bool {
        self.len != 0 && address >= self.start && address < self.start.saturating_add(self.len)
    }
}

/// Построить тождественное отображение и включить MMU.
///
/// После возврата работают атомарные операции, а значит замки, куча и вывод —
/// то есть всё остальное ядро.
///
/// # Safety
///
/// * MMU обязан быть выключен, а исполнение — идти на EL1 (за это отвечает
///   `head_fdt.S`, который спускается с EL2 сам);
/// * вызывать ровно один раз и до первого обращения к чему-либо, что берёт
///   замок;
/// * `fdt` обязано описывать ту машину, на которой мы исполняемся: по нему
///   решается, какая память обычная, а какая — регистры.
pub unsafe fn enable(fdt: &Fdt<'_>, uncached: Option<Span>) {
    let memory = ram_spans(fdt);
    // SAFETY: ядро в этот момент однопоточно — это первые инструкции после
    // входа, других ядер никто не поднимал, прерывания замаскированы.
    unsafe { build(&memory, uncached) };
    // SAFETY: таблицы построены выше и лежат в `.bss` образа, то есть по
    // адресам, которые сами же и отображены.
    unsafe { activate() };
}

/// Где в этой машине обычная память.
///
/// Читается из узлов `/memory` — их бывает несколько, и брать только первый
/// значило бы объявить половину ОЗУ регистрами устройства. Больше восьми банков
/// не бывает даже у серверов; лишние просто не попадут в раннюю раскладку и
/// будут отображены настоящими таблицами ядра.
fn ram_spans(fdt: &Fdt<'_>) -> [Span; 8] {
    let mut spans = [Span { start: 0, len: 0 }; 8];
    let mut count = 0;

    let (address_cells, size_cells) = root_cells(fdt);
    for node in fdt.nodes() {
        if node.property_str("device_type") != Some("memory") {
            continue;
        }
        for region in node.reg(address_cells, size_cells) {
            if count == spans.len() {
                return spans;
            }
            spans[count] = Span { start: region.address, len: region.size };
            count += 1;
        }
    }
    spans
}

/// Размеры ячеек корня. Если их нет — те, что предписывает формат.
fn root_cells(fdt: &Fdt<'_>) -> (usize, usize) {
    let Some(root) = fdt.nodes().next() else {
        return (2, 1);
    };
    (
        root.property_u64("#address-cells").unwrap_or(2) as usize,
        root.property_u64("#size-cells").unwrap_or(1) as usize,
    )
}

/// Заполнить таблицы.
///
/// # Safety
///
/// Ядро должно быть однопоточным: функция пишет в статические таблицы.
unsafe fn build(memory: &[Span], uncached: Option<Span>) {
    let level1 = (&raw const LEVEL1) as u64;

    // SAFETY: обе таблицы статические и выровнены по странице; исполнение
    // однопоточное.
    unsafe {
        (&raw mut LEVEL0).cast::<u64>().write(level1 | DESC_VALID | DESC_TABLE);
    }

    for gib in 0..GIB_COVERED {
        // SAFETY: индекс меньше длины массива, таблицы статические.
        let level2 = unsafe { (&raw const LEVEL2).cast::<Table>().add(gib) } as u64;
        // SAFETY: см. выше; `gib` меньше числа записей таблицы.
        unsafe {
            (&raw mut LEVEL1).cast::<u64>().add(gib).write(level2 | DESC_VALID | DESC_TABLE);
        }

        for slot in 0..ENTRIES_PER_TABLE {
            let address = (gib as u64) * (ENTRIES_PER_TABLE as u64) * BLOCK + slot as u64 * BLOCK;
            let descriptor = block(address, memory, uncached);
            // SAFETY: `gib` и `slot` в границах обоих массивов.
            unsafe {
                (&raw mut LEVEL2).cast::<Table>().add(gib).cast::<u64>().add(slot).write(descriptor);
            }
        }
    }
}

/// Дескриптор блока на 2 МиБ.
///
/// Блок отличается от страницы одним битом: у страницы бит 1 взведён
/// (`DESC_PAGE`), у блока — нет. Спутать их значит получить таблицу, обход
/// которой уйдёт по адресу блока как по адресу следующего уровня.
fn block(address: u64, memory: &[Span], uncached: Option<Span>) -> u64 {
    let inside_ram = memory.iter().any(|span| span.covers(address));
    let inside_uncached = uncached.is_some_and(|span| {
        // Буфер редко выровнен по двум мегабайтам: годится любое пересечение,
        // иначе половина буфера осталась бы кэшируемой, а это хуже, чем вся.
        span.start < address + BLOCK && address < span.start.saturating_add(span.len)
    });

    let (attribute, shareability, executable) = if inside_uncached {
        (ATTR_IDX_NORMAL_NC, SH_INNER_SHAREABLE, false)
    } else if inside_ram {
        (ATTR_IDX_NORMAL, SH_INNER_SHAREABLE, true)
    } else {
        (ATTR_IDX_DEVICE_NGNRNE, SH_NON_SHAREABLE, false)
    };

    let mut descriptor = address
        | DESC_VALID
        | DESC_AF
        | (attribute << DESC_ATTR_INDX_SHIFT)
        | (AP_EL1_RW << DESC_AP_SHIFT)
        | (shareability << DESC_SH_SHIFT)
        // Из EL0 здесь исполнять нечего: программ ещё нет, а когда появятся, у
        // них будет своё пространство.
        | DESC_UXN;
    if !executable {
        descriptor |= DESC_PXN;
    }
    descriptor
}

/// Записать регистры трансляции и включить MMU.
///
/// # Safety
///
/// Таблицы должны быть построены, а код, стек и всё, к чему он обратится сразу
/// после включения, — отображены тождественно. Иначе следующая инструкция
/// выбирается по адресу, которого больше нет.
unsafe fn activate() {
    let root = (&raw const LEVEL0) as u64;
    // Геометрия — ровно та же, что у настоящих таблиц ядра: 48 бит, страницы
    // 4 КиБ. Иначе переключение на них означало бы смену геометрии и смену
    // дерева одной записью, а промежуточное состояние (новый TCR со старыми
    // TTBR) — это чужая геометрия, применённая к нашему дереву.
    //
    // Обход верхней половины запрещён: ранний код туда не обращается, а
    // `TTBR1_EL1` здесь пуст, и разрешённый обход означал бы чтение таблицы по
    // физическому адресу ноль вместо честного отказа.
    const TCR_EPD1: u64 = 1 << 23;
    let tcr = tcr_el1_value(supported_ips()) | TCR_EPD1;

    // SAFETY: значения собраны выше, таблицы построены. Порядок обязателен:
    // словарь типов памяти и геометрия — до корня, корень — до включения, и
    // каждый шаг отделён барьером, иначе процессор вправе начать обход по
    // наполовину записанным регистрам.
    unsafe {
        asm!(
            // Все записи в таблицы обязаны быть видны обходчику до того, как он
            // начнёт по ним ходить.
            "dsb ish",
            "msr mair_el1, {mair}",
            "msr tcr_el1, {tcr}",
            "msr ttbr0_el1, {root}",
            "msr ttbr1_el1, xzr",
            "isb",
            // Кэш трансляций мог остаться от загрузчика: там чужие таблицы.
            "tlbi vmalle1",
            // И кэш инструкций — по той же причине.
            "ic iallu",
            "dsb nsh",
            "isb",
            mair = in(reg) MAIR_EL1_VALUE,
            tcr = in(reg) tcr,
            root = in(reg) root,
            options(nostack, preserves_flags),
        );
    }

    // SAFETY: чтение `SCTLR_EL1` побочных эффектов не имеет.
    let mut sctlr: u64;
    unsafe {
        asm!("mrs {}, sctlr_el1", out(reg) sctlr, options(nomem, nostack, preserves_flags));
    }
    sctlr |= SCTLR_M | SCTLR_C | SCTLR_I | SCTLR_SPAN;

    // SAFETY: со следующей инструкции адреса транслируются нашими таблицами.
    // Код, стек и всё, что понадобится дальше, отображены тождественно, поэтому
    // адреса не меняются — меняется только их смысл.
    unsafe {
        asm!(
            "msr sctlr_el1, {}",
            "isb",
            in(reg) sctlr,
            options(nostack, preserves_flags),
        );
    }
}
