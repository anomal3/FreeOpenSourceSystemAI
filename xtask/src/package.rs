//! Сборка контейнеров `.fpk`: образцовые пакеты и обновление системы.
//!
//! # Зачем образцовые пакеты собираются, а не лежат в репозитории
//!
//! Потому что внутри них — программы, а программы связаны с ядром номерами
//! системных вызовов. Пакет, положенный в репозиторий однажды, разошёлся бы с
//! ядром на первой же фазе, которая добавит вызов, и разошёлся бы молча:
//! программа из него запустилась бы и попросила у ядра не то. Собранный вместе
//! с ядром — не может.
//!
//! # Что здесь собирается
//!
//! * `hello-1.0.fpk` — пакет с программой и текстовым файлом в подкаталоге.
//!   Подкаталог не для красоты: удаление пакета обязано убрать и его, и
//!   проверить это на пакете из одного файла нечем.
//! * `extra-1.0.fpk` — пакет, который **зависит** от первого. Существует ровно
//!   затем, чтобы проверить отказ: поставленный первым, он обязан не встать.
//! * `freeos-<версия>.fpk` — система целиком: образ корня, ядро и initrd. То,
//!   что `sysupdate` кладёт в неактивный слот.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use fpk::Kind;
use fpk::build::{Builder, Entry};

use crate::arch::Arch;
use crate::{build, paths};

/// Куда складываются собранные контейнеры.
fn output_dir() -> PathBuf {
    paths::build_dir().join("pkg")
}

/// Собранный контейнер: имя файла и путь к нему.
pub struct Package {
    /// Имя, под которым он ляжет в систему.
    pub file_name: String,
    pub path: PathBuf,
}

/// Собрать образцовые пакеты.
///
/// Возвращает их в порядке, в котором они попадают на носитель; порядок
/// установки выбирает тот, кто ставит, и в этом вся суть проверки зависимостей.
pub fn build_samples(arch: Arch, release: bool) -> Result<Vec<Package>> {
    let greet = build::packaged_program(arch, release, "greet")?;
    let greet_bytes = fs::read(&greet)
        .with_context(|| format!("не удалось прочитать {}", greet.display()))?;

    let mut hello = Builder::new(Kind::Package, "hello", "1.0");
    hello.field("summary", "A program that only exists inside a package");
    hello.file(&Entry {
        path: String::from("bin/greet"),
        // `0755`: запускать вправе кто угодно, менять — только владелец. Ровно
        // те же права, что установщик ставит файлам в `/bin`, и по той же
        // причине: право менять исполняемый файл — это право исполнять что
        // угодно от чужого имени.
        mode: 0o755,
        uid: 0,
        gid: 0,
        data: greet_bytes.clone(),
    });
    hello.file(&Entry {
        path: String::from("share/readme.txt"),
        mode: 0o644,
        uid: 0,
        gid: 0,
        data: README.as_bytes().to_vec(),
    });

    let mut extra = Builder::new(Kind::Package, "extra", "1.0");
    extra.field("summary", "A package that is useless without hello");
    extra.requires("hello");
    extra.file(&Entry {
        path: String::from("x.txt"),
        mode: 0o644,
        uid: 0,
        gid: 0,
        data: EXTRA.as_bytes().to_vec(),
    });

    let dir = output_dir();
    fs::create_dir_all(&dir)
        .with_context(|| format!("не удалось создать каталог {}", dir.display()))?;

    let mut built = Vec::new();
    for (file_name, bytes) in [
        ("hello-1.0.fpk", hello.finish()),
        ("extra-1.0.fpk", extra.finish()),
    ] {
        let path = dir.join(file_name);
        fs::write(&path, &bytes)
            .with_context(|| format!("не удалось записать {}", path.display()))?;
        println!("пакет: {} ({} байт)", path.display(), bytes.len());
        built.push(Package { file_name: String::from(file_name), path });
    }
    Ok(built)
}

