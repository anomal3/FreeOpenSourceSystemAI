//! Разбор ELF64 и размещение PIE-образа ядра в физической памяти.
//!
//! Разбор написан вручную и намеренно не тянет крейт (`object`, `goblin`,
//! `elf`): загрузчику нужно крошечное подмножество формата — заголовок,
//! `PT_LOAD`, `PT_DYNAMIC` и таблица `RELA`, — а применять релокации всё равно
//! пришлось бы своими руками. Все чтения идут через `from_le_bytes` по срезу:
//! это одновременно и проверка границ, и независимость от выравнивания буфера,
//! так что во всём разборе нет ни одного `unsafe`. `unsafe` появляется только
//! там, где данные реально пишутся в выделенную физическую память.
//!
//! Ядро собрано как PIE (`ET_DYN`), то есть адреса внутри него записаны
//! относительно нуля. Физический адрес выбирает прошивка в момент выделения
//! страниц, поэтому каждый абсолютный адрес внутри образа (таблицы указателей,
//! `&'static` ссылки, vtable) нужно сдвинуть на разницу между фактическим
//! адресом и адресом из ELF. Список таких мест ядро приносит с собой в
//! `.rela.dyn` — без их применения оно разыменует нули и упадёт на первой же
//! статической структуре.

use boot_info::{KernelSegment, SEG_EXEC, SEG_READ, SEG_WRITE};
use uefi::boot::{self, AllocateType, MemoryType, PAGE_SIZE};
use uefi::println;

use crate::Aborted;

const ELF_MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];
const ELFCLASS64: u8 = 2;
const ELFDATA2LSB: u8 = 1;
const EV_CURRENT: u8 = 1;

/// Разделяемый объект — так выглядит PIE-исполняемый файл.
const ET_DYN: u16 = 3;

const EM_X86_64: u16 = 62;
const EM_AARCH64: u16 = 183;

/// Тип машины, который обязан быть у ядра для этой сборки загрузчика.
const EXPECTED_MACHINE: u16 = if cfg!(target_arch = "x86_64") {
    EM_X86_64
} else {
    EM_AARCH64
};

/// `R_X86_64_RELATIVE` (8) либо `R_AARCH64_RELATIVE` (1027) — единственный тип
/// релокаций, который может встретиться в статически слинкованном PIE без
/// внешних символов.
const R_RELATIVE: u32 = if cfg!(target_arch = "x86_64") { 8 } else { 1027 };

/// `R_*_NONE` на обеих архитектурах равен нулю и означает «ничего не делать»:
/// линкер оставляет такие записи как заглушки на месте вычеркнутых.
const R_NONE: u32 = 0;

const EHDR_SIZE: usize = 64;
const PHDR_SIZE: usize = 56;
const DYN_SIZE: usize = 16;
const RELA_SIZE: usize = 24;

const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;

// Биты `p_flags`. Порядок обратен привычному по `readelf` написанию «RWX»:
// младший бит — исполнение, старший — чтение. Перепутать их легко, а цена
// ошибки высока: `.text` уехал бы в ядро как записываемый, а `.data` — как
// исполняемый, то есть W^X превратился бы в свою противоположность.
const PF_X: u32 = 1;
const PF_W: u32 = 2;
const PF_R: u32 = 4;

const DT_NULL: i64 = 0;
const DT_RELA: i64 = 7;
const DT_RELASZ: i64 = 8;
const DT_RELAENT: i64 = 9;
const DT_REL: i64 = 17;
const DT_RELSZ: i64 = 18;
const DT_RELRSZ: i64 = 35;
const DT_RELR: i64 = 36;

/// Сколько неожиданных релокаций перечислить в диагностике, прежде чем
/// свернуться в счётчик.
const MAX_REPORTED_RELOCS: usize = 8;

/// Результат размещения ядра в памяти.
#[derive(Clone, Copy)]
pub struct LoadedKernel {
    /// Физический адрес начала выделенного блока (кратен странице).
    pub base: u64,
    /// Размер блока в байтах (кратен странице).
    pub size: u64,
    /// Точка входа: `load_bias + e_entry`.
    pub entry: u64,
    /// Карта прав на образ: по записи на страничный диапазон.
    segments: SegmentList,
}

