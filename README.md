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
| **Desktop** | Framebuffer compositor: antialiased proportional type, rounded translucent windows, a floating taskbar, start menu, terminal, file manager — in a dark and a light theme. A program can ask for **a window of its own**: it draws straight into mapped pixels and reads its own keys and clicks — the system monitor is exactly that, a program outside the kernel |
| **Userspace** | ELF programs in ring 3 / EL0, one address space each, preemptive scheduling, pipes, `mode`/`uid`/`gid` enforced, memory on request (`mmap`) in 4 KiB or 2 MiB pages, files mapped into memory and paged in on demand, W^X on every page and a **stack canary in every program** — Rust and C alike, with the reference value in a read-only page the kernel fills before the program starts — and a **versioned syscall contract** — `dup`, `fstat`, `isatty`, `poll`, clocks and CPU time, frozen by tests that name every number |
| **Package permissions** | A `.fpk` declares what it needs — `net`, `windows`, `files` — and gets nothing it did not ask for. Network and windows are enforced by the kernel; `files` is declared only, and says so. Two rules, both narrowing: what the launch asked for, intersected with what the package owning the executable asked for, intersected with what the launcher itself may do |
| **Familiar to a Windows user** | `cat C:\etc\system.cfg` works, and so does `\bin`; `D:` is refused by name, because this system has one root. The file manager labels `/bin` as «Программы» and `/home/you` as «Мои документы», folds the service trees, and shows the real path all the while — one key switches it all off |
| **Settings** | Timezone, theme, and a static address are set in a window and survive a reboot — written to `/etc` on the state partition, applied at the next boot before any service starts. Volumes and accounts are listed; the filesystem check runs from there |
| **Kernel stack canary** | The kernel guards its own frames too, not just the programs'. The reference value starts as a constant in `.data` and is replaced once with a random one as soon as there is a source of randomness — by a function with no prologue at all (`#[unsafe(naked)]`, two instructions and a return), because any function that *returns* would check the new value against the one its own prologue saved. Boot says where the randomness came from, `guard` says whether the value is random, and `guard smash` proves the whole thing by overflowing a kernel buffer on purpose. That command ships in the release image deliberately: a check that only exists in a debug build tests a frame layout nobody will ever run |
| **Power** | The screen turns itself off after an idle time nobody has to guess at: the Settings window and the `power` command both set it, it lives in `screen_off` of `/etc/desktop.cfg`, and any key, click or touch brings the screen back. While it is dark the shell wakes twice a minute instead of ten times a second. No ACPI S3 and no backlight off: the picture is black, the lamp is lit, and the code says so where it fills the screen |
| **Parsers under fuzzing** | Everything that reads foreign bytes is fuzzed on the host: ext2 and its `fsck`, btrfs, X.509 and PEM, TLS records, HID descriptors, packages, .NET assemblies, HTTP messages. `cargo xtask fuzz` finds and `cargo test` guards; it found a foreign mouse that stopped the kernel, a foreign disk that walked a bitmap off its buffer, and one that made a read chew for seconds |
| **C and a toolchain** | A picolibc port and a cross toolchain: `x86_64-freeos-cc hello.c -o hello` produces a program that runs. **zlib 1.3.1 builds from its own `configure`, unpatched, for both architectures** — and the result works: 18 000 bytes compress to 123 and come back byte-identical, inside the system |
| **Lua** | **Lua 5.4.9 builds from its own makefile, unpatched, for both architectures** and ships as `/bin/lua`: arithmetic, string patterns, closures, metatables, coroutines, `pcall`, the garbage collector, files, the clock and `os.execute` all work. It found two real gaps of ours on the way — the kernel did not terminate `argv` with a null pointer, which C requires and every foreign program relies on, and libc had neither `rename` nor a `system` that could work without `fork` |
| **Network** | Ethernet, ARP, IPv4, ICMP, UDP, DHCP, DNS, TCP with all eleven states, TLS 1.3 with X.509 |
| **SSH** | A real OpenSSH client logs in with a key and runs programs from `/bin` as the account that logged in; `sftp`, `scp` and WinSCP copy files both ways through the same account |
| **Web server** | `/bin/httpd` serves files, forwards a prefix to another server as a reverse proxy, and counts itself at `/metrics` for Prometheus — four connections at once, no threads. It is strict on purpose, because a proxy that frames a message differently from the server behind it is how a request nobody sent appears: a bare LF inside the head, two `Content-Length` fields, or an upstream answering in chunks are all refused by name. 64 MiB off the disk and out through our own TCP, byte for byte, with no retransmissions |
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

### The look

![The desktop](docs/history/2026-09-07-01-desktop-dark.png)

The desktop draws real type — Inter and JetBrains Mono, rasterised ahead of time on the
developer's machine and blended with the pixels underneath — on rounded, translucent surfaces,
over wallpaper that is computed rather than stored. Icons are drawn as strokes, not bitmaps, so
they recolour with the theme and scale without steps. Windows have soft shadows; their corners
are cut at compositing time, because that is the only place where what shows through them is
known.

There is a dark theme and a light one — one geometry, two palettes — switched by right-clicking
the desktop or in **Параметры → Экран**. Everything repaints at once: wallpaper, icons, taskbar,
every open window, and whatever the terminal had already printed.

![The same desktop in the light theme](docs/history/2026-09-07-02-desktop-light.png)

More pictures are in **[docs/SHOTS.md](docs/SHOTS.md)**; how it is put together, and which
number lives where, is in **[docs/LOOK.md](docs/LOOK.md)**.

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

### Building C programs for it

Needs LLVM, plus `make` and `sh` for foreign projects that bring their own build
system (`winget install LLVM.LLVM ezwinports.make`; `sh` arrives with Git).

