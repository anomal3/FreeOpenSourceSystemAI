//! Описание поддерживаемых архитектур и всего, что от них зависит.

use std::fmt;
use std::path::{Path, PathBuf};

use clap::ValueEnum;

/// Размер flash-устройства машины `virt` в QEMU (hw/arm/virt.c: 64 MiB на банк).
/// Через `-drive if=pflash` образ обязан быть ровно такого размера, иначе QEMU
/// откажется стартовать.
pub const ARM_VIRT_FLASH_SIZE: u64 = 64 * 1024 * 1024;

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum Arch {
    #[value(name = "x86_64", alias = "x64")]
    X86_64,
    #[value(name = "aarch64", alias = "arm64")]
    Aarch64,
}

impl Arch {
    pub const ALL: [Arch; 2] = [Arch::X86_64, Arch::Aarch64];

    pub fn name(self) -> &'static str {
        match self {
            Arch::X86_64 => "x86_64",
            Arch::Aarch64 => "aarch64",
        }
    }

    /// Стандартный путь UEFI removable media (UEFI spec, "Boot Option Behavior"):
    /// если в NVRAM нет записей загрузки, прошивка сама ищет на ESP файл
    /// `\EFI\BOOT\BOOT<MACHINE>.EFI`. Благодаря этому загрузчик стартует без
    /// какой-либо настройки переменных — идеально для одноразовой VM.
    pub fn removable_media_file(self) -> &'static str {
        match self {
            Arch::X86_64 => "BOOTX64.EFI",
            Arch::Aarch64 => "BOOTAA64.EFI",
        }
    }

    pub fn qemu_binary(self) -> &'static str {
        match self {
            Arch::X86_64 => "qemu-system-x86_64",
            Arch::Aarch64 => "qemu-system-aarch64",
        }
    }

    /// Переопределение бинарника QEMU.
    pub fn qemu_env(self) -> &'static str {
        match self {
            Arch::X86_64 => "FREEOS_QEMU_X86_64",
            Arch::Aarch64 => "FREEOS_QEMU_AARCH64",
        }
    }

    /// Переопределение образа прошивки (code / unified).
    pub fn firmware_env(self) -> &'static str {
        match self {
            Arch::X86_64 => "FREEOS_OVMF_X86_64",
            Arch::Aarch64 => "FREEOS_OVMF_AARCH64",
        }
    }

    /// Переопределение шаблона NVRAM (vars).
    pub fn nvram_env(self) -> &'static str {
        match self {
            Arch::X86_64 => "FREEOS_OVMF_VARS_X86_64",
            Arch::Aarch64 => "FREEOS_OVMF_VARS_AARCH64",
        }
    }

    /// Фиксированный размер flash-банка, если платформа его требует.
    ///
    /// x86: q35 создаёт pflash по размеру самого файла — дополнять не нужно.
    /// arm virt: банки жёстко по 64 MiB — короткий образ надо дополнить.
    pub fn pflash_size(self) -> Option<u64> {
        match self {
            Arch::X86_64 => None,
            Arch::Aarch64 => Some(ARM_VIRT_FLASH_SIZE),
        }
    }

    /// Пары (code, vars) раздельной прошивки edk2, в порядке предпочтения.
    ///
    /// Первая пара — то, что кладёт рядом с собой сам QEMU (см. дескрипторы
    /// `pc-bios/descriptors/60-edk2-*.json`). Обратите внимание на асимметрию
    /// имён: varstore для x86_64 называется `edk2-i386-vars.fd`, а для
    /// aarch64 — `edk2-arm-vars.fd`; формат хранилища переменных общий для
    /// 32/64-битных вариантов, поэтому QEMU не дублирует файлы.
    pub fn split_firmware_names(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Arch::X86_64 => &[
                ("edk2-x86_64-code.fd", "edk2-i386-vars.fd"),
                ("edk2-x86_64-secure-code.fd", "edk2-i386-vars.fd"),
                ("OVMF_CODE_4M.fd", "OVMF_VARS_4M.fd"),
                ("OVMF_CODE.4m.fd", "OVMF_VARS.4m.fd"),
                ("OVMF_CODE.fd", "OVMF_VARS.fd"),
            ],
            Arch::Aarch64 => &[
                ("edk2-aarch64-code.fd", "edk2-arm-vars.fd"),
                ("AAVMF_CODE.fd", "AAVMF_VARS.fd"),
                ("QEMU_EFI-pflash.raw", "QEMU_VARS-pflash.raw"),
                ("QEMU_EFI.fd", "QEMU_VARS.fd"),
            ],
        }
    }

    /// Единые образы, пригодные для `-bios`, в порядке предпочтения.
    ///
    /// Для x86 сюда попадает только `OVMF.fd`: это склейка VARS+CODE, которая
    /// действительно работает через `-bios`. Отдельный `OVMF_CODE.fd` через
    /// `-bios` подавать нельзя — прошивка не найдёт хранилище переменных и
    /// зависнет на чёрном экране (см. tianocore wiki "How to run OVMF").
    ///
    /// Для ARM наоборот: `ArmVirtQemu` (QEMU_EFI.fd / edk2-aarch64-code.fd)
    /// штатно грузится через `-bios`, теряя лишь персистентность переменных.
    pub fn unified_firmware_names(self) -> &'static [&'static str] {
        match self {
            Arch::X86_64 => &["OVMF.fd", "edk2-x86_64.fd"],
            Arch::Aarch64 => &[
                "QEMU_EFI.fd",
                "edk2-aarch64-code.fd",
                "AAVMF_CODE.fd",
                "QEMU_EFI-pflash.raw",
            ],
        }
    }
}

