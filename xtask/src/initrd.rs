//! Сборка образа RAM-диска: каталог `initrd/` -> FAT32-том `initrd.img`.
//!
//! Образ собирается целиком в памяти (`Cursor<Vec<u8>>`) и одним куском
//! сбрасывается на диск. Так проще, чем работать поверх `std::fs::File`:
//! форматирование и запись каталогов — это множество мелких перемещений по
//! тому, а буферизованного read-write-seek потока в std нет, пришлось бы либо
//! тянуть `fscommon::BufStream`, либо мириться с тысячами сисколлов.

use std::fs;
use std::io::{Cursor, Write as _};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use fatfs::{FatType, FileSystem, FormatVolumeOptions, FsOptions};

use crate::paths;
use crate::util;

/// Размер сектора тома.
///
/// 512 — минимум, разрешённый спецификацией FAT, и ровно то, чего ждёт любой
/// драйвер. Больший сектор здесь ничего не даёт и только поднял бы планку
/// минимального размера тома (см. [`IMAGE_SIZE`]).
const BYTES_PER_SECTOR: u16 = 512;

/// Размер кластера тома — тоже минимально возможный: кластер не бывает меньше
/// сектора. Это не мелочь, а главный рычаг, которым образ удерживается в
/// разумном размере; подробности — в комментарии к [`IMAGE_SIZE`].
const BYTES_PER_CLUSTER: u32 = 512;

/// Размер образа — 40 MiB.
///
/// Столько нужно не под данные (их здесь десятки килобайт), а чтобы файловая
/// система вообще получилась FAT32. Тип FAT определяется исключительно числом
/// кластеров тома (Microsoft FAT specification, раздел «FAT Type
/// Determination»): меньше 4085 кластеров — FAT12, меньше 65525 — FAT16, и
/// только с 65525 начинается FAT32. Флага «сделать FAT32» не существует ни в
/// формате, ни в fatfs: `FormatVolumeOptions::fat_type` там влияет лишь на
/// выбор размера кластера по умолчанию, а итоговый тип всё равно выводится из
/// числа кластеров — поэтому ниже мы задаём кластер явно, а тип проверяем по
/// факту, уже смонтировав том.
///
/// Отсюда и минимальный размер: 65525 кластеров надо чем-то оплатить, и
/// единственный способ сделать том меньше — уменьшить кластер. Мы берём
/// минимально возможный (512 Б = один сектор): при обычных для FAT32 4 KiB
/// минимальный том вышел бы 256 MiB, а с 512 Б — около 32.5 MiB (65525
/// кластеров данных плюс 8 зарезервированных секторов и две копии FAT
/// примерно по 513 секторов каждая).
///
/// 40 MiB — тот же минимум с запасом около 23% (получается 80650 кластеров):
/// круглое число, которое переживёт правку геометрии и не упрётся в границу
/// FAT16 от одного лишнего сектора. Побочная выгода мелкого кластера: файл в
/// 64 KiB занимает 128 кластеров, то есть обход цепочки в драйвере ядра
/// проверяется по-настоящему, а не «в один переход».
const IMAGE_SIZE: usize = 40 * 1024 * 1024;

/// Метка тома: ровно 11 байт, как в записи каталога FAT.
const VOLUME_LABEL: &[u8; 11] = b"FREEOS INIT";

/// Ревизия способа сборки образа; входит в слепок (см. [`stamp_text`]).
///
/// Слепок отслеживает содержимое `initrd/` и геометрию тома, но не код этого
/// модуля: правка вроде смены провайдера времени содержимое не меняет, а
/// байты образа — меняет, и без явного признака устаревший образ так и лежал
/// бы в `build/`. Увеличивайте число при любом изменении того, как образ
/// формируется.
const FORMAT_REVISION: u32 = 3;

