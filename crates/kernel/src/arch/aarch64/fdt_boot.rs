//! Описание машины из дерева устройств — то, что на UEFI-машине даёт загрузчик.
//!
//! # Зачем
//!
//! На всех машинах, где система работала до сих пор, ядро входило из **нашего**
//! загрузчика и получало готовый [`BootInfo`]: карту памяти, кадровый буфер,
//! адрес таблиц ACPI. Телефон устроен иначе. Заводской загрузчик (LK у
//! MediaTek) входит в ядро по договору Linux — MMU выключен, в `x0` лежит
//! адрес дерева устройств, — и никакого `BootInfo` не существует. Этот модуль
//! составляет его сам, читая то же дерево.
//!
//! Смысл именно в том, чтобы `BootInfo` собрался: всё остальное ядро — учёт
//! кадров, куча, стол, драйверы — не должно знать, откуда взялось описание
//! машины. Ветка «а если мы на телефоне» внутри распределителя памяти была бы
//! началом второй системы внутри первой.
//!
//! # Откуда что берётся
//!
//! * **память** — узлы `/memory` (их бывает несколько);
//! * **занятое** — `/reserved-memory` и та область, куда загрузчик положил само
//!   дерево: затереть её означало бы затереть описание машины по ходу чтения;
//! * **кадровый буфер** — узел `/chosen`, свойства `atag,videolfb-*`, которые
//!   кладёт туда LK.
//!
//! # Чего здесь нет и почему
//!
//! Ни одного выделения памяти. Этот код работает **до** того, как ядро узнает,
//! сколько в машине памяти, — то есть до кучи и до распределителя кадров.
//! Поэтому карта регионов лежит в статическом массиве, а его длина — предел,
//! после которого лишние области просто не попадут в карту. Предел назван
//! вслух: молча потерянная область памяти выглядит как «машина видит вдвое
//! меньше ОЗУ», и искать это будут в распределителе.

// Разбор дерева нужен только там, где машина описана им, — то есть на входе по
// договору Linux. На UEFI-сборке модуль компилируется, но не вызывается, и это
// намеренно: разбор проверяется отдельно от входа, потому что ошибки у них
// разные и чинятся по-разному.
#![allow(dead_code)]

use boot_info::{
    Arch, BootInfo, Framebuffer, KernelImage, MemoryKind, MemoryMap, MemoryRegion, PixelFormat,
};
use fdt::Fdt;

/// Сколько областей памяти помещается в карту.
///
/// Считать надо не банки, а куски, на которые они разрезаны: каждая занятая
/// область делит свободную надвое. У MT6765 один банк ОЗУ и с десяток
/// зарезервированных кусков, то есть около тридцати записей; девяносто шесть —
/// троекратный запас. Предел существует потому, что памяти под массив взять
/// негде: он в `.bss`, и вырасти по ходу не может.
const MAX_REGIONS: usize = 96;

/// Карта памяти. Статическая по той же причине, по которой нет `Vec`.
static mut REGIONS: [MemoryRegion; MAX_REGIONS] =
    [MemoryRegion::new(0, 0, MemoryKind::Reserved); MAX_REGIONS];

/// Само описание машины. Ядро получает на него ссылку и живёт с ней весь сеанс.
///
/// Собирается [`BootInfo::new`], а не литералом: у структуры есть закрытое поле
/// выравнивания, и это правильно — договор между загрузчиком и ядром обязан
/// иметь ровно одну точку, где он создаётся.
static mut INFO: BootInfo = BootInfo::new(Arch::AArch64);

/// Размер экрана, если дерево о нём молчит.
///
/// Молчит оно всегда: LK кладёт в `/chosen` адрес буфера, его объём и имя
/// панели — но **не** ширину с высотой. Их знает драйвер панели, вкомпилированный
/// в сам LK, и наружу они не выходят. Поэтому геометрия приходит снаружи, а не
/// угадывается: значение по умолчанию — это экран того аппарата, на котором
/// система запускается первой (`dandelion`, 720×1600), и оно обязано быть
/// названо здесь, а не подобрано в трёх местах по-разному.
pub const DEFAULT_SCREEN: (u32, u32) = (720, 1600);

