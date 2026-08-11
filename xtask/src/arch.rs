//! Описание поддерживаемых архитектур и всего, что от них зависит.

use std::fmt;

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

    /// Таргет для UEFI-приложений (загрузчик, будущий установщик).
    pub fn uefi_triple(self) -> &'static str {
        match self {
            Arch::X86_64 => "x86_64-unknown-uefi",
            Arch::Aarch64 => "aarch64-unknown-uefi",
        }
    }

    /// Таргет для freestanding-ядра. Пока не используется, но держим рядом,
    /// чтобы соответствие arch -> triple было в одном месте.
    #[allow(dead_code)]
    pub fn bare_triple(self) -> &'static str {
        match self {
            Arch::X86_64 => "x86_64-unknown-none",
            Arch::Aarch64 => "aarch64-unknown-none",
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