/// Метка времени, которая проставляется всем записям каталогов.
///
/// 1980-01-01 — начало эпохи DOS, минимальная представимая дата FAT. Берём
/// фиксированную дату, а не текущее время, по двум причинам. Во-первых, образ
/// остаётся побайтово воспроизводимым: одно и то же содержимое `initrd/` даёт
/// один и тот же `initrd.img`. Во-вторых, штатная альтернатива — фича `chrono`
/// у fatfs, а без неё библиотека пишет не «пустую» дату, а закодированный
/// ноль, то есть месяц 0 и день 0; такую запись сторонние инструменты (7-Zip,
/// например) считают повреждённой.
const TIMESTAMP: fatfs::DateTime = fatfs::DateTime {
    date: fatfs::Date {
        year: 1980,
        month: 1,
        day: 1,
    },
    time: fatfs::Time {
        hour: 0,
        min: 0,
        sec: 0,
        millis: 0,
    },
};

#[derive(Debug)]
struct FixedTimeProvider;

impl fatfs::TimeProvider for FixedTimeProvider {
    fn get_current_date(&self) -> fatfs::Date {
        TIMESTAMP.date
    }

    fn get_current_date_time(&self) -> fatfs::DateTime {
        TIMESTAMP
    }
}

/// fatfs принимает провайдер только как `&'static`.
static FIXED_TIME_PROVIDER: FixedTimeProvider = FixedTimeProvider;

/// Символы, которые FAT запрещает в длинных именах (плюс управляющие).
///
/// Проверяем сами, потому что fatfs на таком имени вернёт малосодержательное
/// `InvalidInput`, а виноват будет файл в `initrd/`, и назвать его надо явно.
const FORBIDDEN_NAME_CHARS: &[char] = &['"', '*', '/', ':', '<', '>', '?', '\\', '|'];

/// Элемент дерева, попадающий в образ.
enum Node {
    Dir,
    File(Vec<u8>),
}

struct Entry {
    /// Путь относительно корня образа, разделитель — `/` (его и понимает fatfs).
    rel: String,
    node: Node,
}

/// Собирает `initrd.img`, если он устарел, и возвращает путь к нему.
/// Собирает `initrd.img`, добавляя к содержимому `initrd/` собранные файлы.
///
/// `extra` — пары «путь внутри образа, путь на хосте». Через них в образ
/// попадают пользовательские программы: держать собранные бинарники в
/// `initrd/` нельзя, они артефакты сборки, а не исходники.
pub fn build(extra: &[(String, PathBuf)]) -> Result<PathBuf> {
    let source = paths::initrd_source_dir();
    if !source.is_dir() {
        bail!(
            "нет каталога с содержимым RAM-диска: {}\n\
             Создайте его и положите внутрь файлы — они попадут в образ рекурсивно.\n\
             Либо соберите без образа: cargo xtask build --no-initrd",
            source.display()
        );
    }

    let mut entries = Vec::new();
    collect(&source, "", &mut entries)?;

    // Каталоги под добавляемые файлы заводятся по пути: `bin/hello` требует,
    // чтобы `bin` уже существовал в образе, а форматтер каталоги сам не создаёт.
    for (rel, path) in extra {
        if let Some((dir, _)) = rel.rsplit_once('/') {
            if !entries.iter().any(|entry| entry.rel == dir) {
                entries.push(Entry { rel: dir.to_string(), node: Node::Dir });
            }
        }
        let data = fs::read(path)
            .with_context(|| format!("не удалось прочитать {}", path.display()))?;
        entries.push(Entry { rel: rel.clone(), node: Node::File(data) });
    }

    let image = paths::initrd_image();
    let stamp_path = paths::initrd_stamp();
    let stamp = stamp_text(&entries);

    // Пересборка только по изменению содержимого. Образ — десятки мегабайт, и
    // формировать его на каждый `run` значит платить заметную паузу за файлы,
    // которые меняются куда реже кода. Слепок хранит размер и хеш каждого
    // файла плюс геометрию тома, так что правка констант выше тоже считается
    // изменением.
    let fresh = util::file_len(&image) == Some(IMAGE_SIZE as u64)
        && fs::read_to_string(&stamp_path).ok().as_deref() == Some(stamp.as_str());
    if fresh {
        println!(
            "initrd: образ актуален, пересборка не нужна ({})",
            image.display()
        );
        return Ok(image);
    }

    let data = format_image(&entries)?;

    if let Some(parent) = image.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("не удалось создать каталог {}", parent.display()))?;
    }
    // Слепок снимается перед записью образа: если запись оборвётся, следующий
    // запуск увидит отсутствующий слепок и пересоберёт образ, а не поверит
    // недописанному файлу.
    let _ = fs::remove_file(&stamp_path);
    fs::write(&image, &data)
        .with_context(|| format!("не удалось записать образ {}", image.display()))?;
    fs::write(&stamp_path, &stamp)
        .with_context(|| format!("не удалось записать слепок {}", stamp_path.display()))?;

    let dirs = entries
        .iter()
        .filter(|e| matches!(e.node, Node::Dir))
        .count();
    let files = entries.len() - dirs;
    println!(
        "initrd: {files} файл(ов), {dirs} каталог(ов) из {} -> {} ({} MiB, FAT32, кластер {BYTES_PER_CLUSTER} Б)",
        source.display(),
        image.display(),
        IMAGE_SIZE / (1024 * 1024),
    );

    Ok(image)
}

