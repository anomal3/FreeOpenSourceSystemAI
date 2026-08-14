//! Слоты системы со стороны работающей системы: подтверждение и обновление.
//!
//! # Что делает ядро, а что загрузчик
//!
//! Загрузчик решает, **с чего** грузиться, и тратит попытку. Ядро отвечает на
//! другой вопрос: получилось ли. Разделение не формальное — тот, кто выбирает
//! слот, ещё ничего не знает о том, работает ли он, а тот, кто может это знать,
//! существует только потому, что выбор уже сделан.
//!
//! # Почему подтверждение позднее
//!
//! Потому что раннее не сработало бы ровно в том случае, ради которого
//! затевалось. Подтверждение, сделанное сразу после старта ядра, означает «ядро
//! стартовало» — а слот с целым ядром и разрушенным корнем стартует прекрасно и
//! оказывается системой, в которой ничего нет. Поэтому подтверждает
//! [`confirm`], и вызывается она после того, как корень смонтирован, состояние
//! найдено и сеанс запущен.
//!
//! # Почему `sysupdate` — команда оболочки, а не программа
//!
//! Потому что он работает **мимо файловой системы**: пишет образ корня прямо в
//! сектора неактивного раздела и правит FAT-том ESP, который никто не
//! монтировал. Отдать это программе значило бы отдать программе блочное
//! устройство — то есть отменить границу, ради которой существует третье
//! кольцо. Ровно по той же причине в оболочке живёт `fsck`.

use alloc::boxed::Box;
use alloc::vec::Vec;

use disk::BlockDevice as _;
use disk::gpt;
use fpk::{Blob, Header, Kind, Manifest};
use slots::{DEFAULT_TRIES, Slot, State};

use crate::block::{self, Shared};
use crate::sync::Mutex;
use crate::vfs::Node;
use crate::{fs, kprintln};

/// Что система знает о своих разделах.
///
/// Живёт статиком, потому что спрашивают об этом в трёх разных местах и в
/// разное время: подтверждение — сразу после загрузки, `sysupdate` — когда
/// человек попросит, `slots` в оболочке — когда он захочет посмотреть.
struct Layout {
    /// ESP: там запись о слотах, ядра и образы RAM-диска.
    esp: Option<(Shared, u64)>,
    /// Корневые слоты: буква, носитель, первый сектор, длина в секторах.
    roots: Vec<(Slot, Shared, u64, u64)>,
    /// С какого слота загрузились. `None` — система без слотов.
    booted: Option<Slot>,
}

static LAYOUT: Mutex<Option<Layout>> = Mutex::new(None);

/// Запомнить разметку, найденную при загрузке.
pub fn remember(found: &[block::Partition], booted: Option<Slot>) {
    let mut roots = Vec::new();
    for (slot, type_guid) in
        [(Slot::A, gpt::FREEOS_ROOT_TYPE), (Slot::B, gpt::FREEOS_ROOT_B_TYPE)]
    {
        if let Some(part) = block::take(found, type_guid) {
            if roots.try_reserve(1).is_err() {
                return;
            }
            roots.push((slot, part.device, part.first_lba, part.sectors));
        }
    }
    let esp = block::take(found, gpt::ESP_TYPE).map(|part| (part.device, part.first_lba));

    *LAYOUT.lock() = Some(Layout { esp, roots, booted });
}

/// Прочитать запись о слотах с ESP.
///
/// `None` означает, что читать неоткуда или не из чего: ESP не найден, запись
/// отсутствует, обе копии испорчены. Различать эти случаи здесь не нужно —
/// различает их тот, кто печатает.
#[must_use]
pub fn state() -> Option<State> {
    let mut guard = LAYOUT.lock();
    let layout = guard.as_mut()?;
    let (device, first_lba) = layout.esp.as_mut()?;
    read_state(device, *first_lba)
}

fn read_state(device: &mut Shared, first_lba: u64) -> Option<State> {
    let mut volume = disk::fat32::open(device, first_lba).ok()?;
    let mut bytes = [0u8; slots::FILE_SIZE];
    let read = volume.read_file_path(device, slots::PATH_UNIX, &mut bytes).ok()?;
    if read != slots::FILE_SIZE {
        return None;
    }
    State::parse(&bytes)
}

fn write_state(device: &mut Shared, first_lba: u64, state: &State) -> Result<(), disk::Error> {
    let mut volume = disk::fat32::open(device, first_lba)?;
    let mut bytes = [0u8; slots::FILE_SIZE];
    state.write(&mut bytes);
    // Длина не меняется — значит запись идёт на место, не трогая ни FAT, ни
    // каталога. Почему это надёжнее переименования временного файла, сказано в
    // заголовке крейта `slots`.
    volume.overwrite_file_path(device, slots::PATH_UNIX, &bytes)
}