```bash
cargo xtask sdk                 # headers, libraries and the x86_64-freeos-* wrappers
cargo xtask sdk --package       # ... and an .fpk with the sysroot in it
cargo xtask thirdparty          # fetch zlib and build it with the toolchain

build/toolchain/bin/x86_64-freeos-cc hello.c -o hello
```

Every program comes out with a stack canary (`-fstack-protector-strong`) and
no build system needs to know: the reference value lives in a read-only page
the kernel maps for the program, and the link script points `__stack_chk_guard`
at it.

The compiler is not part of the package and will not be: clang installs itself
and weighs a gigabyte. What the toolchain adds is the target — headers, libraries,
a link script, and wrappers under the names a foreign `configure` looks for.

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
permission model — and drops the accumulated complexity, starting from nothing. "Real" is
meant literally: `mode`/`uid`/`gid` are enforced, W^X holds on every page, every program
carries a stack canary — the kernel included — and a package gets only the permissions its
manifest asks for.

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
| **v0.4** | A machine that can compute: memory on request, memory-mapped files, huge pages, a second processor core, the system calls a libc needs | **done** |
| **v0.5** | A libc and a toolchain: somebody else's project builds for this system unpatched | **done** |
| **v0.6** | Windows belong to programs; settings that persist; a layout a Windows user recognises | **done** |
| **v0.7** | btrfs: a volume made by `mkfs.btrfs` mounts at `/data` and reads, with crc32c checked on **every** data sector; `fsck` walks the whole volume; our own `mkfs` and writer create, overwrite, truncate, delete and rename files in a way `btrfs check` and the Linux kernel accept; the kernel writes to `/data`, and Linux reads back what it wrote | done; the state partition stays on ext2 until `/data` has been lived with |
| **v0.7b** | A desktop a Windows user does not have to get used to: dialogs with buttons instead of `Y`/`N`, a start menu of applications rather than of `/bin`, a tray with the input language, network and clock, RU/EN keyboard layouts switched with Alt+Shift or Win+Space, a file manager that opens on a double click and has a context menu, a task manager that can end a task | done: С1–С9; the resolution changes without a reboot, a device manager shows what drives each device, window programs share one set of elements, `mc` looks and works like Far — blue panels, dialogs, a viewer and an editor |
| **v0.7c** | C# programs and WinForms: a `.dll` built with `dotnet build` on Windows runs here unchanged, on an own .NET runtime — an IL interpreter, a precise garbage collector, an own base library and an own `System.Windows.Forms` over FreeOS windows | N1–N8 done, awaiting a full run; N10 first trial done — `PriorityQueue` taken from dotnet/runtime (MIT) without a single edit runs as under `dotnet` — a WinForms program starts from Files like any program, and once installed as a package it has its own row in the Start menu; forms laid out in the Visual Studio designer behave as in WinForms: text boxes, check boxes, lists and combo boxes, message boxes and timers, menus with shortcuts and submenus, `Dock`/`Anchor`, radio buttons, progress bars and track bars, tabs, tooltips that pop up under the mouse, `NumericUpDown` over a 96-bit `decimal`, Tab and arrow keys moving focus; an unmodified `dotnet new winforms` project with a button from the designer opens as a FreeOS window and answers the mouse, files and directories, `DateTime`/`TimeSpan`/`Stopwatch` and `Environment` work as on Windows; assemblies are read exactly as `System.Reflection.Metadata` reads them, `dotnet hello.dll` prints Hello, World! from an unmodified `dotnet new console` project, and classes, virtual calls, interfaces, static constructors, structs, boxing, exceptions (filters, `finally`, runtime exceptions caught by the program), generics over value types, delegates, closures, events and string interpolation run over an own base library written in C#, with a precise garbage collector, string methods, `StringBuilder`, formatting and parsing, `List`/`Dictionary`/`HashSet` with iterators, floating point with .NET's own printing and parsing rules, Linq and enums, printing byte for byte what the real `dotnet` prints |
| **v0.8** | Real hardware: a phone — no UEFI, no ACPI, started from the `recovery` partition, USB in device mode, **a working touchscreen** ([docs/PHONE.md](docs/PHONE.md)) | the phone works: the kernel draws on the screen, talks to a computer over the cable, and windows are moved and closed with a finger; still ahead — DeviceTree and a HAL split, and a Raspberry Pi 4 |
| **v0.9** | Mono beside the own runtime: kernel threads, TLS, signals and `mprotect`, pthreads, cairo and libgdiplus, then Mono and its WinForms with a FreeOS window driver | planned |

---

## Layout

```
crates/boot-uefi/    UEFI application: GOP probe, ELF loading, ExitBootServices
crates/boot-info/    Stable #[repr(C)] hand-off contract: bootloader -> kernel
crates/disk/         GPT and a FAT32 formatter          crates/ext2/  the ext2 format
crates/btrfs/        btrfs: B-trees, chunk mapping, crc32c on every data sector, mkfs
crates/ssh/          Packets, curve25519, chacha20-poly1305, public-key login, SFTP v3
crates/mini-ui/      Surfaces, 8x8 text, widgets        crates/installer/  the installer
crates/freeos-cc/    Build rules for C, and the x86_64-freeos-cc wrapper itself
crates/sysconf/      The key=value files in /etc -- parsed here so the tests can run
libc/                The OS layer under picolibc, crt0, and example programs
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

Copyright (C) 2026 Виталий Ардашов ([gerzoid](https://github.com/gerzoid)), Роман Кощеев ([anomal3](https://github.com/anomal3)).

Distributed under the GNU General Public License v3 — see [LICENSE](LICENSE).