/// Собрать описание машины из дерева, лежащего по адресу `dtb`.
///
/// `screen` — кадровый буфер, если вызывающий узнал его сам. На MediaTek это
/// именно так: адрес спрашивается у контроллера дисплея, а не берётся из
/// `/chosen`, где загрузчик называет память, которую он **резервирует**, а не
/// ту, которую показывает (см. [`super::mtk::scanned_buffer`]). `None` означает
/// «поищи в дереве» — путь для машин, где загрузчик честен.
///
/// `image` — где лежит сам работающий код. Без этого распределитель кадров
/// выдал бы страницу ядра под первую же аллокацию: узлы `/memory` описывают всё
/// ОЗУ свободным, включая тот кусок, из которого ядро исполняется.
///
/// Возвращает `None`, если по адресу не дерево или в нём нет памяти: без карты
/// памяти ядру нечего делать, и лучше остановиться здесь, чем в распределителе
/// кадров, где причина будет не видна.
///
/// # Safety
///
/// `dtb` — адрес, полученный от загрузчика в `x0`. Он обязан указывать на
/// отображённую память; всё остальное проверяется разбором.
pub unsafe fn describe(
    dtb: *const u8,
    screen: Option<Framebuffer>,
    image: KernelImage,
) -> Option<&'static BootInfo> {
    // SAFETY: контракт функции.
    let fdt = unsafe { Fdt::from_ptr(dtb) }?;

    let framebuffer = match screen {
        Some(given) => given,
        None => framebuffer(&fdt, DEFAULT_SCREEN),
    };

    // Сначала — всё занятое, до последнего куска, и только потом свободное.
    //
    // # Почему не «позже уточняет раньше»
    //
    // Соблазнительно выписать банки ОЗУ свободными, а поверх них занятые куски,
    // и считать, что читатель разберётся. Читатель не разбирается:
    // распределитель кадров (`mm::frame`) раздаёт нули по регионам `Usable` и
    // **не** проходит по карте второй раз, вычёркивая занятое. Карте UEFI такой
    // проход и не нужен — она разбиение, а не набор наложенных прямоугольников,
    // и один физический адрес описан в ней ровно однажды.
    //
    // Это стоило захода: карта выглядела правильной, ядро печатало «reserved»
    // про собственный образ — и тут же выдавало его страницы под таблицы,
    // затирая себя на ходу. Поэтому здесь свободное **вырезается** вокруг
    // занятого, а не покрывается им.
    let mut taken = [Span::EMPTY; MAX_TAKEN];
    let mut taken_count = 0;

    taken_count = reserved_spans(&fdt, &mut taken, taken_count);
    // Само дерево: затереть его означало бы затереть описание машины по ходу
    // чтения.
    taken_count = add_span(
        &mut taken,
        taken_count,
        dtb as u64,
        blob_len(&fdt),
        MemoryKind::BootloaderReclaimable,
    );
    // Образ: он лежит в обычном ОЗУ и попадает в `/memory` свободным.
    taken_count = add_span(&mut taken, taken_count, image.base, image.size, MemoryKind::Reserved);
    // Кадровый буфер: отданный под кучу, он выглядит как цветной мусор поверх
    // интерфейса — и появляется не сразу, а когда куче понадобится расти.
    taken_count = add_span(
        &mut taken,
        taken_count,
        framebuffer.base,
        framebuffer.size,
        MemoryKind::Framebuffer,
    );
    // RAM-диск из загрузочного образа — наш initrd с `/bin` и `/usr`. Занят он
    // на весь сеанс, как и на UEFI-машине (`MemoryKind::Reserved`): файловая
    // система читает его до выключения.
    let initrd = initrd(&fdt);
    taken_count = add_span(&mut taken, taken_count, initrd.base, initrd.size, MemoryKind::Reserved);

    sort_spans(&mut taken[..taken_count]);

    let mut count = 0;
    for span in &taken[..taken_count] {
        count = mark(span.start, span.len(), span.kind, count);
    }
    count = collect_memory(&fdt, &taken[..taken_count], count);

    if count == 0 {
        return None;
    }

    // SAFETY: ядро однопоточно в этот момент — это первые инструкции после
    // входа, других ядер процессора ещё никто не поднимал, а прерывания
    // запрещены.
    unsafe {
        let info = &mut *(&raw mut INFO);
        info.framebuffer = framebuffer;
        info.initrd = initrd;
        info.device_tree = dtb as u64;
        info.kernel = image;
        info.memory_map = MemoryMap {
            ptr: (&raw const REGIONS) as u64,
            len: count as u64,
        };
        Some(&*(&raw const INFO))
    }
}

/// Записать область в карту. Возвращает новую длину.
///
/// Переполнение карты — не ошибка и не паника: система с потерянной областью
/// памяти работает, просто меньшей. Но молчать об этом нельзя, и поэтому здесь
/// стоит строка в журнал — единственное место модуля, которое печатает.
fn mark(start: u64, len: u64, kind: MemoryKind, count: usize) -> usize {
    if len == 0 {
        return count;
    }
    if count == MAX_REGIONS {
        crate::kprintln!("  fdt         : memory map is full, dropping {start:#x}+{len:#x}");
        return count;
    }
    // SAFETY: `count` меньше длины массива — проверено строкой выше; ядро в
    // этот момент однопоточно.
    unsafe {
        (&raw mut REGIONS).cast::<MemoryRegion>().add(count).write(MemoryRegion::new(
            start, len, kind,
        ));
    }
    count + 1
}