impl LoadedKernel {
    /// Один байт за концом образа.
    pub const fn end(&self) -> u64 {
        self.base + self.size
    }

    /// Сегменты образа с правами из ELF, отсортированные и без пересечений.
    pub fn segments(&self) -> &[KernelSegment] {
        self.segments.as_slice()
    }
}

/// Разобранный program header.
#[derive(Debug, Clone, Copy)]
struct ProgramHeader {
    /// Номер в таблице исходного файла — чтобы диагностика указывала на то же
    /// место, что и `readelf -l`.
    index: usize,
    ty: u32,
    /// `p_flags`: комбинация [`PF_R`], [`PF_W`], [`PF_X`].
    flags: u32,
    offset: u64,
    vaddr: u64,
    filesz: u64,
    memsz: u64,
    align: u64,
}

/// Проверяет ELF, выделяет физическую память, копирует сегменты, применяет
/// релокации и возвращает адрес точки входа.
///
/// Вся диагностика печатается здесь же: у вызывающего нет контекста, чтобы
/// объяснить, какой именно из шагов не сошёлся.
pub fn load(image: &[u8]) -> Result<LoadedKernel, Aborted> {
    let header = parse_header(image)?;
    let segments = parse_program_headers(image, &header)?;

    let (span_start, span_end, max_align) = span(&segments)?;
    let (base, size) = allocate(span_start, span_end, max_align)?;

    // Сдвиг между виртуальными адресами из ELF и физическими, по которым образ
    // реально лёг. Для типичного PIE с нулевым первым сегментом он равен `base`.
    let bias = base.wrapping_sub(span_start);

    println!(
        "  [elf] vaddr span {:#018x}..{:#018x}, align {:#x}",
        span_start, span_end, max_align
    );
    println!("  [elf] placed at {base:#018x}..{:#018x} ({size} bytes)", base + size);

    // SAFETY: `base`/`size` только что получены у `allocate_pages`, страницы
    // принадлежат нам целиком и никем больше не используются. Обнуление всего
    // блока сразу решает две задачи: BSS (`p_memsz - p_filesz`) оказывается
    // нулевым, как того требует ABI, и дырки между сегментами не содержат
    // мусора от предыдущего владельца страниц. Пропущенный BSS — классический
    // источник плавающих багов: статики ядра получают случайные значения.
    unsafe {
        core::ptr::write_bytes(base as *mut u8, 0, size as usize);
    }

    copy_segments(image, &segments, bias)?;
    apply_relocations(&segments, bias, base, size)?;

    let permissions = build_segments(&segments, bias, base, size)?;

    let entry = bias.wrapping_add(header.entry);
    if entry < base || entry >= base + size {
        println!(
            "  [elf] entry point {entry:#018x} falls outside the loaded image -- broken e_entry"
        );
        return Err(Aborted);
    }

    println!("  [elf] entry point {entry:#018x} (e_entry {:#x} + bias)", header.entry);

    Ok(LoadedKernel { base, size, entry, segments: permissions })
}

/// То, что нам нужно из `Elf64_Ehdr`.
struct Header {
    entry: u64,
    phoff: u64,
    phentsize: usize,
    phnum: usize,
}

