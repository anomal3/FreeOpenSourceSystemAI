//! Образ для телефона: собрать `boot-bare` и завернуть его так, как ждёт
//! заводской загрузчик.
//!
//! # Зачем обёртка
//!
//! Заводской загрузчик (на MediaTek это LK) не умеет запускать голый файл. Он
//! умеет ровно одно: Android boot image — заголовок в первой странице, дальше
//! ядро, дальше остальное, всё выровнено по странице. Наш `boot-bare` — это
//! ядро в его понимании, и вся эта обёртка нужна, чтобы он согласился на него
//! посмотреть.
//!
//! # Про адреса
//!
//! Заголовок несёт адреса, по которым загрузчик разложит части образа. У MTK
//! они одинаковы во всём семействе: база 0x40078000, ядро — на 0x8000 выше,
//! то есть ровно 0x40080000. Под этот адрес `boot-bare` и скомпонован, и
//! расхождение здесь стоило бы молчащей машины: переходы внутри образа
//! PC-относительные и продолжают работать, а всё остальное смотрит мимо.
//!
//! Проверить эти числа можно только по заводскому образу — для этого есть
//! `--read`, который печатает заголовок чужого boot.img. Пока сток не скачан,
//! числа ниже остаются соглашением семейства, а не измерением.
//!
//! # Про `bootopt=`
//!
//! Строка запуска у MTK не украшение: по `bootopt=64S3,32N2,64N2` LK узнаёт,
//! что ядро 64-битное. Заводская цепочка на этом аппарате 32-битная, и это
//! единственное место, где мы можем сказать обратное.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::paths;
use crate::util;

/// Соглашение MediaTek о том, куда ложатся части образа.
///
/// Смещения складываются с базой; так же это устроено в `mkbootimg`, и так же
/// написано в заголовке любого заводского образа этого семейства.
const MTK_BASE: u64 = 0x4007_8000;
const KERNEL_OFFSET: u64 = 0x0000_8000;
const RAMDISK_OFFSET: u64 = 0x11a8_8000;
const SECOND_OFFSET: u64 = 0x00f0_0000;
const TAGS_OFFSET: u64 = 0x0788_0000;
const PAGE_SIZE: u32 = 2048;

/// Строка запуска, по которой LK понимает разрядность.
const DEFAULT_CMDLINE: &str = "bootopt=64S3,32N2,64N2";

const MAGIC: &[u8; 8] = b"ANDROID!";

/// Что делать: собрать образ или разобрать чужой.
#[derive(Debug)]
pub struct Options {
    /// Версия заголовка. У аппарата на Android 10 и новее — 2; у машин постарше
    /// встречается 0. Загрузчик разбирает ту версию, под которую собран, и
    /// перебор здесь дешевле догадок.
    pub header_version: u32,
    /// База, от которой считаются смещения.
    pub base: u64,
    /// Строка запуска.
    pub cmdline: String,
    /// Дерево устройств, которое положить в образ (заголовок версии 2 отводит
    /// под него отдельное поле).
    ///
    /// Своего дерева у нас нет и быть не может: оно описывает конкретный
    /// аппарат. Поле оставлено, потому что заводское дерево появится вместе со
    /// стоком, и тогда его сюда подставят.
    pub dtb: Option<PathBuf>,
    /// Куда положить готовый образ.
    pub out: Option<PathBuf>,
    /// Не собирать `boot-bare`, а завернуть готовый файл.
    pub kernel: Option<PathBuf>,
    /// Надеть на ядро 512-байтовый заголовок MediaTek.
    ///
    /// У MTK части загрузочного образа завёрнуты ещё раз, в свой заголовок с
    /// меткой и именем. Одни версии LK его снимают, если он есть, другие
    /// снимают **всегда** — и тогда образ без него теряет первые 512 байт, то
    /// есть свой настоящий заголовок и начало кода, а переход уходит в середину
    /// инструкции. Отличить одно от другого можно только запуском.
    pub mtk_header: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            header_version: 2,
            base: MTK_BASE,
            cmdline: DEFAULT_CMDLINE.to_string(),
            dtb: None,
            out: None,
            kernel: None,
            mtk_header: false,
        }
    }
}

