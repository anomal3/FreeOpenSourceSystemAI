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
    for try in 1 2 3; do\n\
        [ -f \"$WINEPREFIX/drive_c/windows/syswow64/kernel32.dll\" ] && [ -f \"$WINEPREFIX/system.reg\" ] && break\n\
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
echo \"tmnf: starting TmForever.exe\"\n\
exec /opt/debian/bin/bash $P \"$@\" --dxvk \"$G/TmForever.exe\"\n";

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
