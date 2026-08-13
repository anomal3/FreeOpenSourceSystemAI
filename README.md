# FreeOS

An open operating system written from scratch in Rust, targeting **ARM64** and **x86-64**.

> **Status: Phase 24 done.** The system installs itself onto a disk, boots from it, mounts
> its ext2 root, and comes up as a **desktop** with a mouse: wallpaper, taskbar, start menu,
> windows you can drag and close, a terminal, a file manager, a system monitor. It runs
> **programs outside the kernel, each in an address space of its own**: `run /bin/hello`
> loads an ELF into page tables built for that run alone, jumps to ring 3 (EL0), and takes
> the whole space apart when the program ends — including when it ends by faulting. And
> those programs **open files under the account the system was installed with**: the
> `mode`, `uid` and `gid` on disk decide what they may read, path component by path
> component. A program is a scheduler task, so several run at once while the shell keeps
> answering. On both architectures.

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
cargo xtask image --arch x86_64         # just write build/FreeOS_0.1.<build>_x86_64_debug.img

cargo xtask install --arch x86_64       # run the installer against a blank 1 GiB disk
cargo xtask run --arch x86_64 --installed   # boot what the installer just wrote

cargo xtask test                        # the whole bench, both architectures, nobody at the keyboard
cargo xtask test --list                 # what the bench checks
cargo xtask test -a x86_64 -s boot      # one scenario on one architecture
cargo xtask test --full                 # both profiles on both architectures

cargo xtask iso --arch x86_64           # bootable ISO: build/ISO/FreeOS_0.1.<build>_x86_64_debug.iso
cargo xtask iso --arch x86_64 --installer   # the installer as an ISO
```

### Trying it in a virtual machine

The ISO needs no QEMU and no spare machine. In VirtualBox: create a VM of type
*Other/Unknown (64-bit)*, **turn on EFI** (Settings → System → Enable EFI), attach the `.iso`
to the optical drive, start it. VMware and Hyper-V work the same way; on Hyper-V pick
*Generation 2*, which is the EFI one. There is no BIOS boot path at all, by design, so a
machine left on legacy boot will simply not find the medium.

Neither the chipset nor the pointing device needs changing any more: the system was made to
work with VirtualBox's defaults (PIIX3 and a USB tablet), not the other way round.

The **pointing device** needs no setting any more. VirtualBox defaults to a *USB Tablet*,
which reports absolute coordinates and declares no HID boot protocol; until Phase 23 that
left you with no cursor at all, and the advice here was to switch the setting to *USB
Mouse*. The kernel now reads the device's own report descriptor, so either one works — as
does anything else that describes itself, which is every USB pointing device made. What was
found is in the boot log: `usb : report descriptor 74 bytes: pointer, absolute 0..32767, 5
buttons, wheel`. (A PS/2 mouse still will not work: the second i8042 port has no driver
here, on purpose, because the other architecture has no such port to share the code with.)

The last number in the file name is the build counter, and it describes **the state of the
source**, not the individual file: every image built from one state of the tree carries the
same number, across both architectures and both profiles. Debug and release are one build
shown two ways. Change the code — fix something in the ARM64 path, say — and all four
images move to the next number together. The version ahead of it is bumped by hand, in
`[workspace.package]`, because what counts as new functionality is a decision, not a
counter.

The number is not stored inside the image. It is a property of this build directory's
history, so putting it in a volume label or a banner would mean identical sources produce
different bytes on a different machine — and byte-reproducibility is deliberate here. The
kernel therefore prints the version at boot, but not the build number; that one lives in
the file name, which is where you pick the file anyway.

Two honest limits:

- **The live ISO writes nothing.** It boots, mounts its initrd and gives you the desktop and
  the shell. That is the whole of it, and it is the point — nothing on the host is touched.
- **The installer ISO can install, but the result will not boot yet.** The installer writes
  through UEFI Block I/O, so the firmware's own SATA or NVMe driver does the work and the
  disk really does get written. The kernel, however, mounts its root through virtio-blk,
  which exists in QEMU and not in VirtualBox. The driver that closes this — NVMe — is the
  next phase, and until it lands, installing into a hypervisor is a one-way trip.

`xtask` locates QEMU and its UEFI firmware automatically; override with the
`FREEOS_OVMF_X86_64` / `FREEOS_OVMF_AARCH64` environment variables.

By default `run` hands QEMU a host directory through its VVFAT driver, which fakes a FAT
partition — no image is rebuilt between edits, so the loop stays short. `--image` instead
writes a genuine disk: protective MBR, GPT with both header copies, a 1 MiB-aligned ESP and
a FAT32 volume, all produced by `crates/disk` — the same code the installer will run against
a physical disk. What the firmware then reads is our partition table and our filesystem, so
booting that image is itself the test. The image is byte-reproducible: identical inputs give
an identical file, which is why "the image changed" means the content changed.

Once the boot log settles, the screen turns into a desktop. The mouse does what a mouse
does; from the keyboard, **Meta** (or **F1**) opens the start menu, **Tab** moves between
windows, **Ctrl+W** closes one, **Ctrl+arrows** moves it.
The terminal answers `help`, reads the mounted filesystem with `ls` and `cat`, and ends the
session with `exit`. Type into the QEMU window — `xtask` attaches a USB keyboard
(`qemu-xhci` + `usb-kbd`) on both architectures, and on x86-64 the PS/2 keyboard works
alongside it — or into the terminal QEMU was started from, because the serial line is an
input device too and every line the shell prints goes there as well.

Without a framebuffer the same shell runs on the serial console alone; graphics is not a
condition for the system to work. With nobody typing, the prompt gives up after twenty
seconds so unattended runs still terminate.

## The desktop

Four layers, and their order is the whole thing: a background that costs no memory at all
(a gradient computed from the row number, with a sparse dot grid over it), then windows
bottom-to-top, then the taskbar, then the start menu when it is open. Only rectangles that
actually changed are pushed to the screen — typing a character in the terminal repaints one
cell, not 1.02 million pixels.

The part that is new in kind, rather than in looks, is the **window manager**. Until Phase
10 every keypress went to the shell and "switching windows" only reordered them by depth;
the shell owned the keyboard. Now the desktop sees each event first and decides: the start
menu takes everything while it is open, then the manager's own chords, then the focused
window if it handles keys (the file manager does), and only then the shell — and only if
its window is the focused one. When userspace arrives, that last step becomes "deliver the
event to a process", not a rewritten window manager.

Three things are worth knowing about how it behaves:

- **The panel shows both the time and the uptime.** They answer different questions — what
  time it is, and how long this machine has been on — and neither substitutes for the other.
  Where the wall clock comes from, and what it costs, is [its own section](#the-clock).
- **Compositing does not run under the lock.** `SpinLock` is held with interrupts disabled,
  and a full repaint is a million-odd writes to device memory. Drawing under it delayed
  interrupts long enough that input events arrived out of the order they happened in — the
  symptom was Ctrl staying "held" after Ctrl+W. The desktop is now lifted out of the lock
  for the duration of the work.
- **Input from two devices can still be reordered under load.** Both the USB keyboard and
  the serial line now arrive on interrupts, but they are different interrupts taking
  different paths, and a debug build repainting can still let one overtake the other. From a
  single device the order always holds, which is what a person actually uses.

**The mouse** arrived in Phase 11 and works the way one expects: click a window to raise and
focus it, drag it by the title bar, close it with the button in the corner, click the taskbar
to switch windows or open the start menu, click a menu entry to launch it. The pointer is a
layer the compositor draws last, as two one-bit masks — a dark outline and a light fill,
because a white arrow vanishes on a light title bar and a dark one vanishes on the desktop.
Nothing is saved from underneath it: the framebuffer cannot be read, so a move marks two
rectangles (where the arrow was, where it now is) and both are composed again from the
background and the windows.

The work was not in the UI. The xHCI driver held exactly one device — one slot, one
interrupt ring, one report parser — and a mouse is a second of each. Events for every device
arrive in one ring, so a transfer is now matched to its device by the slot id in the event
itself; without that, mouse movement would be parsed as keystrokes.

## Userspace

`run /bin/hello` reads an ELF off the mounted filesystem, copies its `PT_LOAD` segments into
pages marked reachable from ring 3 (EL0 on ARM), and jumps there. From that moment the code
is on the far side of a wall the CPU enforces, not the kernel: it cannot read a byte of
kernel memory, execute a kernel instruction or touch a device. The only door is a trap —
`int 0x80` on x86-64, `svc #0` on ARM — and behind it four calls: `write`, `exit`, `yield`
and `uptime`. `uptime` exists to show that data crosses the boundary *inward* too, not only
out.