/// Заголовок MediaTek: метка, длина, имя части — и всё это ровно 512 байт.
fn mtk_wrap(payload: &[u8], name: &str) -> Vec<u8> {
    const SIZE: usize = 512;
    let mut header = vec![0xffu8; SIZE];
    header[0..4].copy_from_slice(&0x5888_1688u32.to_le_bytes());
    header[4..8].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    // Имя дополняется нулями, а не 0xff: LK сравнивает его как строку.
    for slot in header[8..40].iter_mut() {
        *slot = 0;
    }
    header[8..8 + name.len()].copy_from_slice(name.as_bytes());

    let mut out = header;
    out.extend_from_slice(payload);
    out
}

/// Собрать `boot-bare` и завернуть его в загрузочный образ.
pub fn build(options: &Options) -> Result<PathBuf> {
    let kernel = match &options.kernel {
        Some(path) => {
            if !path.is_file() {
                bail!("нет файла ядра: {}", path.display());
            }
            path.clone()
        }
        None => build_bare(options.base + KERNEL_OFFSET)?,
    };

    let out = match &options.out {
        Some(path) => path.clone(),
        None => paths::build_dir().join("bare-boot.img"),
    };

    let image = pack(&kernel, options)?;
    std::fs::write(&out, &image)
        .with_context(|| format!("не удалось записать {}", out.display()))?;

    say!();
    say!("образ для телефона собран: {}", out.display());
    describe(&image)?;
    say!();
    say!("Запустить **из ОЗУ**, ничего не записывая в аппарат:");
    say!("    fastboot boot {}", out.display());

    Ok(out)
}

/// Собрать standalone-образ `boot-bare` и вынуть из ELF голый двоичный код.
///
/// `expected_load` — адрес, по которому загрузчик положит образ. Он обязан
/// совпасть с адресом компоновки, и здесь это последнее место, где ещё можно
/// сверить: в голом двоичном файле адреса уже нет. Расхождение не даёт ни
/// отказа, ни сообщения — машина исправно крутится и молчит.
pub fn build_bare(expected_load: u64) -> Result<PathBuf> {
    let script = paths::workspace_root().join("crates/boot-bare/bare.ld");
    if !script.is_file() {
        bail!("нет компоновочного сценария: {}", script.display());
    }

    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut cmd = std::process::Command::new(cargo);
    cmd.current_dir(paths::workspace_root())
        .arg("build")
        .arg("--package")
        .arg("boot-bare")
        .arg("--target")
        .arg("aarch64-unknown-none")
        .arg("-Zbuild-std=core,compiler_builtins")
        .arg("-Zbuild-std-features=compiler-builtins-mem");

    // Компоновочный сценарий подаётся через RUSTFLAGS этого запуска: в
    // `.cargo/config.toml` он подействовал бы на весь workspace. Путь обязан
    // быть в родной для компоновщика записи — lld не находит файл по пути вида
    // `/e/...`, который подставляет оболочка git bash, и жалуется на
    // отсутствующий сценарий, а не на путь.
    let flags = format!(
        "-C link-arg=-T{} -C link-arg=--no-dynamic-linker \
         -C relocation-model=static -C panic=abort",
        script.display()
    );
    cmd.env("RUSTFLAGS", flags);
    util::run(&mut cmd, "cargo build (boot-bare)")?;

    let elf = paths::workspace_root()
        .join("target/aarch64-unknown-none/debug/boot-bare");
    if !elf.is_file() {
        bail!("boot-bare не собрался: нет {}", elf.display());
    }

    let linked = link_address(&elf)?;
    if linked != expected_load {
        bail!(
            "образ скомпонован под {linked:#x}, а загрузчик положит его по {expected_load:#x}\n\
             Совпасть обязаны: либо поправьте адрес в crates/boot-bare/bare.ld, \
             либо задайте --base."
        );
    }

    let out = paths::build_dir().join("bare-aarch64.img");
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir).ok();
    }

    // Загрузчик читает файл как есть, с первого байта: заголовок образа обязан
    // оказаться в самом начале файла, а не за заголовками ELF.
    let mut cmd = std::process::Command::new(objcopy()?);
    cmd.arg("-O").arg("binary").arg(&elf).arg(&out);
    util::run(&mut cmd, "llvm-objcopy (boot-bare)")?;

    Ok(out)
}

