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
///
/// В каталог воркера, а не в общий `build/pkg`: имя файла несёт версию, но не
/// архитектуру (`freeos-0.8.fpk`), а внутри — образ корня с программами под
/// конкретную машину. Два прогона разных архитектур, пишущие сюда
/// одновременно, обменялись бы содержимым молча.
fn output_dir() -> PathBuf {
    paths::work_dir().join("pkg")
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
    // Прав не просит ни одного, и это не забывчивость, а половина проверки:
    // `greet` пробует открыть сокет и обязан получить отказ. Пакет, который о
    // правах молчит, не получает ничего — см. `fpk::Manifest::rights`.
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

    // Программа WinForms пакетом (фаза N8): управляемая сборка, её
    // `.runtimeconfig.json` и строка запуска в манифесте — по `start=` стол
    // ставит программу в «Пуск». Файлы берутся из `initrd/`, куда их кладёт
    // `cargo xtask clr-check`: образ системы собирается без .NET SDK, и пакет
    // тоже.
    let samples = paths::initrd_source_dir().join("usr/share/dotnet/samples");
    let mut winforms = Builder::new(Kind::Package, "winforms", "1.0");
    winforms.field("summary", "A WinForms program from the Visual Studio designer");
    winforms.field("caption", "WinForms");
    winforms.field("about", "форма из дизайнера Visual Studio");
    winforms.field("start", "/bin/dotnet /opt/winforms/winforms.dll");
    // Окно — и только окно. Сети программе на WinForms не нужно, и теперь это
    // не намерение автора, а то, что проверяет ядро.
    winforms.field("permissions", "windows");
    for name in ["winforms.dll", "winforms.runtimeconfig.json"] {
        let path = samples.join(name);
        let data = fs::read(&path).with_context(|| format!("не удалось прочитать {}", path.display()))?;
        winforms.file(&Entry { path: String::from(name), mode: 0o644, uid: 0, gid: 0, data });
    }

    // Драйвер устройства, которого ядро не знает (веха «драйверы», Д1):
    // учебная карта QEMU `edu`. Право `devices` — самое широкое из всех, и
    // пакет, который его просит, в Д2 обязан будет быть подписан.
    let edudrv = build::packaged_program(arch, release, "edudrv")?;
    let edudrv_bytes = fs::read(&edudrv)
        .with_context(|| format!("не удалось прочитать {}", edudrv.display()))?;
    let mut edu = Builder::new(Kind::Package, "edu", "1.0");
    edu.field("summary", "A driver for the QEMU edu card, run as a program");
    edu.field("permissions", "devices");
    edu.field("drives", "1234:11e8");
    // Какую программу запускать для устройства — её ищет `drvd` (Д3).
    edu.field("driver", "bin/edudrv");
    edu.file(&Entry { path: String::from("bin/edudrv"), mode: 0o755, uid: 0, gid: 0, data: edudrv_bytes.clone() });

    // Тот же драйвер без права `devices`: ради проверки, что право — это то,
    // что даёт устройство, а не то, что о нём пишут в манифесте.
    let mut edu_nodev = Builder::new(Kind::Package, "edu-nodev", "1.0");
    edu_nodev.field("summary", "The edu driver without the right to a device");
    edu_nodev.file(&Entry { path: String::from("bin/edudrv"), mode: 0o755, uid: 0, gid: 0, data: edudrv_bytes.clone() });

    // И с правом, но без подписи: `pkg` обязан его отвергнуть (Д2).
    let mut edu_raw = Builder::new(Kind::Package, "edu-raw", "1.0");
    edu_raw.field("summary", "The edu driver asking for devices without a signature");
    edu_raw.field("permissions", "devices");
    edu_raw.file(&Entry { path: String::from("bin/edudrv"), mode: 0o755, uid: 0, gid: 0, data: edudrv_bytes });
    // Драйвер с правом на устройство подписывается рабочим ключом — тем же, что
    // обновления системы.
    let mut edu_bytes = edu.finish();
    crate::keys::sign(&mut edu_bytes, &crate::keys::release()?);

    let dir = output_dir();
    fs::create_dir_all(&dir)
        .with_context(|| format!("не удалось создать каталог {}", dir.display()))?;

    let mut built = Vec::new();
    for (file_name, bytes) in [
        ("hello-1.0.fpk", hello.finish()),
        ("extra-1.0.fpk", extra.finish()),
        ("winforms-1.0.fpk", winforms.finish()),
        ("edu-1.0.fpk", edu_bytes),
        ("edu-nodev-1.0.fpk", edu_nodev.finish()),
        ("edu-raw-1.0.fpk", edu_raw.finish()),
    ] {
        let path = dir.join(file_name);
        fs::write(&path, &bytes)
            .with_context(|| format!("не удалось записать {}", path.display()))?;
        say!("пакет: {} ({} байт)", path.display(), bytes.len());
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

/// Меньше какого размера образ корня для обновления не бывает.
///
/// Сам размер считается по содержимому ([`update_root_bytes`]): образ **меньше**
/// раздела, в который ляжет, и это не экономия: ext2 описывает свой размер в
/// суперблоке, поэтому файловая система на 24 МиБ, записанная в начало раздела
/// на гигабайт, монтируется и работает — просто не пользуется остатком. Гнать
/// по линии гигабайт нулей ради того, чтобы «совпало», было бы бессмысленно.
const UPDATE_ROOT_MIN_BYTES: u64 = 24 * 1024 * 1024;

/// Сколько места отвести под образ корня, который уезжает в обновление.
///
/// По содержимому, а не константой. Константа в 24 МиБ держалась, пока
/// программы были мелкими; с фазой 54 в `/bin` едет `big` на шесть мегабайт, и
/// на aarch64 образ перестал собираться («the filesystem has no free blocks
/// left») — четыре сценария обновления падали, не запустив гостя. Запас в
/// четверть — на метаданные ext2 и каталоги; округление до мегабайта — чтобы
/// размер не менялся от каждого байта в программе.
fn update_root_bytes(programs: &[(&'static str, PathBuf)]) -> Result<u64> {
    const MIB: u64 = 1024 * 1024;
    let mut content = 0u64;
    for (_, path) in programs {
        content += fs::metadata(path)
            .with_context(|| format!("не удалось узнать размер {}", path.display()))?
            .len();
    }
    // Тот же список, по которому образ и собирается: считать размер по одному
    // набору, а писать другой — это ровно тот способ получить «не хватило
    // места» на файле, который в расчёт не попал.
    for group in &crate::arch::IMAGE_SHARE {
        for (name, _) in group.files {
            let source = paths::initrd_source_dir().join(group.dir).join(name);
            content += fs::metadata(&source)
                .with_context(|| format!("не удалось узнать размер {}", source.display()))?
                .len();
        }
    }
    let wanted = (content + content / 4).div_ceil(MIB) * MIB;
    Ok(wanted.max(UPDATE_ROOT_MIN_BYTES))
}

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
    say!(
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
    let sectors = update_root_bytes(programs)? / 512;
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

    // Всё, что образ несёт под `/usr/share`. Обновление обязано нести **тот
    // же** набор, что кладёт установщик, и причина не в аккуратности: `/etc`,
    // `/home`, `/opt` живут на разделе состояния, до которого обновление не
    // дотягивается, а `/usr/share` принадлежит образу и заменяется вместе с
    // ним. Чего образ не принёс, того на обновлённой машине не станет.
    //
    // Список берётся общий — `arch::IMAGE_SHARE`, тот же, по которому
    // собирается установочный носитель. До 21.09.2026 здесь был свой набор
    // циклов, и он отстал: эталонные настройки и .NET образ нёс, а образец Lua
    // и страницу веб-сервера — нет. Обновлённая машина их теряла.
    for group in &crate::arch::IMAGE_SHARE {
        // Недостающие звенья пути `create_dir_path` создаёт сам, а уже
        // существующие находит: `usr` и `usr/share` общие у всех групп.
        for dir in std::iter::once(group.dir.to_string())
            .chain(group.subdirs.iter().map(|sub| format!("{}/{sub}", group.dir)))
        {
            fs_image
                .create_dir_path(&mut disk, &dir, 0o755, 0, 0)
                .map_err(|err| anyhow::anyhow!("не удалось создать /{dir} в образе: {err}"))?;
        }
        for (name, _) in group.files {
            let source = paths::initrd_source_dir().join(group.dir).join(name);
            let data = fs::read(&source)
                .with_context(|| format!("не удалось прочитать {}", source.display()))?;
            let target = format!("{}/{name}", group.dir);
            fs_image
                .write_file_path(&mut disk, &target, &data, 0o644, 0, 0)
                .map_err(|err| anyhow::anyhow!("не удалось записать /{target} в образ: {err}"))?;
        }
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
        .flush_everywhere(&mut disk)
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
        say!("обновление положено в образ: /{target} ({} байт)", data.len());
    }

    fs.flush_everywhere(&mut dev)
        .map_err(|err| anyhow::anyhow!("не удалось сбросить корневой раздел: {err}"))?;
    fs.mark_clean(&mut dev)
        .map_err(|err| anyhow::anyhow!("не удалось пометить том чистым: {err}"))?;
    dev.flush()
        .map_err(|err| anyhow::anyhow!("не удалось сбросить образ: {err}"))?;
    Ok(())
}

/// Сколько байт в файле, которым проверяется **масштаб** отображения.
///
/// Двести мебибайт выбраны не «на глаз», а относительно предела ядра: область
/// файлового отображения держит резидентными не больше 4096 страниц, то есть
/// 16 МиБ (`RESIDENT_MAX_PAGES` в `kernel/src/user/mod.rs`). Здесь страниц в
/// пятьдесят раз больше, чем помещается, и вытеснение поэтому не «может
/// случиться», а обязано случиться сорок семь тысяч раз. Файл поменьше уместился
/// бы под пределом целиком — и сценарий доказывал бы работу подкачки, ни разу
/// её не включив.
///
/// Цена названа вслух: столько же байт стенд строит в памяти хоста и столько же
/// кладёт в образ (см. [`big_file_bytes`] и [`place_big_file`]).
pub const BIG_FILE_BYTES: usize = 64 * 1024 * 1024;

/// Имя файла в `/media` установленной системы.
pub const BIG_FILE_NAME: &str = "big.dat";

/// Через сколько байт берётся следующий отсчёт.
///
/// Сотня, потому что ровно столько по умолчанию берёт сама программа
/// (`DEFAULT_STRIDE` в `crates/user-progs/src/bin/filemap.rs`). Разойдись эти
/// два числа — и ожидаемая сумма перестала бы быть ожидаемой, причём молча.
const BIG_FILE_STRIDE: usize = 100;

/// Свёртка FNV-1a по каждому сотому байту файла — то число, которое обязан
/// напечатать гость.
///
/// # Почему свёртка, а не сумма
///
/// Сумма не зависит от порядка слагаемых. Отображение, в котором две страницы
/// поменялись местами или обе ведут на один кадр, дало бы ровно тот же ответ,
/// что исправное, — то есть проверка суммой не ловит именно ту ошибку, ради
/// которой её тут и заводят. Свёртка, где каждый байт домножает накопленное, к
/// порядку чувствительна. То же рассуждение записано и со стороны гостя, в
/// заголовке программы: числа обязаны считаться одинаково с обеих сторон.
///
/// # Почему число выписано, а не считается при сборке сценария
///
/// Потому что проверка обязана быть **чужой**. Число, посчитанное здесь тем же
/// кодом, что кладёт файл, сошлось бы с напечатанным и в том случае, когда
/// генератор ошибся: сценарий сверял бы одну реализацию с ней же самой.
/// Выписанное число посчитано отдельно и вне этого дерева, а модуль
/// `big_file_check` в конце файла сверяет с ним генератор — то есть падает тот,
/// кто ошибся, а не оба молча.
///
/// Есть и вторая, скучная причина: [`crate::harness::scenarios::Step`] хранит
/// `&'static str`, а превратить `u64` в строковый литерал на этапе компиляции
/// нечем — `concat!` берёт литералы, не константы. Поэтому строка ниже
/// существует целиком, а тест следит, чтобы она кончалась этим числом.
pub const BIG_FILE_CHECKSUM: u64 = 12_530_236_768_682_792_638;

/// Строка целиком — ровно в том виде, в каком её печатает гость.
///
/// Сценарий ждёт именно её, а не собирает по кускам: подстрока без числа
/// проверяла бы, что программа досчитала, но не что она досчитала **до того
/// самого**.
/// Строка, которой программа называет длину файла.
///
/// Тоже константой, и по той же причине, что и свёртка: `Step` держит
/// `&'static str`, а подставить в литерал значение [`BIG_FILE_BYTES`] на этапе
/// сборки нечем. Разъехаться им не даёт проверка на хосте — она сверяет обе
/// строки с числами, из которых они собраны.
pub const BIG_FILE_SIZE_LINE: &str = "filemap: /media/big.dat is 67108864 bytes";

pub const BIG_FILE_CHECKSUM_LINE: &str =
    "filemap: checksum over every 100th byte is 12530236768682792638";

/// Байт файла по его смещению.
///
/// Зависит и от номера страницы (`index >> 12`), и от смещения внутри неё, и в
/// этом весь смысл. Узор, зависящий только от смещения, повторялся бы в каждой
/// странице — и отображение, где две виртуальные страницы смотрят на один
/// кадр, дало бы правильную сумму. Узор, зависящий только от номера страницы,
/// не заметил бы сдвига внутри неё. Здесь не проходит ни то, ни другое.
pub const fn big_file_byte(index: usize) -> u8 {
    (((index >> 12) ^ index) & 0xFF) as u8
}

/// Построить содержимое файла целиком.
///
/// Двести мебибайт в куче хоста — и это осознанная плата, а не недосмотр:
/// [`ext2::Editor::write_file_path`] принимает один сплошной `&[u8]`, то есть
/// иначе пришлось бы заводить файл вручную и дописывать его кусками через
/// `write_at`. Кусками было бы экономнее по памяти и дороже по коду ровно там,
/// где код обязан повторять то, что рядом уже делает [`place_updates`].
/// Двести мебибайт на машине, которая собирает ядро, ничего не стоят.
fn big_file_bytes() -> Vec<u8> {
    let mut data = Vec::with_capacity(BIG_FILE_BYTES);
    data.extend((0..BIG_FILE_BYTES).map(big_file_byte));
    data
}

/// Положить в установленный образ файл, который заведомо не помещается под
/// предел резидентных страниц.
///
/// Делает ровно то же и ровно тем же способом, что [`place_updates`]: открывает
/// корневой раздел уже установленного диска, помечает том используемым, кладёт
/// файл в `/media` и закрывает том чисто. Отдельная функция, а не флаг у той —
/// потому что содержимое здесь не читается из файла, а вычисляется, и общего у
/// них ровно один абзац кода из десяти.
///
/// # Почему файл не перезаписывается
///
/// Диск переживает прогон, а записанное переживает диск: цепочка сценариев
/// установленной системы работает с одним и тем же образом, и файл, положенный
/// в первом прогоне, лежит в нём и во втором. Двести мебибайт — это несколько
/// сотен тысяч обращений к образу через [`crate::diskfile::DiskFile`], то есть
/// десятки секунд на каждом прогоне ни за что. Поэтому наличие проверяется
/// **до** построения содержимого: узнать «уже лежит» по отказу
/// `ext2::Error::Exists`, как это делает [`place_updates`], здесь означало бы
/// сначала собрать двести мебибайт и только потом выяснить, что они не нужны.
pub fn place_big_file(disk_path: &Path) -> Result<()> {
    use disk::BlockDevice as _;

    let mut dev = crate::diskfile::DiskFile::open(disk_path, 512)?;
    let table = disk::gpt::read(&mut dev)
        .map_err(|err| anyhow::anyhow!("на образе {} нет GPT: {err}", disk_path.display()))?;
    let root = table
        .find(disk::gpt::FREEOS_ROOT_TYPE)
        .ok_or_else(|| anyhow::anyhow!("на образе нет корневого раздела слота A"))?;
    let first_lba = root.first_lba;

    let mut fs = ext2::Editor::open(&mut dev, first_lba)
        .map_err(|err| anyhow::anyhow!("корневой раздел не открывается: {err}"))?;

    // Каталога `/media` может не быть вовсе: его заводит тот, кто первым
    // положил туда файл, а сценарии обновления в этой цепочке могли и не
    // запускаться. Отсутствие каталога — это «файла нет», а не ошибка.
    let media = fs
        .lookup(&mut dev, ext2::ROOT_INODE, "media")
        .map_err(|err| anyhow::anyhow!("не удалось прочитать корень образа: {err}"))?;
    let existing = match media {
        Some((dir, _)) => fs
            .lookup(&mut dev, dir, BIG_FILE_NAME)
            .map_err(|err| anyhow::anyhow!("не удалось прочитать /media: {err}"))?,
        None => None,
    };
    // Имени мало: оно переживает смену размера, а размер здесь — часть условия
    // задачи. Файл от прошлого прогона, собранный по другой константе, дал бы
    // другую свёртку, и стенд ждал бы от гостя число, которого тот не мог бы
    // получить ни при какой исправной работе ядра. Поэтому «уже лежит» здесь
    // означает «лежит **тот самый**», а всё прочее сносится и кладётся заново.
    if let (Some((dir, _)), Some((number, _))) = (media, existing) {
        let size = fs.size_of(&mut dev, number).map_err(|err| {
            anyhow::anyhow!("не удалось прочитать размер /media/{BIG_FILE_NAME}: {err}")
        })?;
        if size == BIG_FILE_BYTES as u64 {
            say!(
                "стенд: /media/{} уже лежит в образе, заново не строим; от гостя ждём свёртку {}",
                BIG_FILE_NAME,
                BIG_FILE_CHECKSUM
            );
            return Ok(());
        }
        say!(
            "стенд: /media/{} в образе длиной {} вместо {} — кладём заново",
            BIG_FILE_NAME,
            size,
            BIG_FILE_BYTES
        );
        fs.mark_dirty(&mut dev)
            .map_err(|err| anyhow::anyhow!("не удалось пометить том используемым: {err}"))?;
        fs.unlink(&mut dev, dir, BIG_FILE_NAME).map_err(|err| {
            anyhow::anyhow!("не удалось убрать прежний /media/{BIG_FILE_NAME}: {err}")
        })?;
        fs.flush_everywhere(&mut dev)
            .map_err(|err| anyhow::anyhow!("не удалось сбросить корневой раздел: {err}"))?;
        fs.mark_clean(&mut dev)
            .map_err(|err| anyhow::anyhow!("не удалось пометить том чистым: {err}"))?;
    }

    // Том помечается используемым на время правки и чистым в конце — ровно так
    // же, как это делает установщик. Без этого система при следующей загрузке
    // объявила бы корень грязным и проверила бы его целиком.
    fs.mark_dirty(&mut dev)
        .map_err(|err| anyhow::anyhow!("не удалось пометить том используемым: {err}"))?;

    // Освободить место, занятое контейнерами обновления.
    //
    // Их кладёт в `/media` сам стенд ([`place_updates`]) перед сценариями,
    // которым они нужны, и кладёт заново каждый раз. А `httpd-load` идёт в
    // цепочке позже, и на корне в 511 МиБ его двухсотмегабайтный файл просто
    // не помещается рядом с тремя контейнерами по 111,6 МБ: 21.09.2026
    // сценарий упал на «the filesystem has no free blocks left» — не на
    // дефекте системы, а на арифметике.
    //
    // Снести их безопасно именно потому, что положил их стенд: сценарию,
    // которому они понадобятся, `place_updates` положит их снова.
    if let Some((dir, _)) = media {
        for name in [
            system_file_name(UPDATE_VERSION),
            String::from("freeos-broken.fpk"),
            String::from("freeos-forged.fpk"),
        ] {
            match fs.unlink(&mut dev, dir, &name) {
                Ok(()) => say!("стенд: /media/{name} убран, чтобы освободить место под {BIG_FILE_NAME}"),
                // Их могло и не быть: сценарии обновления в этой цепочке могли
                // не запускаться.
                Err(ext2::Error::NotFound) => {}
                Err(err) => {
                    return Err(anyhow::anyhow!("не удалось убрать /media/{name}: {err}"));
                }
            }
        }
    }

    let data = big_file_bytes();
    let target = format!("media/{BIG_FILE_NAME}");
    match fs.write_file_path(&mut dev, &target, &data, 0o644, 0, 0) {
        // Ожидаемая свёртка печатается здесь же, рядом с размером: когда
        // сценарий упадёт на числе, первое, что понадобится человеку, — знать,
        // какого файла это число и тем ли шагом оно посчитано.
        Ok(_) => say!(
            "стенд: в образ положен /{target} ({} байт); свёртка по каждому {}-му \
             байту обязана выйти {}",
            data.len(),
            BIG_FILE_STRIDE,
            BIG_FILE_CHECKSUM
        ),
        // Сюда попасть можно только если файл завёлся между проверкой выше и
        // этой строкой, то есть никогда; ветка оставлена, чтобы отказ «уже
        // есть» не выглядел поломкой образа.
        Err(ext2::Error::Exists) => say!("стенд: /{target} в образе уже лежит"),
        Err(err) => return Err(anyhow::anyhow!("не удалось записать /{target}: {err}")),
    }

    fs.flush_everywhere(&mut dev)
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
///
/// «Новее» здесь — не украшение, а условие работы: `osupdate::newer` сравнивает
/// версии почленно, и обновление, оказавшееся **старше** установленной системы,
/// отвергается запретом отката. Пока система была `0.1.<сборка>`, здесь стояло
/// `0.2`; с переходом на `0.3` эта константа обязана была уехать выше — иначе
/// сценарии `update` и `rollback` проверяли бы отказ вместо установки, причём
/// не сказав об этом ни слова. С переходом системы на `0.7` — снова выше.
pub const UPDATE_VERSION: &str = "0.8";

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

/// Проверка ожидаемого ответа — на хосте и без эмулятора.
///
/// # Зачем она вообще
///
/// Правило проекта: число, которое печатает гость, обязано быть проверено
/// счётом, который на госте не выполнялся. Сценарий сравнивает напечатанное с
/// [`BIG_FILE_CHECKSUM`], но сама эта константа выписана человеком — и без
/// проверки ниже она была бы утверждением, а не фактом: ошибись генератор в
/// формуле байта, сценарий начал бы честно требовать неверное число, и
/// разбираться пришлось бы на прогоне под QEMU, минут через пятнадцать после
/// причины.
///
/// Здесь же обе половины сходятся за долю секунды и без образа: свёртка
/// считается по тому самому [`big_file_byte`], которым файл и наполняется, а
/// сверяется с числом, посчитанным отдельно и вне этого дерева.
#[cfg(test)]
mod big_file_check {
    use super::{
        BIG_FILE_BYTES, BIG_FILE_CHECKSUM, BIG_FILE_CHECKSUM_LINE, BIG_FILE_SIZE_LINE,
        BIG_FILE_STRIDE, big_file_byte,
    };

    /// Свёртка по генератору совпадает с выписанным числом.
    ///
    /// Обход идёт по смещениям, а не по построенному буферу, и это не экономия
    /// ради экономии: [`super::big_file_bytes`] — это `map` по тому же
    /// [`big_file_byte`], то есть расходиться им нечем, а двести мебибайт в
    /// куче ради двух миллионов отсчётов сделали бы `cargo test -p xtask`
    /// заметно тяжелее без единого нового утверждения.
    #[test]
    fn the_written_out_checksum_is_the_one_the_generator_produces() {
        // Те же начальное значение и множитель, что у FNV-1a в программе:
        // разойдись хоть один — сойтись числа не смогут в принципе.
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        let mut offset = 0usize;
        while offset < BIG_FILE_BYTES {
            hash ^= u64::from(big_file_byte(offset));
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            offset += BIG_FILE_STRIDE;
        }
        assert_eq!(
            hash, BIG_FILE_CHECKSUM,
            "свёртка по генератору разошлась с числом, которого ждёт сценарий: \
             либо поменялась формула байта в big_file_byte, либо размер файла, \
             либо шаг обхода. Сценарий 'filemap-big' с этого мгновения требует \
             от гостя неверное число"
        );
    }

    /// Строка, которую ждёт сценарий, кончается тем самым числом.
    ///
    /// Строка и константа — две записи одного факта, и разойтись им нельзя:
    /// сценарий читает строку, а тест выше проверил число.
    #[test]
    fn the_awaited_line_ends_with_that_checksum() {
        let tail = BIG_FILE_CHECKSUM.to_string();
        assert!(
            BIG_FILE_CHECKSUM_LINE.ends_with(&tail),
            "строка сценария {BIG_FILE_CHECKSUM_LINE:?} не кончается на {tail}"
        );
    }
    /// И строка длины — тем же приёмом, что и строка свёртки.
    ///
    /// Ловит она ровно одно, зато то самое: файл поменяли, а строку, которую
    /// ждёт стенд, забыли. Сценарий после такой забывчивости падал бы на
    /// ожидании, ничего не сказав о причине.
    #[test]
    fn the_awaited_line_names_the_real_length() {
        let expected = alloc_line(BIG_FILE_BYTES);
        assert_eq!(BIG_FILE_SIZE_LINE, expected);
    }

    fn alloc_line(bytes: usize) -> String {
        format!("filemap: /media/big.dat is {bytes} bytes")
    }

}
