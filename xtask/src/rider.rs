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

use crate::java::{Item, RIDER_TOP, cached_download, rider_archive, stream_tar};

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

/// .NET SDK для сборки проектов из Rider (у Rider только среда исполнения, а
/// его кнопка «Install» качает SDK через `curl`, которого в системе нет).
/// SHA-512 Microsoft (`releases.json` 10.0) сверен вручную; здесь — SHA-256
/// того же архива: стенд умеет только его.
const DOTNET_SDK_URL: &str = "https://builds.dotnet.microsoft.com/dotnet/Sdk/10.0.403/dotnet-sdk-10.0.403-linux-x64.tar.gz";
const DOTNET_SDK_SHA256: &str = "cd7224fe8e8ba57c15f1601439ff8785a1a72b54ae5c89dd5302a32dcfbddf18";

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

/// Запись реестра для .NET SDK в `/opt/dotnet`: без неё у `dotnet` нет права
/// `jit`, и его JIT падает на первой странице «запись+исполнение» (Rider
/// создаёт проекты и собирает их этим `dotnet`).
const DOTNET_MANIFEST: &str = "# FossAi package manifest\n\
    name=dotnet\n\
    version=10.0.403\n\
    kind=package\n\
    requires=debian\n\
    summary=.NET SDK 10.0.403 for Linux, placed by the bench next to Rider\n\
    about=.NET SDK: сборка проектов C# для Rider\n\
    caption=.NET SDK\n\
    desktop=no\n\
    permissions=files net jit\n";

/// Параметры Java-машины Rider: его собственные плюс окна через Wayland, без
/// окна согласия на сбор данных при первом запуске и с кучей поменьше — у
/// машины стенда восемь гигабайт на всё.
///
/// Наблюдатель файлов включён снова (31.10): `fsnotifier` — статическая
/// программа под адрес `0x400000`, и до переезда ядра в верхнюю половину
/// `execve` отвечал на неё `ENOEXEC` («File watcher failed repeatedly»), а
/// с 30.10 стоял `idea.filewatcher.disabled`. Теперь нижние адреса —
/// программам.
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
    # Сервер диагностики .NET (сокет для dotnet-trace и профилировщиков) у\n\
    # нас не поднимается и крутит poll на несвязанном сокете — выключен.\n\
    export DOTNET_EnableDiagnostics=0\n\
    # Ссылки (вход в аккаунт JetBrains) — в браузер по умолчанию: xdg-open корня Debian.\n\
    export BROWSER=/usr/bin/xdg-open\n\
    # .NET SDK для сборки проектов (стенд кладёт его в /opt/dotnet).\n\
    export DOTNET_ROOT=/opt/dotnet\n\
    export PATH=/opt/dotnet:$PATH\n\
    export DOTNET_CLI_TELEMETRY_OPTOUT=1\n\
    export DOTNET_NOLOGO=1\n\
    # Первый запуск dotnet — настройка NuGet и кэш шаблонов, на стенде это\n\
    # десятки секунд; Rider ждёт шаблоны 30 секунд и сдаётся (Unable to\n\
    # expand project template). Пока поднимается Rider — прогреть в фоне.\n\
    if [ ! -d \"$HOME/.templateengine\" ]; then (dotnet new list >/dev/null 2>&1 &); fi\n\
    export RIDER_VM_OPTIONS=/opt/rider/fossai/rider64.vmoptions\n\
    exec /opt/rider/bin/rider \"$@\"\n";

/// Разложить Rider в раздел состояния образа `disk_path`.
pub fn place(disk_path: &Path) -> Result<()> {
    // Уже лежит тот же — второй раз не раскладываем: это минуты записи.
    if let Some(placed) = crate::statefs::read_file(disk_path, "opt/rider/fossai/placed")? {
        if placed.len() == BUILD.len() {
            say!("стенд: Rider {BUILD} уже разложен в /opt/rider");
            return Ok(());
        }
    }

    let archive = rider_archive()?;
    // ext2 или btrfs — по пробе тома; btrfs фиксируется по ходу раскладки.
    let mut fs = crate::statefs::StateVolume::open(disk_path)?;

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
                match fs.write_file_path(&place, &data, (mode & 0o7777) as u16, 0, 0) {
                    Ok(Some(_)) => {}
                    Ok(None) => failure = Some(anyhow::anyhow!("/{place} уже есть")),
                    Err(err) => failure = Some(err),
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
    let write = |fs: &mut crate::statefs::StateVolume, path: &str, data: &[u8], mode: u16| -> Result<()> {
        match fs.write_file_path(path, data, mode, 0, 0)? {
            Some(_) => Ok(()),
            None => Err(anyhow::anyhow!("/{path} уже есть")),
        }
    };
    write(&mut fs, "opt/rider/fossai/rider64.vmoptions", &options, 0o644)?;
    write(&mut fs, "opt/rider/fossai/rider", SCRIPT.as_bytes(), 0o755)?;
    // .NET SDK — в /opt/dotnet.
    let sdk_archive = cached_download(DOTNET_SDK_URL, DOTNET_SDK_SHA256)?;
    let (mut sdk_files, mut sdk_failure) = (0usize, None::<anyhow::Error>);
    stream_tar(&sdk_archive, |path| !path.is_empty(), |path, item| {
        if sdk_failure.is_some() {
            return;
        }
        if let Item::File { data, mode } = item {
            let place = format!("opt/dotnet/{path}");
            sdk_files += 1;
            match fs.write_file_path(&place, &data, (mode & 0o7777) as u16, 0, 0) {
                Ok(Some(_)) => {}
                Ok(None) => sdk_failure = Some(anyhow::anyhow!("/{place} уже есть")),
                Err(err) => sdk_failure = Some(err),
            }
        }
    })?;
    if let Some(err) = sdk_failure {
        return Err(err);
    }
    say!("стенд: .NET SDK 10.0.403 — {sdk_files} файлов в /opt/dotnet");
    for plugin in SKIPPED_PLUGINS {
        let place = format!("opt/rider/{plugin}/FOSSAI-SKIPPED");
        write(&mut fs, &place, b"not placed by the FossAi bench: no room on the bench disk\n", 0o644)?;
    }
    // Значок на столе и на панели — свой, логотип Rider (`bin/rider.png`,
    // 128×128), как у Chromium: не значок набора.
    let logo = logo.context("в архиве Rider нет bin/rider.png")?;
    let icon = crate::fimg::from_png(&logo).context("bin/rider.png не разобран")?;
    write(&mut fs, "opt/rider/fossai/icon.fimg", &icon, 0o644)?;
    fs.remove_path("var/lib/pkg/rider.pkg")?;
    fs.remove_path("var/lib/pkg/dotnet.pkg")?;
    write(&mut fs, "var/lib/pkg/rider.pkg", MANIFEST.as_bytes(), 0o644)?;
    write(&mut fs, "var/lib/pkg/dotnet.pkg", DOTNET_MANIFEST.as_bytes(), 0o644)?;
    write(&mut fs, "opt/rider/fossai/placed", BUILD.as_bytes(), 0o644)?;
    fs.finish()?;
    say!("стенд: Rider {BUILD} разложен в /opt/rider — {files} файлов, {} МиБ", bytes >> 20);
    Ok(())
}