/// Рекурсивно читает дерево `initrd/` в плоский список.
///
/// Порядок обхода — «каталог раньше своего содержимого» и по алфавиту внутри
/// каждого каталога. Первое нужно, чтобы при записи в образ можно было
/// обращаться к элементам просто по относительному пути (родитель к этому
/// моменту уже создан), второе — чтобы образ не зависел от того, в каком
/// порядке отдаёт записи файловая система хоста.
fn collect(dir: &Path, prefix: &str, out: &mut Vec<Entry>) -> Result<()> {
    let mut names: Vec<String> = Vec::new();
    let entries = fs::read_dir(dir)
        .with_context(|| format!("не удалось прочитать каталог {}", dir.display()))?;
    for entry in entries {
        let entry =
            entry.with_context(|| format!("не удалось прочитать каталог {}", dir.display()))?;
        let name = entry.file_name().into_string().map_err(|raw| {
            anyhow::anyhow!(
                "имя {:?} в каталоге {} не является корректным UTF-8",
                raw,
                dir.display()
            )
        })?;
        names.push(name);
    }
    names.sort();

    for name in names {
        let path = dir.join(&name);
        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        check_name(&name, &path)?;

        let meta = fs::symlink_metadata(&path)
            .with_context(|| format!("не удалось получить сведения о {}", path.display()))?;
        let file_type = meta.file_type();

        if file_type.is_dir() {
            out.push(Entry {
                rel: rel.clone(),
                node: Node::Dir,
            });
            collect(&path, &rel, out)?;
        } else if file_type.is_file() {
            let data = fs::read(&path)
                .with_context(|| format!("не удалось прочитать {}", path.display()))?;
            out.push(Entry {
                rel,
                node: Node::File(data),
            });
        } else {
            // Симлинки и прочая экзотика: в FAT их представить нечем, а тихо
            // разыменовать — значит положить в образ не то, что видит человек.
            bail!(
                "{}: не обычный файл и не каталог; в FAT такое перенести нельзя",
                path.display()
            );
        }
    }

    Ok(())
}

fn check_name(name: &str, path: &Path) -> Result<()> {
    if let Some(bad) = name.chars().find(|c| FORBIDDEN_NAME_CHARS.contains(c)) {
        bail!(
            "{}: символ {bad:?} недопустим в имени файла на FAT",
            path.display()
        );
    }
    if let Some(bad) = name.chars().find(|c| c.is_control()) {
        bail!(
            "{}: управляющий символ U+{:04X} недопустим в имени файла на FAT",
            path.display(),
            bad as u32
        );
    }
    // 255 символов — предел длинного имени VFAT (20 записей LFN по 13 символов).
    if name.chars().count() > 255 {
        bail!(
            "{}: имя длиннее 255 символов, длинное имя FAT его не вместит",
            path.display()
        );
    }
    Ok(())
}

/// Текст слепка: геометрия тома плюс строка на каждый элемент дерева.
///
/// Хранится текстом, а не одним числом, намеренно: когда образ вдруг
/// пересобирается (или, наоборот, не пересобирается), `build/initrd.stamp`
/// можно просто открыть и увидеть, что именно разошлось.
fn stamp_text(entries: &[Entry]) -> String {
    let mut text = format!(
        "rev={FORMAT_REVISION} image={IMAGE_SIZE} sector={BYTES_PER_SECTOR} \
         cluster={BYTES_PER_CLUSTER}\n"
    );
    for entry in entries {
        match &entry.node {
            Node::Dir => text.push_str(&format!("d {:>10} {:16} {}\n", "-", "-", entry.rel)),
            Node::File(data) => text.push_str(&format!(
                "f {:>10} {:016x} {}\n",
                data.len(),
                fnv1a64(data),
                entry.rel
            )),
        }
    }
    text
}