/// Адрес, под который скомпонован ELF, — его точка входа.
///
/// У `boot-bare` вход и начало образа — одно и то же: первая инструкция лежит в
/// первом байте, так требует договор загрузчика.
fn link_address(elf: &Path) -> Result<u64> {
    let bytes = std::fs::read(elf)
        .with_context(|| format!("не удалось прочитать {}", elf.display()))?;
    if bytes.len() < 32 || &bytes[..4] != b"\x7fELF" || bytes[4] != 2 {
        bail!("{} — не 64-разрядный ELF", elf.display());
    }
    Ok(u64::from_le_bytes(bytes[24..32].try_into().unwrap()))
}

/// Путь к `llvm-objcopy` из установленного toolchain.
///
/// Не «какой найдётся в PATH»: на машине может стоять objcopy от другого
/// набора инструментов, не понимающий наш ELF.
fn objcopy() -> Result<PathBuf> {
    let sysroot = std::process::Command::new(
        std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()),
    )
    .arg("--print")
    .arg("sysroot")
    .output()
    .context("не удалось спросить у rustc его sysroot")?;
    let sysroot = String::from_utf8_lossy(&sysroot.stdout).trim().to_string();
    let root = Path::new(&sysroot).join("lib/rustlib");

    // Имя host-триплета заранее не известно, поэтому каталог ищется перебором.
    let entries = std::fs::read_dir(&root)
        .with_context(|| format!("нет каталога {}", root.display()))?;
    for entry in entries.flatten() {
        for name in ["llvm-objcopy.exe", "llvm-objcopy"] {
            let candidate = entry.path().join("bin").join(name);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }

    bail!(
        "нет llvm-objcopy в {}\n\
         Поставьте его: rustup component add llvm-tools",
        root.display()
    )
}

/// Собрать байты загрузочного образа.
fn pack(kernel: &Path, options: &Options) -> Result<Vec<u8>> {
    if options.header_version > 2 {
        bail!(
            "версия заголовка {} не поддерживается: у неё ядро лежит уже не здесь, \
             а в отдельном разделе",
            options.header_version
        );
    }

    let kernel_bytes = std::fs::read(kernel)
        .with_context(|| format!("не удалось прочитать {}", kernel.display()))?;
    check_arm64_header(&kernel_bytes, kernel, options.base + KERNEL_OFFSET)?;
    let kernel_bytes = if options.mtk_header {
        mtk_wrap(&kernel_bytes, "KERNEL")
    } else {
        kernel_bytes
    };

    let dtb_bytes = match &options.dtb {
        Some(path) => std::fs::read(path)
            .with_context(|| format!("не удалось прочитать {}", path.display()))?,
        None => Vec::new(),
    };
    if !dtb_bytes.is_empty() && options.header_version < 2 {
        bail!("дерево устройств помещается в образ только с заголовком версии 2");
    }

    let page = PAGE_SIZE as usize;
    let mut header = vec![0u8; page];
    header[..8].copy_from_slice(MAGIC);

    let mut put32 = |header: &mut Vec<u8>, at: usize, value: u32| {
        header[at..at + 4].copy_from_slice(&value.to_le_bytes());
    };
    let base = options.base;

    put32(&mut header, 8, kernel_bytes.len() as u32);
    put32(&mut header, 12, (base + KERNEL_OFFSET) as u32);
    // Диска у нас нет, файловой системы в ОЗУ тоже: `boot-bare` самодостаточен.
    // Нулевой размер здесь — это заявление «ядру нечего подавать», а не
    // забытое поле.
    put32(&mut header, 16, 0);
    put32(&mut header, 20, (base + RAMDISK_OFFSET) as u32);
    put32(&mut header, 24, 0);
    put32(&mut header, 28, (base + SECOND_OFFSET) as u32);
    put32(&mut header, 32, (base + TAGS_OFFSET) as u32);
    put32(&mut header, 36, PAGE_SIZE);
    put32(&mut header, 40, options.header_version);
    put32(&mut header, 44, 0); // версия ОС и уровень заплаток — не наши

    let cmdline = options.cmdline.as_bytes();
    if cmdline.len() >= 512 {
        bail!("строка запуска длиннее 511 байт");
    }
    header[64..64 + cmdline.len()].copy_from_slice(cmdline);

    if options.header_version >= 1 {
        put32(&mut header, 1632, 0); // recovery_dtbo_size
        // recovery_dtbo_offset — восемь байт, у нас ноль
        put32(&mut header, 1644, if options.header_version == 1 { 1648 } else { 1660 });
    }
    if options.header_version >= 2 {
        put32(&mut header, 1648, dtb_bytes.len() as u32);
        let dtb_addr = if dtb_bytes.is_empty() { 0 } else { base + TAGS_OFFSET };
        header[1652..1660].copy_from_slice(&dtb_addr.to_le_bytes());
    }

    // Отпечаток: SHA-1 по содержимому и размерам всех частей. Загрузчик его не
    // проверяет, а вот `unpack_bootimg` и глаз человека — да, и образ с нулями
    // в этом поле выглядит как собранный на коленке.
    let id = image_id(&kernel_bytes, &dtb_bytes, options.header_version);
    header[576..576 + 20].copy_from_slice(&id);

    let mut image = header;
    push_padded(&mut image, &kernel_bytes, page);
    if !dtb_bytes.is_empty() {
        push_padded(&mut image, &dtb_bytes, page);
    }

    Ok(image)
}