fn parse_header(image: &[u8]) -> Result<Header, Aborted> {
    if image.len() < EHDR_SIZE {
        println!("  [elf] file is {} bytes, shorter than an ELF64 header", image.len());
        return Err(Aborted);
    }

    if image[..4] != ELF_MAGIC {
        println!(
            "  [elf] bad magic {:02x?} -- this is not an ELF file",
            &image[..4]
        );
        return Err(Aborted);
    }
    if image[4] != ELFCLASS64 {
        println!("  [elf] EI_CLASS is {}, expected 2 (ELF64)", image[4]);
        return Err(Aborted);
    }
    if image[5] != ELFDATA2LSB {
        println!("  [elf] EI_DATA is {}, expected 1 (little-endian)", image[5]);
        return Err(Aborted);
    }
    if image[6] != EV_CURRENT {
        println!("  [elf] EI_VERSION is {}, expected 1", image[6]);
        return Err(Aborted);
    }

    let ty = u16_at(image, 16).ok_or(Aborted)?;
    if ty != ET_DYN {
        println!(
            "  [elf] e_type is {ty} (expected {ET_DYN} = ET_DYN): the kernel must be linked as PIE"
        );
        return Err(Aborted);
    }

    let machine = u16_at(image, 18).ok_or(Aborted)?;
    if machine != EXPECTED_MACHINE {
        println!(
            "  [elf] e_machine is {machine}, expected {EXPECTED_MACHINE} -- kernel built for another architecture"
        );
        return Err(Aborted);
    }

    let entry = u64_at(image, 24).ok_or(Aborted)?;
    let phoff = u64_at(image, 32).ok_or(Aborted)?;
    let phentsize = u16_at(image, 54).ok_or(Aborted)? as usize;
    let phnum = u16_at(image, 56).ok_or(Aborted)? as usize;

    if phentsize != PHDR_SIZE {
        println!("  [elf] e_phentsize is {phentsize}, expected {PHDR_SIZE}");
        return Err(Aborted);
    }
    if phnum == 0 {
        println!("  [elf] the image has no program headers -- nothing to load");
        return Err(Aborted);
    }

    Ok(Header { entry, phoff, phentsize, phnum })
}

fn parse_program_headers(image: &[u8], header: &Header) -> Result<PhdrList, Aborted> {
    let mut list = PhdrList::new();

    for index in 0..header.phnum {
        let Some(offset) = header
            .phoff
            .checked_add((index * header.phentsize) as u64)
            .and_then(|off| usize::try_from(off).ok())
        else {
            println!("  [elf] program header table overflows the address space");
            return Err(Aborted);
        };

        let Some(raw) = image.get(offset..offset.saturating_add(PHDR_SIZE)) else {
            println!("  [elf] program header {index} lies outside the {} byte file", image.len());
            return Err(Aborted);
        };

        let ph = ProgramHeader {
            index,
            ty: u32_at(raw, 0).ok_or(Aborted)?,
            flags: u32_at(raw, 4).ok_or(Aborted)?,
            offset: u64_at(raw, 8).ok_or(Aborted)?,
            vaddr: u64_at(raw, 16).ok_or(Aborted)?,
            filesz: u64_at(raw, 32).ok_or(Aborted)?,
            memsz: u64_at(raw, 40).ok_or(Aborted)?,
            align: u64_at(raw, 48).ok_or(Aborted)?,
        };

        if ph.ty != PT_LOAD && ph.ty != PT_DYNAMIC {
            continue;
        }

        if ph.ty == PT_LOAD {
            if ph.filesz > ph.memsz {
                println!(
                    "  [elf] segment {index}: p_filesz {} exceeds p_memsz {}",
                    ph.filesz, ph.memsz
                );
                return Err(Aborted);
            }
            let end = ph
                .offset
                .checked_add(ph.filesz)
                .and_then(|end| usize::try_from(end).ok());
            match end {
                Some(end) if end <= image.len() => {}
                _ => {
                    println!(
                        "  [elf] segment {index}: file range {:#x}..+{:#x} runs past end of file",
                        ph.offset, ph.filesz
                    );
                    return Err(Aborted);
                }
            }
        }

        if list.push(ph).is_err() {
            println!("  [elf] more than {} interesting program headers", PhdrList::CAPACITY);
            return Err(Aborted);
        }
    }

    if !list.iter().any(|ph| ph.ty == PT_LOAD) {
        println!("  [elf] no PT_LOAD segments -- the image contains nothing to place in memory");
        return Err(Aborted);
    }

    Ok(list)
}