`run /bin/crash` writes to address zero. Before Phase 12a that was a kernel panic and a
stopped machine; now it prints one line, kills the program and returns to the shell.

### An address space per program

Every run builds its own page tables. The root table is a **copy** of the kernel's, with one
entry blanked: the one the program's memory goes under. A copy, not an empty tree, because
the kernel executes through those same tables — `CR3` describes both halves at once, and on
ARM the kernel image is identity-mapped through `TTBR0_EL1`, the very register being switched.
An empty tree would pull the kernel's own code out from under it, and the first trap out of
the program would be a triple fault rather than a system call. Copying gives the program no
rights: the kernel's entries are not marked user-accessible, and it is the MMU that refuses,
not a check in code.

Two consequences follow, and both are printed on the serial line at every run rather than
asserted in prose:

- **The kernel's own tables do not map the program at all.** Not "map it inaccessibly" —
  there is no translation. The kernel walks both trees and says so:
  `the kernel space maps nothing at 0x0000008000000260`.
- **The program's memory does not outlive it.** Everything belonging to a program sits under
  a single root entry, so tearing the space down is a walk of one subtree — which is why
  there is no `unmap` anywhere in this kernel and no need for one. All 136 pages and the 4
  tables holding them go back to the pool, on both exit paths: `exit`, and the fault that
  kills the program from a place it never returned from. Run a program three times and its
  root table lands on the same physical frame each time — that repetition *is* the proof the
  frames came back.

`run /bin/peek` reads kernel memory at an address its own page tables describe. It gets a
fault, and that fault is the one worth having: not "no such page", but "a kernel page is not
handed to ring 3". It is the check that copying the root did not hand out permissions along
with the mappings.

The window is a fixed size (512 KiB of image, 32 KiB of stack) rather than derived from the
file. That is a bound, not a shortcut: a segment that does not fit is rejected before the
first byte is written anywhere.

### Files, and permissions with someone to check

Ten system calls now: `write`, `exit`, `yield`, `uptime`, `open`, `read`, `close`, `stat`,
`getuid`, `getgid`. Enough for a program to read a file, and that is exactly what makes the
`mode`, `uid` and `gid` fields — carried since Phase 9, enforced by nobody until now — start
to mean something.

The system takes its identity from `/etc/passwd`, the file its own installer wrote: `whoami`
answers `roman (uid 1000 gid 1000)`, and every program runs with those credentials. There is
no login: no password is asked, and nothing switches users. That is stated rather than
disguised — the file carries a password digest, and the code that verifies one belongs with
the code that can read a password without echoing it, which does not exist yet. Booted from
the install medium there is no `/etc/passwd` at all, the session is root, and the boot log
says so, because "the checks are on" and "the checks have nothing to deny" must not look the
same.

`run /bin/perms` runs as that user and reports four files:

```
perms: /etc/system.cfg: mode 0644 owner 0:0 -> read 64 bytes
perms: /etc/passwd:     mode 0640 owner 0:0 -> permission denied
perms: /home/roman/notes.txt: mode 0600 owner 1000:1000 -> read 64 bytes
perms: /root/notes.txt: permission denied
```

Each answer arrives for its own reason, and the last is the one that matters. `/root/notes.txt`
is mode `0644` — readable by everyone — inside a directory that is `0700` and owned by root.
A check that looks at the file says yes; a check that walks the path says no, because you
cannot search `/root`. The installer lays permissions out the way Unix does, which only works
if the walk is the thing being checked. So it is: every directory on the way is asked for its
search bit before the next name is looked up, and it is asked *before* the lookup, so that
"no such file" never reports on the contents of a directory you were not allowed to enter.

Two boundaries are worth stating plainly:

- The checks stand at the **system call**, not in front of every filesystem call in the
  kernel. The shell's `cat /etc/passwd` still prints it. That is not an oversight: the shell
  runs in ring 0, and checking code that could read the disk sector by sector would draw a
  border where there is none. The border is the trap instruction, and it is visible in that
  `run /bin/perms` is refused exactly where `cat` is not.
- The kernel writing into a program's buffer is checked against the **program's** page
  tables, not the kernel's. Ring 0 may write a page the program may only read, so `read`
  into its own code would have faulted inside the kernel — a program stopping the machine.
  Every user pointer is now resolved through the tables from Phase 12b before a byte moves.

### A program is a task

`run -b /bin/count` twice, and the log interleaves:

```
count 8: tick 1 of 5 at 4200 ms
count 9: tick 1 of 5 at 4600 ms
count 8: tick 2 of 5 at 5090 ms
count 9: tick 2 of 5 at 5400 ms
```

Two programs, each in its own address space, at the same virtual addresses, taking turns —
and the shell answers commands while they run. The number each prints is its task id, the
one `tasks` shows: a program *is* a scheduler task, so a second namespace for the same
objects was not invented.

Everything that used to exist once per system is now state of a task: the kernel stack, the
page-table root, the table of open files, and the stack a trap from ring 3 lands on. That
last one is why this phase touched both architecture layers. On x86-64 `TSS.RSP0` used to
point at one global stack; two programs would have put one trap frame on top of another, so
it now points into the running task's own kernel stack. On ARM the kernel ran on `SP_EL0`
with `SP_EL1` reserved for handlers, which stopped working for a subtler reason: switching
tasks from inside a trap handler would write one task's `SP_EL0` into `SP_EL1`. The kernel
now runs on `SP_EL1` throughout, exactly as Linux does on arm64, and `SP_EL0` belongs to the
program alone — which also means it had to become part of the saved context.

That trade cost something, and it is worth naming: the ARM handler no longer starts on a
stack of its own, so an overflow of the *boot* stack — the only one with a genuinely
unmapped guard page — will now fault while trying to report the fault, and hang instead of
printing. Task stacks are unaffected (their overflow is caught by a painted guard band at
the next context switch), and on x86-64 the same case is still covered by the IST.

### Preemption

`/bin/spin` counts to a billion in a two-instruction assembly loop. Between its first line
and its last it makes no system call at all — no `yield`, no `write`, not even a look at the
clock. Run it in the background and the shell still answers:

```
freeos> run -b /bin/spin
  /bin/spin: started as #5
spin 5: no system calls from here on
echo preempted-ok
  preempted-ok
spin 5: done after 1490 ms, never yielded once
```

That middle line is the whole phase. A cooperative scheduler could not have printed it: the
shell would not have received a single cycle until the count was over. `tasks` says the same
thing in numbers — that run ended with `#5 program finished 31 switches (30 forced)`, and a
forced switch is one the task did not ask for.

The scheduler needed one new reason to switch and nothing else — that separation was designed
in at Phase 4 and it held. What did need care was *where* the switch happens. The obvious
place, the timer handler itself, is wrong on both architectures for the same reason: until
the interrupt is acknowledged it stays active and masks everything at or below its priority.
Switching before the acknowledgement would hand the CPU to a task whose timer is silent —
which is to say, a task nothing can preempt — and the acknowledgement itself would wait for
control to wander back into the abandoned handler. So the tick only counts the slice and
raises a flag; the switch happens at a preemption point the architecture layer calls at the
end of the interrupt, after `EOI` on x86-64 and after `end_of_interrupt` on ARM.

Preemption from ring 3 works because of Phase 13a and would not have worked before it: the
trap from the running program lands on that task's own kernel stack, so the interrupt frame
sitting there is saved and resumed along with everything else.

Two things had to be reordered to survive it. Running a program tells the scheduler about its
address space *before* switching the CPU to it, and returning from one tells the scheduler
*before* switching back — knowledge first, action second. The reverse order has a window in
which a preempted task comes back with the program's page-table root in the register while
the code below is about to hand those very frames back to the pool.