/// Дописать часть образа и добить страницу нулями.
fn push_padded(image: &mut Vec<u8>, part: &[u8], page: usize) {
    image.extend_from_slice(part);
    let tail = image.len() % page;
    if tail != 0 {
        image.resize(image.len() + (page - tail), 0);
    }
}

/// Убедиться, что заворачиваем именно образ arm64, а не что попало.
///
/// Проверка дешёвая и снимает целый класс поисков: загрузчик, получив не тот
/// файл, обычно молчит и перезагружается, и отличить «не наш формат» от «наш
/// код упал» по такому поведению нельзя.
fn check_arm64_header(bytes: &[u8], path: &Path, load: u64) -> Result<()> {
    if bytes.len() < 64 {
        bail!("{} короче заголовка arm64", path.display());
    }
    if &bytes[56..60] != b"ARM\x64" {
        bail!(
            "в {} нет метки ARM\\x64 на 56-м байте — это не образ arm64",
            path.display()
        );
    }

    // Договор ARM64 говорит про адрес не «какой угодно»: образ обязан лежать по
    // `text_offset` от базы, выровненной на 2 МиБ. Это ровно то, что делает
    // QEMU с ключом `-kernel`, и ровно то, чему обязан соответствовать адрес из
    // заголовка загрузочного образа, — иначе одно и то же ядро запустится на
    // одной машине и промолчит на другой.
    let text_offset = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
    let ram_base = load.wrapping_sub(text_offset);
    if ram_base % (2 * 1024 * 1024) != 0 {
        bail!(
            "образ просит смещение {text_offset:#x}, загрузчик кладёт его по {load:#x} — \
             база {ram_base:#x} не выровнена на 2 МиБ"
        );
    }

    let declared = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
    if declared as usize > bytes.len() * 64 {
        bail!("в заголовке {} неправдоподобный размер {declared:#x}", path.display());
    }

    Ok(())
}

/// Отпечаток образа так, как его считает `mkbootimg`: по каждой части — её
/// содержимое, следом её размер.
fn image_id(kernel: &[u8], dtb: &[u8], header_version: u32) -> [u8; 20] {
    let mut sha = Sha1::new();
    let mut part = |sha: &mut Sha1, bytes: &[u8]| {
        sha.update(bytes);
        sha.update(&(bytes.len() as u32).to_le_bytes());
    };
    part(&mut sha, kernel);
    part(&mut sha, &[]); // ramdisk
    part(&mut sha, &[]); // second
    if header_version >= 1 {
        part(&mut sha, &[]); // recovery_dtbo
    }
    if header_version >= 2 {
        part(&mut sha, dtb);
    }
    sha.finish()
}

/// Прочитать заголовок чужого образа и напечатать его.
///
/// Ради одного: заводской boot.img — единственный источник настоящих адресов.
/// Всё, что до него, — соглашение семейства.
pub fn read(path: &Path) -> Result<()> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("не удалось прочитать {}", path.display()))?;
    if bytes.len() < 2048 || &bytes[..8] != MAGIC {
        bail!("{} — не Android boot image", path.display());
    }
    describe(&bytes)
}