/// Суммарный диапазон виртуальных адресов всех `PT_LOAD` и максимальное
/// требование к выравниванию среди них.
fn span(segments: &PhdrList) -> Result<(u64, u64, u64), Aborted> {
    let mut start = u64::MAX;
    let mut end = 0u64;
    let mut align = PAGE_SIZE as u64;

    for ph in segments.iter().filter(|ph| ph.ty == PT_LOAD) {
        let Some(seg_end) = ph.vaddr.checked_add(ph.memsz) else {
            println!("  [elf] segment at {:#x} overflows when adding p_memsz", ph.vaddr);
            return Err(Aborted);
        };
        start = start.min(ph.vaddr);
        end = end.max(seg_end);
        // p_align > 4 KiB встречается у ядер, рассчитывающих на крупные
        // страницы; сдвиг образа обязан быть кратен ему, иначе внутренние
        // предположения о выравнивании перестанут выполняться.
        if ph.align > align && ph.align.is_power_of_two() {
            align = ph.align;
        }
    }

    // Округляем до границ страниц: выделять память мы всё равно можем только
    // страницами, а хвост последней страницы обнуляется вместе со всем блоком.
    let page = PAGE_SIZE as u64;
    let start = start & !(page - 1);
    let Some(end) = end.checked_next_multiple_of(page) else {
        println!("  [elf] image span overflows when rounded up to a page boundary");
        return Err(Aborted);
    };

    if end <= start {
        println!("  [elf] degenerate image span {start:#x}..{end:#x}");
        return Err(Aborted);
    }

    Ok((start, end, align))
}

/// Выделяет физическую память под образ и возвращает `(base, size)`.
///
/// Если сегменты требуют выравнивания крупнее страницы, берём с запасом и
/// подравниваем начало внутри выделенного блока: `allocate_pages` обещает
/// только 4 КиБ.
fn allocate(span_start: u64, span_end: u64, align: u64) -> Result<(u64, u64), Aborted> {
    let page = PAGE_SIZE as u64;
    let span = span_end - span_start;
    let slack = align.saturating_sub(page);

    let Some(request) = span.checked_add(slack) else {
        println!("  [elf] image is too large to allocate ({span} bytes + {slack} of padding)");
        return Err(Aborted);
    };
    let Ok(pages) = usize::try_from(request / page) else {
        println!("  [elf] image needs more pages than this machine can address");
        return Err(Aborted);
    };
    if pages == 0 {
        println!("  [elf] image span rounds down to zero pages");
        return Err(Aborted);
    }

    // LOADER_DATA, а не собственный тип памяти: пользовательские значения
    // MemoryType ломают часть прошивок, а нужный ядру ярлык Kernel мы всё равно
    // проставим сами при конвертации карты памяти.
    let ptr = match boot::allocate_pages(AllocateType::AnyPages, MemoryType::LOADER_DATA, pages) {
        Ok(ptr) => ptr,
        Err(err) => {
            println!("  [elf] cannot allocate {pages} pages for the kernel image ({err:?})");
            return Err(Aborted);
        }
    };

    let raw = ptr.as_ptr() as usize as u64;
    let base = if align > page {
        match raw.checked_next_multiple_of(align) {
            Some(base) => base,
            None => {
                println!("  [elf] cannot align {raw:#018x} up to {align:#x}");
                return Err(Aborted);
            }
        }
    } else {
        raw
    };

    // Образ занимает ровно `span` байт начиная с выровненного `base`; хвост
    // запаса остаётся LOADER_DATA и достанется ядру как reclaimable-память.
    // Неравенство `base + span <= raw + request` выполняется по построению:
    // `raw` выровнен на страницу, поэтому `base - raw <= align - page = slack`.
    Ok((base, span))
}

/// Копирует `p_filesz` байт каждого `PT_LOAD` на своё место. Хвост `p_memsz`
/// уже нулевой: блок обнулён целиком до вызова.
fn copy_segments(image: &[u8], segments: &PhdrList, bias: u64) -> Result<(), Aborted> {
    for ph in segments.iter().filter(|ph| ph.ty == PT_LOAD) {
        let index = ph.index;
        if ph.filesz == 0 {
            continue;
        }

        let offset = usize::try_from(ph.offset).map_err(|_| Aborted)?;
        let len = usize::try_from(ph.filesz).map_err(|_| Aborted)?;
        let src = match offset.checked_add(len).and_then(|end| image.get(offset..end)) {
            Some(src) => src,
            None => {
                println!("  [elf] segment {index}: source range vanished between checks");
                return Err(Aborted);
            }
        };

        let dest = bias.wrapping_add(ph.vaddr);

        // SAFETY: `dest` лежит внутри блока, выделенного под образ: `bias`
        // подобран так, что `span_start` попадает в `base`, а `ph.vaddr +
        // ph.memsz <= span_end` проверено в `span()`. Источник — срез входного
        // файла с проверенными границами, приёмник — свежие страницы, никаких
        // пересечений между ними нет (разные аллокации).
        unsafe {
            core::ptr::copy_nonoverlapping(src.as_ptr(), dest as *mut u8, len);
        }

        println!(
            "  [elf] segment {index}: {:#010x} -> {dest:#018x}, {} of {} bytes ({} bytes bss)",
            ph.vaddr,
            ph.filesz,
            ph.memsz,
            ph.memsz - ph.filesz
        );
    }

    Ok(())
}