/// Банки ОЗУ из узлов `/memory`, за вычетом всего занятого.
///
/// Узлов бывает несколько, и это не редкость: у машин с раздельными банками
/// каждый описан своим. Брать только первый значило бы увидеть половину памяти
/// — то есть работающую систему, у которой необъяснимо мало ОЗУ.
///
/// `taken` обязан быть отсортирован по началу: банк режется одним проходом
/// слева направо, а такой проход возможен только по упорядоченному списку.
fn collect_memory(fdt: &Fdt<'_>, taken: &[Span], mut count: usize) -> usize {
    let (address_cells, size_cells) = root_cells(fdt);
    for node in fdt.nodes() {
        if node.property_str("device_type") != Some("memory") {
            continue;
        }
        for region in node.reg(address_cells, size_cells) {
            let mut start = region.address;
            let end = region.address.saturating_add(region.size);
            for span in taken {
                if span.end <= start {
                    continue;
                }
                if span.start >= end {
                    break;
                }
                if span.start > start {
                    count = mark(start, span.start - start, MemoryKind::Usable, count);
                }
                start = start.max(span.end);
            }
            if start < end {
                count = mark(start, end - start, MemoryKind::Usable, count);
            }
        }
    }
    count
}

/// Занятый кусок физической памяти.
#[derive(Clone, Copy)]
struct Span {
    start: u64,
    end: u64,
    kind: MemoryKind,
}

impl Span {
    const EMPTY: Self = Self { start: 0, end: 0, kind: MemoryKind::Reserved };

    fn len(&self) -> u64 {
        self.end - self.start
    }
}

/// Сколько занятых кусков помещается.
///
/// У телефона их с десяток: модем, доверенная среда, кадровый буфер, само
/// дерево, образ ядра. Двадцать четыре — вдвое больше, чем видно на аппарате.
const MAX_TAKEN: usize = 24;

/// Добавить кусок, округлив его наружу до целых страниц.
///
/// Округление именно наружу: полстраницы, оставшейся «свободной» внутри чужой
/// области, хватит, чтобы распределитель выдал её целиком — страница неделима.
fn add_span(
    taken: &mut [Span; MAX_TAKEN],
    count: usize,
    start: u64,
    len: u64,
    kind: MemoryKind,
) -> usize {
    if len == 0 {
        return count;
    }
    if count == MAX_TAKEN {
        crate::kprintln!("  fdt         : too many reserved areas, dropping {start:#x}+{len:#x}");
        return count;
    }
    const PAGE: u64 = 4096;
    taken[count] = Span {
        start: start & !(PAGE - 1),
        end: start.saturating_add(len).next_multiple_of(PAGE),
        kind,
    };
    count + 1
}

/// Упорядочить по началу. Вставками: список короткий, а сортировки без
/// выделения памяти в `core` нет.
fn sort_spans(spans: &mut [Span]) {
    for index in 1..spans.len() {
        let mut slot = index;
        while slot > 0 && spans[slot - 1].start > spans[slot].start {
            spans.swap(slot - 1, slot);
            slot -= 1;
        }
    }
}

/// Где загрузчик оставил RAM-диск: `/chosen`, `linux,initrd-start` и `-end`.
///
/// Это договор Linux, и LK его соблюдает: RAM-диск из загрузочного образа он
/// кладёт по адресу из заголовка и называет границы в дереве. Раньше там лежал
/// заводской диск Android, которым ядро не пользовалось; теперь `cargo xtask
/// phone --ramdisk` кладёт туда наш initrd, и телефон получает те же `/bin` и
/// `/usr/share`, что машина с UEFI.
///
/// Диск без загрузочного сектора FAT не принимается: initrd — том FAT32
/// (`xtask/src/initrd.rs`), а заводской образ по этому адресу — сжатый cpio, и
/// смонтировать его значило бы получить отказ позже и невнятно. Отказ отдаётся
/// как «нет диска», и ядро грузится без файловой системы, как грузилось до сих
/// пор.
///
/// Длина берётся у загрузчика и округляется вниз до сектора. Образ для телефона
/// обрезан по последнему занятому блоку (`cargo xtask phone --ramdisk`,
/// `trim_volume`): свободные кластеры драйвер не читает, а целый том в 40 МиБ
/// не помещается в раздел recovery до блока подписи.
fn initrd(fdt: &Fdt<'_>) -> boot_info::Initrd {
    let none = boot_info::Initrd::NONE;
    let Some(chosen) = fdt.find("/chosen") else {
        return none;
    };
    let (Some(start), Some(end)) =
        (chosen.property_u64("linux,initrd-start"), chosen.property_u64("linux,initrd-end"))
    else {
        return none;
    };
    const SECTOR: u64 = 512;
    let size = end.saturating_sub(start) / SECTOR * SECTOR;
    if size == 0 || start % 8 != 0 {
        return none;
    }
    // SAFETY: MMU в этот момент отображает ОЗУ один в один (`boot_mmu`), а
    // границы — от загрузчика и лежат внутри банка памяти, куда он образ и
    // положил. Читается первый сектор, а размер не меньше сектора.
    let signature = unsafe { core::ptr::read_unaligned((start + 510) as *const [u8; 2]) };
    if signature != [0x55, 0xaa] {
        return none;
    }
    boot_info::Initrd { base: start, size }
}