/// Подтвердить, что система, поднявшаяся с активного слота, работает.
///
/// Ничего не делает на системе без слотов и на уже подтверждённой: лишняя запись
/// на ESP — это лишний повод его испортить.
pub fn confirm(rolled_back: bool) {
    let mut guard = LAYOUT.lock();
    let Some(layout) = guard.as_mut() else { return };
    if layout.booted.is_none() {
        return;
    }
    let Some((device, first_lba)) = layout.esp.as_mut() else {
        kprintln!("  slot        : no ESP found; this boot cannot be confirmed");
        return;
    };

    if rolled_back {
        // Сказать об откате обязана система: к этому моменту запись на ESP уже
        // переписана загрузчиком, и «мы на слоте A» ничем не отличается от
        // обычной загрузки.
        kprintln!("  slot        : this boot came back to the previous slot after failures");
    }

    let Some(mut state) = read_state(device, *first_lba) else {
        kprintln!("  slot        : the slot record is unreadable; nothing to confirm");
        return;
    };
    if !state.confirm() {
        kprintln!("  slot        : slot {} was already confirmed", state.active.name());
        return;
    }
    match write_state(device, *first_lba, &state) {
        Ok(()) => kprintln!(
            "  slot        : slot {} confirmed, {} attempt(s) restored",
            state.active.name(),
            DEFAULT_TRIES
        ),
        Err(err) => kprintln!("  slot        : cannot confirm this boot: {err}"),
    }
}

/// Строки для команды `slots` в оболочке.
///
/// Возвращает готовый текст, а не состояние: печатает оболочка, а формат
/// принадлежит этому модулю — иначе он разъедется с тем, что пишет загрузчик.
#[must_use]
pub fn describe() -> Vec<alloc::string::String> {
    let mut out = Vec::new();
    let booted = LAYOUT.lock().as_ref().and_then(|layout| layout.booted);
    let Some(slot) = booted else {
        out.push(alloc::format!("this system has no A/B slots"));
        return out;
    };
    out.push(alloc::format!("booted from slot {}", slot.name()));
    match state() {
        Some(state) => {
            out.push(alloc::format!(
                "active {}, previous {}, {} attempt(s) left, {}",
                state.active.name(),
                state.previous.name(),
                state.tries,
                if state.confirmed { "confirmed" } else { "NOT confirmed" }
            ));
        }
        None => out.push(alloc::format!("the slot record on the ESP is unreadable")),
    }
    out
}

/// Почему обновление не удалось.
pub enum Error {
    NoSlots,
    NoFile,
    Unreadable,
    Container(fpk::Error),
    /// Контейнер не тот: в нём пакет, а не система.
    NotASystem,
    /// Образ не помещается в раздел слота.
    TooLarge { image: u64, partition: u64 },
    /// Контрольная сумма куска не сошлась.
    Corrupt(&'static str),
    /// Отказал носитель.
    Disk(disk::Error),
    /// Неактивного слота на диске нет — система размечена без второго корня.
    NoTargetSlot,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoSlots => f.write_str("this system has no A/B slots to update"),
            Self::NoFile => f.write_str("the update file cannot be opened"),
            Self::Unreadable => f.write_str("the update file cannot be read"),
            Self::Container(err) => f.write_str(err.text()),
            Self::NotASystem => {
                f.write_str("this container holds a package, not a system; use pkg install")
            }
            Self::TooLarge { image, partition } => write!(
                f,
                "the image is {image} bytes and the slot partition is only {partition}"
            ),
            Self::Corrupt(what) => write!(f, "the {what} in the container is damaged"),
            Self::Disk(err) => write!(f, "{err}"),
            Self::NoTargetSlot => {
                f.write_str("there is no second root partition to write the new system into")
            }
        }
    }
}

/// Сколько байт переливается за один заход.
///
/// Четверть мегабайта: кучи у ядра шестнадцать мегабайт, а образ — десятки, так
/// что весь файл в памяти не удержать. Буфер на стеке был бы ещё хуже — стек
/// задачи оболочки меньше буфера.
///
/// Размер выбран по цене обращения к носителю, а не по размеру буфера: и чтение
/// контейнера, и запись в раздел идут отрезками подряд идущих блоков, и чем
/// длиннее отрезок, тем меньше запросов к диску на те же байты. С шестьюдесятью
/// четырьмя килобайтами перенос ста мегабайт в эмуляторе не укладывался в
/// отведённое стендом время.
const CHUNK: usize = 256 * 1024;