Everything else was already safe, and by construction rather than by luck: every shared
structure in the kernel lives behind a spinlock that disables interrupts while it is held, so
a timer tick cannot arrive in the middle of one. That is still how everything an interrupt
handler touches is protected; the locks that were held *long* have since moved to a mutex
that stops the task instead of the machine — see [Two kinds of lock](#two-kinds-of-lock).

One thing did break, and it broke the way this sort of thing does — in the log rather than in
the machine. A `write!` with a substitution in it is several calls under the hood, and a task
that used to finish its line before yielding now gets stopped between them: the serial console
started producing `freeos> echo shell-count 9: tick 2 of 5`. Kernel output through the shell
now takes one lock for the whole line. A program's line is not covered by it and should not
be — a program prints in six separate system calls, and `write` is atomic by itself, not in
company with its neighbours, exactly as in Unix.

### Stopping a program

Preemption means a runaway program no longer owns the machine. It does not mean you can get
rid of it, and `/bin/forever` is the program that makes the difference obvious: one assembly
instruction jumping to itself, no system calls, no end. Until this phase the only way to be
rid of it was to switch the machine off.

```
freeos> run -b /bin/forever
  /bin/forever: started as #5
forever 5: this program never ends on its own
freeos> kill 5
  kill: #5 asked to stop
  user        : killed by request, task #5
  user        : space released, 136 pages and 4 tables returned
  #5 /bin/forever: killed by request
```

The shell only says the request was taken. The killing happens elsewhere and later — at the
program's next return to ring 3, which the timer guarantees within a tick. That delay is the
design, not a shortcut: `return_to_kernel` throws away the handler's stack whole, so anything
that was on it goes with it. Done at an arbitrary point inside the kernel that would include
the guard of a held lock, and the system would deadlock at the next `lock()` on a lock nobody
can release. At the trap boundary there is nothing on that stack to lose — the handler has
finished its work, and the only frame below is the one `enter_user` left.

Which is also the boundary of what `kill` can do, and it is worth stating rather than
papering over: only a program can be stopped, because only a program has a place for the
kernel to return *to*. Ask to kill the shell and you get told no. A program stuck inside a
system call is not killable either, until it comes back out — no system call in this kernel
blocks, so today it always does, but that is a property of what exists so far and not a
promise.

Everything after the decision was already written. `kill` lands in the same place a fault
lands, and from there the Phase 12b teardown runs unchanged: the address space goes back to
the pool page by page, the file table closes what the program left open, and the task ends
with a code of its own — `-13`, distinct from the `-1` of a program the kernel killed for
misbehaving. The two are not the same event and should not print the same line.

### Waiting

Up to here nothing in the system actually waited. Every wait was a loop that gave up the
processor and got it straight back: `wait` for a task, the shell between keystrokes,
`/bin/count` measuring out a pause. The machine was busy at all times, and the only reason
that looked acceptable is that there was never anyone else to give the time to.

`/bin/nap` asks to sleep for three seconds and `tasks` answers with the point of the phase:

```
nap 6: sleeping 3000 ms, asking for nothing
freeos> tasks
  #4 usb      blocked    20 switches (0 forced), stack held
  #5 shell    running   112 switches (3 forced), stack held
  #6 program  blocked     3 switches (2 forced), stack held
  idle       : 34% of 288 tick(s) with nothing to run
```

`blocked`, not `ready` — the task is out of the rotation until its tick comes round, and the
line under it says what that buys: a third of all ticks (61% on ARM, 80% in a release build)
found the machine with nothing to run, where before the number would have been zero by
construction. What is left is mostly the compositor: the status window redraws twice a second,
and in a debug build under emulation that is not cheap.

`TaskState` gained the one variant a comment in it has been promising since Phase 4, and
picking the next task did not change a line — it selects `Ready`, so a blocked task fell out
of the rotation by itself. Three things can be waited for: a tick (`sleep`), another task
ending, an input event.

Every wait but one carries a deadline, and that is a safety property rather than a
convenience. A sleeper is woken by the timer, a waiter by the task it waits on — both by code
that can take the scheduler's lock. Input arrives in an interrupt handler, which may not wait
for a lock and so may fail to wake anyone. Without a deadline a lost wakeup is a task that
hangs for good; with one it is a delay no longer than the deadline.

The other half of that problem is the wakeup that arrives *before* the sleep. Between "the
queue is empty" and "I am asleep" an interrupt can deliver a key, wake nobody, and leave the
shell asleep with a keystroke already waiting. So the input path counts every event, and the
shell reads that counter before draining the queue and hands the scheduler a check to run
under its lock: if the count moved, do not sleep. Inside that lock interrupts are disabled,
so there is no third case.

Two consequences fell out that were not in the plan. The first: xHCI reports were polled
back then — and the polling lived in the shell. A shell that sleeps until input arrives would
have been waiting for events that only its own polling could produce. Polling moved into a
task of its own with its own clock; [Phase 18](#interrupts-instead-of-polling) later gave
that task an interrupt to wait for instead. The second: that task never ends, and `run` stops
the machine when no task is left — so tasks now say whether they are the reason the system
keeps running. The USB task says no, and `exit` still halts.

The idle task itself no longer spins either. When nothing is ready it stops the processor
(`hlt`, `wfi`) until an interrupt, which is what makes all of the above visible from outside
the emulator rather than only in a counter.

### Writing

Until here the system could read a disk and nothing else. Now it can keep something:

```
freeos> mkdir /home/roman/notes
  created /home/roman/notes
freeos> echo persisted-by-the-shell > /home/roman/notes/first.txt
  wrote 23 bytes to /home/roman/notes/first.txt
freeos> run /bin/save
save: wrote 20 bytes
save: read back: written from ring 3
```

...and after the machine is switched off and booted again, both files are still there, the one
that was deleted is still gone, and the installer's own files are untouched. That is the whole
claim, and the bench makes it in the only way that means anything: two scenarios, one boot
each, on the same disk.

`Node` grew six write methods, and every one has a default implementation that refuses. That
is not a stub. A read-only filesystem is not an unfinished one — the initrd image lives in RAM
and vanishes with the power, so writing to it is meaningless, and the RAM disk exists to
exercise the VFS itself. Making them refuse by hand would be the same refusal written in three
places instead of one.

Permission checks moved to where the new entry appears: creating and deleting ask the
**directory** for write access, because the file either does not exist yet or is about to stop
existing, and asking it anything is asking the wrong object. Everything else follows the rule
Phase 12c set — the walk is checked, one directory at a time, before the name is looked up.

Two decisions worth naming:

- The shell writes as the **session user**, not as the kernel. It runs in ring 0 and could
  write straight past every check; then `echo > /root/x` would succeed for an ordinary user
  exactly where `run /bin/save` is refused, and the permission system would be a decoration on
  one path out of two.
- `echo t > path` is the only redirection there is, and it lives inside the `echo` command
  rather than in the parser. Real redirection means a command's output is a descriptor the
  shell can replace; commands here print directly and have no descriptors. Promising `>`
  everywhere while implementing it once would be worse than not promising it.

The counters get flushed to the superblock after **every** operation. They live in the
editor's memory, and a machine that reboots before the flush would come back with a
free-block count that is too high — and then hand a new file a block an old one is using. That
is not a lost counter, it is lost data, so the flush is not optional and its cost is not worth
arguing about.

### The clock

The kernel has no clock of its own, and it is not going to grow one. Every target keeps the
time somewhere different — CMOS behind two I/O ports on x86-64, a PL031 at an address from
the firmware tables on the QEMU `virt` board, and nothing whatsoever on a Raspberry Pi 4,
which ships without a battery-backed clock. UEFI already abstracts all three behind
`GetTime`, so the bootloader asks it once, as its last act before boot services disappear,
and hands the answer over in `BootInfo`. From then on the time of day is *that number plus
the uptime*, and everything internal — file stamps, comparisons, the log — is UTC. The
offset appears only where a human reads it, and comes from `/etc/system.cfg`, which the
installer wrote after asking on its own screen.

What this bought is the thing that was quietly wrong before: **file timestamps**. `Editor`
takes its stamp from the superblock's last-write time, so until now every file the system
created was marked with the moment of installation — the same date, unmoving, for as long as
the install lived. Now the stamp is read from the clock on every change, `ls` shows it, and
`stat` prints it twice: as a date, and as a number that can be checked against something
outside the machine.

**What makes it keep time** is that it is not counted in timer ticks. A tick counter counts
*delivered interrupts*, and interrupts are lost whenever they are disabled — the controller
keeps one pending per vector and drops the rest — which is most of what a boot is. Measured
against the host's clock when this was first built: the guest was 23 seconds behind by the
time the shell appeared on x86-64 debug, 3 behind in release, and the gap was the
repainting. So time is read from a counter that runs on its own instead: `CNTPCT_EL0` on
AArch64, whose frequency the firmware puts in `CNTFRQ_EL0`, and the TSC on x86-64, whose
frequency nobody will tell you — `CPUID` often declines — so it is measured against the ACPI
timer, whose 3.579545 MHz is fixed by the spec. Ticks stay where they belong: counting
scheduler quanta, which are *defined* in ticks.

The second half is knowing *when* the firmware clock was read. Everything between that
reading and the kernel's first tick — `ExitBootServices`, the jump, page tables, the heap —
used to become permanent lag, and it was seconds. So the bootloader samples the monotonic
counter in the same breath as the clock and passes both; the counter is the same register on
both sides of the hand-off, so the kernel can subtract an interval it would otherwise have
no way to measure. It prints what it subtracted (`boot lag : 3717 ms`), because a correction
nobody can see is indistinguishable from a fudge factor.

Together that is the whole difference: −23 s before, **−1 to −2 s after**, and no longer
growing. The scenario asserts it with a ±10 s tolerance, tight enough that the old behaviour
would fail it.

One limit stands, and it is not going away: **there is no way to set the clock.** `SetTime`
lives in boot services and they are gone by the time anything could ask. A machine whose
firmware clock is wrong stays wrong until it is fixed in firmware; a machine whose firmware
has no clock at all says so, marks its files with zero, and prints `unknown` rather than
inventing a date.

### What a program is given

Until now a program received nothing. It could print, read a file whose path was compiled
into it, and ask how long the machine had been up — and that was the whole of it. Three
things were missing, and each of them is small on its own; together they are the difference
between a demo and a program someone would write.

**Arguments.** The kernel writes them into the program's own stack before entering ring 3 —
the strings, then an array of pointers to them — and passes the count and the array address
in registers. Registers rather than a stack layout, because System V and AAPCS64 disagree
about the layout and agree about the first two arguments; the entry point is `extern "C"`,
so the program declares `_start(argc, argv)` and the compiler does the rest. Argument zero
is the path the program was launched with, as everywhere in Unix — a program printing its
own name in an error message has nowhere else to get it.

**`seek`.** The descriptor always had a position; nothing could move it. So a file could
only be read straight through, and a program wanting the last line of a log, or merely its
size, had to read everything else first. `SEEK_END` with an offset of zero is now how you
ask how big a file is — asked of the descriptor, not of the name, because a name can point
somewhere else by the time you ask.

**Directories.** Phase 20 finished the set. `open` on a directory used to be refused, because
a descriptor you cannot read from is worse than an error; now it yields one that `readdir`
walks, one entry per call, and `ls` became a program rather than a kernel command. The list is
snapshotted when the directory is opened — the same promise POSIX makes, for the same reason:
an enumeration that changes under the reader can neither be finished nor explained. It also
removes a quadratic trap, since every entry would otherwise cost a full re-listing, and every
listing an inode read per name.

Both `ls` still exist, deliberately. The kernel one is needed where `/bin` is not mounted —
typically while working out *why* it is not mounted. The program one is the interesting one:
nobody hands it the filesystem, so it sees exactly what the account it runs as is allowed to
see. `ls /root` as an ordinary user fails at `open` rather than printing an empty directory,
because an empty listing would report on contents the caller was not permitted to see.

**The time of day.** The kernel has had a wall clock since Phase 16 and kept it to itself.
Zero means "the system does not know", not 1970, and the distinction is in the contract
because a program stamping a file must be able to tell those apart.

`/bin/wc` uses all three: it counts lines, words and bytes of the file named on the command
line, and checks its own answer — the size from `seek` must equal the bytes it read. Two
numbers by two different routes; they agree only if `seek` really moves the position. The
bench asserts the exact counts (18 lines, 143 words, 853 bytes of this repository's
`initrd/README.TXT`), because "something was counted" reads the same as "the wrong thing was
counted".

### Finding the interrupt controller

The ARM side had its GIC addresses as constants, correct for QEMU `virt` and for nothing
else. The bill arrived the first time someone booted the ISO on a different machine —
VirtualBox on Apple Silicon — and the kernel printed `unsupported interrupt controller
(Unknown) at 0x08000000`, then ran with no timer and no keyboard. Everything else worked:
memory, the clock, initrd, PCI, xHCI. The one part whose address had been *guessed* was the
one part that failed.

Addresses now come from the MADT, and with them the version — which turned out to matter,
because GICv3 is not a bigger GICv2. Its CPU interface is not memory at all but system
registers (`ICC_*`), and private interrupts (the timer among them) are enabled through a
per-core redistributor rather than the distributor. Both versions are supported; which one
is in front of us the firmware says.

Three things this cost, each worth naming:

- **ACPI cannot be read that early.** Parsing the MADT before the kernel takes over memory
  crashes into the *firmware's* exception handler, before the kernel has installed its own —
  an `ASSERT [ArmCpuDxe]` and nothing else. The lookup now happens where every other ACPI
  lookup happens, after the switch to our own tables, and the GIC windows are mapped
  individually at that point rather than up front.
- **A nested `kprintln!` deadlocks.** Printing inside the arguments of another print
  re-enters the output lock. It killed the first attempt in a way that looked identical to
  the problem above.
- **v2m does not exist on GICv3.** Its place is taken by the ITS, which we do not implement,
  and touching the v2m address there is not a read of garbage but an external abort from the
  bus. The system died with `page fault at 0x08020008` while setting up xHCI, and the address
  was the only clue. On GICv3 the driver stays on polling — the same path it takes on
  machines with no MSI-X at all, which is exactly what VirtualBox turned out to be.

The bench runs a scenario with `-machine gic-version=3` and asserts what actually matters:
the version is recognised, ticks arrive, and the shell answers a typed command.

### Letting the device describe itself

The same lesson, one layer up. The USB stack read *boot protocol* only: three bytes from a
mouse, eight from a keyboard, formats fixed by specification so that a BIOS could use them
without knowing anything about the device. Every device in QEMU speaks it, so for a long
time nothing else seemed necessary.

VirtualBox offers a **tablet** as its default pointing device, and a tablet declares no boot
protocol at all — it reports where the pen is, not how far it moved, and there is nothing to
agree on in three bytes. So there was no cursor, and the README told people to go and change
a setting in their hypervisor.

Every USB device carries a *report descriptor*: a stream of items that says which bits of
its report mean what. Parsing it is the general answer, and it replaces the special case
rather than sitting beside it — pointer and keyboard are both read through the descriptor
now, with boot protocol kept only for a device whose descriptor made no sense. Keeping it
the other way round would have meant a path the system takes only on machines where nobody
can debug it.

Two consequences worth naming:

- **Absolute pointing needed a new kind of event.** A mouse reports a delta; the driver can
  hand it straight on. A tablet reports a position, and turning that into a delta requires
  knowing where the cursor is — which the driver must not know, since it would mean knowing
  the screen size. So the event carries the position as a *fraction* of the device's own
  range, and the compositor, which owns the screen, turns it into pixels and computes the
  delta that a dragged window follows.
- **The parser is its own crate, with tests.** `crates/usb-hid` is `no_std` and has no
  dependencies, so `cargo test -p usb-hid` runs it on the host against descriptors taken
  from QEMU and VirtualBox sources, plus ones QEMU has no device for: a report with an ID,
  a joystick that must *not* become a cursor, truncated and garbage input. The failure mode
  here is a field offset out by one bit, which looks like a cursor drifting diagonally —
  hours to find through an emulator, seconds to find through a test.

The bench got a `tablet` scenario: same desktop, same drags and clicks, but the machine has
`usb-tablet` instead of `usb-mouse`. It also needed a second control channel. HMP's
`mouse_move` sends a *relative* event, and QEMU delivers those only to devices that declare
they understand them — send one to a machine whose only pointer is a tablet and it reaches
nobody, silently. Absolute events exist in QMP, so the harness now speaks both protocols.

### Two more addresses that were never ours to guess

Testing the above on the real hypervisor turned up two more of the same defect, and they
are worth naming because both looked like missing hardware and were nothing of the kind.

**No MCFG, no bus.** VirtualBox with its default chipset (PIIX3) publishes no `MCFG` table,
so the kernel printed `no 'MCFG' table` and saw *no PCI devices at all* — no USB controller,
no disk. The devices were there; what was missing was a way to ask. ECAM is a PCI Express
mechanism, and a machine can simply not have it. The historical mechanism does: ports
`0xCF8`/`0xCFC`, which predate PCI Express and work on anything PC-shaped. That is now the
fallback on x86-64, and the boot log says which path is in use. On ARM there is no fallback
and cannot be — the architecture has no I/O port space — so there a machine without MCFG is
a machine without PCI, and the kernel says so instead of pretending.

**No SPCR, no voice.** The UART address on ARM is not fixed by anything: QEMU `virt` puts
PL011 at `0x0900_0000`, VirtualBox at `0xffdd_f000`. The kernel knew only the first, so on
VirtualBox it printed nothing at all — and debugging why a keyboard did not come up, with no
log and a boot console already painted over by the desktop, is not debugging. The address
now comes from ACPI: `SPCR` first (the table that says where the firmware's own console is),
then `DBG2`. QEMU publishes SPCR and confirms the address it was already using, which is how
that path gets exercised on every run.

For the machine that publishes neither — VirtualBox on ARM is one — the shell now prints
what USB enumeration found: how many occupied ports were brought up, how many keyboards and
pointers came out of it, and where the last failure stopped. On a machine with no serial
port that line is the only diagnosis available, and it is the line that turned "the mouse
does not work" into a specific answer.

### Devices arrive after the kernel does

Enumeration ran once, at boot, and whatever was plugged in by then was all the machine
would ever have. That is not how anyone uses USB, and it showed up as a keyboard that
"kept falling off" on VirtualBox for ARM: the tablet came up every time, the keyboard about
half the time. It was not falling off — it had never arrived. The hypervisor attaches its
keyboard a few seconds into the guest's life, by which point the kernel had already looked
at the ports and stopped caring.

The controller does report this: a Port Status Change event lands in the same ring as
everything else, and the driver was reading it and throwing it away. It now means what it
says, and there is a second path as well — the mask of occupied ports is re-read twice a
second — because "the controller is obliged to send an event" describes a working machine,
and the machines that need this are the ones behaving oddly.

Two details that decide whether this works at all:

- **Enumeration cannot run in the handler.** Resetting a port and reading descriptors is
  hundreds of milliseconds of waiting, and a `SpinLock` is held with interrupts disabled —
  which would stop the clock those waits are measured against. So the task takes the whole
  controller out of its global, works on it, and puts it back; meanwhile events pile up in
  the ring, which is what a ring is for.
- **A device that leaves must return its slot.** There are four, and a slot never released
  is a slot lost until reboot. Unplugging now issues Disable Slot and clears the entry in
  the context array — that second part matters even when the command fails, since a
  controller reading a context for a device that is gone is reading memory the kernel may
  hand out again.

The bench plugs a tablet into a running machine with `device_add`, asserts the kernel brings
it up and parses its descriptor, then pulls it with `device_del` and asserts the slot comes
back. Both paths are exercised: x86-64 has MSI-X and finds out by event, ARM has no MSI-X
here and finds out by the mask.

### A second opinion about the clock

`-machine pit=off` is not a hypothetical. On VirtualBox the x86-64 side came up with no
timer at all: calibrating the local APIC against the PIT reads bit 5 of port 0x61, the
channel-2 output, and that bit never moved. The system printed `calibration against the PIT
failed` and sat at 0% CPU waiting for an interrupt that could not arrive.

The ACPI timer runs at a rate fixed by specification, and the kernel was already using it —
to calibrate `rdtsc`, since Phase 17. So the fallback is not new code so much as new
plumbing: the same FADT lookup, the same measurement loop, pointed at the APIC's counter
instead. It runs only when the PIT has failed, because the PIT needs nothing found or
parsed, and fewer lookups mean fewer addresses to get wrong.

The bench runs the whole thing with the PIT switched off and asserts all three steps: that
the first source failed, that the second was used, and that ticks then arrive.

### The bootable ISO

A disk image has to be written somewhere; an ISO is attached. That is the whole difference,
and it decides who can try the system: a hypervisor takes an ISO from a menu, and nothing on
the host is touched.

It does not boot *through* ISO 9660. The firmware reads **El Torito** — a boot catalogue
invented for CDs — finds the entry whose platform is `0xEF` ("EFI"), and mounts the image it
points at as an ordinary FAT partition. So what sits inside the ISO is a complete FAT32
volume, written by the same `crates/disk` code that formats an ESP; the ISO 9660 around it
exists so the medium counts as a medium — a volume descriptor, a path table, a root
directory.

Two things went wrong on the way, and both are the kind that leave no trace:

- **Two catalogue entries are not enough.** With a validation entry marked `0xEF` and a boot
  entry after it, OVMF *saw* the medium — it appeared in the UEFI Shell as `FS0: CDROM`,
  meaning the ISO 9660 parsed fine — and then booted the Shell instead. No error, no log
  line. EFI is bolted onto El Torito as a *section*, not as a replacement: the layout has to
  be validation entry, default entry (a BIOS one, marked non-bootable here), section header
  with platform `0xEF`, section entry. The temptation to drop the default entry is strong
  and wrong — the section header is specified to follow it, not replace it.
- **The volume label is a directory entry.** FAT32 stores it in the root directory as a
  record shaped like a file, so a volume labelled `FREEOS` made it impossible to create a
  directory named `FREEOS` — which is exactly what the installer medium needs. The error read
  "a path component exists but is not a directory" about a name nothing had created. Fixed in
  the FAT writer, where it belongs, with a test.

The bench boots the ISO on both architectures, attached the way a person would attach it —
as a drive on x86-64, as a SCSI CD-ROM on `virt`, which has no optical drive at all.

### Interrupts instead of polling

The USB controller could always interrupt the processor; the kernel just never asked it to,
because the path from a PCIe device to a handler is a subsystem of its own on each
architecture and none of it is about USB. So reports were polled a hundred times a second.
That works — the latency was never the problem — but the task woke on a timer whether
anything had happened or not: about two thousand wake-ups per twenty seconds, each one a
context switch, a walk of the event ring and a handful of MMIO reads. On interrupts, over
the same stretch: **fifty**.

The path is MSI-X, and the reason is worth stating because it is not the obvious one. A PCIe
device has no interrupt line to raise; it *writes to an address*, and what that write does is
up to the interrupt controller. On x86-64 the address is recognised by the local APIC and the
data carries a vector already sitting in the IDT — which means no `_PRT` parsing, which means
no AML interpreter, which is exactly why MSI-X was chosen over INTx. On AArch64 the write is
caught by the GICv2m frame and turned into an ordinary SPI. So the architecture layer answers
one question — where to write and what — and the driver contains no `cfg` at all.

One detail there cost a debugging session and is the kind that does not announce itself: an
MSI is a *pulse*, not a level. The SPI has to be configured edge-triggered, and QEMU's `virt`
defaults every SPI to level, which is correct for the rest of its peripherals. Left level, the
interrupt is not delivered at all — not misrouted, not spurious, simply absent. Everything
looks configured: the table is written, the capability is enabled, the controller reports the
vector. The keyboard just stops working, and no handler runs to say why.

The driver itself did not change shape. The handler acknowledges the interrupt at the
controller and wakes the task; the ring is still walked by that task, through the same
`service()` the timer used to call. Doing the walk inside the handler would mean hundreds of
microseconds with interrupts disabled — the very disease interrupts are the cure for. Waiting
is a new scheduler state (`Wait::Irq`), built like the mutex wait from Phase 15: the
"has anything arrived" check happens under the scheduler's lock, so an interrupt landing
between the check and the sleep cannot be lost.

A machine without MSI-X keeps polling, and `usb` says so — `irqs none`. That is not a
safety net nobody will hit: a controller whose vector table lives in an unmapped BAR is a
real thing, and its keyboard should still work.

### Two kinds of lock

Until now the kernel had one lock, and it bought its safety by disabling interrupts for as
long as it was held. For a short critical section that is exactly right: an interrupt handler
that needed the same lock would otherwise deadlock the machine against itself. But long
sections pay the same price, and by now there are some. Repainting a window in a debug build
holds the compositor for tens of milliseconds. Editing ext2 is dozens of trips to the disk,
each one a wait on a device.

While such a section is held, the machine is deaf. The timer does not advance: the local APIC
and the GIC keep one pending interrupt per vector, so extra ticks are simply lost, and with
them both the clock and everyone's quantum. The UART is not drained: a PL011 receive FIFO
holds 32 bytes and drops the rest in silence, which is how the one external diagnostic channel
starts tearing commands in half. And preemption — the whole point of Phase 13b — does not
happen at all.

So there is a second primitive. A task that cannot take a `Mutex` goes to `Blocked(Wait::Lock)`
keyed by the address of the lock itself, and whoever releases it wakes everyone waiting on that
address; the losers try again and go back to sleep. Interrupts stay enabled throughout. The
race that usually haunts this — the lock being released between "it is busy" and "I am asleep"
— is closed by doing the re-check inside the scheduler, under the scheduler's own spinlock:
while that is held interrupts are off, so the holder is a task that cannot be running, and the
state cannot change underneath the decision.

Two rules come with it. A mutex may not protect anything an interrupt handler touches, because
a handler has nowhere to block — it runs on the stack of whatever task it interrupted, and
sleeping would put *that* task to sleep instead of itself. And having taken a spinlock you must
not reach for a mutex: interrupts are off, so the mutex's holder will never be scheduled and
never let go. That is why chains convert whole, not link by link — shell output, the root
filesystem, the ext2 volume and the program table all moved together. The desktop deliberately
did not: `with_desktop` already lifts the compositor out from under its lock and draws outside
it, which solves the same problem a different way.

One honest caveat, because the bench cannot say otherwise: this improvement is argued from the
code, not demonstrated by a test. There is no new line in the log proving "interrupts are no
longer held off for tens of milliseconds" — the 56 passing scenarios establish only that
nothing broke on the way.

## The test bench

`cargo xtask test` boots the system in QEMU and drives it with nobody at the keyboard:
it waits for lines on the serial console, presses real keys through the QEMU monitor and
takes screenshots. A scenario passes only if the guest said what it was supposed to say —
screenshots are evidence of *how it looked*, never of *what happened*, because a screendump
shows the last painted frame and after a crash that frame can be three screens stale.

Fifteen scenarios today: a program runs in an address space of its own, one that faults is
killed without taking the system with it, one that reaches for kernel memory is refused, and
every run's pages go back to the pool (`userspace`); a program that makes no system call at
all is taken off the CPU anyway, and the shell answers a command between its two lines
(`preempt`); a program that never ends is stopped by name and its window returns to the pool,
while the shell and a task that is not a program are refused (`kill`); a sleeping program
shows up as `blocked` rather than `ready` and the machine reports time with nothing to run
(`sleep`); the wall clock the firmware handed over matches the *host's* clock, still matches
it ten seconds later — within ten seconds, which the tick-counted clock it replaced would
not have managed — and a file on a filesystem that stores no time says so instead of showing
an invented date (`clock`);
the system boots and the shell answers (`boot`); keys arrive over
xHCI and USB HID rather than PS/2 (`keyboard`, which switches `i8042` off, since `sendkey`
reaches exactly one keyboard and QEMU picks PS/2 when both are attached); the start menu
opens a program, the window moves and closes and focus comes back (`desktop`); the pointer
drives all of it with clicks and a drag (`mouse`); a terminal that sends a lone carriage
return works as Enter (`serial-cr`); the system boots off a disk this repo partitioned
(`image`); the installer walks all seven screens and writes a disk (`install`); and that
disk boots, mounts its ext2 root, takes its identity from the `/etc/passwd` it was installed
with, and gets four different answers to four files whose permissions differ (`installed`);
the shell and a ring-3 program both write to that root, a file created a moment ago reports
an age of zero seconds rather than the installation date, and a directory that is not empty
refuses to be deleted (`write`); and a second boot of the same disk finds what was written,
does not find what was deleted, and still has the installer's files (`persist`).

The mouse scenario never names a coordinate. A mouse is relative — there is no way to *put*
the cursor anywhere, only to drive it — and the two machines do not even have the same
screen (1280×800 from OVMF, 800×600 from `ramfb`). So the bench aims at meaning ("the title
bar of the `System` window") and reads the pixels out of the guest's own log, which prints
the screen size and every window's rectangle for exactly this purpose.

The bench lives in `xtask/src/harness/` and shares one QEMU command line with `run` —
a second, independent one would mean the tests check a machine the developer never sees.
It talks to the guest over TCP sockets that QEMU connects *to*, which is what makes the
carriage-return path testable at all: the Windows pipe used by the previous, out-of-tree
PowerShell version swallowed `0x0D` outright.

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
reading all of it back, and — since Phase 14a — changing a volume that already exists:
creating and deleting files and directories, writing at any offset, truncating, and handing
freed space back out. What is not: hard and symbolic links, and triple indirection — absent
because they have no consumer today, and unexercised code in something that writes to a disk
is worse than missing code.

There used to be two writers of this format. One formatted a volume and filled it, and could
do nothing else; the other did not exist yet, and would have been the one the kernel needed.
There is one now: `format` lays out an empty volume and hands back an `Editor`, and the
installer fills the root partition with the same code the kernel will write with. The path
that matters is therefore exercised by every install, not only by `cargo test`.

The two differ in exactly one way, and it is the interesting one. Formatting keeps both
bitmaps in memory and writes them once at the end — a volume that does not exist yet cannot
be lost, and an interrupted format simply means no filesystem. An editor may not do that: on
a live volume every allocation and release goes to disk immediately, because what is written
on disk is all that protects the files already there. Only the counters — free blocks, free
inodes, directories per group — stay in memory until `flush`, and losing power before that
costs exactly what ext2 costs anyway: `e2fsck` says "free blocks count wrong" and fixes it,
with the data intact. Promising more without a journal would be a lie.

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
gets `/home/<name>` at `0750` owned by uid 1000. The programs go to `/bin` at `0755
root:root` — an installed system that cannot run a program is not an installed system, and
they are copied from the medium as separate files rather than dug out of the initrd image,
because reading FAT would be a whole reader the installer otherwise has no use for. Those
permissions are not decoration: from Phase 12c they are what programs are actually measured
against. The password digest is **not** produced by
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
crates/calendar/    Unix seconds ↔ a civil date, no_std: bootloader + installer + kernel
crates/disk/        GPT and a FAT32 formatter, no_std: host image builder + installer
crates/ext2/        The ext2 format: formatter, writer and reader, no_std
crates/mini-ui/     Surfaces, 8x8 text (ASCII + Cyrillic), widgets: kernel + installer
crates/usb-hid/     HID report descriptors: what a device says about its own reports
crates/installer/   UEFI application: disk selection, partitioning, account, install
crates/kernel/      Freestanding kernel; PIE, loaded and relocated by boot-uefi
  src/mm/           Frame allocator, page tables, kernel heap, DMA-coherent arena
  src/sched/        Preemptive round-robin scheduler and tasks
  src/vfs/ src/fs/  VFS traits, RAM disk, FAT32 reader
  src/input/        Key codes, event queue, US keymap, line editor
  src/gfx/          Rects, surfaces in RAM, the screen, bitmap text
  src/ui/           Compositor: windows, z-order, damage tracking
  src/shell.rs      Prompt, commands, output that works with or without a screen
  src/time.rs       The wall clock: the firmware's answer plus uptime, and the time zone
  src/acpi.rs       Table lookup by signature (MADT on x86-64, MCFG everywhere)
  src/pci.rs        ECAM configuration space, bus walk across bridges
  src/usb/          xHCI host controller, HID reports to input events
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
| 10 | Desktop: wallpaper, taskbar, start menu, window manager, file manager | **done** |
| 11 | USB HID mouse on a multi-device xHCI: pointer, click to focus, drag, close | **done** |
| 12a | Userspace: ELF loader, ring 3 / EL0, system calls, a faulting program is killed | **done** |
| 12b | An address space per program: the kernel root cloned, switched, and torn down at exit | **done** |
| 12c | File system calls, and `mode`/`uid`/`gid` checked against the program asking | **done** |
| 13a | A program is a task: its own kernel stack, address space and files; two at once | **done** |
| 13b | Preemption: a program that never yields no longer owns the machine | **done** |
| 13c | `kill`: a program that never ends can be stopped, and its memory comes back | **done** |
| 13d | Waiting stops burning the processor: blocked tasks, `sleep`, an idle CPU | **done** |
| 14a | ext2 can be changed in place: create, write, truncate, delete, one writer | **done** |
| 14b | The kernel writes: `open` for writing, `write`, `mkdir`, `rm`, files that survive a reboot | **done** |
| 15 | A mutex that stops the task, not the machine: long locks no longer hold interrupts off | **done** |
| 16 | The time of day: the firmware clock, a time zone, and files stamped with when they were written | **done** |
| 17 | Time that does not depend on interrupts arriving: a monotonic counter, and the boot lag subtracted | **done** |
| 18 | Interrupts instead of polling: MSI-X on both architectures, and a driver that sleeps until something happens | **done** |
| 19 | Programs get told what to do: arguments, `seek`, and the time of day | **done** |
| 20 | A program can read a directory: `readdir`, and `ls` moves out of the kernel | **done** |
| 21 | A bootable ISO: ISO 9660 with an El Torito EFI catalog, so the system can be handed to someone | **done** |
| 22 | The interrupt controller is read from ACPI, not guessed, and GICv3 works | **done** |
| 23 | The device describes itself: HID report descriptors, so a tablet is a mouse; PCI without MCFG, the UART from ACPI, and a second source for the clock | **done** |
| 24 | Devices arrive after the kernel does: hot-plug, and a slot that comes back when one leaves | **done** |

Phases 6, 8, 9 and 12 were all split, for the same reason: their halves are not the same size.
PS/2 is two I/O ports and a scancode table, whereas a host-side USB stack is PCIe
enumeration, DMA-coherent allocation, transfer rings and device enumeration. Likewise, the
installer's disk work can be developed and unit-tested on the host, where `cargo test`
exists, while the installer itself only ever runs under firmware. Keeping either pair in one
commit would have meant shipping a first half nobody could run — and, worse, debugging the
partitioning code inside a UEFI application instead of in a test. Phase 12 split the same
way: address spaces are page-table work in two architecture modules, permissions are
filesystem work along the whole VFS path, and they share nothing but the phase number.

Deliberately out of scope for now, but not architecturally blocked: a PE loader and a
Wine-style Win32 compatibility layer. The kernel avoids ELF/Unix-only assumptions — loaders
sit behind a trait, kernel objects are handle-based, and page protection flags are an open
bitflag set rather than a three-bit Unix enum.

## Licence

MIT OR Apache-2.0
