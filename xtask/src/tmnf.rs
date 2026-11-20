//! Trackmania Nations Forever (12.11.2026, шаг 8 плана Proton): настоящая
//! бесплатная игра на DirectX 9 (Nadeo, 2008) — итог вехи Proton.
//!
//! Пакет `tmnf` — установщик игры как есть (Inno Setup, 530 МБ) и сценарий
//! `bin/tmnf`: при первом запуске он ставит игру в свой префикс Wine (WoW64:
//! игра 32-битная) молча, потом запускает `TmForever.exe` через DXVK. Пакет
//! стоит на пакете `proton` и собирается вместе с ним (`FOSSAI_WITH_PROTON=1`).

use std::fs;
use std::path::PathBuf;

use anyhow::Result;
use fpk::Kind;
use fpk::build::{Builder, Entry};

use crate::cbuild;

/// Имя пакета.
pub const PACKAGE_FILE: &str = "tmnf-2.11.fpk";

/// Установщик с сервера Nadeo (Last-Modified 29.03.2011).
pub const URL: &str = "http://files.trackmaniaforever.com/tmnationsforever_setup.exe";
pub const SHA256: &str = "2f659138ed4409da404970841e18f03d29921beaf6a424824c8312ddb20f6355";

/// Где лежит собранный пакет.
pub fn package_path() -> PathBuf {
    cbuild::games_dir().join(PACKAGE_FILE)
}

