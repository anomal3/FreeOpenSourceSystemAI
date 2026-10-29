//! Rider на стенде: дистрибутив JetBrains для Linux, разложенный в
//! `/opt/rider` установленной системы со стороны хоста.
//!
//! # Почему не пакетом
//!
//! Шесть с половиной гигабайт. Пакеты едут на стенд носителем установщика, а
//! это FAT32 — четыре гигабайта на файл, и сам пакет держится в памяти при
//! сборке целиком. Поэтому так же, как большой файл и ключ SSH (см.
//! `crate::package::place_big_file`): файлы кладутся в раздел состояния образа
//! до запуска, вместе с записью реестра `/var/lib/pkg/rider.pkg` — для ядра
//! это установленный пакет, и право `jit` (Java и .NET внутри Rider) он даёт
//! так же, как настоящий.
//!
//! # Что не кладётся
//!
//! Двоичные файлы ReSharper под ARM, musl, Windows и macOS и каркасы macOS.
//! Плагины — все (до фазы 74 выбрасывались отладчик C++, `fullLine`, `jcef`,
//! RenderDoc и сервер удалённой разработки, и Rider не запускал свою половину
//! на .NET).
//!
//! # Запуск
//!
//! `/opt/rider/fossai/rider` — сценарий bash: библиотеки Wayland и шрифты из
//! пакета `java`, окна через `WLToolkit` (в `rider64.vmoptions` стоит `auto`),
//! .NET ReSharper без ICU (`DOTNET_SYSTEM_GLOBALIZATION_INVARIANT`), и родной
//! загрузчик `bin/rider`, который сам читает `product-info.json` и поднимает
//! Java в своём процессе.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};

use crate::java::{Item, RIDER_TOP, rider_archive, stream_tar};

/// Номер сборки — метка в `/opt/rider/fossai/placed`: тот же Rider второй раз
/// не раскладывается.
const BUILD: &str = "262.10968.170";

/// Пропускаемое — пути внутри каталога Rider.
///
/// Плагины кладутся все (с фазы 74): их список зашит в реестр модулей Rider
/// (`modules/module-descriptors.dat`), и перед запуском своей половины на
/// .NET Rider обходит каталог каждого — выброшенный `cidr-debugger-plugin`
/// обрывал запуск ReSharperHost на `NoSuchFileException`.
const SKIP: [&str; 14] = [
    "bin/JBDevice.framework/",
    "bin/Bridge.framework/",
    // Тяжёлые плагины без дела в системе: отладчик C++ с GDB/LLDB (1,2 ГБ),
    // локальная модель (434 МБ), встроенный Chromium (379 МБ, ему нужен X11).
    // Каталоги их остаются с пометкой (`SKIPPED_PLUGINS`): Rider перечисляет
    // каждый, а целиком они не влезали в раздел состояния стенда.
    "plugins/cidr-debugger-plugin/",
    "plugins/fullLine/",
    "plugins/jcef-plugin/",
    "lib/ReSharperHost/linux-arm64/",
    "lib/ReSharperHost/linux-arm/",
    "lib/ReSharperHost/linux-musl-x64/",
    "lib/ReSharperHost/linux-musl-arm64/",
    "lib/ReSharperHost/linux-musl-arm/",
    "lib/ReSharperHost/windows-",
    "lib/ReSharperHost/macos-",
    "lib/ReSharperHost/runtimes/win",
    "lib/ReSharperHost/runtimes/linux-musl",
];

/// Каталоги пропущенных плагинов: кладутся с файлом-пометкой.
const SKIPPED_PLUGINS: [&str; 3] = ["plugins/cidr-debugger-plugin", "plugins/fullLine", "plugins/jcef-plugin"];

/// Запись реестра: манифест, как его оставил бы `pkg`.
const MANIFEST: &str = "# FossAi package manifest\n\
    name=rider\n\
    version=2026.2.3.1\n\
    kind=package\n\
    requires=debian java\n\
    summary=JetBrains Rider 2026.2.3.1 for Linux, placed by the bench (too big for a package on the install media)\n\
    about=Rider: среда разработки C# от JetBrains\n\
    caption=Rider\n\
    start=/opt/debian/bin/bash /opt/rider/fossai/rider\n\
    desktop=yes\n\
    icon=/opt/rider/fossai/icon.fimg\n\
    permissions=windows files net jit\n";