/// FNV-1a, 64 бита.
///
/// Содержимое `initrd/` — десятки килобайт, и вопрос стоит «изменился файл или
/// нет», а не «подделали ли его»: криптостойкость тут не нужна, а лишняя
/// зависимость на хеш-крейт — нужна ещё меньше.
fn fnv1a64(data: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in data {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Форматирует том и записывает в него дерево, возвращая готовый образ.
fn format_image(entries: &[Entry]) -> Result<Vec<u8>> {
    let mut disk = Cursor::new(vec![0u8; IMAGE_SIZE]);

    let options = FormatVolumeOptions::new()
        .bytes_per_sector(BYTES_PER_SECTOR)
        .bytes_per_cluster(BYTES_PER_CLUSTER)
        .volume_label(*VOLUME_LABEL);
    fatfs::format_volume(&mut disk, options)
        .context("не удалось отформатировать том FAT32 в памяти")?;

    let fs = FileSystem::new(
        &mut disk,
        FsOptions::new().time_provider(&FIXED_TIME_PROVIDER),
    )
    .context("не удалось смонтировать только что отформатированный том")?;

    // Проверяем, а не верим на слово. Тип FAT — производная от числа кластеров
    // (см. IMAGE_SIZE), поэтому уменьшение образа или укрупнение кластера молча
    // превратит том в FAT16, и узнали бы мы об этом уже из отказа драйвера в
    // QEMU — с отладкой не там, где ошибка.
    let fat_type = fs.fat_type();
    if fat_type != FatType::Fat32 {
        bail!(
            "образ отформатирован как {fat_type:?}, а не FAT32: при размере {IMAGE_SIZE} байт и \
             кластере {BYTES_PER_CLUSTER} Б получилось {} кластеров, а FAT32 начинается с 65525.\n\
             Увеличьте IMAGE_SIZE или уменьшите BYTES_PER_CLUSTER в xtask/src/initrd.rs.",
            fs.stats()
                .map(|stats| stats.total_clusters().to_string())
                .unwrap_or_else(|_| "?".to_string()),
        );
    }

    check_capacity(&fs, entries)?;

    // Блок обязателен: `Dir` держит заимствование `fs`, а `unmount` забирает
    // `fs` по значению — без явной области видимости корневой каталог дожил бы
    // до конца функции и не дал бы этого сделать.
    {
        let root = fs.root_dir();
        for entry in entries {
            match &entry.node {
                Node::Dir => {
                    root.create_dir(&entry.rel).with_context(|| {
                        format!("не удалось создать в образе каталог {}", entry.rel)
                    })?;
                }
                Node::File(data) => {
                    let mut file = root.create_file(&entry.rel).with_context(|| {
                        format!("не удалось создать в образе файл {}", entry.rel)
                    })?;
                    file.truncate().with_context(|| {
                        format!("не удалось очистить в образе файл {}", entry.rel)
                    })?;
                    file.write_all(data).with_context(|| {
                        format!("не удалось записать в образ файл {}", entry.rel)
                    })?;
                    file.flush().with_context(|| {
                        format!("не удалось сбросить в образ файл {}", entry.rel)
                    })?;
                }
            }
        }
    }

    fs.unmount()
        .context("не удалось сбросить структуры FAT в образ")?;

    let mut image = disk.into_inner();
    normalize_dot_entries(&mut image)?;
    Ok(image)
}

// --- приведение образа к спецификации -----------------------------------------

const DIR_ENTRY_SIZE: usize = 32;
const ATTR_LONG_NAME_MASK: u8 = 0x0F;
const ATTR_DIRECTORY: u8 = 0x10;
/// Начиная с этого значения запись FAT означает конец цепочки.
const END_OF_CHAIN: u32 = 0x0FFF_FFF8;

/// Геометрия тома, вычитанная из BPB готового образа.
struct Geometry {
    fat_start: usize,
    data_start: usize,
    cluster_bytes: usize,
    root_cluster: u32,
    max_cluster: u32,
}

impl Geometry {
    fn cluster_offset(&self, cluster: u32) -> usize {
        self.data_start + (cluster as usize - 2) * self.cluster_bytes
    }
}

/// Приводит служебные записи «.» и «..» к тому, что требует спецификация FAT.
///
/// fatfs делает с ними две вещи, которых делать не должен.
///
/// Во-первых, подкаталог он создаёт тем же кодом, что и любую другую запись, а
/// тот всегда пишет цепочку длинных имён — в том числе для «.» и «..», которым
/// длинное имя не полагается вовсе. Во-вторых, в «..» подкаталога, лежащего в
/// корне, он кладёт настоящий номер корневого кластера, тогда как формат
/// требует там ноль (Microsoft FAT specification, описание поля
/// `DIR_FstClusLO`): именно так размечает тома Windows, и именно ноль драйвер
/// обязан уметь разбирать, чтобы работать с настоящими носителями.
///
/// Оба отклонения большинство драйверов переживает, но 7-Zip, например, честно
/// помечает такой том как повреждённый. Человеку, который пишет драйвер FAT в
/// ядре, образ с претензиями от сторонних инструментов не нужен: сомнение в
/// образе стоит дороже, чем этот проход.
///
/// Разбор идёт по сырым байтам: fatfs доступа к записям каталога не даёт, а
/// нужно здесь ровно вычитать BPB, пройти цепочки кластеров и переложить
/// записи.
fn normalize_dot_entries(image: &mut [u8]) -> Result<()> {
    let bytes_per_sector = u16_at(image, 11) as usize;
    let sectors_per_cluster = image[13] as usize;
    let reserved_sectors = u16_at(image, 14) as usize;
    let fats = image[16] as usize;
    let sectors_per_fat = u32_at(image, 36) as usize;
    let root_cluster = u32_at(image, 44);

    if bytes_per_sector == 0 || sectors_per_cluster == 0 || sectors_per_fat == 0 {
        bail!("в образе нет корректного BPB FAT32 — форматирование прошло не так, как ожидалось");
    }

    let data_start = (reserved_sectors + fats * sectors_per_fat) * bytes_per_sector;
    let cluster_bytes = sectors_per_cluster * bytes_per_sector;
    let geometry = Geometry {
        fat_start: reserved_sectors * bytes_per_sector,
        data_start,
        cluster_bytes,
        root_cluster,
        // Номер последнего кластера, который вообще помещается в образ; нужен,
        // чтобы битая цепочка приводила к ошибке, а не к чтению за границей.
        max_cluster: ((image.len() - data_start) / cluster_bytes) as u32 + 1,
    };

    normalize_dir(image, &geometry, root_cluster, None)
}

/// `parent` — первый кластер родительского каталога; `None` означает корень.
fn normalize_dir(
    image: &mut [u8],
    geometry: &Geometry,
    first: u32,
    parent: Option<u32>,
) -> Result<()> {
    let clusters = cluster_chain(image, geometry, first)?;

    let mut buf = Vec::with_capacity(clusters.len() * geometry.cluster_bytes);
    for &cluster in &clusters {
        let offset = geometry.cluster_offset(cluster);
        buf.extend_from_slice(&image[offset..offset + geometry.cluster_bytes]);
    }

    // В корне «.» и «..» не бывает — там править нечего.
    if parent.is_some() {
        let mut kept: Vec<u8> = Vec::with_capacity(buf.len());
        // Записи LFN идут перед своей короткой записью, поэтому решение
        // «оставить или выбросить» принимается, только когда дошли до неё.
        let mut pending: Vec<u8> = Vec::new();
        for entry in buf.chunks_exact(DIR_ENTRY_SIZE) {
            if entry[0] == 0x00 {
                break;
            }
            if entry[11] & ATTR_LONG_NAME_MASK == ATTR_LONG_NAME_MASK {
                pending.extend_from_slice(entry);
                continue;
            }
            if !is_dot_entry(entry) {
                kept.extend_from_slice(&pending);
            }
            pending.clear();
            let at = kept.len();
            kept.extend_from_slice(entry);
            if &entry[..11] == b"..         " && parent == Some(geometry.root_cluster) {
                kept[at + 20..at + 22].fill(0);
                kept[at + 26..at + 28].fill(0);
            }
        }
        kept.extend_from_slice(&pending);
        // Хвост нулей — признак конца каталога; размер цепочки не меняется.
        kept.resize(buf.len(), 0);
        buf = kept;

        for (index, &cluster) in clusters.iter().enumerate() {
            let offset = geometry.cluster_offset(cluster);
            let from = index * geometry.cluster_bytes;
            image[offset..offset + geometry.cluster_bytes]
                .copy_from_slice(&buf[from..from + geometry.cluster_bytes]);
        }
    }

    let mut subdirs: Vec<u32> = Vec::new();
    for entry in buf.chunks_exact(DIR_ENTRY_SIZE) {
        if entry[0] == 0x00 {
            break;
        }
        if entry[0] == 0xE5 || entry[11] & ATTR_LONG_NAME_MASK == ATTR_LONG_NAME_MASK {
            continue;
        }
        if entry[11] & ATTR_DIRECTORY == 0 {
            continue;
        }
        if is_dot_entry(entry) {
            continue;
        }
        let high = u32::from(u16::from_le_bytes([entry[20], entry[21]]));
        let low = u32::from(u16::from_le_bytes([entry[26], entry[27]]));
        subdirs.push((high << 16) | low);
    }

    for cluster in subdirs {
        normalize_dir(image, geometry, cluster, Some(first))?;
    }

    Ok(())
}

/// Служебная запись «.» или «..» — они хранятся как имена 8.3, дополненные
/// пробелами.
fn is_dot_entry(entry: &[u8]) -> bool {
    matches!(&entry[..11], b".          " | b"..         ")
}

/// Цепочка кластеров, начиная с `first`.
fn cluster_chain(image: &[u8], geometry: &Geometry, first: u32) -> Result<Vec<u32>> {
    let mut chain = Vec::new();
    let mut cluster = first;
    while (2..END_OF_CHAIN).contains(&cluster) {
        if cluster > geometry.max_cluster {
            bail!("цепочка кластеров каталога уходит за пределы образа (кластер {cluster})");
        }
        chain.push(cluster);
        if chain.len() > geometry.max_cluster as usize {
            bail!("цепочка кластеров каталога зациклилась");
        }
        cluster = u32_at(image, geometry.fat_start + cluster as usize * 4) & 0x0FFF_FFFF;
    }
    if chain.is_empty() {
        bail!("каталог с первым кластером {first} пуст — такого в корректном FAT32 не бывает");
    }
    Ok(chain)
}

fn u16_at(image: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([image[offset], image[offset + 1]])
}

fn u32_at(image: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        image[offset],
        image[offset + 1],
        image[offset + 2],
        image[offset + 3],
    ])
}