/// Куски, которые занял кто-то до нас: `/reserved-memory`.
///
/// На телефоне их много и они не украшение: там живут модем, доверенная среда и
/// сам кадровый буфер. Отдать такой кусок распределителю — это перезаписать
/// чужую память, и проявится оно не сразу и не там.
fn reserved_spans(fdt: &Fdt<'_>, taken: &mut [Span; MAX_TAKEN], mut count: usize) -> usize {
    let Some(parent) = fdt.find("/reserved-memory") else {
        return count;
    };
    // Размеры ячеек берутся у самого `/reserved-memory`, а не у корня: узел
    // вправе объявить свои, и обычно объявляет те же — но «обычно» здесь стоит
    // неверного адреса.
    let address_cells = parent.property_u64("#address-cells").unwrap_or(2) as usize;
    let size_cells = parent.property_u64("#size-cells").unwrap_or(2) as usize;

    for node in fdt.nodes().filter(|node| node.depth == parent.depth + 1) {
        for region in node.reg(address_cells, size_cells) {
            count = add_span(taken, count, region.address, region.size, MemoryKind::Reserved);
        }
    }
    count
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

/// Кадровый буфер, который загрузчик уже зажёг.
///
/// # Почему именно так, а не через драйвер панели
///
/// Панель телефона — это MIPI DSI: чтобы зажечь её самим, нужен драйвер
/// контроллера дисплея, драйвер шины, тайминги конкретной матрицы и её
/// последовательность включения. Всё это уже сделал загрузчик, показывая
/// заставку, и на момент входа в ядро панель работает и сканирует буфер. Нам
/// достаточно знать, где он: писать туда — значит рисовать на экране.
///
/// Адрес LK передаёт двумя половинами по 32 бита. Старый вариант — одно
/// свойство `atag,videolfb` со структурой `{u64 base; u32 islcmfound; u32 fps;
/// u32 vram; ...}`; читаются оба, потому что версия LK у аппарата своя, а
/// разница видна только на нём.
fn framebuffer(fdt: &Fdt<'_>, screen: (u32, u32)) -> Framebuffer {
    let Some(chosen) = fdt.find("/chosen") else {
        return Framebuffer::NONE;
    };

    let (base, vram) = match videolfb_split(&chosen) {
        Some(pair) => pair,
        None => match videolfb_blob(&chosen) {
            Some(pair) => pair,
            None => return Framebuffer::NONE,
        },
    };
    if base == 0 || screen.0 == 0 || screen.1 == 0 {
        return Framebuffer::NONE;
    }

    // Панель, которую загрузчик не нашёл, не сканирует ничего: писать по этому
    // адресу можно сколько угодно, на экране не появится ничего, и выглядеть
    // это будет как неработающая графика.
    if chosen.property_u64("atag,videolfb-islcmfound") == Some(0) {
        return Framebuffer::NONE;
    }

    // Шаг строки равен ширине: у LK буфер плотный. Если это окажется не так,
    // видно будет сразу — картинка поедет косой лесенкой, и это тот редкий
    // случай, когда дефект нельзя ни с чем перепутать.
    let stride = screen.0;
    let frame = u64::from(stride) * u64::from(screen.1) * 4;
    Framebuffer {
        base,
        // Объём из дерева — это **вся** видеопамять, а в ней у LK несколько
        // кадров подряд. Сканируется первый, и отдавать наружу надо его размер,
        // иначе учёт занятой памяти прав, а обрезка рисования — нет.
        size: frame.min(vram.max(frame)),
        width: screen.0,
        height: screen.1,
        stride,
        // BGRA — то, что LK оставляет на MediaTek: синий в младшем байте.
        // Перепутать здесь порядок каналов означает синий интерфейс, ставший
        // красным, — ошибка, которую видно с одного взгляда и которую поэтому
        // дешевле проверить на аппарате, чем выводить рассуждением.
        format: PixelFormat::Bgr,
    }
}

/// Новый способ: адрес двумя половинами.
fn videolfb_split(chosen: &fdt::Node<'_>) -> Option<(u64, u64)> {
    let high = chosen.property_u64("atag,videolfb-fb_base_h")?;
    let low = chosen.property_u64("atag,videolfb-fb_base_l")?;
    let vram = chosen.property_u64("atag,videolfb-vramSize").unwrap_or(0);
    Some(((high << 32) | (low & 0xffff_ffff), vram))
}

/// Старый способ: одно свойство со структурой внутри.
fn videolfb_blob(chosen: &fdt::Node<'_>) -> Option<(u64, u64)> {
    let value = chosen.property("atag,videolfb")?;
    // `{u64 fb_base; u32 islcmfound; u32 fps; u32 vram; char lcmname[]}` —
    // двадцать байт до имени. Короче — не эта структура, и разбирать её как эту
    // значит прочитать адрес из чужих байт.
    if value.len() < 20 {
        return None;
    }
    let base = u64::from_be_bytes(value[0..8].try_into().ok()?);
    let vram = u64::from(u32::from_be_bytes(value[16..20].try_into().ok()?));
    Some((base, vram))
}

/// Где в этой машине контроллер прерываний.
///
/// То же, что MADT даёт на UEFI-машине, только из дерева. Без этого ядро
/// осталось бы с умолчаниями QEMU (`0x08000000`), а у телефона распределитель
/// лежит по `0x0C000000` — то есть первое же обращение к контроллеру ушло бы в
/// пустое место шины. Настроенный и молчащий контроллер снаружи неотличим от
/// работающего, поэтому догадка здесь опаснее отказа.
///
/// Порядок окон в `reg` задан привязкой самого дерева: у GICv3 сначала
/// распределитель, потом redistributor; у GICv2 — распределитель и
/// процессорный интерфейс. Перепутать их местами значит настроить одно через
/// другое.
pub fn gic_layout(fdt: &Fdt<'_>) -> Option<super::acpi::GicLayout> {
    let node = fdt
        .find_compatible("arm,gic-v3")
        .map(|node| (node, 3u8))
        .or_else(|| fdt.find_compatible("arm,gic-400").map(|node| (node, 2)))
        .or_else(|| fdt.find_compatible("arm,cortex-a15-gic").map(|node| (node, 2)));
    let (node, version) = node?;

    let (address_cells, size_cells) = root_cells(fdt);
    let mut windows = node.reg(address_cells, size_cells);
    let distributor = windows.next()?.address;
    if distributor == 0 {
        return None;
    }
    let second = windows.next().map(|region| region.address).filter(|address| *address != 0);

    Some(super::acpi::GicLayout {
        distributor: distributor as usize,
        cpu_interface: if version == 2 { second.map(|address| address as usize) } else { None },
        redistributor: if version == 3 { second.map(|address| address as usize) } else { None },
        // Машина, описанная деревом, остальных процессоров не запускает, и
        // искать их redistributor'ы незачем.
        redistributor_span: 0,
        version,
    })
}

/// Сколько шин I²C помещается в перечисление. У MT6765 их семь.
pub const MAX_I2C: usize = 8;

/// Одна шина I²C так, как её описывает дерево.
#[derive(Clone, Copy)]
pub struct I2cBus {
    /// Окно регистров контроллера.
    pub base: u64,
    /// Блок настройки выводов, которому принадлежат SCL и SDA этой шины.
    ///
    /// Дерево называет его само (`gpio_start`), и это единственное место, где
    /// про выводы шины сказано хоть что-то: узлов состояния выводов у шин I²C в
    /// этом дереве нет вовсе — в отличие от карт памяти, у которых они есть.
    pub pins: Option<u64>,
    /// Номера выводов данных и такта.
    pub sda: u32,
    pub scl: u32,
}

/// Окна регистров всех шин I²C, какие описывает дерево.
///
/// Возвращаются адресами, а не узлами: всё, что нужно дальше, — это куда
/// писать. Пустые места означают, что шин меньше, а не что какая-то пропущена.
///
/// Шины перечисляются целиком и опрашиваются потом все, потому что дерево не
/// говорит, на какой из них тачскрин: у MediaTek он регистрируется
/// платформенным кодом, и в дереве от него остаётся узел `/touch` с одним
/// свойством `compatible` — без адреса и без шины.
pub fn i2c_buses(fdt: &Fdt<'_>) -> [Option<I2cBus>; MAX_I2C] {
    let mut buses = [None; MAX_I2C];
    let mut count = 0;
    let (address_cells, size_cells) = root_cells(fdt);

    for node in fdt.nodes() {
        if !node.is_compatible("mediatek,i2c") && !node.is_compatible("mediatek,mt6577-i2c") {
            continue;
        }
        // Первое окно — сам контроллер; второе, если оно есть, принадлежит
        // каналу DMA, а он нам не нужен: передачи короткие и укладываются в
        // FIFO.
        let Some(region) = node.reg(address_cells, size_cells).next() else {
            continue;
        };
        if region.address == 0 || count == MAX_I2C {
            continue;
        }
        buses[count] = Some(I2cBus {
            base: region.address,
            pins: node.property_u64("gpio_start").filter(|value| *value != 0),
            sda: node.property_u64("sda-gpio-id").unwrap_or(u64::MAX) as u32,
            scl: node.property_u64("scl-gpio-id").unwrap_or(u64::MAX) as u32,
        });
        count += 1;
    }
    buses
}

/// Окна регистров всех контроллеров SPI, какие описывает дерево.
///
/// Их шесть, и дерево не говорит, на каком из них тачскрин: узел `/touch` несёт
/// одно свойство `compatible` и больше ничего. Значит, спрашивать надо все —
/// ровно как с шинами I²C, и с тем же выводом: перебор дешевле догадки.
pub fn spi_buses(fdt: &Fdt<'_>) -> [Option<u64>; MAX_I2C] {
    let mut buses = [None; MAX_I2C];
    let mut count = 0;
    let (address_cells, size_cells) = root_cells(fdt);

    for node in fdt.nodes() {
        if !node.is_compatible("mediatek,mt6765-spi") && !node.is_compatible("mediatek,spi") {
            continue;
        }
        let Some(region) = node.reg(address_cells, size_cells).next() else {
            continue;
        };
        if region.address == 0 || count == MAX_I2C {
            continue;
        }
        buses[count] = Some(region.address);
        count += 1;
    }
    buses
}

/// Узел последовательного порта: тот, что назвал загрузчик, или первый знакомый.
///
/// `stdout-path` — это выбор загрузчика, и уважать его важнее, чем найти первый
/// попавшийся порт: портов у машины несколько, а наружу выведен обычно один.
///
/// Возвращает адрес окна регистров и признак «это PL011». Всё остальное, что
/// встречается на ARM, — 16550 или его родня, включая `mediatek,mt6577-uart`.
pub fn uart(fdt: &Fdt<'_>) -> Option<(usize, bool)> {
    let node = stdout(fdt).or_else(|| {
        fdt.find_compatible("arm,pl011")
            .or_else(|| fdt.find_compatible("mediatek,mt6577-uart"))
            .or_else(|| fdt.find_compatible("ns16550a"))
            .or_else(|| fdt.find_compatible("ns16550"))
    })?;
    let pl011 = node.is_compatible("arm,pl011");
    let (address_cells, size_cells) = root_cells(fdt);
    let region = node.reg(address_cells, size_cells).next()?;
    (region.address != 0).then_some((region.address as usize, pl011))
}

/// Окно адресов моста PCIe: где его видит процессор, где — шина, и сколько.
#[derive(Clone, Copy, Debug)]
pub struct PcieWindow {
    pub cpu: u64,
    pub bus: u64,
    pub len: u64,
}

/// Мост PCIe так, как его описывает дерево (`pci-host-ecam-generic`).
#[derive(Clone, Copy, Debug)]
pub struct PcieHost {
    /// Окно ECAM и его длина.
    pub ecam: u64,
    pub ecam_len: u64,
    pub first_bus: u8,
    pub last_bus: u8,
    /// Окно 32-битной памяти — туда встают BAR, которые мы расставляем сами.
    pub mem32: Option<PcieWindow>,
    /// Окно 64-битной памяти, если оно есть.
    pub mem64: Option<PcieWindow>,
}

/// Код пространства в старшей ячейке адреса PCI (`phys.hi`, биты 25:24).
const PCI_SPACE_SHIFT: u32 = 24;
const PCI_SPACE_MASK: u32 = 0b11;
const PCI_SPACE_MEM32: u32 = 0b10;
const PCI_SPACE_MEM64: u32 = 0b11;

/// Мост PCIe, если дерево его описывает (фаза 51b).
///
/// На машине с ACPI то же самое сообщает `MCFG`, а окна адресов не нужны вовсе:
/// BAR уже расставила прошивка. Здесь прошивки нет, и окна — единственное, что
/// говорит, **куда** их ставить.
///
/// # Как читается `ranges`
///
/// Запись — три поля подряд: адрес на шине PCI (у моста `#address-cells` = 3:
/// `phys.hi` с кодом пространства и два слова адреса), адрес у процессора
/// (ячейки корня) и длина (`#size-cells` моста). Ячейки считаются по
/// объявленным размерам, а не по тому, что «обычно» лежит в QEMU: запись,
/// прочитанная с чужим шагом, даёт правдоподобное и неверное окно.
pub fn pcie_host(fdt: &Fdt<'_>) -> Option<PcieHost> {
    let node = pcie_node(fdt)?;
    let (address_cells, size_cells) = root_cells(fdt);
    let window = node.reg(address_cells, size_cells).next()?;
    if window.address == 0 || window.size == 0 {
        return None;
    }

    // Без `bus-range` мост владеет всеми шинами, какие помещаются в окно.
    let (first_bus, last_bus) = match node.property("bus-range") {
        Some(value) => (
            u8::try_from(be32_at(value, 0)?).ok()?,
            u8::try_from(be32_at(value, 4)?.min(255)).ok()?,
        ),
        None => (0, u8::try_from((window.size >> 20).saturating_sub(1).min(255)).ok()?),
    };

    let child_address = node.property_u64("#address-cells").unwrap_or(3) as usize;
    let child_size = node.property_u64("#size-cells").unwrap_or(2) as usize;
    if child_address != 3 || child_size == 0 || child_size > 2 || address_cells > 2 {
        return None;
    }
    let entry = (child_address + address_cells + child_size) * 4;

    let mut mem32 = None;
    let mut mem64 = None;
    let ranges = node.property("ranges").unwrap_or(&[]);
    let mut offset = 0;
    while offset + entry <= ranges.len() {
        let space = (be32_at(ranges, offset)? >> PCI_SPACE_SHIFT) & PCI_SPACE_MASK;
        let bus = cells_at(ranges, offset + 4, 2)?;
        let cpu = cells_at(ranges, offset + 12, address_cells)?;
        let len = cells_at(ranges, offset + 12 + address_cells * 4, child_size)?;
        let found = PcieWindow { cpu, bus, len };
        match space {
            PCI_SPACE_MEM32 if mem32.is_none() => mem32 = Some(found),
            PCI_SPACE_MEM64 if mem64.is_none() => mem64 = Some(found),
            // Порты ввода-вывода не нужны ни одному нашему драйверу: всё
            // современное отвечает в памяти, а портов на этой архитектуре у
            // процессора нет вовсе — их окно здесь всего лишь ещё одна память.
            _ => {}
        }
        offset += entry;
    }

    Some(PcieHost {
        ecam: window.address,
        ecam_len: window.size,
        first_bus,
        last_bus,
        mem32,
        mem64,
    })
}

/// Узел моста `pci-host-ecam-generic`, если он есть и не выключен.
fn pcie_node<'a>(fdt: &Fdt<'a>) -> Option<fdt::Node<'a>> {
    let node = fdt.find_compatible("pci-host-ecam-generic")?;
    if node.property_str("status").is_some_and(|status| status != "okay" && status != "ok") {
        return None;
    }
    Some(node)
}

