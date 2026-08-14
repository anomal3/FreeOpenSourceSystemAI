//! Загрузочная запись UEFI: чтобы после установки машина шла в систему.
//!
//! # Что было не так
//!
//! Установщик клал загрузчик в `\EFI\BOOT\BOOT*.EFI` и на этом останавливался,
//! полагаясь на «запасной путь» прошивки: раздел, на котором лежит файл с таким
//! именем, считается загрузочным. Путь этот и правда работает — но он
//! **последний** в порядке перебора. Прошивка сначала идёт по своим записям
//! `Boot####`, а первой среди них у только что установленной машины стоит
//! носитель, с которого запускали установщик. Он никуда не делся, и машина
//! послушно загружала установщик снова.
//!
//! Со стороны это выглядит как «установка не сработала». Ровно так это и
//! выглядело на VirtualBox в релизе `v0.1.57`.
//!
//! # Что делается вместо этого
//!
//! То же, что делает всякий чужой установщик: заводится своя запись `Boot####`
//! и ставится первой в `BootOrder`. Запись — это описание («FreeOS»), путь
//! устройства до файла загрузчика и флаг «активна».
//!
//! # Почему путь строится байтами
//!
//! Путь устройства до раздела — это путь **носителя** плюс узел `HardDrive`,
//! который называет раздел его GUID из GPT. GUID мы только что записали сами,
//! то есть знаем; а вот дождаться, пока прошивка перечитает таблицу разделов и
//! заведёт хендл для нового раздела, — это надежда, а не действие. Поэтому узел
//! собирается вручную: три поля заголовка и содержимое, как это описано в UEFI
//! 2.10, §10.3.6. Так же (руками, по трём полям заголовка) разбирает пути
//! соседний модуль [`crate::disks`].
//!
//! # Отказ здесь не отменяет установку
//!
//! Прошивка вправе не дать записать переменную — например, если хранилище
//! переполнено или защищено. Система от этого не перестаёт быть установленной:
//! остаётся тот самый запасной путь `\EFI\BOOT\BOOT*.EFI`, ради которого файл
//! туда и кладётся. Поэтому отказ — это строка в журнале и подсказка человеку
//! («извлеките носитель»), а не прерванная установка.

use alloc::vec::Vec;

use disk::gpt;
use disk::guid::Guid;
use uefi::runtime::{self, VariableAttributes, VariableVendor};
use uefi::{CStr16, CString16, Status};

use crate::disks::{self, Disk};
use crate::logln;

/// Имя, под которым запись видна в меню прошивки.
const DESCRIPTION: &str = "FreeOS";

/// Путь к загрузчику на ESP — тот самый запасной, который кладёт установщик.
///
/// Записывать в запись именно его, а не своё имя, — решение: два имени
/// означали бы два файла, и обновление, заменившее один, оставило бы второй
/// прежним.
const LOADER: &str = if cfg!(target_arch = "x86_64") {
    "\\EFI\\BOOT\\BOOTX64.EFI"
} else {
    "\\EFI\\BOOT\\BOOTAA64.EFI"
};

/// Флаг `LOAD_OPTION_ACTIVE` (UEFI 2.10, §3.1.3).
const LOAD_OPTION_ACTIVE: u32 = 0x0000_0001;

/// Атрибуты переменных загрузки: пережить выключение, быть видимыми и до, и
/// после выхода из boot services.
const BOOT_VARIABLE: VariableAttributes = VariableAttributes::from_bits_truncate(
    VariableAttributes::NON_VOLATILE.bits()
        | VariableAttributes::BOOTSERVICE_ACCESS.bits()
        | VariableAttributes::RUNTIME_ACCESS.bits(),
);

/// Сколько номеров `Boot####` перебирать в поисках свободного.
///
/// Двести пятьдесят шесть: у машины, где заняты все они, что-то не так с
/// прошивкой, а перебирать все 65536 значит делать 65536 обращений к
/// хранилищу переменных ради записи, которую можно не заводить вовсе.
const MAX_SLOTS: u16 = 0x100;