/// Параметры Java-машины Rider: его собственные плюс окна через Wayland, без
/// окна согласия на сбор данных при первом запуске и с кучей поменьше — у
/// машины стенда восемь гигабайт на всё.
const VMOPTIONS_EXTRA: &str = "-Dawt.toolkit.name=WLToolkit\n\
    -Djb.consents.confirmation.enabled=false\n\
    -Djb.privacy.policy.text=<!--999.999-->\n\
    -Dide.show.tips.on.startup.default.value=false\n";

const SCRIPT: &str = "#!/bin/bash\n\
    # Rider на FossAi: библиотеки Wayland и шрифты — из пакета java, окна через Wayland.\n\
    export LD_LIBRARY_PATH=/opt/java/lib\n\
    export FONTCONFIG_FILE=/opt/java/fonts/fonts.conf\n\
    export XKB_CONFIG_ROOT=/opt/java/xkb\n\
    export XDG_RUNTIME_DIR=/run/user/0\n\
    export WAYLAND_DISPLAY=wayland-0\n\
    export XDG_SESSION_TYPE=wayland\n\
    export HOME=${HOME:-/root}\n\
    export TMPDIR=${TMPDIR:-$HOME}\n\
    # ICU в системе нет: .NET внутри Rider — без языковых таблиц.\n\
    export DOTNET_SYSTEM_GLOBALIZATION_INVARIANT=1\n\
    # JIT .NET — сразу в память «запись+исполнение» (право пакета jit), без\n\
    # двойного отображения memfd, которое .NET держит по умолчанию.\n\
    export DOTNET_EnableWriteXorExecute=0\n\
    # Куча .NET не больше 4 ГиБ: иначе сборщик мусора резервирует сотни\n\
    # гигабайт адресов разом — больше окна программы, и .NET падает «Out of memory».\n\
    export DOTNET_GCHeapHardLimit=0x100000000\n\
    # Ссылки (вход в аккаунт JetBrains) — в браузер по умолчанию: xdg-open корня Debian.\n\
    export BROWSER=/usr/bin/xdg-open\n\
    export RIDER_VM_OPTIONS=/opt/rider/fossai/rider64.vmoptions\n\
    exec /opt/rider/bin/rider \"$@\"\n";