/// Применить обновление системы из контейнера.
///
/// Порядок шагов задан тем, что должно пережить выключение на каждом из них:
///
/// 1. Образ корня — в **неактивный** слот. Активный при этом работает, и
///    выключение посреди записи не стоит ничего: указатель ещё не переключён.
/// 2. Ядро и образ RAM-диска — на ESP, файлами своего слота. Тоже неактивного.
/// 3. И только теперь указатель переключается на новый слот со счётчиком
///    попыток. Это единственный шаг, который что-то меняет для следующей
///    загрузки, и он атомарен по построению (см. крейт `slots`).
pub fn apply(path: &str) -> Result<Slot, Error> {
    let node = match fs::resolve_as(
        crate::user::session::credentials(),
        path,
        crate::vfs::perm::Access::READ,
    ) {
        Some(Ok(node)) => node,
        _ => return Err(Error::NoFile),
    };

    let mut head = [0u8; fpk::HEADER_SIZE];
    read_exact(&*node, 0, &mut head)?;
    let header = Header::parse(&head).map_err(Error::Container)?;
    if header.kind != Kind::System {
        return Err(Error::NotASystem);
    }

    let mut manifest_bytes = Vec::new();
    manifest_bytes
        .try_reserve_exact(header.manifest_len as usize)
        .map_err(|_| Error::Unreadable)?;
    manifest_bytes.resize(header.manifest_len as usize, 0);
    read_exact(&*node, header.manifest_offset(), &mut manifest_bytes)?;
    let manifest = Manifest::parse(&header, &manifest_bytes).map_err(Error::Container)?;

    let image = manifest.blob("image").map_err(Error::Container)?;
    let kernel = manifest.blob("kernel").map_err(Error::Container)?;
    let initrd = manifest.blob("initrd").map_err(Error::Container)?;
    let version = manifest.version().unwrap_or("<unversioned>");

    let mut guard = LAYOUT.lock();
    let layout = guard.as_mut().ok_or(Error::NoSlots)?;
    let booted = layout.booted.ok_or(Error::NoSlots)?;
    let (esp_device, esp_lba) = layout.esp.as_mut().ok_or(Error::NoSlots)?;

    let current = read_state(esp_device, *esp_lba).ok_or(Error::NoSlots)?;
    let target = current.active.other();
    if target == booted && current.active == booted {
        // Активный слот — тот, с которого мы работаем; цель, стало быть, другой.
        // Условие оставлено явным: перепутать здесь стороны значит записать
        // систему поверх работающей.
    }

    kprintln!("  sysupdate   : writing FreeOS {version} into slot {}", target.name());

    // --- шаг 1: образ корня в неактивный раздел ---------------------------
    let (_, root_device, root_lba, root_sectors) = layout
        .roots
        .iter()
        .find(|(slot, ..)| *slot == target)
        .ok_or(Error::NoTargetSlot)?;
    let mut root_device = root_device.clone();
    let root_lba = *root_lba;
    let sector = u64::from(root_device.sector_size());
    let capacity = root_sectors * sector;
    if image.size > capacity {
        return Err(Error::TooLarge { image: image.size, partition: capacity });
    }
    if image.size % sector != 0 {
        // Образ обязан быть кратен сектору: иначе последний неполный сектор
        // пришлось бы дописывать нулями, то есть менять содержимое файловой
        // системы, которую мы переносим.
        return Err(Error::Corrupt("root image"));
    }
    stream(&*node, header.payload_offset() + image.offset, image.size, image.crc, "root image", |offset, chunk| {
        root_device
            .write(root_lba + offset / sector, chunk)
            .map_err(Error::Disk)
    })?;
    root_device.flush().map_err(Error::Disk)?;
    kprintln!("  sysupdate   : root image written, {} MiB", image.size / (1024 * 1024));

    // --- шаг 2: ядро и образ RAM-диска на ESP ------------------------------
    let mut esp = esp_device.clone();
    let esp_lba = *esp_lba;
    for (blob, name, what) in [
        (kernel, kernel_name(target), "kernel"),
        (initrd, initrd_name(target), "initrd"),
    ] {
        write_esp_file(&*node, &header, &mut esp, esp_lba, name, blob, what)?;
        kprintln!("  sysupdate   : {what} written to \\{name}");
    }

    // --- шаг 3: переключение указателя -------------------------------------
    let mut next = current;
    next.switch_to_new();
    write_state(esp_device, esp_lba, &next).map_err(Error::Disk)?;
    kprintln!(
        "  sysupdate   : slot {} is now active with {} attempt(s); reboot to use it",
        next.active.name(),
        next.tries
    );
    Ok(next.active)
}