/// Куда выведен один вывод `INTx` одного устройства корневой шины моста.
#[derive(Clone, Copy, Debug)]
pub struct PcieIntx {
    pub device: u8,
    /// 0 — INTA, 3 — INTD.
    pub pin: u8,
    /// Номер прерывания у GIC.
    pub intid: u32,
    pub level: bool,
}

/// Записей разводки у корневой шины не больше, чем устройств на ней, умноженных
/// на выводы: 32 × 4.
pub const MAX_PCIE_INTX: usize = 128;

/// Сколько строк `interrupt-map` помещается. У QEMU `virt` их шестнадцать, у
/// Raspberry Pi 4 — одна.
const MAX_MAP_ENTRIES: usize = 64;

/// Разводка линий `INTx` моста: то, что на ACPI сообщает `_PRT`.
///
/// Дерево описывает её свойством `interrupt-map`: строка — адрес устройства на
/// шине (три ячейки) и вывод (одна), затем ссылка на контроллер прерываний, его
/// адрес (сколько ячеек — говорит **сам контроллер**, `#address-cells`) и
/// описание прерывания (сколько ячеек — тоже он, `#interrupt-cells`). Строки
/// сравниваются с адресом устройства после маски `interrupt-map-mask`: у QEMU
/// `virt` маска оставляет два младших бита номера устройства, и шестнадцать
/// строк покрывают все тридцать два устройства.
///
/// Результат раскрыт в таблицу «устройство, вывод → INTID» на всю корневую
/// шину — ровно в том виде, в каком `_PRT` даёт её на x86, чтобы дальше путь
/// был один. Строки, ведущие не в GIC, не берутся и считаются: второй ответ.
pub fn pcie_intx(fdt: &Fdt<'_>, out: &mut [PcieIntx; MAX_PCIE_INTX]) -> (usize, usize) {
    #[derive(Clone, Copy)]
    struct Entry {
        key: [u32; 4],
        intid: u32,
        level: bool,
    }

    let Some(node) = pcie_node(fdt) else {
        return (0, 0);
    };
    let Some(map) = node.property("interrupt-map") else {
        return (0, 0);
    };
    // Адрес устройства на шине — три ячейки, вывод — одна. Другое у моста PCI
    // не встречается, и читать такое наугад значило бы получить чужие линии.
    if node.property_u64("#interrupt-cells").unwrap_or(1) != 1
        || node.property_u64("#address-cells").unwrap_or(3) != 3
    {
        return (0, 0);
    }
    let mask_cells = node.property("interrupt-map-mask");
    let mut mask = [u32::MAX; 4];
    for (index, cell) in mask.iter_mut().enumerate() {
        if let Some(value) = mask_cells.and_then(|cells| be32_at(cells, index * 4)) {
            *cell = value;
        }
    }

    let mut entries = [Entry { key: [0; 4], intid: 0, level: true }; MAX_MAP_ENTRIES];
    let mut count = 0;
    let mut foreign = 0;
    // Контроллер у всех строк обычно один; искать его по дереву заново на каждую
    // строку незачем.
    let mut parent: Option<(u32, usize, usize, bool)> = None;
    let mut at = 0;
    while at + 20 <= map.len() && count < MAX_MAP_ENTRIES {
        let (Some(hi), Some(mid), Some(lo), Some(pin), Some(phandle)) = (
            be32_at(map, at),
            be32_at(map, at + 4),
            be32_at(map, at + 8),
            be32_at(map, at + 12),
            be32_at(map, at + 16),
        ) else {
            break;
        };
        let (address_cells, interrupt_cells, gic) = match parent {
            Some((known, address, interrupt, gic)) if known == phandle => (address, interrupt, gic),
            _ => {
                let Some(controller) = fdt.find_phandle(phandle) else {
                    break;
                };
                let Some(interrupt) = controller.property_u64("#interrupt-cells") else {
                    break;
                };
                let address = controller.property_u64("#address-cells").unwrap_or(0) as usize;
                let gic = controller.strings("compatible").any(|name| name.contains("gic"));
                parent = Some((phandle, address, interrupt as usize, gic));
                (address, interrupt as usize, gic)
            }
        };
        let spec = at + 20 + address_cells * 4;
        let next = spec + interrupt_cells * 4;
        if next > map.len() {
            break;
        }
        // У GIC описание — три ячейки: вид (0 — SPI, 1 — PPI), номер внутри
        // вида и флаги (4 и 8 — по уровню).
        let intid = if gic && interrupt_cells == 3 {
            match (be32_at(map, spec), be32_at(map, spec + 4)) {
                (Some(0), Some(number)) => number.checked_add(32),
                (Some(1), Some(number)) => number.checked_add(16),
                _ => None,
            }
        } else {
            None
        };
        match intid {
            Some(intid) => {
                let flags = be32_at(map, spec + 8).unwrap_or(4);
                entries[count] = Entry {
                    key: [hi & mask[0], mid & mask[1], lo & mask[2], pin & mask[3]],
                    intid,
                    level: matches!(flags & 0xF, 4 | 8),
                };
                count += 1;
            }
            None => foreign += 1,
        }
        at = next;
    }

    let mut len = 0;
    for device in 0..32u8 {
        for pin in 0..4u8 {
            // Адрес устройства на корневой шине — как в `phys.hi`: шина в битах
            // 23:16, устройство в 15:11. Шина здесь нулевая относительно моста.
            let hi = u32::from(device) << 11;
            let key = [hi & mask[0], 0, 0, u32::from(pin + 1) & mask[3]];
            let Some(found) = entries[..count].iter().find(|entry| entry.key == key) else {
                continue;
            };
            out[len] = PcieIntx { device, pin, intid: found.intid, level: found.level };
            len += 1;
        }
    }
    (len, foreign)
}

