# FreeOS

An open operating system written from scratch in Rust, targeting **ARM64** and **x86-64**.

> **Status: Phase 9 done.** A graphical installer partitions a disk, creates an ext2 root
> filesystem on it and installs the system; the installed system boots, finds its own disk
> over virtio-blk, reads the partition table, mounts that root and serves it to the shell
> with real uid/gid/mode — on both architectures. There is no userspace yet: everything
> above runs in the kernel. See [Roadmap](#roadmap).

## Why

Linux is a fine kernel with a graphics stack that is painful to build on — X11 and Wayland
both make writing a simple, predictable GUI far harder than it should be. This project takes
the parts of the Unix model that are worth keeping (no telemetry, no forced network calls,
a real permission model) and drops the accumulated complexity, starting from an empty
directory and a modern systems language.

Design bias throughout: **prefer the boring, well-specified path over the clever one.**

## Design decisions

| Decision | Rationale |
|---|---|
| **Rust everywhere** | Memory safety matters more in a kernel than anywhere else. `unsafe` is confined to arch/MMIO layers and every block carries a `// SAFETY:` justification. |
| **64-bit only: ARM64 + x86-64** | No 32-bit x86, no instruction translation. A PE binary built for x64 runs on x64 hardware natively. |
| **UEFI as the single boot protocol** | One bootloader source compiles to `BOOTX64.EFI` and `BOOTAA64.EFI`. On Raspberry Pi 4 this works via the [pftf/RPi4](https://github.com/pftf/RPi4) UEFI firmware. UEFI unifies the *software* interface — it does not erase hardware differences, which stay behind the HAL. |
| **Framebuffer compositor, not X11/Wayland** | UEFI GOP hands us a linear framebuffer with the video mode already set. A compositor on top of that is a few thousand lines, not a few hundred thousand. |
| **FAT32 for ESP, ext2 for root** | FAT32 on the EFI System Partition is mandated by the UEFI spec. It is unsuitable for root: its on-disk format has no uid/gid/mode fields, so unix permissions could never be added later without migrating user data. ext2 has them, and — decisively — has independent implementations to check ours against. See [The root filesystem](#the-root-filesystem). |
| **QEMU as the only dev target** | Built-in gdbstub, both architectures, fully scriptable. VMware Workstation on a Windows host cannot run ARM guests at all, so it would only ever cover half the project. |
| **ESP32 explicitly excluded** | No MMU and ~520 KB SRAM. Virtual memory and process isolation — the foundation of this design — are not implementable there. It would also require a forked, non-mainline Rust toolchain. |

## Requirements

- Rust **nightly** (pinned by `rust-toolchain.toml`; targets install automatically)
- **QEMU** 9.0+ with edk2 firmware (`edk2-x86_64-code.fd`, `edk2-aarch64-code.fd`)
- A host linker for the `xtask` helper binary (MSVC Build Tools on Windows)

On Windows, both dependencies install via winget:

```powershell
winget install Rustlang.Rustup
winget install SoftwareFreedomConservancy.QEMU
```

## Quick start

```bash
cargo xtask run --arch x86_64     # build + boot in QEMU
cargo xtask run --arch aarch64    # same source, ARM64
cargo xtask run --arch x86_64 --gdb     # halt before first instruction, gdbstub on :1234
cargo xtask run --arch x86_64 --image   # boot from a real GPT disk image, not VVFAT
cargo xtask image --arch x86_64         # just write build/freeos-x86_64-debug.img

cargo xtask install --arch x86_64       # run the installer against a blank 1 GiB disk
cargo xtask run --arch x86_64 --installed   # boot what the installer just wrote
```

`xtask` locates QEMU and its UEFI firmware automatically; override with the
`FREEOS_OVMF_X86_64` / `FREEOS_OVMF_AARCH64` environment variables.

By default `run` hands QEMU a host directory through its VVFAT driver, which fakes a FAT
partition — no image is rebuilt between edits, so the loop stays short. `--image` instead
writes a genuine disk: protective MBR, GPT with both header copies, a 1 MiB-aligned ESP and
a FAT32 volume, all produced by `crates/disk` — the same code the installer will run against
a physical disk. What the firmware then reads is our partition table and our filesystem, so
booting that image is itself the test. The image is byte-reproducible: identical inputs give
an identical file, which is why "the image changed" means the content changed.

Once the boot log settles, the screen turns into a desktop with two windows and the shell
takes the keyboard: `help` lists what it answers, `ls` and `cat` read the mounted FAT32
image, `Tab` raises the window underneath, `exit` ends the session and halts. Type into the
QEMU window — `xtask` attaches a USB keyboard (`qemu-xhci` + `usb-kbd`) on both
architectures, and on x86-64 the PS/2 keyboard works alongside it — or into the terminal
QEMU was started from, because the serial line is an input device too and every line the
shell prints goes there as well.

Without a framebuffer the same shell runs on the serial console alone; graphics is not a
condition for the system to work. With nobody typing, the prompt gives up after twenty
seconds so unattended runs still terminate.

## The root filesystem

**ext2**, implemented here rather than borrowed — we take the on-disk layout, not anyone's
code. The plan originally called for a custom inode filesystem, and the argument that
changed it was not about the filesystem at all. It was about what could check it.

This project's standing rule is *check rather than assert*. The FAT32 writer is verified by
reading its output back with the foreign `fatfs` crate, because a writer checked by its own
reader proves only that both halves misread the format the same way. ext2 offers the same
escape hatch and a custom format never could: the tests here read every image back with
[`ext4-view`](https://crates.io/crates/ext4-view), and `e2fsck` plus an ordinary `mount`
work on any Linux machine. A broken FreeOS install can be repaired from outside.

The original reason FAT32 was rejected for root still holds and is satisfied: `uid`, `gid`
and `mode` live in the on-disk inode, so the installer writes `/etc/passwd` as `0640
root:root` and `/home/<user>` as `0750` owned by uid 1000 — real values, set at creation
time, years before anything will enforce them.

The cost, stated plainly: ext2 is a 1993 design with no checksums and no snapshots, and it
needs fsck after a power cut. ext3 journaling is additive over the same on-disk format, so
it can come later without migrating data.

What is implemented: formatting, directories, files, indirect and doubly-indirect blocks,
and reading all of it back. What is not: deletion, truncation, hard and symbolic links, and
triple indirection — each absent because it has no consumer today, and unexercised code in
something that writes to a disk is worse than missing code.

Run `cargo xtask inspect` after an install to see what actually landed: our own code parses
the partition table, and a foreign implementation reads the filesystem.

The kernel reaches that partition over **virtio-blk**, for the same reason the keyboard
comes over xHCI: one driver for both architectures. AHCI exists only where SATA does — on
`q35` and not on `virt` — while virtio-blk works identically on both. On a real Raspberry
Pi 4 there is no virtio at all; the disk there will arrive over USB mass storage on top of
the xHCI stack that already exists, and that work is named in the roadmap rather than
quietly assumed. Nothing tells the kernel which disk it booted from and no hand-off field
was added for it: the partition is recognised by its GPT type GUID, which the installer
wrote and only we use.

## The installer

A separate UEFI application, not a first-boot wizard inside the system. Partitioning is a
pre-OS operation, and doing it from the kernel would mean trusting other people's data to
our own not-yet-debugged disk code at the one moment when no debugger exists. Out here the
firmware is still alive and provides Block I/O for the media, Simple Text Input for the
keyboard and GOP for the screen.

The practical consequence matters more than the principle: **the installer's readiness does
not depend on the kernel's drivers.** Its keyboard comes from the firmware, so it would have
worked before the kernel had USB at all.

The one thing it does itself is partitioning, and that goes through `crates/disk` — the same
code `xtask image` runs on the host under `cargo test`. Code that erases someone's disk has
to be debugged before it reaches someone's disk.

Seven screens: language (English or Russian), what will happen, target disk, account,
keyboard layout, time zone, and only then confirmation. Confirmation is last because it is
the single point of no return; everything you might change your mind about is asked before
it, not after. The medium the installer itself booted from is detected by device path and
cannot be selected — an installation USB stick and a target disk look identical on screen.

It writes a GPT with two partitions: an ESP (FAT32 — the UEFI spec leaves no choice)
carrying the bootloader, kernel and initrd, and a FreeOS root partition holding ext2. The
split follows from who reads what: the ESP is read by firmware and the bootloader, before
any FreeOS driver exists, while the root partition is read by the system itself and can
therefore afford a format with permissions.

The account lands at `/etc/passwd` on the root partition, `0640 root:root`, and the user
gets `/home/<name>` at `0750` owned by uid 1000. The password digest is **not** produced by
a key derivation function: no PBKDF2, scrypt or Argon2 exists in this project, and pulling
a crypto dependency into a UEFI application is a decision to take deliberately, not in
passing. What is stored is a salted, iterated FNV-1a, and the algorithm is named in the
record itself (`fnv1a64-4096`) so a real KDF can be added later as a second tag without a
migration. It keeps the password off the disk in plaintext; it is not protection against an
attacker, and it is labelled as such rather than dressed up as one.

The Cyrillic in the interface is hand-drawn: `font8x8` covers ASCII, Latin, Greek, box
drawing and hiragana, and no Cyrillic at all, so `crates/mini-ui/src/font.rs` carries 66
glyphs written as 8x8 ASCII art — a form in which a typo is visible in the source.

## Layout

```
crates/boot-info/   Stable #[repr(C)] hand-off contract: bootloader → kernel
crates/boot-uefi/   UEFI application: GOP probe, ELF loading, ExitBootServices
crates/disk/        GPT and a FAT32 formatter, no_std: host image builder + installer
crates/ext2/        The ext2 format: formatter, writer and reader, no_std
crates/mini-ui/     Surfaces, 8x8 text (ASCII + Cyrillic), widgets: kernel + installer
crates/installer/   UEFI application: disk selection, partitioning, account, install
crates/kernel/      Freestanding kernel; PIE, loaded and relocated by boot-uefi
  src/mm/           Frame allocator, page tables, kernel heap, DMA-coherent arena
  src/sched/        Cooperative round-robin scheduler and tasks
  src/vfs/ src/fs/  VFS traits, RAM disk, FAT32 reader
  src/input/        Key codes, event queue, US keymap, line editor
  src/gfx/          Rects, surfaces in RAM, the screen, bitmap text
  src/ui/           Compositor: windows, z-order, damage tracking
  src/shell.rs      Prompt, commands, output that works with or without a screen
  src/acpi.rs       Table lookup by signature (MADT on x86-64, MCFG everywhere)
  src/pci.rs        ECAM configuration space, bus walk across bridges
  src/usb/          xHCI host controller, HID boot protocol
  src/virtio/       virtio over PCI: split virtqueue, virtio-blk
  src/arch/         Everything that differs between x86-64 and AArch64
xtask/              Host-side build / image / QEMU orchestration
```

Where a driver lives says what it is. The i8042 sits under `src/arch/x86_64/` because it is not
an x86 driver that happens to run on PCs — it *is* the PC platform, addressed through
instructions that exist nowhere else. The xHCI driver sits outside `arch/` for the same reason
read backwards: it talks to a PCIe device through memory, and nothing in it can tell which
architecture it is running on. That claim is checked rather than asserted — the same code
drives the keyboard on `q35` and on `virt`.

The kernel does **not** yet execute from the upper half. It is a PIE whose
relocations the bootloader already applied against a physical base, so a real
higher-half move needs either relocations computed from a virtual base (making
page-table setup the bootloader's job) or a self-relocation pass over
`.rela.dyn`. What exists today is a direct map of all physical memory at
`PHYS_MAP_BASE`, in the spirit of Linux's `PAGE_OFFSET`: kernel code keeps
running identity-mapped where its relocations are valid, while the heap, the
stack and access to arbitrary physical pages live in the upper half.

Crates arrive as the roadmap advances. The intended shape keeps every
architecture- and board-specific decision behind a trait boundary:

```
hal            arch-independent traits (paging, timer, interrupt controller)
hal-x86_64     GDT/IDT, APIC, paging
hal-aarch64    MMU, GIC, exception vectors
board-rpi4     board specifics layered on hal-aarch64
```

Porting to a phone means writing a new `board-*` crate — the kernel, drivers,
filesystem and compositor stay untouched. That is the entire point of the split.

## Roadmap

| Phase | Scope | State |
|---|---|---|
| 0 | Workspace, toolchain, UEFI app boots and prints on both arches | **done** |
| 1 | `BootInfo` hand-off, `ExitBootServices`, jump to kernel | **done** |
| 2 | Frame allocator, kernel-owned page tables with W^X, heap, own stack | **done** |
| 3 | Interrupts: IDT+APIC (x86), exception vectors+GIC (ARM), timer tick | **done** |
| 4 | Cooperative scheduler, designed so preemption is an additive change | **done** |
| 5 | RAM-disk, VFS traits, FAT32 reader | **done** |
| 6a | Input core, PS/2 keyboard on x86-64, serial-line input on both, line editing | **done** |
| 6b | PCIe enumeration, xHCI host controller, USB HID boot protocol | **done** |
| 7 | Framebuffer compositor with damage tracking, shell in a window | **done** |
| 8a | GPT + FAT32 writer, real bootable disk image instead of VVFAT | **done** |
| 8b | Graphical UEFI installer (disk selection, partitioning, user account) | **done** |
| 9a | ext2: formatter, writer and reader; the installer creates a real root | **done** |
| 9b | virtio-blk driver; the kernel mounts the root partition it was installed on | **done** |

Phases 6 and 8 were both split, for the same reason: their halves are not the same size.
PS/2 is two I/O ports and a scancode table, whereas a host-side USB stack is PCIe
enumeration, DMA-coherent allocation, transfer rings and device enumeration. Likewise, the
installer's disk work can be developed and unit-tested on the host, where `cargo test`
exists, while the installer itself only ever runs under firmware. Keeping either pair in one
commit would have meant shipping a first half nobody could run — and, worse, debugging the
partitioning code inside a UEFI application instead of in a test.

Deliberately out of scope for now, but not architecturally blocked: userspace
isolation with an ELF loader, then a PE loader and a Wine-style Win32
compatibility layer. The kernel avoids ELF/Unix-only assumptions — loaders sit
behind a trait, kernel objects are handle-based, and page protection flags are
an open bitflag set rather than a three-bit Unix enum.

## Licence

MIT OR Apache-2.0
