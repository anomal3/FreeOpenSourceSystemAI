//! Ключи SSH для живой системы — `\FREEOS\authorized_keys` (фаза 68o).
//!
//! У живого образа нет учётных записей, и его сервер SSH не пускает никого.
//! Единственный вход — ключ, который человек положил на флешку: на загрузочную
//! (если записал её копированием файлов) или на любую другую, воткнутую рядом.
//! Ищет загрузчик, а не ядро: прошивка читает флешки на любом контроллере USB,
//! а драйверы ядра — пока не на всех.
//!
//! Имён два: `authorized_keys` — как привык человек, и `SSHKEYS` — в 8.3,
//! так его кладёт `cargo xtask image --live-key` (писатель FAT32 сборки длинных
//! имён не умеет).
//!
//! Тома перебираются все, первый найденный файл и берётся. Тома открываются
//! `GetProtocol`, без захвата: захват отключил бы от устройства потребителей
//! прошивки (урок ASUS K53SD — см. `graphics.rs`).

use boot_info::{BootInfo, LIVE_KEYS_MAX};
use uefi::boot::{self, OpenProtocolAttributes, OpenProtocolParams, SearchType};
use uefi::proto::media::file::{File, FileAttribute, FileMode, FileType};
use uefi::proto::media::fs::SimpleFileSystem;
use uefi::{Identify, println};

/// Найти ключи и положить их в `info`. Молчит, если файла нет нигде.
pub fn find(info: &mut BootInfo) {
    let Ok(handles) = boot::locate_handle_buffer(SearchType::ByProtocol(&SimpleFileSystem::GUID)) else {
        return;
    };
    for (index, handle) in handles.iter().enumerate() {
        let params = OpenProtocolParams { handle: *handle, agent: boot::image_handle(), controller: None };
        // SAFETY: `GetProtocol` не делает загрузчик потребителем устройства;
        // протокол используется только здесь и закрывается в конце итерации.
        let Ok(mut fs) = (unsafe { boot::open_protocol::<SimpleFileSystem>(params, OpenProtocolAttributes::GetProtocol) })
        else {
            continue;
        };
        let Ok(mut root) = fs.open_volume() else { continue };
        let names = [uefi::cstr16!("\\FREEOS\\authorized_keys"), uefi::cstr16!("\\FREEOS\\SSHKEYS")];
        let Some(handle) = names
            .iter()
            .find_map(|path| root.open(path, FileMode::Read, FileAttribute::empty()).ok())
        else {
            continue;
        };
        let Ok(FileType::Regular(mut file)) = handle.into_type() else { continue };
        let read = match file.read(&mut info.live_keys) {
            Ok(read) => read.min(LIVE_KEYS_MAX),
            Err(_) => continue,
        };
        if read == 0 {
            continue;
        }
        info.live_keys_len = read as u32;
        let lines = info.live_keys[..read]
            .split(|byte| *byte == b'\n')
            .filter(|line| line.starts_with(b"ssh-"))
            .count();
        println!("  [ssh] {lines} key(s) for a live SSH login from volume #{index}");
        return;
    }
}
