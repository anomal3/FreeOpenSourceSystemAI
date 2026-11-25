//! Значки темы для программ (22.11): «Файлы», окно выбора файла.
//!
//! Тема — та же, что у стола: ядро отдаёт её номер в `SysInfo::flags`
//! (`user_abi::sysinfo_icon_theme`), каркас [`crate::app`] зовёт
//! [`set_theme`] при запуске и при смене на ходу. Значки лежат пачкой
//! `/usr/share/icons/<тема>.pak` (формат — `mini_ui::image::pack_find`):
//! читается оглавление, а значок — по требованию, своим куском файла, так что
//! в памяти программы только то, что она показала.
//!
//! Раньше «Файлы» и окно выбора файла несли свои копии значков 64 точек
//! внутри программы; теперь значки одни на систему и меняются темой.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;

use mini_ui::image::{self, Image};
use mini_ui::{Rect, Surface};

use crate::{SEEK_SET, close, open, read, seek};

/// Оглавление пачки (`None` — файла нет) и разобранные значки.
struct Pack {
    theme: u8,
    index: Option<Vec<u8>>,
    /// Имя, сторона, под которую разобран, и картинка (один размер).
    cache: Vec<(String, u32, Option<Image>)>,
}

/// Программа однопоточная: состояние живёт в ячейке без замка.
struct Cell(RefCell<Vec<Pack>>);

// SAFETY: программы третьего кольца однопоточны — к ячейке ходит один поток.
unsafe impl Sync for Cell {}

static PACKS: Cell = Cell(RefCell::new(Vec::new()));

/// Самое большое оглавление, которое стоит читать.
const INDEX_LIMIT: usize = 256 * 1024;
/// Самый большой значок (все размеры вместе).
const ENTRY_LIMIT: usize = 512 * 1024;

/// Сменить тему значков (номер из `SysInfo::flags`). `true` — сменилась.
pub fn set_theme(theme: u8) -> bool {
    let changed = image::set_icon_theme(theme);
    if changed {
        // Куча программы мала: значки прежней темы больше не нужны.
        PACKS.0.borrow_mut().clear();
    }
    changed
}

/// Прочитать ровно `buffer.len()` байт (короткое чтение — дочитать).
fn read_exact(fd: i64, buffer: &mut [u8]) -> bool {
    let mut done = 0;
    while done < buffer.len() {
        let got = read(fd, &mut buffer[done..]);
        if got <= 0 {
            return false;
        }
        done += got as usize;
    }
    true
}

/// Оглавление пачки темы `theme`.
fn load_index(theme: u8) -> Option<Vec<u8>> {
    let id = image::ICON_THEMES.get(usize::from(theme))?.0;
    let fd = open(&image::icon_pack_path(id));
    if fd < 0 {
        return None;
    }
    let mut head = [0u8; image::PACK_HEAD];
    let index = if read_exact(fd, &mut head) {
        image::pack_index_end(&head).filter(|end| *end >= image::PACK_HEAD && *end <= INDEX_LIMIT).and_then(|end| {
            let mut index = vec![0u8; end];
            index[..image::PACK_HEAD].copy_from_slice(&head);
            read_exact(fd, &mut index[image::PACK_HEAD..]).then_some(index)
        })
    } else {
        None
    };
    close(fd);
    index
}

/// Данные значка из файла пачки по оглавлению.
fn load_entry(theme: u8, index: &[u8], name: &str) -> Option<Vec<u8>> {
    let (offset, size) = image::pack_find(index, name)?;
    if size == 0 || size > ENTRY_LIMIT {
        return None;
    }
    let id = image::ICON_THEMES.get(usize::from(theme))?.0;
    let fd = open(&image::icon_pack_path(id));
    if fd < 0 {
        return None;
    }
    let mut data = vec![0u8; size];
    let ok = seek(fd, offset as i64, SEEK_SET) == offset as i64 && read_exact(fd, &mut data);
    close(fd);
    ok.then_some(data)
}

/// Нарисовать значок `name` темы `theme` в `rect`. `None` — такого нет.
fn draw_from(theme: u8, s: &mut Surface, rect: Rect, name: &str) -> Option<()> {
    let mut packs = PACKS.0.borrow_mut();
    let at = match packs.iter().position(|pack| pack.theme == theme) {
        Some(at) => at,
        None => {
            packs.push(Pack { theme, index: load_index(theme), cache: Vec::new() });
            packs.len() - 1
        }
    };
    let pack = &mut packs[at];
    let index = pack.index.as_deref()?;
    let side = rect.w.max(rect.h);
    let slot = match pack.cache.iter().position(|(known, wanted, _)| known == name && *wanted == side) {
        // The last used goes last: the cache drops from the front.
        Some(slot) => {
            let entry = pack.cache.remove(slot);
            pack.cache.push(entry);
            pack.cache.len() - 1
        }
        None => {
            // Только нужный размер: со всеми значок весит под сто килобайт.
            let parsed = load_entry(theme, index, name).and_then(|bytes| Image::parse_for(&bytes, side));
            // The cache is bounded (24.11): every size stayed for ever, and
            // after a few `ui scale` changes (1.25 → 1.5 → 2) «Файлы» ran out
            // of their megabyte heap at 200 % («user: panic», exit 101).
            let room = cache_budget().saturating_sub(parsed.as_ref().map_or(0, image_bytes));
            while !pack.cache.is_empty() && cached_bytes(&pack.cache) > room {
                pack.cache.remove(0);
            }
            pack.cache.push((String::from(name), side, parsed));
            pack.cache.len() - 1
        }
    };
    pack.cache[slot].2.as_ref()?.draw(s, rect);
    Some(())
}

/// Bytes a parsed icon keeps (one size, `R G B A`).
fn image_bytes(image: &Image) -> usize {
    image.width as usize * image.height as usize * 4
}

/// Bytes the cache of one pack keeps.
fn cached_bytes(cache: &[(String, u32, Option<Image>)]) -> usize {
    cache.iter().filter_map(|(_, _, image)| image.as_ref()).map(image_bytes).sum()
}

/// How much the parsed icons may take: a quarter of the program heap.
fn cache_budget() -> usize {
    crate::heap_capacity() / 4
}

/// Нарисовать значок `name` текущей темы (нет в ней — темы по умолчанию).
/// `false` — значка нет нигде: рисовать свою замену.
pub fn draw(s: &mut Surface, rect: Rect, name: &str) -> bool {
    let theme = image::icon_theme();
    draw_from(theme, s, rect, name).is_some() || (theme != 0 && draw_from(0, s, rect, name).is_some())
}