/// Разложить Rider в раздел состояния образа `disk_path`.
pub fn place(disk_path: &Path) -> Result<()> {
    use disk::BlockDevice as _;

    let mut dev = crate::diskfile::DiskFile::open(disk_path, 512)?;
    let table = disk::gpt::read(&mut dev)
        .map_err(|err| anyhow::anyhow!("на образе {} нет GPT: {err}", disk_path.display()))?;
    let state = table
        .find(disk::gpt::FREEOS_STATE_TYPE)
        .ok_or_else(|| anyhow::anyhow!("на образе нет раздела состояния"))?;
    let mut fs = ext2::Editor::open(&mut dev, state.first_lba)
        .map_err(|err| anyhow::anyhow!("раздел состояния не открывается: {err}"))?;

    // Уже лежит тот же — второй раз не раскладываем: это минуты записи.
    let placed = lookup_path(&mut fs, &mut dev, "opt/rider/fossai/placed")?;
    if let Some(number) = placed {
        if fs.size_of(&mut dev, number).ok() == Some(BUILD.len() as u64) {
            say!("стенд: Rider {BUILD} уже разложен в /opt/rider");
            return Ok(());
        }
    }

    let archive = rider_archive()?;
    fs.mark_dirty(&mut dev)
        .map_err(|err| anyhow::anyhow!("не удалось пометить том используемым: {err}"))?;

    let mut links: BTreeMap<String, String> = BTreeMap::new();
    let mut vmoptions: Option<Vec<u8>> = None;
    let mut logo: Option<Vec<u8>> = None;
    let (mut files, mut bytes) = (0usize, 0u64);
    let mut failure: Option<anyhow::Error> = None;
    let keep = |path: &str| {
        path.strip_prefix(RIDER_TOP)
            .is_some_and(|rest| !rest.is_empty() && !SKIP.iter().any(|skip| rest.starts_with(skip)))
    };
    stream_tar(&archive, keep, |path, item| {
        if failure.is_some() {
            return;
        }
        let Some(rest) = path.strip_prefix(RIDER_TOP) else { return };
        let place = format!("opt/rider/{rest}");
        match item {
            Item::Link(target) => {
                links.insert(place, target);
            }
            Item::File { data, mode } => {
                if rest == "bin/rider64.vmoptions" {
                    vmoptions = Some(data.clone());
                }
                if rest == "bin/rider.png" {
                    logo = Some(data.clone());
                }
                files += 1;
                bytes += data.len() as u64;
                if files % 2000 == 0 {
                    say!("стенд: Rider — {files} файлов, {} МиБ", bytes >> 20);
                }
                if let Err(err) = fs.write_file_path(&mut dev, &place, &data, (mode & 0o7777) as u16, 0, 0) {
                    failure = Some(anyhow::anyhow!("не удалось записать /{place}: {err}"));
                }
            }
        }
    })?;
    if let Some(err) = failure {
        return Err(err);
    }
    // Ссылки в архиве Rider — только тексты лицензий (`jbr/legal`, на
    // соседний каталог) и каркасы macOS; для работы не нужно ни то, ни другое.
    say!("стенд: {} ссылок архива Rider не кладутся (лицензии jbr/legal, каркасы macOS)", links.len());

    let mut options = vmoptions.context("в архиве Rider нет bin/rider64.vmoptions")?;
    // `auto` выбирает X11, если не уверен в Wayland, — своё значение ниже
    // перекрывает его: последнее слово у последней строки.
    options.extend_from_slice(VMOPTIONS_EXTRA.as_bytes());
    let write = |fs: &mut ext2::Editor, dev: &mut crate::diskfile::DiskFile, path: &str, data: &[u8], mode: u16| {
        fs.write_file_path(dev, path, data, mode, 0, 0).map_err(|err| anyhow::anyhow!("не удалось записать /{path}: {err}"))
    };
    write(&mut fs, &mut dev, "opt/rider/fossai/rider64.vmoptions", &options, 0o644)?;
    write(&mut fs, &mut dev, "opt/rider/fossai/rider", SCRIPT.as_bytes(), 0o755)?;
    for plugin in SKIPPED_PLUGINS {
        let place = format!("opt/rider/{plugin}/FOSSAI-SKIPPED");
        write(&mut fs, &mut dev, &place, b"not placed by the FossAi bench: no room on the bench disk\n", 0o644)?;
    }
    // Значок на столе и на панели — свой, логотип Rider (`bin/rider.png`,
    // 128×128), как у Chromium: не значок набора.
    let logo = logo.context("в архиве Rider нет bin/rider.png")?;
    let icon = crate::fimg::from_png(&logo).context("bin/rider.png не разобран")?;
    write(&mut fs, &mut dev, "opt/rider/fossai/icon.fimg", &icon, 0o644)?;
    if let Some(parent) = lookup_path(&mut fs, &mut dev, "var/lib/pkg")? {
        let _ = fs.unlink(&mut dev, parent, "rider.pkg");
    }
    write(&mut fs, &mut dev, "var/lib/pkg/rider.pkg", MANIFEST.as_bytes(), 0o644)?;
    write(&mut fs, &mut dev, "opt/rider/fossai/placed", BUILD.as_bytes(), 0o644)?;

    fs.flush_everywhere(&mut dev)
        .map_err(|err| anyhow::anyhow!("не удалось сбросить раздел состояния: {err}"))?;
    fs.mark_clean(&mut dev)
        .map_err(|err| anyhow::anyhow!("не удалось пометить том чистым: {err}"))?;
    dev.flush().map_err(|err| anyhow::anyhow!("не удалось сбросить образ: {err}"))?;
    say!("стенд: Rider {BUILD} разложен в /opt/rider — {files} файлов, {} МиБ", bytes >> 20);
    Ok(())
}

/// Номер inode по пути от корня тома; `None` — нет такого.
fn lookup_path(fs: &mut ext2::Editor, dev: &mut crate::diskfile::DiskFile, path: &str) -> Result<Option<u32>> {
    let mut at = ext2::ROOT_INODE;
    for component in path.split('/').filter(|part| !part.is_empty()) {
        match fs.lookup(dev, at, component).map_err(|err| anyhow::anyhow!("не удалось прочитать /{path}: {err}"))? {
            Some((number, _)) => at = number,
            None => return Ok(None),
        }
    }
    Ok(Some(at))
}
