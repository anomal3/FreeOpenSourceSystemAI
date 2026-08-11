# FreeOS

An open operating system written from scratch in Rust, targeting **ARM64** and **x86-64**.

> **Status: Phase 0 — bring-up.** Nothing here boots to a usable system yet. See [Roadmap](#roadmap).

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
| **FAT32 for ESP, custom inode FS for root** | FAT32 on the EFI System Partition is mandated by the UEFI spec. It is unsuitable for root: its on-disk format has no uid/gid/mode fields, so unix permissions could never be added later without migrating user data. |
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
cargo xtask run --arch x86_64 --gdb   # halt before first instruction, gdbstub on :1234
```

`xtask` locates QEMU and its UEFI firmware automatically; override with the
`FREEOS_OVMF_X86_64` / `FREEOS_OVMF_AARCH64` environment variables.

## Layout

```
crates/boot-info/   Stable #[repr(C)] hand-off contract: bootloader → kernel
crates/boot-uefi/   UEFI application: GOP probe, ELF loading, ExitBootServices
crates/kernel/      Freestanding kernel; PIE, loaded and relocated by boot-uefi
  src/mm/           Frame allocator, page-table contract, kernel heap
  src/arch/         Everything that differs between x86-64 and AArch64
xtask/              Host-side build / image / QEMU orchestration
```

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
| 3 | Interrupts: IDT+APIC (x86), exception vectors+GIC (ARM), timer tick | next |
| 4 | Cooperative scheduler, designed so preemption is an additive change | |
| 5 | RAM-disk, VFS traits, FAT32 reader | |
| 6 | Input & display: PS/2 first, then xHCI + USB HID boot protocol | |
| 7 | Framebuffer compositor, minimal shell | |
| 8 | Graphical UEFI installer (GPT, ESP, user account) | |

Deliberately out of scope for now, but not architecturally blocked: userspace
isolation with an ELF loader, then a PE loader and a Wine-style Win32
compatibility layer. The kernel avoids ELF/Unix-only assumptions — loaders sit
behind a trait, kernel objects are handle-based, and page protection flags are
an open bitflag set rather than a three-bit Unix enum.

## Licence

MIT OR Apache-2.0
