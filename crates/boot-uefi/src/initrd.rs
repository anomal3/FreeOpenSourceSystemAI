//! Чтение образа RAM-диска с загрузочного тома.
//!
//! `\initrd.img` — образ файловой системы, который ядро читает как блочное
//! устройство. Он лежит рядом с ядром на том же ESP и попадает в память здесь,
//! пока прошивка ещё умеет читать файлы: своего дискового драйвера у ядра нет,
//! и до появления AHCI/SD это единственный способ дать ему файлы.
//!
//! # Почему отсутствие файла — не ошибка
//!
//! Образ ФС не нужен, чтобы ядро стартовало: без него оно просто не смонтирует
//! корень. Отказ в загрузке из-за отсутствующего `initrd.img` сделал бы
//! невозможной отладку самого ядра (`xtask run --no-kernel` и подобные
//! урезанные прогоны) и превратил бы необязательный компонент в обязательный.
//! Поэтому отсутствие файла — строка в диагностике и [`Initrd::NONE`], а вот
//! файл, который есть, но не читается, — отказ: молча загрузиться без ФС в
//! этом случае значило бы скрыть повреждённый носитель.
//!
//! # Почему память отдельная и `Reserved`
//!
//! Образ переживает `ExitBootServices` и читается всё время работы ядра, а не
//! только на старте. Блок [`crate::handoff::Handoff`] для него не годится: там
//! лежат структуры хэндоффа размером в килобайты, а образ — десятки мегабайт.
//! Тип памяти — [`MemoryKind::Reserved`], а не `BootloaderReclaimable` и не
//! `Kernel`: ядро не должно ни считать эту память своей, ни освобождать её
//! после разбора хэндоффа, ни выделять из неё кадры.

use core::ptr::NonNull;

use boot_info::Initrd;
use uefi::boot::{self, AllocateType, MemoryType, PAGE_SIZE};
use uefi::proto::media::file::RegularFile;
use uefi::{CStr16, cstr16, println};

use crate::Aborted;
use crate::volume::{self, BootVolume};

/// Путь к образу относительно корня тома — по тем же правилам, что и
/// `\kernel.elf`.
const INITRD_PATH: &CStr16 = cstr16!("\\initrd.img");

/// Образы RAM-диска слотов.
///
/// Свой на каждый слот, а не один общий, и это не запас: initrd несёт `/bin`, а
/// программы связаны с ядром номерами системных вызовов. Общий образ означал бы,
/// что откат к прежнему ядру оставляет программы от новой — то есть пару,
/// которая расходится молча.
const INITRD_A: &CStr16 = cstr16!("\\initrd-a.img");
const INITRD_B: &CStr16 = cstr16!("\\initrd-b.img");

/// Путь к образу выбранного слота.
#[must_use]
pub const fn path_for(slot: Option<slots::Slot>) -> &'static CStr16 {
    match slot {
        Some(slots::Slot::A) => INITRD_A,
        Some(slots::Slot::B) => INITRD_B,
        None => INITRD_PATH,
    }
}

/// Больше этого образ быть не должен: гигабайт — заведомо избыточный размер для
/// корневой ФС, которая целиком живёт в оперативной памяти. Проверка нужна,
/// чтобы не пытаться выделить страницы под мусорный `file_size` с повреждённого
/// тома.
const MAX_INITRD_SIZE: u64 = 1024 * 1024 * 1024;