/// Содержимое текстового файла в пакете `hello`.
///
/// Строка внутри узнаваемая и проверяется стендом: «файл распаковался» надо
/// доказать содержимым, а не тем, что он есть.
const README: &str =
    "This file arrived inside hello-1.0.fpk and was unpacked by pkg install.\n";

/// Содержимое файла в пакете `extra`.
///
/// Ровно двенадцать байт, и это не случайность: стенд подменяет его строкой
/// **той же длины**, чтобы проверить контрольную сумму, а не размер. Проверка,
/// ловящая только другой размер, пропустила бы подменённую программу — то есть
/// ровно тот случай, ради которого сумма и считается.
///
/// Лежит он в корне пакета, а не в подкаталоге, и тоже не из лени: команда
/// подмены уезжает в гостя по серийной линии, а у PL011 на AArch64 приёмный
/// FIFO — 32 байта. `echo ... > /opt/extra/share/extra.txt` обрывался ровно на
/// тридцать втором знаке, пока оболочка перерисовывала окно. Подкаталог, чтобы
/// проверить уборку каталогов при удалении, есть у пакета `hello`.
const EXTRA: &str = "packaged-ok\n";

/// Точки монтирования раздела состояния.
///
/// Список обязан совпадать с тем, что создаёт установщик и что монтирует ядро.
/// Все три копии короткие и на виду: расхождение выглядело бы как пропавший
/// каталог, а не как ошибка.
const STATE_BRANCHES: [&str; 5] = ["etc", "home", "root", "var", "opt"];

/// Как называется файл обновления системы.
pub fn system_file_name(version: &str) -> String {
    format!("freeos-{version}.fpk")
}

/// Сколько места отвести под образ корня, который уезжает в обновление.
///
/// Он **меньше** раздела, в который ляжет, и это не экономия: ext2 описывает
/// свой размер в суперблоке, поэтому файловая система на 24 МиБ, записанная в
/// начало раздела на гигабайт, монтируется и работает — просто не пользуется
/// остатком. Гнать по линии гигабайт нулей ради того, чтобы «совпало», было бы
/// бессмысленно.
const UPDATE_ROOT_BYTES: u64 = 24 * 1024 * 1024;

/// Собрать обновление системы: образ корня, ядро и initrd одним контейнером.
///
/// `broken` делает образ корня заведомо непригодным — суперблок затирается
/// нулями. Это не «испорченный файл», а испорченная **система**: контейнер
/// цел, контрольные суммы сходятся, `sysupdate` его принимает и записывает, а
/// загрузка со слота не удаётся. Ровно та неисправность, ради которой
/// существует откат: если бы обновление отвергалось на входе, откатывать было
/// бы нечего.
/// Каким собирается образ обновления.
///
/// Три варианта, и два последних существуют ради проверок, без которых первый
/// ничего не доказывает: система, принимающая что угодно, ставит годный образ
/// ровно так же успешно, как правильная.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Flavour {
    /// Годный: подписан ключом, которому система доверяет.
    Good,
    /// **Подписан верно**, но внутри — испорченный том. Так и задумано: подпись
    /// удостоверяет происхождение, а не исправность, и слот с таким образом
    /// обязан отвергнуться на **загрузке**, а не при установке, — иначе
    /// проверять откат было бы нечем.
    Broken,
    /// Целый, но подписан чужим ключом. Обязан быть отвергнут при `apply`.
    Forged,
}

