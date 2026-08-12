# FreeOS

An open operating system written from scratch in Rust, targeting **ARM64** and **x86-64**.

> **Status: Phase 12c done.** The system installs itself onto a disk, boots from it, mounts
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
cargo xtask image --arch x86_64         # just write build/freeos-x86_64-debug.img

cargo xtask install --arch x86_64       # run the installer against a blank 1 GiB disk
cargo xtask run --arch x86_64 --installed   # boot what the installer just wrote

cargo xtask test                        # the whole bench, both architectures, nobody at the keyboard
cargo xtask test --list                 # what the bench checks
cargo xtask test -a x86_64 -s boot      # one scenario on one architecture
cargo xtask test --full                 # both profiles on both architectures
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
- **Input from two devices can still be reordered under load.** The USB keyboard is polled
  by a task, the serial line arrives on an interrupt. While a debug build repaints, a key
  release waiting for the next poll can lose the race to bytes from the UART. From a single
  device the order always holds, which is what a person actually uses.

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

Two consequences fell out that were not in the plan. The first: xHCI has no interrupt here,
its reports are polled — and the polling lived in the shell. A shell that sleeps until input
arrives would have been waiting for events that only its own polling could produce. Polling
moved into a task of its own with its own clock. The second: that task never ends, and `run`
stops the machine when no task is left — so tasks now say whether they are the reason the
system keeps running. The USB poller says no, and `exit` still halts.

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

Two things are worth saying plainly, because both are visible on the bench:

- **The clock keeps time, but it starts in debt.** The uptime is counted in delivered timer
  ticks, and ticks are lost whenever interrupts stay disabled — which is most of what a boot
  is. Measured: about twenty seconds of debt on x86-64 by the time the shell appears, and
  under ten on AArch64. After that it runs true; ten idle seconds on the bench cost the
  guest exactly ten. The fix is not more locking discipline but a counter that does not
  depend on interrupts arriving at all (`CNTVCT_EL0`, or the TSC calibrated against the ACPI
  timer) — which is a phase of its own.
- **There is no way to set it.** `SetTime` lives in boot services, and they are gone by the
  time anything could ask. A machine whose firmware clock is wrong will be wrong until it is
  fixed in firmware; a machine whose firmware has no clock at all says so, marks its files
  with zero, and prints `unknown` rather than inventing a date.

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
it ten seconds later, and a file on a filesystem that stores no time says so instead of
showing an invented date (`clock`);
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
