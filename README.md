# Free OpenSource System by AI

> **Где всё началось:** https://dtf.ru/id549403 — пост, с которого начался этот разговор.

An operating system written from an empty directory, in Rust, **by an AI working with one
person**. No kernel was forked, no driver was copied. It targets **x86-64** and **ARM64**,
installs itself onto a disk, comes up as a desktop, runs programs outside the kernel, talks
over the network — and, since August 2026, **runs on a real phone and answers to a finger on
its screen**.

The unusual part is not the code. It is that the code was written by a model, one phase at a
time, and every phase had to work on real hardware or in an emulator before the next one
began. The reasoning behind each decision — including the mistakes and what they cost — is
written down in the source, in Russian, next to the code that resulted from it.

---

## What works today

| | |
|---|---|
| **Boots** | UEFI on x86-64 and ARM64; a live ISO that writes nothing, and an installer that partitions a disk |
| **Filesystem** | ext2, read and written by us — created, verified and repaired from inside the system (`fsck`) |
| **Desktop** | Framebuffer compositor: wallpaper, taskbar, start menu, draggable windows, terminal, file manager |
| **Userspace** | ELF programs in ring 3 / EL0, one address space each, preemptive scheduling, pipes, `mode`/`uid`/`gid` enforced |
| **Network** | Ethernet, ARP, IPv4, ICMP, UDP, DHCP, DNS, TCP with all eleven states, TLS 1.3 with X.509 |
| **SSH** | A real OpenSSH client logs in with a key and runs programs from `/bin` as the account that logged in |
| **Updates** | A/B root slots, signed images, automatic rollback after three failed boots; over HTTP or GitHub Releases |
| **Phone** | Redmi 9A (MT6762, `dandelion`): our kernel boots from the recovery partition, draws the desktop, talks over USB — and **the touchscreen works** |

### The phone

The most recent and the hardest part. The kernel boots through the factory bootloader as an
Android boot image, takes the framebuffer the bootloader left, brings up the MediaTek USB
controller in device mode so the log can be read over the cable (`fastboot oem log`), and
drives the Novatek NT36525B touch panel over SPI — including **downloading the panel's
firmware into it**, because that chip keeps none of its own. Windows are dragged, resized and
closed with a finger, at sixty-five samples a second.

The full story, with every wrong turn and what each one cost, is in **[docs/PHONE.md](docs/PHONE.md)**.

---

## Getting it