/// Применяет `R_*_RELATIVE` из `.rela.dyn`.
fn apply_relocations(
    segments: &PhdrList,
    bias: u64,
    base: u64,
    size: u64,
) -> Result<(), Aborted> {
    let Some(dynamic) = segments.iter().find(|ph| ph.ty == PT_DYNAMIC) else {
        // Ядро без единого абсолютного адреса — теоретически возможно, но
        // настолько нетипично, что об этом стоит сказать вслух.
        println!("  [elf] no PT_DYNAMIC segment -- assuming the image needs no relocations");
        return Ok(());
    };

    let table = read_image(bias.wrapping_add(dynamic.vaddr), dynamic.memsz, base, size)
        .ok_or_else(|| {
            println!("  [elf] PT_DYNAMIC lies outside the loaded image");
            Aborted
        })?;

    let mut rela = 0u64;
    let mut relasz = 0u64;
    let mut relaent = RELA_SIZE as u64;

    for chunk in table.chunks_exact(DYN_SIZE) {
        let tag = i64_at(chunk, 0).ok_or(Aborted)?;
        let val = u64_at(chunk, 8).ok_or(Aborted)?;
        match tag {
            DT_NULL => break,
            DT_RELA => rela = val,
            DT_RELASZ => relasz = val,
            DT_RELAENT => relaent = val,
            DT_REL | DT_RELSZ => {
                println!(
                    "  [elf] the image carries a DT_REL table; only RELA (with explicit addends) is supported"
                );
                return Err(Aborted);
            }
            DT_RELR | DT_RELRSZ => {
                println!(
                    "  [elf] the image carries a packed DT_RELR table; relink without -z pack-relative-relocs"
                );
                return Err(Aborted);
            }
            _ => {}
        }
    }

    if rela == 0 || relasz == 0 {
        println!("  [elf] .rela.dyn is empty -- no relocations to apply");
        return Ok(());
    }
    if relaent != RELA_SIZE as u64 {
        println!("  [elf] DT_RELAENT is {relaent}, expected {RELA_SIZE}");
        return Err(Aborted);
    }

    let entries = read_image(bias.wrapping_add(rela), relasz, base, size).ok_or_else(|| {
        println!("  [elf] .rela.dyn ({relasz} bytes at {rela:#x}) lies outside the loaded image");
        Aborted
    })?;

    let mut applied = 0usize;
    let mut unexpected = 0usize;

    for entry in entries.chunks_exact(RELA_SIZE) {
        let r_offset = u64_at(entry, 0).ok_or(Aborted)?;
        let r_info = u64_at(entry, 8).ok_or(Aborted)?;
        let r_addend = i64_at(entry, 16).ok_or(Aborted)?;
        let r_type = (r_info & 0xffff_ffff) as u32;

        if r_type == R_NONE {
            continue;
        }
        if r_type != R_RELATIVE {
            // Молчаливое игнорирование здесь означало бы ядро, которое
            // запускается и падает в случайном месте. Ненулевой тип — признак
            // того, что ядро слинковано не так, как договаривались: остались
            // внешние символы, GOT или TLS.
            if unexpected < MAX_REPORTED_RELOCS {
                println!(
                    "  [elf] unsupported relocation type {r_type} (symbol {}) at {r_offset:#x}",
                    r_info >> 32
                );
            }
            unexpected += 1;
            continue;
        }

        let target = bias.wrapping_add(r_offset);
        if target < base || target.saturating_add(8) > base + size {
            println!("  [elf] relocation target {target:#018x} escapes the loaded image");
            return Err(Aborted);
        }

        let value = bias.wrapping_add(r_addend as u64);

        // SAFETY: проверка выше гарантирует, что все восемь байт по `target`
        // лежат внутри блока, который мы сами выделили и в который никто больше
        // не пишет. `write_unaligned` снимает вопрос о выравнивании: линкер
        // обычно выравнивает такие слоты на 8, но формат этого не обещает.
        unsafe {
            (target as *mut u64).write_unaligned(value);
        }

        applied += 1;
    }

    if unexpected > 0 {
        if unexpected > MAX_REPORTED_RELOCS {
            println!(
                "  [elf] ... and {} more unsupported relocations",
                unexpected - MAX_REPORTED_RELOCS
            );
        }
        println!("  [elf] {unexpected} relocations could not be applied -- refusing to boot");
        return Err(Aborted);
    }

    println!("  [elf] applied {applied} relative relocations (bias {bias:#018x})");

    Ok(())
}