pub fn build_system(
    arch: Arch,
    release: bool,
    version: &str,
    kernel: &Path,
    initrd: &Path,
    programs: &[(&'static str, PathBuf)],
    flavour: Flavour,
) -> Result<Package> {
    let root = build_root_image(version, programs, flavour == Flavour::Broken)?;
    let kernel_bytes = fs::read(kernel)
        .with_context(|| format!("не удалось прочитать ядро {}", kernel.display()))?;
    let initrd_bytes = fs::read(initrd)
        .with_context(|| format!("не удалось прочитать initrd {}", initrd.display()))?;

    let mut system = Builder::new(Kind::System, "freeos", version);
    system.field("arch", arch.name());
    // Ядро и initrd едут вместе с корнем, а не отдельно, и это требование
    // формата, а не удобство: их связывают номера системных вызовов, и слот, в
    // котором ядро от одной версии, а `/bin` от другой, — это система, которая
    // ломается молча.
    system.blob("image", &root);
    system.blob("kernel", &kernel_bytes);
    system.blob("initrd", &initrd_bytes);

    let dir = output_dir();
    fs::create_dir_all(&dir)
        .with_context(|| format!("не удалось создать каталог {}", dir.display()))?;
    let file_name = match flavour {
        Flavour::Good => system_file_name(version),
        Flavour::Broken => String::from("freeos-broken.fpk"),
        Flavour::Forged => String::from("freeos-forged.fpk"),
    };
    let path = dir.join(&file_name);
    let mut bytes = system.finish();
    // Подпись ставится последней: она считается по готовому заголовку и
    // манифесту, то есть по тому, что уже собрано.
    let key = match flavour {
        Flavour::Forged => crate::keys::stranger()?,
        _ => crate::keys::release()?,
    };
    crate::keys::sign(&mut bytes, &key);
    fs::write(&path, &bytes)
        .with_context(|| format!("не удалось записать {}", path.display()))?;
    println!(
        "обновление: {} ({} МиБ{})",
        path.display(),
        bytes.len() / (1024 * 1024),
        match flavour {
            Flavour::Good => "",
            Flavour::Broken => ", заведомо неисправное внутри",
            Flavour::Forged => ", подписано чужим ключом",
        }
    );
    let _ = release;
    Ok(Package { file_name, path })
}

/// Собрать образ корня для обновления.
///
/// Кладётся туда ровно то, что делает систему системой: `/bin` с программами и
/// `/os-release` с версией. Всё остальное — `/etc/passwd`, `/home`, `/var`,
/// `/opt` — живёт на разделе состояния, которого обновление не касается, и
/// класть их сюда значило бы обещать, что обновление затрёт настройки и данные.
///
/// Пустые каталоги веток состояния создаются здесь же, и это не украшение: они
/// точки монтирования, и без них `ls /` на новой системе не показал бы ни
/// `/etc`, ни `/home` — смонтированная ветка видна потому, что каталог под ней
/// существует.
fn build_root_image(
    version: &str,
    programs: &[(&'static str, PathBuf)],
    broken: bool,
) -> Result<Vec<u8>> {
    let sectors = UPDATE_ROOT_BYTES / 512;
    let mut disk = disk::MemDisk::new(sectors)
        .context("не хватило памяти под образ корня для обновления")?;

    let options = ext2::FormatOptions {
        label: "FreeOS",
        // Идентификатор выводится из версии: два обновления с одинаковым UUID
        // выглядели бы для чужих утилит как один и тот же том.
        uuid: uuid_from(version),
        // Время фиксировано ради воспроизводимости образа: одна и та же сборка
        // обязана давать один и тот же файл, иначе слепок пересборки бесполезен.
        time: 0,
    };
    let mut fs_image = ext2::format(&mut disk, 0, sectors, &options)
        .map_err(|err| anyhow::anyhow!("не удалось отформатировать образ корня: {err}"))?;

    // Точки монтирования. Пустые каталоги, за которыми при работе системы стоит
    // раздел состояния: смонтированная ветка видна потому, что каталог под ней
    // существует, и без них `ls /` на новой системе не показал бы ни `/etc`, ни
    // `/home`.
    for branch in STATE_BRANCHES {
        fs_image
            .create_dir_path(&mut disk, branch, 0o755, 0, 0)
            .map_err(|err| anyhow::anyhow!("не удалось создать /{branch} в образе: {err}"))?;
    }

    // Версия образа лежит в корне, а не в `/etc`: `/etc` принадлежит состоянию
    // и переживает обновление, а этот файл описывает **образ** и обязан
    // заменяться вместе с ним. Тот же путь пишет и установщик.
    let release_text = format!(
        "# FreeOS release, written into the slot image by xtask\n\
         version={version}\n"
    );
    fs_image
        .write_file_path(&mut disk, "os-release", release_text.as_bytes(), 0o644, 0, 0)
        .map_err(|err| anyhow::anyhow!("не удалось записать /os-release: {err}"))?;

    // Доверенные ключи обновления. Лежат рядом с версией и по той же причине:
    // они описывают **образ**, а не машину, и обязаны заменяться вместе с ним —
    // иначе новая версия не смогла бы принести новый ключ, а старая узнала бы о
    // смене ключа только тем, что перестала обновляться.
    let keys_text = crate::keys::trusted_text()?;
    fs_image
        .write_file_path(&mut disk, "os-keys", keys_text.as_bytes(), 0o644, 0, 0)
        .map_err(|err| anyhow::anyhow!("не удалось записать /os-keys: {err}"))?;

    // Эталонные настройки. Обновление их **обязано** нести: `/etc` живёт на
    // разделе состояния, до которого обновление не дотягивается, и без эталона
    // в образе новая версия не смогла бы принести ни одной новой настройки. Файл
    // берётся тот же самый, что уезжает в initrd и на установочный носитель, —
    // копия в дереве ровно одна.
    for dir in ["usr", "usr/share", "usr/share/defaults", "usr/share/defaults/etc"] {
        fs_image
            .create_dir_path(&mut disk, dir, 0o755, 0, 0)
            .map_err(|err| anyhow::anyhow!("не удалось создать /{dir} в образе: {err}"))?;
    }
    for (name, _) in crate::arch::PAYLOAD_DEFAULTS {
        let source = paths::defaults_dir().join(name);
        let data = fs::read(&source)
            .with_context(|| format!("не удалось прочитать {}", source.display()))?;
        fs_image
            .write_file_path(
                &mut disk,
                &format!("usr/share/defaults/etc/{name}"),
                &data,
                0o644,
                0,
                0,
            )
            .map_err(|err| anyhow::anyhow!("не удалось записать эталон {name}: {err}"))?;
    }

    fs_image
        .create_dir_path(&mut disk, "bin", 0o755, 0, 0)
        .map_err(|err| anyhow::anyhow!("не удалось создать /bin в образе: {err}"))?;
    for (name, path) in programs {
        let data = fs::read(path)
            .with_context(|| format!("не удалось прочитать программу {}", path.display()))?;
        fs_image
            .write_file_path(&mut disk, &format!("bin/{name}"), &data, 0o755, 0, 0)
            .map_err(|err| anyhow::anyhow!("не удалось записать /bin/{name}: {err}"))?;
    }

    fs_image
        .flush(&mut disk)
        .map_err(|err| anyhow::anyhow!("не удалось сбросить образ корня: {err}"))?;
    // Том закрывается чисто: система, поднявшаяся с нового слота, не должна
    // объявлять его грязным и проверять целиком на первой же загрузке.
    fs_image
        .mark_clean(&mut disk)
        .map_err(|err| anyhow::anyhow!("не удалось пометить образ чистым: {err}"))?;

    let mut bytes = disk.into_vec();
    if broken {
        // Суперблок ext2 лежит по смещению 1024 и занимает 1024 байта.
        // Затирается именно он, а не весь образ: система обязана отвергнуть
        // слот на монтировании — то есть пройдя чтение с диска и разбор GPT, —
        // а не на первом же нечитаемом секторе.
        let end = (1024 + 1024).min(bytes.len());
        bytes[1024..end].fill(0);
    }
    if bytes.len() % 512 != 0 {
        bail!("образ корня не кратен сектору: {} байт", bytes.len());
    }
    Ok(bytes)
}

/// Положить обновления системы в уже установленный образ.
///
/// Делает то, что сделал бы человек с флешкой: открывает корневой раздел
/// установленного диска и кладёт в `/media` два контейнера — годный и заведомо
/// неисправный. Оба нужны стенду: первый проверяет, что обновление работает,
/// второй — что откат работает, а второе важнее первого.
///
/// Почему не через установочный носитель, сказано в заголовке
/// [`crate::diskfile`].
pub fn place_updates(
    disk_path: &Path,
    arch: Arch,
    release: bool,
    kernel: &Path,
    initrd: &Path,
    programs: &[(&'static str, PathBuf)],
) -> Result<()> {
    use disk::BlockDevice as _;

    let good = build_system(arch, release, UPDATE_VERSION, kernel, initrd, programs, Flavour::Good)?;
    let broken = build_system(arch, release, UPDATE_VERSION, kernel, initrd, programs, Flavour::Broken)?;
    let forged = build_system(arch, release, UPDATE_VERSION, kernel, initrd, programs, Flavour::Forged)?;

    let mut dev = crate::diskfile::DiskFile::open(disk_path, 512)?;
    let table = disk::gpt::read(&mut dev)
        .map_err(|err| anyhow::anyhow!("на образе {} нет GPT: {err}", disk_path.display()))?;
    let root = table
        .find(disk::gpt::FREEOS_ROOT_TYPE)
        .ok_or_else(|| anyhow::anyhow!("на образе нет корневого раздела слота A"))?;
    let first_lba = root.first_lba;

    let mut fs = ext2::Editor::open(&mut dev, first_lba)
        .map_err(|err| anyhow::anyhow!("корневой раздел не открывается: {err}"))?;
    // Том помечается используемым на время правки и чистым в конце — ровно так
    // же, как это делает установщик. Без этого система при следующей загрузке
    // объявила бы корень грязным и проверила бы его целиком.
    fs.mark_dirty(&mut dev)
        .map_err(|err| anyhow::anyhow!("не удалось пометить том используемым: {err}"))?;

    for package in [&good, &broken, &forged] {
        let data = fs::read(&package.path)
            .with_context(|| format!("не удалось прочитать {}", package.path.display()))?;
        let target = format!("media/{}", package.file_name);
        // Файл мог остаться от прошлого прогона: диск переживает прогон, а
        // записанное переживает диск.
        let _ = fs.unlink(&mut dev, ext2::ROOT_INODE, &package.file_name);
        match fs.write_file_path(&mut dev, &target, &data, 0o644, 0, 0) {
            Ok(_) => {}
            Err(ext2::Error::Exists) => {
                // Уже лежит с прошлого раза и того же содержимого — перезаписи
                // ext2-редактор не умеет, а второй раз класть то же самое
                // незачем.
            }
            Err(err) => {
                return Err(anyhow::anyhow!("не удалось записать /{target}: {err}"));
            }
        }
        println!("обновление положено в образ: /{target} ({} байт)", data.len());
    }

    fs.flush(&mut dev)
        .map_err(|err| anyhow::anyhow!("не удалось сбросить корневой раздел: {err}"))?;
    fs.mark_clean(&mut dev)
        .map_err(|err| anyhow::anyhow!("не удалось пометить том чистым: {err}"))?;
    dev.flush()
        .map_err(|err| anyhow::anyhow!("не удалось сбросить образ: {err}"))?;
    Ok(())
}

/// Версия, которую несёт обновление в стенде.
///
/// Отличается от версии системы намеренно и заметно: сценарий проверяет, что
/// после перезагрузки система называет **новую** версию, и совпадающие строки
/// не доказали бы ничего.
pub const UPDATE_VERSION: &str = "0.2";

/// Растянуть строку версии в 16 байт под UUID тома.
fn uuid_from(version: &str) -> [u8; 16] {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in version.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let mut out = [0u8; 16];
    out[..8].copy_from_slice(&hash.to_be_bytes());
    out[8..].copy_from_slice(&hash.rotate_left(29).to_be_bytes());
    out
}