/// Напечатать разбор заголовка.
fn describe(bytes: &[u8]) -> Result<()> {
    let word = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());

    let version = word(40);
    let page = word(36);
    let kernel_addr = word(12) as u64;
    // База не хранится: её восстанавливают вычитанием того же смещения, которое
    // при сборке прибавляли. Иначе сравнить два образа не с чем.
    let base = kernel_addr.wrapping_sub(KERNEL_OFFSET);

    say!("  заголовок  : версия {version}, страница {page} байт");
    say!("  база       : {:#010x}", base);
    say!("  ядро       : {:#010x}, {} байт", kernel_addr, word(8));
    say!("  ramdisk    : {:#010x}, {} байт", word(20), word(16));
    say!("  метки      : {:#010x}", word(32));
    if version >= 2 && bytes.len() >= 1660 {
        let dtb_addr = u64::from_le_bytes(bytes[1652..1660].try_into().unwrap());
        say!("  дерево     : {:#010x}, {} байт", dtb_addr, word(1648));
    }
    let cmdline = &bytes[64..64 + 512];
    let end = cmdline.iter().position(|&b| b == 0).unwrap_or(cmdline.len());
    say!("  строка     : {}", String::from_utf8_lossy(&cmdline[..end]));
    Ok(())
}

// --- SHA-1 -----------------------------------------------------------------
//
// Своя, потому что ради одного поля заголовка тянуть зависимость незачем, а
// стойкость здесь ни при чём: отпечаток описывает образ, а не защищает его.

struct Sha1 {
    state: [u32; 5],
    buffer: [u8; 64],
    filled: usize,
    length: u64,
}

impl Sha1 {
    fn new() -> Self {
        Self {
            state: [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476, 0xc3d2_e1f0],
            buffer: [0; 64],
            filled: 0,
            length: 0,
        }
    }

    fn update(&mut self, mut data: &[u8]) {
        self.length += data.len() as u64;
        while !data.is_empty() {
            let take = (64 - self.filled).min(data.len());
            self.buffer[self.filled..self.filled + take].copy_from_slice(&data[..take]);
            self.filled += take;
            data = &data[take..];
            if self.filled == 64 {
                let block = self.buffer;
                self.compress(&block);
                self.filled = 0;
            }
        }
    }

    fn finish(mut self) -> [u8; 20] {
        let bits = self.length * 8;
        self.update(&[0x80]);
        while self.filled != 56 {
            self.update(&[0]);
        }
        self.update(&bits.to_be_bytes());

        let mut out = [0u8; 20];
        for (chunk, word) in out.chunks_mut(4).zip(self.state) {
            chunk.copy_from_slice(&word.to_be_bytes());
        }
        out
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut words = [0u32; 80];
        for (index, chunk) in block.chunks_exact(4).enumerate() {
            words[index] = u32::from_be_bytes(chunk.try_into().unwrap());
        }
        for index in 16..80 {
            let value = words[index - 3] ^ words[index - 8] ^ words[index - 14] ^ words[index - 16];
            words[index] = value.rotate_left(1);
        }

        let [mut a, mut b, mut c, mut d, mut e] = self.state;
        for (index, word) in words.iter().enumerate() {
            let (mix, constant) = match index {
                0..=19 => ((b & c) | (!b & d), 0x5a82_7999),
                20..=39 => (b ^ c ^ d, 0x6ed9_eba1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8f1b_bcdc),
                _ => (b ^ c ^ d, 0xca62_c1d6),
            };
            let next = a
                .rotate_left(5)
                .wrapping_add(mix)
                .wrapping_add(e)
                .wrapping_add(constant)
                .wrapping_add(*word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = next;
        }

        for (slot, value) in self.state.iter_mut().zip([a, b, c, d, e]) {
            *slot = slot.wrapping_add(value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha1_matches_the_published_vectors() {
        let mut sha = Sha1::new();
        sha.update(b"abc");
        assert_eq!(
            sha.finish(),
            [
                0xa9, 0x99, 0x3e, 0x36, 0x47, 0x06, 0x81, 0x6a, 0xba, 0x3e, 0x25, 0x71, 0x78, 0x50,
                0xc2, 0x6c, 0x9c, 0xd0, 0xd8, 0x9d,
            ]
        );

        // Длина ровно в блок и длина за блоком: границы, на которых ошибаются.
        let mut sha = Sha1::new();
        sha.update(&[b'a'; 64]);
        let one = sha.finish();
        let mut sha = Sha1::new();
        for _ in 0..64 {
            sha.update(b"a");
        }
        assert_eq!(one, sha.finish(), "по байту и целиком — один и тот же ответ");
    }
}
