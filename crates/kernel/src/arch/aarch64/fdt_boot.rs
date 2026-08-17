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
        version,
    })
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