const SCRIPT: &str = "#!/bin/bash\n\
# Trackmania Nations Forever через Proton (12.11.2026).\n\
# Первый запуск ставит игру в свой префикс (WoW64 — игра 32-битная).\n\
P=/opt/proton/bin/fossai-proton\n\
export WINEPREFIX=${WINEPREFIX:-$HOME/.tmnf}\n\
export WINEARCH=wow64\n\
G=\"$WINEPREFIX/drive_c/TmNationsForever\"\n\
if [ ! -f \"$G/TmForever.exe\" ]; then\n\
    echo \"tmnf: installing into $WINEPREFIX\"\n\
    # Префикс — заранее и до конца: создание WoW64-префикса в QEMU дольше\n\
    # ожидания Wine («boot event wait timed out» → установщик без kernel32).\n\
    # Первая попытка — `--prefix` (17.11): шаблон из пакета Proton за секунды,\n\
    # без него — `wineboot`; дальше — `wineboot` ещё раз.\n\
    for try in 1 2 3; do\n\
        [ -f \"$WINEPREFIX/drive_c/windows/syswow64/kernel32.dll\" ] && [ -f \"$WINEPREFIX/system.reg\" ] && break\n\
        if [ $try = 1 ]; then echo \"tmnf: prefix\"; /opt/debian/bin/bash $P --prefix; continue; fi\n\
        echo \"tmnf: wineboot, try $try\"\n\
        /opt/debian/bin/bash $P wineboot -i\n\
        /opt/debian/bin/bash $P --run=wineserver -w\n\
    done\n\
    echo \"tmnf: prefix ready\"\n\
    /opt/debian/bin/bash $P /opt/tmnf/tmnationsforever_setup.exe /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /SP- '/DIR=C:\\TmNationsForever'\n\
    if [ ! -f \"$G/TmForever.exe\" ]; then echo \"tmnf: install FAILED\"; exit 1; fi\n\
    echo \"tmnf: installed\"\n\
fi\n\
cd \"$G\"\n\
# Отрисовка (18.11): видеокарты у Wine пока нет, кадры рисует процессор.\n\
# По умолчанию wined3d (Direct3D 9 -> OpenGL на llvmpipe, кадры/с - в\n\
# журнал): замер 18.11 в QEMU на 8 ЦП - 11 к/с в 1280x720 и 10 в 2560x1440\n\
# против 3-4 и 2,7 у DXVK на lavapipe. `--dxvk` - Vulkan (счётчик на экране),\n\
# пригодится, когда у Vulkan будет видеокарта.\n\
MODE=\n\
# Размер (P2, 18.11): игра сама берёт размер монитора (2560x1440 на ПК\n\
# Романа - вчетверо больше точек, чем 720p, и всё на процессоре).\n\
# `--720p` - виртуальный рабочий стол Wine 1280x720, `--1080p` - 1920x1080,\n\
# `--desktop=ШxВ` - свой. Пока ОПЫТ, не по умолчанию: в QEMU (18.11) стол\n\
# под нашим Wayland вышел отдельным окном 665x434, окно игры - своим, кадров\n\
# DXVK не было. По умолчанию (`--native`) - как раньше.\n\
# `--bench[=СЕКУНДЫ]` - замер: через СЕКУНДЫ (60) в журнал строка\n\
# `bench: tmnf dxvk 1280x720 avg N fps`, игра снимается.\n\
SIZE=\n\
BENCH=\n\
# Видеокарта (20.11): `--nvk` - DXVK на NVK без проверки, `--cpu` - кадры на\n\
# процессоре всегда; по умолчанию - сам (см. ниже).\n\
GPU=auto\n\
while :; do\n\
    case \"$1\" in\n\
        --wined3d) MODE=; shift ;;\n\
        --dxvk) MODE=--dxvk; shift ;;\n\
        --nvk) GPU=nvk; shift ;;\n\
        --cpu) GPU=cpu; shift ;;\n\
        --720p) SIZE=1280x720; shift ;;\n\
        --1080p) SIZE=1920x1080; shift ;;\n\
        --desktop=*) SIZE=${1#--desktop=}; shift ;;\n\
        --native) SIZE=; shift ;;\n\
        --bench) BENCH=60; shift ;;\n\
        --bench=*) BENCH=${1#--bench=}; shift ;;\n\
        *) break ;;\n\
    esac\n\
done\n\
# Есть узел видеокарты (/dev/dri/renderD128: `nvk hw on` или `gpu gametest`)\n\
# и NVK за 60 с называет себя в vulkaninfo - Direct3D 9 через DXVK на\n\
# видеокарте (RTX 3060 через GSP-RM). Иначе - как раньше, процессор.\n\
if [ \"$GPU\" = auto ] && [ -e /dev/dri/renderD128 ] && [ \"$FOSSAI_VULKAN\" != lvp ]; then\n\
    I=$(FOSSAI_VULKAN=nvk /opt/debian/bin/timeout -k 5 60 /opt/debian/bin/bash $P --run=vulkaninfo --summary 2>&1)\n\
    case $I in\n\
        *NVK*) GPU=nvk; echo \"tmnf: the video card answers through NVK - DXVK on the GPU\" ;;\n\
        *) GPU=cpu; echo \"tmnf: NVK did not answer in 60 s - drawing on the CPU: ${I:0:300}\" ;;\n\
    esac\n\
fi\n\
if [ \"$GPU\" = nvk ]; then MODE=--dxvk; export FOSSAI_VULKAN=nvk; fi\n\
if [ \"$GPU\" = cpu ] && [ -z \"$MODE\" ]; then export FOSSAI_VULKAN=lvp; fi\n\
# `+fps` и у DXVK (20.11): Wine 11 считает кадры Vulkan в win32u, а\n\
# fossai-proton раз в 5 с кладёт сводку в журнал ядра (/dev/kmsg).\n\
[ -n \"$MODE\" ] && export DXVK_HUD=${DXVK_HUD:-fps,frametimes}\n\
export WINEDEBUG=${WINEDEBUG:-fixme-all,+fps}\n\
A=()\n\
[ -n \"$SIZE\" ] && A+=(\"--desktop=$SIZE\")\n\
R=dxvk; [ -z \"$MODE\" ] && R=wined3d; [ \"$GPU\" = nvk ] && R=dxvk-nvk\n\
[ -n \"$BENCH\" ] && A+=(\"--bench=$BENCH,tmnf $R ${SIZE:-native}\")\n\
echo \"tmnf: starting TmForever.exe ($R, ${SIZE:-native size})\"\n\
exec /opt/debian/bin/bash $P \"${A[@]}\" \"$@\" $MODE \"$G/TmForever.exe\"\n";

/// Собрать пакет.
pub fn build() -> Result<()> {
    let setup = crate::java::cached_download(URL, SHA256)?;
    let data = fs::read(&setup)?;
    let mut builder = Builder::new(Kind::Package, "tmnf", "2.11");
    builder.field("summary", "Trackmania Nations Forever (Nadeo, freeware, DirectX 9) through Proton and DXVK");
    builder.field("about", "Trackmania Nations Forever");
    builder.field("start", "/opt/debian/bin/bash /opt/tmnf/bin/tmnf");
    builder.field("permissions", "windows files net jit");
    builder.requires("proton");
    builder.file(&Entry { path: String::from("bin/tmnf"), mode: 0o755, uid: 0, gid: 0, data: SCRIPT.as_bytes().to_vec() });
    builder.file(&Entry { path: String::from("tmnationsforever_setup.exe"), mode: 0o644, uid: 0, gid: 0, data });
    let out = package_path();
    fs::create_dir_all(out.parent().unwrap())?;
    fs::write(&out, builder.finish())?;
    say!("tmnf: {}", out.display());
    Ok(())
}