Ready images are on the [releases page](https://github.com/anomal3/FreeOpenSourceSystemAI/releases).

- `FreeOS-Installer_*.iso` — installs onto a disk, with A/B slots and a state partition
- `FreeOS_*.iso` — boots the running system, touching nothing

`x86_64` for a PC or a virtual machine, `aarch64` for ARM64. **UEFI only** — there is no BIOS
boot path and there will not be one.

In VirtualBox: type *Other/Unknown (64-bit)*, **turn EFI on**, attach the ISO. Hyper-V:
*Generation 2*. Defaults work; nothing needs changing.

---

## Building it

Requires Rust **nightly** (pinned by `rust-toolchain.toml`), **QEMU 9.0+** with edk2 firmware,
and a host linker for the `xtask` helper.

```powershell
winget install Rustlang.Rustup
winget install SoftwareFreedomConservancy.QEMU
```

```bash
cargo xtask run --arch x86_64        # build and boot in QEMU
cargo xtask run --arch aarch64       # same source, ARM64
cargo xtask run --arch x86_64 --gdb  # halt before the first instruction, gdbstub on :1234

cargo xtask install --arch x86_64        # run the installer against a blank disk
cargo xtask run --arch x86_64 --installed

cargo xtask iso --arch x86_64            # bootable ISO
cargo xtask test                         # the whole bench, both architectures
cargo xtask test --full                  # both profiles too -- the bar a phase must clear
```

### For the phone

```bash
cargo xtask phone-firmware               # fetch the panel firmware (not ours to redistribute)
cargo xtask phone --full-kernel --gzip \
  --dtb  /path/to/dandelion.dtb \
  --ramdisk /path/to/ramdisk.img

adb reboot bootloader
fastboot flash recovery build/bare-boot.img
fastboot reboot recovery                 # then press power at the bootloader logo
fastboot oem log                         # the whole kernel log, over the cable
```

---

## Why

Linux is a fine kernel with a graphics stack that is painful to build on. This project keeps
the parts of the Unix model worth keeping — no telemetry, no forced network calls, a real
permission model — and drops the accumulated complexity, starting from nothing.

Design bias throughout: **prefer the boring, well-specified path over the clever one.**

| Decision | Why |
|---|---|
| **Rust everywhere** | `unsafe` is confined to arch and MMIO layers, and every block carries a `// SAFETY:` justification |
| **64-bit only** | No 32-bit x86, no instruction translation |
| **UEFI as the single boot protocol** | One bootloader source compiles to `BOOTX64.EFI` and `BOOTAA64.EFI` |
| **Framebuffer compositor, not X11/Wayland** | The firmware hands over a linear framebuffer with the mode already set; a compositor on that is thousands of lines, not hundreds of thousands |
| **FAT32 for ESP, ext2 for root** | FAT32 is mandated for the ESP and has no uid/gid/mode. ext2 has them — and, decisively, has independent implementations to check ours against |
| **QEMU as the development target** | Both architectures, a gdbstub, fully scriptable |
| **Linux binary compatibility is not a goal** | Everything one would want it for is open source and gets rebuilt. It stays addable later, beside the native ABI |

---

## Roadmap

Fifty-odd phases are done. What each one is for, what checks it, and what is known to be
waiting to go wrong in it is in **[ROADMAP.md](ROADMAP.md)**.

| Milestone | Scope | State |
|---|---|---|
| **v0.1** | Boot, memory, interrupts, scheduler, filesystem, compositor, installer, desktop | **done** |
| **v0.2** | Userspace, permissions, packages, A/B updates, services, power, `fsck`, safe mode | **done** |
| **v0.3** | Network: Ethernet through TCP, SSH with key login, signed updates, TLS 1.3 | **done** |
| **v0.4** | Real hardware: a phone — its own bootloader, no UEFI, no ACPI, USB in device mode, **a working touchscreen** | in progress |
| next | `mmap`, demand paging, huge pages, SMP | planned |
| then | A libc and a toolchain: somebody else's project builds for this system unpatched | planned |
| then | Windows belong to programs; settings and desktop icons | planned |
| then | btrfs; a Raspberry Pi 4 — the first machine that is not an emulator | planned |

---

## Layout

```
crates/boot-uefi/    UEFI application: GOP probe, ELF loading, ExitBootServices
crates/boot-info/    Stable #[repr(C)] hand-off contract: bootloader -> kernel
crates/disk/         GPT and a FAT32 formatter          crates/ext2/  the ext2 format
crates/ssh/          Packets, curve25519, chacha20-poly1305, public-key login
crates/mini-ui/      Surfaces, 8x8 text, widgets        crates/installer/  the installer
crates/kernel/
  src/mm/            Frames, page tables, heap, DMA arena
  src/sched/         Preemptive scheduler and tasks
  src/vfs/ src/fs/   VFS traits, RAM disk, FAT32        src/block/  AHCI, NVMe
  src/gfx/ src/ui/   Surfaces, the screen, the compositor
  src/net/           Ethernet through TCP, sockets, DNS, TLS
  src/usb/           xHCI, OHCI, HID reports to input events
  src/user/          ELF loading, address spaces, system calls, pipes
  src/arch/          Everything that differs between x86-64, AArch64 -- and the phone
xtask/               Host-side build, image, QEMU and phone orchestration
```

Where a driver lives says what it is. The i8042 sits under `src/arch/x86_64/` because it *is*
the PC platform. The xHCI driver sits outside `arch/` for the same reason read backwards: it
talks to a PCIe device through memory, and nothing in it can tell which architecture it runs
on — a claim checked rather than asserted, because the same code drives the keyboard on
`q35` and on `virt`.

---

## Licence

MIT OR Apache-2.0