/// Читает `\initrd.img` в память, переживающую `ExitBootServices`.
///
/// Возвращает [`Initrd::NONE`], если файла на томе нет.
pub fn load(volume: &mut BootVolume, path: &CStr16) -> Result<Initrd, Aborted> {
    let Some(mut file) = volume.open_regular(path)? else {
        println!("  [fs ] {path}: absent -- booting without a filesystem image");
        return Ok(Initrd::NONE);
    };

    let size = volume::size_of(&mut file, path)?;

    if size == 0 {
        // Пустой файл — не «образа нет», а «образ собран неправильно»: ни одна
        // файловая система не помещается в ноль байт. Промолчать здесь значило
        // бы отдать ядру заведомо нерабочий носитель под видом отсутствующего.
        println!("  [fs ] {path} is empty -- the image was built incorrectly");
        return Err(Aborted);
    }
    if size > MAX_INITRD_SIZE {
        println!("  [fs ] {path} claims {size} bytes, refusing (limit {MAX_INITRD_SIZE})");
        return Err(Aborted);
    }

    let (base, pages) = allocate(size, path)?;

    read_into(&mut file, base, size, path).inspect_err(|_| {
        // Десятки мегабайт, которые никому уже не нужны: загрузчик вернёт
        // управление прошивке, и та вполне может запустить его снова.
        //
        // SAFETY: `base` — начало блока, полученного парой строк выше от
        // `allocate_pages` ровно на `pages` страниц; никто, кроме нас, на него
        // не ссылается, и после этой точки мы к нему не обращаемся.
        let freed = unsafe { boot::free_pages(base, pages) };
        if freed.is_err() {
            println!("  [fs ] note: could not return {pages} page(s) to the firmware");
        }
    })?;

    let address = base.as_ptr() as usize as u64;

    println!(
        "  [fs ] {path}: {size} bytes at {address:#018x}..{:#018x} ({pages} page(s), reserved)",
        address + size
    );

    Ok(Initrd { base: address, size })
}

/// Выделяет страницы под образ и обнуляет их. Возвращает `(база, число страниц)`.
///
/// Выделение идёт страницами, поэтому база кратна 4 КиБ: ядро читает образ
/// блоками по 512 байт, и любая другая база заставила бы его собирать блок из
/// двух кусков.
fn allocate(size: u64, path: &CStr16) -> Result<(NonNull<u8>, usize), Aborted> {
    let Ok(pages) = usize::try_from(size.div_ceil(PAGE_SIZE as u64)) else {
        println!("  [fs ] {path} needs more pages than this machine can address");
        return Err(Aborted);
    };

    // LOADER_DATA, а не собственный тип памяти: вендорские значения MemoryType
    // ломают часть прошивок, а нужный ядру ярлык Reserved всё равно
    // проставляется при конвертации карты памяти — см. `handoff::Override`.
    let base = match boot::allocate_pages(AllocateType::AnyPages, MemoryType::LOADER_DATA, pages) {
        Ok(base) => base,
        Err(err) => {
            println!("  [fs ] cannot allocate {pages} page(s) for {path} ({err:?})");
            println!("  [fs ] the image is {size} bytes; give the machine more memory or shrink it");
            return Err(Aborted);
        }
    };

    // SAFETY: `allocate_pages` вернула `pages` полностью наших страниц, то есть
    // ровно `pages * PAGE_SIZE` байт. Обнуление решает две задачи: делает
    // память инициализированной (без этого срез в `read_into` был бы построен
    // над неинициализированными байтами), и не даёт ядру прочитать в хвосте
    // последней страницы мусор от предыдущего владельца.
    unsafe {
        core::ptr::write_bytes(base.as_ptr(), 0, pages * PAGE_SIZE);
    }

    Ok((base, pages))
}

/// Вычитывает `size` байт файла в блок по адресу `base`.
fn read_into(
    file: &mut RegularFile,
    base: NonNull<u8>,
    size: u64,
    path: &CStr16,
) -> Result<(), Aborted> {
    // `size <= MAX_INITRD_SIZE` проверено вызывающим, поэтому каст точен.
    let len = size as usize;

    // SAFETY: блок выделен под `size.div_ceil(PAGE_SIZE)` страниц, то есть не
    // меньше `len` байт, целиком принадлежит нам и только что обнулён — значит,
    // инициализирован. Других ссылок на него не существует.
    let buffer = unsafe { core::slice::from_raw_parts_mut(base.as_ptr(), len) };

    // EFI_FILE_PROTOCOL.Read имеет право вернуть меньше запрошенного, поэтому
    // читаем в цикле, а не одним вызовом.
    let mut filled = 0usize;
    while filled < len {
        match file.read(&mut buffer[filled..]) {
            Ok(0) => {
                println!("  [fs ] short read: {filled} of {size} bytes before end of file");
                return Err(Aborted);
            }
            Ok(n) => filled += n,
            Err(err) => {
                println!("  [fs ] read error at offset {filled} of {path} ({err:?})");
                return Err(Aborted);
            }
        }
    }

    Ok(())
}