/// Срез размещённого образа по физическому адресу, если он целиком укладывается
/// в блок `base..base + size`.
fn read_image(addr: u64, len: u64, base: u64, size: u64) -> Option<&'static [u8]> {
    let end = addr.checked_add(len)?;
    if addr < base || end > base + size {
        return None;
    }
    let len = usize::try_from(len).ok()?;

    // SAFETY: диапазон проверен на принадлежность блоку, который мы выделили и
    // полностью инициализировали (обнулили, затем скопировали сегменты). Время
    // жизни 'static честно: страницы не освобождаются до передачи управления
    // ядру, а сама ссылка живёт лишь внутри вызывающей функции.
    Some(unsafe { core::slice::from_raw_parts(addr as *const u8, len) })
}

// --- Карта прав на образ ----------------------------------------------------

/// Строковое представление прав сегмента в порядке `RWX`.
///
/// Индекс собирается из битов `SEG_READ | SEG_WRITE | SEG_EXEC`, которые как
/// раз равны 1, 2 и 4, поэтому таблица индексируется значением флагов напрямую.
pub fn perms(flags: u32) -> &'static str {
    const NAMES: [&str; 8] = ["---", "R--", "-W-", "RW-", "--X", "R-X", "-WX", "RWX"];
    NAMES[(flags & (SEG_READ | SEG_WRITE | SEG_EXEC)) as usize]
}

/// Переводит `p_flags` program header'а в флаги [`KernelSegment`].
fn seg_flags(p_flags: u32) -> u32 {
    let mut flags = 0;
    if p_flags & PF_R != 0 {
        flags |= SEG_READ;
    }
    if p_flags & PF_W != 0 {
        flags |= SEG_WRITE;
    }
    if p_flags & PF_X != 0 {
        flags |= SEG_EXEC;
    }
    flags
}