/// Завести запись о загрузке установленной системы и поставить её первой.
///
/// Возвращает номер записи. Ошибку не поднимает выше: см. заголовок модуля.
pub fn register(target: &Disk, esp: gpt::Range, esp_guid: Guid) -> Option<u16> {
    let Some(disk_path) = disks::device_path_bytes(target.handle) else {
        logln!("[install] the target disk has no device path; no boot entry made");
        return None;
    };

    let path = build_path(&disk_path, esp, esp_guid)?;
    let option = build_option(&path);

    // Своя прежняя запись переиспользуется, а не добавляется рядом. Иначе
    // каждая переустановка оставляла бы в меню прошивки ещё одну строку
    // «FreeOS», и через три установки человек выбирал бы из четырёх.
    let slot = existing_slot().or_else(free_slot)?;
    let name = variable_name(slot);
    if let Err(err) = runtime::set_variable(&name, &VariableVendor::GLOBAL_VARIABLE, BOOT_VARIABLE, &option) {
        logln!("[install] the firmware refused Boot{slot:04X}: {:?}", err.status());
        return None;
    }
    logln!(
        "[install] boot entry Boot{slot:04X} -> {LOADER} on the ESP ({} bytes)",
        option.len()
    );

    if !put_first(slot) {
        // Запись есть, но порядок не поменялся: машина, скорее всего, всё
        // равно загрузится (запись новая и обычно оказывается впереди), но
        // обещать этого нельзя.
        logln!("[install] BootOrder was not updated; the firmware may still prefer the medium");
    }
    Some(slot)
}

/// Путь устройства: носитель + раздел + файл.
fn build_path(disk_path: &[u8], esp: gpt::Range, esp_guid: Guid) -> Option<Vec<u8>> {
    /// Завершающий узел: тип 0x7F, подтип 0xFF, длина 4.
    const END: [u8; 4] = [0x7F, 0xFF, 0x04, 0x00];
    /// Тип узла «носитель» (Media Device Path).
    const MEDIA: u8 = 0x04;
    /// Подтип `HardDrive`.
    const SUB_HARD_DRIVE: u8 = 0x01;
    /// Подтип `File Path`.
    const SUB_FILE_PATH: u8 = 0x04;
    /// Разметка — GPT (а не MBR).
    const FORMAT_GPT: u8 = 0x02;
    /// Подпись раздела — GUID (а не четыре байта MBR).
    const SIGNATURE_GUID: u8 = 0x02;
    /// Длина узла `HardDrive`: заголовок 4, номер 4, начало 8, размер 8,
    /// подпись 16, формат 1, тип подписи 1.
    const HARD_DRIVE_LEN: u16 = 42;

    // У пути носителя в конце стоит завершающий узел; в середине длинного пути
    // его быть не может.
    if disk_path.len() < END.len() {
        return None;
    }
    let mut path = Vec::new();
    path.extend_from_slice(&disk_path[..disk_path.len() - END.len()]);

    path.push(MEDIA);
    path.push(SUB_HARD_DRIVE);
    path.extend_from_slice(&HARD_DRIVE_LEN.to_le_bytes());
    // ESP — первый раздел таблицы, и создаём её здесь мы (см. `install::run`).
    path.extend_from_slice(&1u32.to_le_bytes());
    path.extend_from_slice(&esp.first_lba.to_le_bytes());
    path.extend_from_slice(&esp.sectors().to_le_bytes());
    path.extend_from_slice(&esp_guid.to_bytes());
    path.push(FORMAT_GPT);
    path.push(SIGNATURE_GUID);

    // Имя файла — UTF-16 с завершающим нулём, как всё в UEFI.
    let name: Vec<u16> = LOADER.encode_utf16().chain(core::iter::once(0)).collect();
    let file_len = u16::try_from(4 + name.len() * 2).ok()?;
    path.push(MEDIA);
    path.push(SUB_FILE_PATH);
    path.extend_from_slice(&file_len.to_le_bytes());
    for unit in name {
        path.extend_from_slice(&unit.to_le_bytes());
    }

    path.extend_from_slice(&END);
    Some(path)
}