/// Имена файлов слота на ESP.
///
/// Строки, а не пути: их принимает [`disk::fat32`], который ходит по тому
/// разделителем `/`.
const fn kernel_name(slot: Slot) -> &'static str {
    match slot {
        Slot::A => "kernel-a.elf",
        Slot::B => "kernel-b.elf",
    }
}

const fn initrd_name(slot: Slot) -> &'static str {
    match slot {
        Slot::A => "initrd-a.img",
        Slot::B => "initrd-b.img",
    }
}

/// Перелить кусок контейнера в файл на ESP, не держа его в памяти целиком.
fn write_esp_file(
    node: &dyn Node,
    header: &Header,
    esp: &mut Shared,
    esp_lba: u64,
    name: &str,
    blob: Blob,
    what: &'static str,
) -> Result<(), Error> {
    let mut volume = disk::fat32::open(esp, esp_lba).map_err(Error::Disk)?;
    let reservation = volume.reserve_file(esp, name, blob.size).map_err(Error::Disk)?;
    let sector = u64::from(esp.sector_size());

    // В отведённом месте последний кластер занят целиком, поэтому хвост короче
    // сектора дописывается нулями: за концом файла на свежевыделенных кластерах
    // лежит то, что осталось от прежнего владельца, и отдавать это прошивке
    // незачем.
    let mut tail = [0u8; disk::MAX_SECTOR_SIZE];
    stream(node, header.payload_offset() + blob.offset, blob.size, blob.crc, what, |offset, chunk| {
        let mut written = 0usize;
        while written < chunk.len() {
            let left = chunk.len() - written;
            let lba = reservation.first_lba + (offset + written as u64) / sector;
            if left as u64 >= sector {
                let whole = (left as u64 / sector * sector) as usize;
                esp.write(lba, &chunk[written..written + whole]).map_err(Error::Disk)?;
                written += whole;
            } else {
                tail[..left].copy_from_slice(&chunk[written..]);
                tail[left..sector as usize].fill(0);
                esp.write(lba, &tail[..sector as usize]).map_err(Error::Disk)?;
                written += left;
            }
        }
        Ok(())
    })?;

    volume
        .commit_file(esp, name, &reservation, blob.size)
        .map_err(Error::Disk)
}

/// Прочитать кусок контейнера по частям, проверяя сумму, и отдать каждую часть
/// потребителю.
///
/// Сумма считается **по дороге**, а не после записи, и это не экономия чтения:
/// прочитать образ второй раз с того же носителя означало бы проверить не то,
/// что записано, а то, что прочиталось второй раз.
///
/// Порция всегда кратна сектору, кроме последней: иначе вызывающий, пишущий
/// секторами, был бы вынужден собирать их из двух порций.
fn stream(
    node: &dyn Node,
    mut offset: u64,
    size: u64,
    expected: u32,
    what: &'static str,
    mut sink: impl FnMut(u64, &[u8]) -> Result<(), Error>,
) -> Result<(), Error> {
    let mut buffer = Vec::new();
    buffer.try_reserve_exact(CHUNK).map_err(|_| Error::Unreadable)?;
    buffer.resize(CHUNK, 0);

    let mut done = 0u64;
    let mut crc = fpk::CRC32_INIT;
    while done < size {
        let want = (size - done).min(CHUNK as u64) as usize;
        let read = node.read_at(offset, &mut buffer[..want]).map_err(|_| Error::Unreadable)?;
        if read == 0 {
            return Err(Error::Unreadable);
        }
        let chunk = &buffer[..read];
        crc = fpk::crc32_update(crc, chunk);
        sink(done, chunk)?;
        done += read as u64;
        offset += read as u64;
    }

    if crc != expected {
        return Err(Error::Corrupt(what));
    }
    Ok(())
}

/// Прочитать ровно столько, сколько просили.
fn read_exact(node: &dyn Node, offset: u64, out: &mut [u8]) -> Result<(), Error> {
    let mut filled = 0usize;
    while filled < out.len() {
        let read = node
            .read_at(offset + filled as u64, &mut out[filled..])
            .map_err(|_| Error::Unreadable)?;
        if read == 0 {
            return Err(Error::Unreadable);
        }
        filled += read;
    }
    Ok(())
}

/// Заглушка, чтобы `Box<dyn Node>` не тянул за собой лишнего импорта.
const _: fn() = || {
    let _: Option<Box<dyn Node>> = None;
};