/// Строит карту прав на размещённый образ: по записи на каждый `PT_LOAD`,
/// с адресами после релокации и границами, выровненными на страницу.
///
/// # Почему выравнивание на страницу — опасное место
///
/// Права в таблицах страниц применяются только целыми страницами, поэтому
/// начало сегмента приходится округлять вниз, а конец — вверх. Если после
/// такого округления хвост `.text` и начало `.data` попадают на одну и ту же
/// страницу, никакой набор битов не удовлетворит обоих: страница обязана быть
/// одновременно исполняемой и записываемой. Объединение прав («так точно
/// заработает») даёт ровно RWX — то есть W^X молча вырождается именно там, где
/// он нужнее всего, и никто об этом не узнает.
///
/// Развести такие сегменты загрузчик не может: сдвиг одного из них относительно
/// остальных сломал бы PC-относительные ссылки внутри образа, а они уже
/// зафиксированы компилятором. Поэтому конфликт разрешается явно: пересечение
/// выделяется в отдельную запись с объединёнными правами, а в консоль уходит
/// предупреждение с точным диапазоном страниц.
///
/// На практике конфликта нет: `-z max-page-size=4096` заставляет lld разносить
/// `PT_LOAD` с разными правами по разным страницам (соседние сегменты стыкуются
/// ровно по границе). Проверка оставлена потому, что это свойство раскладки
/// линкера, а не гарантия формата: смена флагов линковки способна вернуть
/// конфликт, и тогда он должен быть виден, а не подразумеваться.
fn build_segments(
    headers: &PhdrList,
    bias: u64,
    base: u64,
    size: u64,
) -> Result<SegmentList, Aborted> {
    let page = PAGE_SIZE as u64;
    let mut raw = SegmentList::new();

    for ph in headers.iter().filter(|ph| ph.ty == PT_LOAD) {
        let index = ph.index;
        if ph.memsz == 0 {
            continue;
        }

        let start = bias.wrapping_add(ph.vaddr);
        let Some(end) = start
            .checked_add(ph.memsz)
            .and_then(|end| end.checked_next_multiple_of(page))
        else {
            println!("  [elf] segment {index}: end address overflows when rounded up to a page");
            return Err(Aborted);
        };
        let start = start & !(page - 1);

        // Округление не имеет права вывести сегмент за пределы блока: и
        // `span()`, и `allocate()` уже работают в страничных границах, так что
        // нарушение здесь означало бы ошибку в расчёте `bias`.
        if start < base || end > base + size {
            println!(
                "  [elf] segment {index}: page range {start:#018x}..{end:#018x} escapes the image {base:#018x}..{:#018x}",
                base + size
            );
            return Err(Aborted);
        }

        if raw.push(KernelSegment::new(start, end - start, seg_flags(ph.flags))).is_err() {
            println!("  [elf] more than {} loadable segments", SegmentList::CAPACITY);
            return Err(Aborted);
        }
    }

    // Program headers обычно уже идут по возрастанию p_vaddr, но формат этого
    // не требует, а весь дальнейший разбор пересечений опирается на порядок.
    raw.as_mut_slice().sort_unstable_by_key(|seg| seg.base);

    resolve_overlaps(raw.as_slice(), page)
}

/// Сливает соседей с одинаковыми правами и разбирает страницы, на которые
/// претендуют сегменты с разными правами. Вход обязан быть отсортирован.
fn resolve_overlaps(sorted: &[KernelSegment], page: u64) -> Result<SegmentList, Aborted> {
    let mut out = SegmentList::new();

    for &seg in sorted {
        let seg_end = seg.base + seg.len;

        let Some(last) = out.last() else {
            out.push(seg).map_err(|()| overflow())?;
            continue;
        };
        let last_end = last.base + last.len;

        // Одинаковые права — одна запись, если сегменты стыкуются или
        // пересекаются: ядру проще пройти по короткому списку, а разбиение
        // данных на несколько `PT_LOAD` линкер делает регулярно. Через дырку
        // не сливаем: страницы между сегментами прав получать не должны.
        if last.flags == seg.flags && seg.base <= last_end {
            out.set_last_end(last_end.max(seg_end));
            continue;
        }

        if seg.base >= last_end {
            out.push(seg).map_err(|()| overflow())?;
            continue;
        }

        let shared_end = last_end.min(seg_end);
        let merged = last.flags | seg.flags;
        println!(
            "  [elf] WARNING: pages {:#018x}..{shared_end:#018x} are claimed by both a {} and a {} segment",
            seg.base,
            perms(last.flags),
            perms(seg.flags)
        );
        println!(
            "  [elf] WARNING: {} page(s) must become {} -- W^X does not hold there; relink with -z max-page-size=4096",
            (shared_end - seg.base) / page,
            perms(merged)
        );

        // Предыдущая запись усекается до начала общих страниц; если от неё
        // ничего не осталось, она целиком поглощена пересечением.
        out.set_last_end(seg.base);
        out.push(KernelSegment::new(seg.base, shared_end - seg.base, merged))
            .map_err(|()| overflow())?;

        // Хвост той из двух записей, что кончается позже. Из
        // `shared_end = min(last_end, seg_end)` следует, что продолжение
        // существует не более чем у одной из них, так что порядок по адресу
        // сохраняется сам собой.
        let tail = if last_end > shared_end {
            KernelSegment::new(shared_end, last_end - shared_end, last.flags)
        } else {
            KernelSegment::new(shared_end, seg_end.saturating_sub(shared_end), seg.flags)
        };
        out.push(tail).map_err(|()| overflow())?;
    }

    Ok(out)
}