/// Собрать `EFI_LOAD_OPTION` (UEFI 2.10, §3.1.3).
fn build_option(path: &[u8]) -> Vec<u8> {
    let mut option = Vec::new();
    option.extend_from_slice(&LOAD_OPTION_ACTIVE.to_le_bytes());
    // Длина **только** пути устройства: описание идёт до неё и меряется своим
    // завершающим нулём, а необязательные данные — после, и их длина выводится
    // из размера переменной. Ошибиться здесь значит получить запись, которую
    // прошивка молча пропустит.
    option.extend_from_slice(&(path.len() as u16).to_le_bytes());
    for unit in DESCRIPTION.encode_utf16().chain(core::iter::once(0)) {
        option.extend_from_slice(&unit.to_le_bytes());
    }
    option.extend_from_slice(path);
    option
}

/// Имя переменной: `Boot0001` и так далее — четыре шестнадцатеричных знака.
fn variable_name(slot: u16) -> CString16 {
    let digits = b"0123456789ABCDEF";
    let mut text = [0u16; 9];
    for (at, byte) in b"Boot".iter().enumerate() {
        text[at] = u16::from(*byte);
    }
    for index in 0..4 {
        let shift = 12 - index * 4;
        text[4 + index] = u16::from(digits[((slot >> shift) & 0xF) as usize]);
    }
    // Завершающий ноль уже на месте: массив создан нулями.
    CStr16::from_u16_with_nul(&text)
        .map(CString16::from)
        .unwrap_or_else(|_| CString16::try_from("Boot0000").expect("восемь букв ASCII"))
}

/// Найти свою прежнюю запись — ту, у которой описание «FreeOS».
fn existing_slot() -> Option<u16> {
    let wanted: Vec<u16> = DESCRIPTION.encode_utf16().chain(core::iter::once(0)).collect();
    for slot in 0..MAX_SLOTS {
        let name = variable_name(slot);
        let Ok((data, _)) = runtime::get_variable_boxed(&name, &VariableVendor::GLOBAL_VARIABLE)
        else {
            continue;
        };
        // Описание начинается после четырёх байт атрибутов и двух байт длины
        // пути и заканчивается нулём.
        if data.len() < 6 + wanted.len() * 2 {
            continue;
        }
        let matches = wanted.iter().enumerate().all(|(index, unit)| {
            let at = 6 + index * 2;
            u16::from_le_bytes([data[at], data[at + 1]]) == *unit
        });
        if matches {
            logln!("[install] reusing the boot entry Boot{slot:04X}");
            return Some(slot);
        }
    }
    None
}

/// Первый номер, которым никто не пользуется.
fn free_slot() -> Option<u16> {
    (0..MAX_SLOTS).find(|slot| {
        let name = variable_name(*slot);
        matches!(
            runtime::get_variable_boxed(&name, &VariableVendor::GLOBAL_VARIABLE),
            Err(err) if err.status() == Status::NOT_FOUND
        )
    })
}

/// Поставить запись первой в `BootOrder`, сохранив остальные.
fn put_first(slot: u16) -> bool {
    let name = CString16::try_from("BootOrder").expect("девять букв ASCII");
    let mut order: Vec<u16> = alloc::vec![slot];
    if let Ok((data, _)) = runtime::get_variable_boxed(&name, &VariableVendor::GLOBAL_VARIABLE) {
        for pair in data.chunks_exact(2) {
            let existing = u16::from_le_bytes([pair[0], pair[1]]);
            // Свой номер не дублируется: повторная установка иначе добавляла бы
            // его в список каждый раз.
            if existing != slot {
                order.push(existing);
            }
        }
    }

    let mut bytes = Vec::with_capacity(order.len() * 2);
    for entry in &order {
        bytes.extend_from_slice(&entry.to_le_bytes());
    }
    match runtime::set_variable(&name, &VariableVendor::GLOBAL_VARIABLE, BOOT_VARIABLE, &bytes) {
        Ok(()) => {
            logln!("[install] BootOrder: FreeOS first, {} entry(ies) total", order.len());
            true
        }
        Err(err) => {
            logln!("[install] cannot write BootOrder: {:?}", err.status());
            false
        }
    }
}