/// Заранее сверяет объём данных со свободным местом на томе.
///
/// Без этой проверки переполнение всплыло бы как `Not enough space` где-то
/// посреди записи очередного файла — верно по сути, но не объясняет ни
/// сколько места есть, ни сколько нужно.
fn check_capacity<T: fatfs::ReadWriteSeek>(fs: &FileSystem<T>, entries: &[Entry]) -> Result<()> {
    let stats = fs
        .stats()
        .context("не удалось получить статистику тома FAT32")?;
    let cluster = u64::from(stats.cluster_size());

    let needed: u64 = entries
        .iter()
        .map(|entry| match &entry.node {
            // Каталогу нужен минимум один кластер под записи; при 512 Б на
            // кластер это 16 записей, а с длинными именами — меньше, поэтому
            // берём с двойным запасом, чтобы оценка оставалась оценкой сверху.
            Node::Dir => 2,
            Node::File(data) => (data.len() as u64).div_ceil(cluster),
        })
        .sum();
    let free = u64::from(stats.free_clusters());

    if needed > free {
        bail!(
            "содержимое не помещается в образ: нужно около {} КиБ, свободно {} КиБ.\n\
             Увеличьте IMAGE_SIZE в xtask/src/initrd.rs.",
            needed * cluster / 1024,
            free * cluster / 1024,
        );
    }

    Ok(())
}