impl fmt::Display for Arch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Имя файла ядра в корне ESP.
///
/// Значение — часть контракта с загрузчиком: он открывает на своём ESP ровно
/// `\kernel.elf`, без поиска по маске и без вариантов. Менять только вместе с
/// загрузчиком.
pub const KERNEL_ESP_FILE: &str = "kernel.elf";

/// Имя образа RAM-диска в корне ESP.
///
/// Как и с ядром, это контракт: загрузчик открывает ровно `\initrd.img` и
/// передаёт его ядру. Отсутствие файла — не ошибка (см. `--no-initrd`), ядро
/// обязано подниматься и без файловой системы.
pub const INITRD_ESP_FILE: &str = "initrd.img";

/// Собираемый компонент ОС.
///
/// Заведён ради одного: чтобы соответствие (компонент, архитектура) -> триплет
/// жило в единственном месте — в [`Component::triple`]. Триплет зависит не
/// только от архитектуры: загрузчик — это PE-приложение, которое вызывает
/// прошивка, ему нужен `*-unknown-uefi` со всем UEFI-ABI; ядро же получает
/// управление после ExitBootServices, когда прошивки уже нет, и собирается под
/// freestanding-таргет `*-unknown-none`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Component {
    BootUefi,
    Kernel,
    Installer,
}

impl Component {
    /// Порядок важен: загрузчик собирается первым, потому что без него нечего
    /// запускать, и его ошибки пользователь увидит раньше.
    pub const ALL: [Component; 3] = [
        Component::BootUefi,
        Component::Kernel,
        Component::Installer,
    ];

    /// Компоненты, из которых состоит установленная система.
    ///
    /// Установщик в неё не входит: он живёт только на установочном носителе, и
    /// класть его на целевой диск значило бы предлагать переустановку с уже
    /// установленной системы.
    pub const SYSTEM: [Component; 2] = [Component::BootUefi, Component::Kernel];