/// 32 бита со старшим байтом вперёд — ячейка дерева.
fn be32_at(bytes: &[u8], at: usize) -> Option<u32> {
    let cell = bytes.get(at..at + 4)?;
    Some(u32::from_be_bytes([cell[0], cell[1], cell[2], cell[3]]))
}

/// Число из одной или двух ячеек подряд.
fn cells_at(bytes: &[u8], at: usize, count: usize) -> Option<u64> {
    match count {
        1 => Some(u64::from(be32_at(bytes, at)?)),
        2 => Some((u64::from(be32_at(bytes, at)?) << 32) | u64::from(be32_at(bytes, at + 4)?)),
        _ => None,
    }
}

/// Узел, названный в `/chosen/stdout-path`.
fn stdout<'a>(fdt: &Fdt<'a>) -> Option<fdt::Node<'a>> {
    let path = fdt.find("/chosen")?.property_str("stdout-path")?;
    // Путь бывает с параметрами линии через двоеточие: `/pl011@9000000:115200n8`.
    let path = path.split(':').next().unwrap_or(path);
    fdt.find(path)
}

/// Длина самого дерева — чтобы отметить его память занятой.
fn blob_len(_fdt: &Fdt<'_>) -> u64 {
    // Разборщик длину наружу не отдаёт, а перечитывать заголовок отсюда значит
    // знать формат в двух местах. Двух страниц хватает с запасом: настоящие
    // деревья телефонов не доходят и до двухсот килобайт, а отметить занятым
    // чуть больше, чем занято, дешевле, чем отдать распределителю описание
    // машины, по которому он и работает.
    2 * 1024 * 1024
}