fn overflow() -> Aborted {
    println!(
        "  [elf] the segment map does not fit into {} entries",
        SegmentList::CAPACITY
    );
    Aborted
}

/// Карта прав фиксированной ёмкости.
///
/// Ёмкость с запасом вдвое от числа program headers: разбор конфликта прав
/// заменяет две записи на три.
#[derive(Clone, Copy)]
struct SegmentList {
    items: [KernelSegment; Self::CAPACITY],
    len: usize,
}

impl SegmentList {
    const CAPACITY: usize = PhdrList::CAPACITY * 2;

    const fn new() -> Self {
        const EMPTY: KernelSegment = KernelSegment::new(0, 0, 0);
        Self { items: [EMPTY; Self::CAPACITY], len: 0 }
    }

    fn push(&mut self, seg: KernelSegment) -> Result<(), ()> {
        if seg.len == 0 {
            return Ok(());
        }
        if self.len == Self::CAPACITY {
            return Err(());
        }
        self.items[self.len] = seg;
        self.len += 1;
        Ok(())
    }

    fn last(&self) -> Option<KernelSegment> {
        self.len.checked_sub(1).map(|i| self.items[i])
    }

    /// Двигает конец последней записи. Запись нулевой длины выбрасывается:
    /// ядру такие отдавать незачем, а пустой диапазон легко принять за ошибку.
    fn set_last_end(&mut self, end: u64) {
        let Some(i) = self.len.checked_sub(1) else {
            return;
        };
        if end <= self.items[i].base {
            self.len -= 1;
        } else {
            self.items[i].len = end - self.items[i].base;
        }
    }

    fn as_slice(&self) -> &[KernelSegment] {
        &self.items[..self.len]
    }

    fn as_mut_slice(&mut self) -> &mut [KernelSegment] {
        &mut self.items[..self.len]
    }
}

// --- Чтение полей фиксированного размера -----------------------------------
//
// Через `get` + `from_le_bytes`, а не через приведение указателя к `*const
// Elf64_Ehdr`: буфер файла не обязан быть выровнен под u64, а границы среза
// проверяются бесплатно.

fn u16_at(buf: &[u8], off: usize) -> Option<u16> {
    let raw: [u8; 2] = buf.get(off..off.checked_add(2)?)?.try_into().ok()?;
    Some(u16::from_le_bytes(raw))
}

fn u32_at(buf: &[u8], off: usize) -> Option<u32> {
    let raw: [u8; 4] = buf.get(off..off.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_le_bytes(raw))
}

fn u64_at(buf: &[u8], off: usize) -> Option<u64> {
    let raw: [u8; 8] = buf.get(off..off.checked_add(8)?)?.try_into().ok()?;
    Some(u64::from_le_bytes(raw))
}

fn i64_at(buf: &[u8], off: usize) -> Option<i64> {
    u64_at(buf, off).map(|v| v as i64)
}

// --- Список program headers без аллокаций ----------------------------------

/// Фиксированный буфер под интересные program headers.
///
/// `Vec` здесь тоже сработал бы, но пул прошивки лучше не трогать лишний раз:
/// каждый его вызов меняет карту памяти, а число сегментов у ядра — единицы.
struct PhdrList {
    items: [ProgramHeader; Self::CAPACITY],
    len: usize,
}

impl PhdrList {
    const CAPACITY: usize = 16;

    const fn new() -> Self {
        const EMPTY: ProgramHeader = ProgramHeader {
            index: 0,
            ty: 0,
            flags: 0,
            offset: 0,
            vaddr: 0,
            filesz: 0,
            memsz: 0,
            align: 0,
        };
        Self { items: [EMPTY; Self::CAPACITY], len: 0 }
    }

    fn push(&mut self, ph: ProgramHeader) -> Result<(), ()> {
        if self.len == Self::CAPACITY {
            return Err(());
        }
        self.items[self.len] = ph;
        self.len += 1;
        Ok(())
    }

    fn iter(&self) -> core::slice::Iter<'_, ProgramHeader> {
        self.items[..self.len].iter()
    }
}