    /// Имя пакета для `cargo --package`.
    pub fn package(self) -> &'static str {
        match self {
            Component::BootUefi => "boot-uefi",
            Component::Kernel => "kernel",
            Component::Installer => "installer",
        }
    }

    /// Как компонент называется в сообщениях пользователю.
    pub fn title(self) -> &'static str {
        match self {
            Component::BootUefi => "загрузчик",
            Component::Kernel => "ядро",
            Component::Installer => "установщик",
        }
    }

    /// Единственная таблица «(компонент, архитектура) -> триплет» в крейте.
    pub fn triple(self, arch: Arch) -> &'static str {
        match (self, arch) {
            (Component::BootUefi | Component::Installer, Arch::X86_64) => "x86_64-unknown-uefi",
            (Component::BootUefi | Component::Installer, Arch::Aarch64) => "aarch64-unknown-uefi",
            (Component::Kernel, Arch::X86_64) => "x86_64-unknown-none",
            // Именно softfloat-вариант: обычный `aarch64-unknown-none` объявлен
            // hardfloat, и компилятор вправе эмитить SIMD, которую обработчик
            // прерывания не сохраняет. Подробности — в `.cargo/config.toml`.
            (Component::Kernel, Arch::Aarch64) => "aarch64-unknown-none-softfloat",
        }
    }

    /// Ожидаемое имя файла в `target/<triple>/<profile>/`.
    ///
    /// Расширение задаётся спецификацией таргета (поле `exe_suffix`), а не
    /// cargo: у `*-unknown-uefi` это `.efi`, потому что прошивка грузит PE; у
    /// `*-unknown-none` суффикса нет вовсе, и артефакт ядра лежит просто как
    /// `kernel`, без расширения. Переименование в `kernel.elf` происходит уже
    /// при раскладке ESP.
    pub fn artifact_file(self) -> &'static str {
        match self {
            Component::BootUefi => "boot-uefi.efi",
            Component::Kernel => "kernel",
            Component::Installer => "installer.efi",
        }
    }

    /// Путь внутри ESP относительно корня раздела.
    pub fn esp_path(self, arch: Arch) -> PathBuf {
        match self {
            Component::BootUefi | Component::Installer => Path::new("EFI")
                .join("BOOT")
                .join(arch.removable_media_file()),
            Component::Kernel => PathBuf::from(KERNEL_ESP_FILE),
        }
    }

    /// Путь внутри установочного носителя, откуда установщик берёт этот
    /// компонент.
    ///
    /// Загрузчик лежит не по стандартному пути, потому что тот занят самим
    /// установщиком: прошивка запускает `\EFI\BOOT\BOOT*.EFI`, и там обязан
    /// быть тот, кто ставит систему, а не тот, кого ставят. Имена в верхнем
    /// регистре — контракт с установщиком, который открывает ровно эти пути.
    pub fn payload_path(self, arch: Arch) -> Option<String> {
        match self {
            Component::BootUefi => Some(format!(
                "{PAYLOAD_DIR}/{}",
                arch.removable_media_file()
            )),
            Component::Kernel => Some(format!("{PAYLOAD_DIR}/KERNEL.ELF")),
            Component::Installer => None,
        }
    }
}

/// Каталог на установочном носителе, где лежит переносимая система.
pub const PAYLOAD_DIR: &str = "FREEOS";

/// Имя образа RAM-диска в каталоге полезной нагрузки.
pub const PAYLOAD_INITRD: &str = "FREEOS/INITRD.IMG";

/// Доверенные ключи обновления на установочном носителе.
///
/// Установщик кладёт их в корень как `/os-keys`. Имя в верхнем регистре и без
/// точки — тот же контракт с установщиком, что и у остальной полезной нагрузки:
/// он открывает ровно эти пути.
pub const PAYLOAD_KEYS: &str = "FREEOS/OSKEYS";

/// Каталог эталонных настроек на установочном носителе.
///
/// Единственный экземпляр этих файлов лежит в репозитории, в
/// `initrd/usr/share/defaults/etc/`. Установщик переносит их в **корневой
/// образ**, в `/usr/share/defaults/etc/`, а не на раздел состояния: умолчание
/// принадлежит образу и обязано заменяться вместе с ним, иначе настройка,
/// появившаяся в новой версии, не досталась бы ни одной обновившейся машине.
pub const PAYLOAD_DEFAULTS_DIR: &str = "FREEOS/DEF";

/// Какие эталонные настройки едут на носитель и под какими именами.
///
/// Слева — имя файла в `initrd/usr/share/defaults/etc/`, справа — имя на
/// носителе: том там пишется без длинных имён (см. заголовок `disk::fat32`), то
/// есть 8.3. Список обязан совпадать с `DEFAULTS` в
/// `crates/installer/src/payload.rs` — установщик открывает ровно эти пути.
pub const PAYLOAD_DEFAULTS: [(&str, &str); 3] =
    [("services", "SERVICES"), ("update.cfg", "UPDATE.CFG"), ("ca.pem", "CA.PEM")];

/// Каталог с пользовательскими программами на установочном носителе.
///
/// Программы лежат на носителе **отдельными файлами**, хотя те же самые уже
/// есть внутри образа RAM-диска. Причина в том, кто их оттуда достаёт:
/// установщик переносит их на корневой раздел, а читать FAT он не умеет —
/// он умеет её только создавать. Дублирование в сотню килобайт дешевле
/// FAT-читалки в установщике, у которой не было бы другого применения.
///
/// Имена в верхнем регистре — тот же контракт с установщиком, что и у
/// остальной полезной нагрузки: он открывает ровно эти пути.
pub const PAYLOAD_BIN_DIR: &str = "FREEOS/BIN";

/// Каталог образцовых пакетов на установочном носителе.
///
/// Отдельно от `/bin`: пакеты не программы, их не запускают, а ставят — и на
/// целевом диске они ложатся в `/media`, а не в `/bin`.
pub const PAYLOAD_PKG_DIR: &str = "FREEOS/PKG";

impl fmt::Display for Component {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.package())
    }
}
