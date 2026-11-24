//! The C runtime openh264 and libopus get inside `/bin/screenrec` (23.11).
//!
//! The recorder is a `no_std` Rust program: no picolibc, no `crt0`, no TLS
//! of the C library. The two encoders need little of a C library, and it is
//! all here (the rest — stdio of debug dumps, `strncpy`, `abs` — is in
//! `ports/screenrec/crt_stubs.c`):
//!
//! * memory: `malloc` & co. and C++ `new`/`delete` on their own heap — the
//!   Rust heap of the program is single-threaded, and the encoders allocate
//!   from their threads. Arenas of 16 MiB (`linked_list_allocator`, behind a
//!   futex lock), blocks of 256 KiB and more straight from `mmap`;
//! * threads: `pthread_create`/`join` on `SYS_THREAD_CREATE` (1 MiB stacks),
//!   mutexes and semaphores on futexes, `usleep`, `gettimeofday` (the
//!   monotonic clock: openh264 only measures intervals and sets semaphore
//!   deadlines with it), `sysconf` for the CPU count;
//! * `log`, `log10`, `logf`, `floor`, `sqrtf` from the `libm` crate;
//! * the C++ ABI bits: guards of function statics, pure virtual calls.
//!
//! Layouts follow the SDK headers the library was compiled with
//! (`sys/_pthreadtypes.h`, `semaphore.h`): a mutex starts with its state word,
//! a semaphore is `{ value, waiters }`.

#![allow(clippy::missing_safety_doc, clippy::not_unsafe_ptr_arg_deref)]

use core::alloc::Layout;
use core::cell::UnsafeCell;
use core::ffi::{c_int, c_long, c_void};
use core::ptr::NonNull;
use core::sync::atomic::{AtomicU8, AtomicU32, Ordering};

use linked_list_allocator::Heap;
use user_progs::{Lock, futex_wait, futex_wait_for, futex_wake, mmap, munmap, println};

// ---------------------------------------------------------------------------
// Memory
// ---------------------------------------------------------------------------

const ARENA: usize = 16 << 20;
const MAX_ARENAS: usize = 32;
/// Blocks this large and larger get their own mapping.
const BIG: usize = 256 << 10;
const HEADER: usize = 16;
const BIG_TAG: usize = usize::MAX;

struct Arenas {
    heaps: [Heap; MAX_ARENAS],
    count: usize,
}

struct Shared(UnsafeCell<Arenas>);

// SAFETY: every access goes through `LOCK`.
unsafe impl Sync for Shared {}

static LOCK: Lock = Lock::new();
static ARENAS: Shared = Shared(UnsafeCell::new(Arenas { heaps: [const { Heap::empty() }; MAX_ARENAS], count: 0 }));

/// A fatal error inside the encoders. No formatting: this may run on a C
/// thread, and the Rust heap belongs to the main thread.
fn fail(what: &str) -> ! {
    println("screenrec: FAILED C runtime:");
    println(what);
    user_progs::exit(134)
}

fn round(n: usize, to: usize) -> usize {
    n.div_ceil(to) * to
}

/// Allocate `size` bytes aligned to 16; null — no memory.
fn allocate(size: usize) -> *mut u8 {
    let Some(total) = size.checked_add(HEADER).map(|n| round(n.max(32), 16)) else {
        return core::ptr::null_mut();
    };
    if total >= BIG {
        let len = round(total, 4096);
        let at = mmap(len, 0);
        if at <= 0 {
            return core::ptr::null_mut();
        }
        let base = at as usize as *mut usize;
        // SAFETY: a fresh mapping of `len` bytes.
        unsafe {
            base.write(len);
            base.add(1).write(BIG_TAG);
            return base.cast::<u8>().add(HEADER);
        }
    }
    let layout = Layout::from_size_align(total, 16).unwrap_or(Layout::new::<u128>());
    LOCK.acquire();
    // SAFETY: under the lock.
    let arenas = unsafe { &mut *ARENAS.0.get() };
    let mut found = None;
    for index in 0..arenas.count {
        if let Ok(block) = arenas.heaps[index].allocate_first_fit(layout) {
            found = Some((index, block));
            break;
        }
    }
    if found.is_none() && arenas.count < MAX_ARENAS {
        let at = mmap(ARENA, 0);
        if at > 0 {
            let index = arenas.count;
            // SAFETY: a fresh mapping of `ARENA` bytes, given to this heap only.
            unsafe { arenas.heaps[index].init(at as usize as *mut u8, ARENA) };
            arenas.count += 1;
            found = arenas.heaps[index].allocate_first_fit(layout).ok().map(|block| (index, block));
        }
    }
    LOCK.release();
    let Some((index, block)) = found else {
        return core::ptr::null_mut();
    };
    let base = block.as_ptr().cast::<usize>();
    // SAFETY: the block is `total` bytes, the header fits.
    unsafe {
        base.write(total);
        base.add(1).write(index);
        block.as_ptr().add(HEADER)
    }
}

/// Bytes usable in a block from [`allocate`].
unsafe fn usable(ptr: *mut u8) -> usize {
    // SAFETY: the caller passes a block of ours.
    unsafe { ptr.sub(HEADER).cast::<usize>().read() - HEADER }
}

unsafe fn release(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }
    // SAFETY: the caller passes a block of ours.
    let base = unsafe { ptr.sub(HEADER) };
    let (size, tag) = unsafe { (base.cast::<usize>().read(), base.cast::<usize>().add(1).read()) };
    if tag == BIG_TAG {
        munmap(base as usize, size);
        return;
    }
    let Ok(layout) = Layout::from_size_align(size, 16) else { fail("bad block header") };
    LOCK.acquire();
    // SAFETY: under the lock; the block came from heap `tag` with this layout.
    unsafe {
        let arenas = &mut *ARENAS.0.get();
        if tag >= arenas.count {
            LOCK.release();
            fail("free of a foreign pointer");
        }
        arenas.heaps[tag].deallocate(NonNull::new_unchecked(base), layout);
    }
    LOCK.release();
}

#[unsafe(no_mangle)]
pub extern "C" fn malloc(size: usize) -> *mut c_void {
    allocate(size).cast()
}

#[unsafe(no_mangle)]
pub extern "C" fn calloc(count: usize, size: usize) -> *mut c_void {
    let Some(total) = count.checked_mul(size) else { return core::ptr::null_mut() };
    let ptr = allocate(total);
    if !ptr.is_null() {
        // SAFETY: `total` bytes were just allocated.
        unsafe { core::ptr::write_bytes(ptr, 0, total) };
    }
    ptr.cast()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn realloc(old: *mut c_void, size: usize) -> *mut c_void {
    if old.is_null() {
        return malloc(size);
    }
    let new = allocate(size);
    if !new.is_null() {
        // SAFETY: both blocks are ours; copy what fits in both.
        unsafe {
            let keep = usable(old.cast()).min(size);
            core::ptr::copy_nonoverlapping(old.cast::<u8>(), new, keep);
            release(old.cast());
        }
    }
    new.cast()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn free(ptr: *mut c_void) {
    // SAFETY: C code frees what it allocated here.
    unsafe { release(ptr.cast()) }
}

fn new_or_die(size: usize) -> *mut c_void {
    let ptr = malloc(size);
    if ptr.is_null() {
        fail("out of memory in operator new");
    }
    ptr
}

/// `operator new(size_t)`.
#[unsafe(no_mangle)]
pub extern "C" fn _Znwm(size: usize) -> *mut c_void {
    new_or_die(size)
}

/// `operator new[](size_t)`.
#[unsafe(no_mangle)]
pub extern "C" fn _Znam(size: usize) -> *mut c_void {
    new_or_die(size)
}

/// `operator delete(void*)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _ZdlPv(ptr: *mut c_void) {
    // SAFETY: as `free`.
    unsafe { free(ptr) }
}

/// `operator delete(void*, size_t)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _ZdlPvm(ptr: *mut c_void, _size: usize) {
    // SAFETY: as `free`.
    unsafe { free(ptr) }
}

/// `operator delete[](void*)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _ZdaPv(ptr: *mut c_void) {
    // SAFETY: as `free`.
    unsafe { free(ptr) }
}

/// `operator delete[](void*, size_t)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _ZdaPvm(ptr: *mut c_void, _size: usize) {
    // SAFETY: as `free`.
    unsafe { free(ptr) }
}

#[unsafe(no_mangle)]
pub extern "C" fn __cxa_pure_virtual() {
    fail("pure virtual call");
}

/// Guard of a function-local static: byte 0 — initialised, byte 1 — taken.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __cxa_guard_acquire(guard: *mut u64) -> c_int {
    // SAFETY: the guard is a live 64-bit word of the C++ object file.
    let (done, busy) = unsafe { (&*guard.cast::<AtomicU8>(), &*guard.cast::<AtomicU8>().add(1)) };
    loop {
        if done.load(Ordering::Acquire) == 1 {
            return 0;
        }
        if busy.compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed).is_ok() {
            if done.load(Ordering::Acquire) == 1 {
                busy.store(0, Ordering::Release);
                return 0;
            }
            return 1;
        }
        user_progs::yield_now();
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __cxa_guard_release(guard: *mut u64) {
    // SAFETY: as in `__cxa_guard_acquire`.
    unsafe {
        (*guard.cast::<AtomicU8>()).store(1, Ordering::Release);
        (*guard.cast::<AtomicU8>().add(1)).store(0, Ordering::Release);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __cxa_guard_abort(guard: *mut u64) {
    // SAFETY: as in `__cxa_guard_acquire`.
    unsafe { (*guard.cast::<AtomicU8>().add(1)).store(0, Ordering::Release) };
}

// ---------------------------------------------------------------------------
// Maths
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub extern "C" fn floor(x: f64) -> f64 {
    libm::floor(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn log(x: f64) -> f64 {
    libm::log(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn log10(x: f64) -> f64 {
    libm::log10(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn logf(x: f32) -> f32 {
    libm::logf(x)
}

#[unsafe(no_mangle)]
pub extern "C" fn sqrtf(x: f32) -> f32 {
    libm::sqrtf(x)
}

// ---------------------------------------------------------------------------
// Time
// ---------------------------------------------------------------------------

fn now_ns() -> u64 {
    user_progs::clock(user_abi::CLOCK_MONOTONIC).map_or(0, |t| t.seconds * 1_000_000_000 + u64::from(t.nanos))
}

/// `struct timeval` of the SDK: `time_t` (64) and `suseconds_t` (32).
#[repr(C)]
pub struct TimeVal {
    sec: i64,
    usec: i32,
}

/// `struct timespec`.
#[repr(C)]
pub struct TimeSpec {
    sec: i64,
    nsec: c_long,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gettimeofday(tv: *mut TimeVal, _tz: *mut c_void) -> c_int {
    let ns = now_ns();
    if !tv.is_null() {
        // SAFETY: the caller's structure.
        unsafe {
            (*tv).sec = (ns / 1_000_000_000) as i64;
            (*tv).usec = (ns % 1_000_000_000 / 1000) as i32;
        }
    }
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn usleep(us: u32) -> c_int {
    let us = u64::from(us).max(1);
    user_progs::nanosleep(us / 1_000_000, ((us % 1_000_000) * 1000) as u32);
    0
}

/// `_SC_NPROCESSORS_ONLN` of the SDK's `unistd.h` (and Linux' 84).
#[unsafe(no_mangle)]
pub extern "C" fn sysconf(name: c_int) -> c_long {
    match name {
        139 | 84 | 83 => user_progs::sysinfo().map_or(1, |i| i.cpus.max(1)) as c_long,
        _ => -1,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn getpid() -> c_int {
    user_progs::pid() as c_int
}

// ---------------------------------------------------------------------------
// Threads
// ---------------------------------------------------------------------------

/// Stack of a C thread (openh264's workers keep macroblock state there).
const THREAD_STACK: usize = 1 << 20;

#[repr(C)]
struct Thread {
    func: extern "C" fn(*mut c_void) -> *mut c_void,
    arg: *mut c_void,
    done: AtomicU32,
    result: *mut c_void,
}

extern "C" fn trampoline(arg: usize) -> ! {
    let thread = arg as *mut Thread;
    // SAFETY: the record lives until `pthread_join` frees it, which waits
    // for `done`.
    unsafe {
        let result = ((*thread).func)((*thread).arg);
        (*thread).result = result;
        (*thread).done.store(1, Ordering::Release);
        futex_wake(&(*thread).done, 0);
    }
    user_progs::thread_exit(0)
}

#[unsafe(no_mangle)]
pub extern "C" fn pthread_attr_init(_attr: *mut c_void) -> c_int {
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn pthread_attr_destroy(_attr: *mut c_void) -> c_int {
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_create(out: *mut *mut c_void, _attr: *const c_void, func: extern "C" fn(*mut c_void) -> *mut c_void, arg: *mut c_void) -> c_int {
    let record = malloc(core::mem::size_of::<Thread>()).cast::<Thread>();
    if record.is_null() {
        return 11; // EAGAIN
    }
    // SAFETY: a fresh block of the right size.
    unsafe { record.write(Thread { func, arg, done: AtomicU32::new(0), result: core::ptr::null_mut() }) };
    if user_progs::thread_create(trampoline, record as usize, THREAD_STACK).is_none() {
        // SAFETY: ours, never seen by a thread.
        unsafe { free(record.cast()) };
        return 11;
    }
    // SAFETY: the caller's handle slot.
    unsafe { *out = record.cast() };
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_join(thread: *mut c_void, result: *mut *mut c_void) -> c_int {
    let record = thread.cast::<Thread>();
    if record.is_null() {
        return 3; // ESRCH
    }
    // SAFETY: a record from `pthread_create`, joined once.
    unsafe {
        while (*record).done.load(Ordering::Acquire) == 0 {
            futex_wait(&(*record).done, 0);
        }
        if !result.is_null() {
            *result = (*record).result;
        }
        free(record.cast());
    }
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn pthread_self() -> *mut c_void {
    core::ptr::null_mut()
}

/// The state word at the start of `pthread_mutex_t`: 0 free, 1 taken, 2
/// taken and somebody waits (as `user_progs::Lock`).
unsafe fn mutex_word<'a>(mutex: *mut c_void) -> &'a AtomicU32 {
    // SAFETY: the caller's mutex starts with an aligned `unsigned int`.
    unsafe { &*mutex.cast::<AtomicU32>() }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_mutex_init(mutex: *mut c_void, _attr: *const c_void) -> c_int {
    // SAFETY: the caller's mutex.
    unsafe { mutex_word(mutex).store(0, Ordering::Release) };
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn pthread_mutex_destroy(_mutex: *mut c_void) -> c_int {
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_mutex_lock(mutex: *mut c_void) -> c_int {
    // SAFETY: the caller's mutex.
    let state = unsafe { mutex_word(mutex) };
    if state.compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed).is_ok() {
        return 0;
    }
    while state.swap(2, Ordering::Acquire) != 0 {
        futex_wait(state, 2);
    }
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_mutex_unlock(mutex: *mut c_void) -> c_int {
    // SAFETY: the caller's mutex.
    let state = unsafe { mutex_word(mutex) };
    if state.swap(0, Ordering::Release) == 2 {
        futex_wake(state, 1);
    }
    0
}

/// `sem_t` of the SDK: `{ value, waiters }`.
#[repr(C)]
pub struct Sem {
    value: AtomicU32,
    waiters: AtomicU32,
}

fn sem_take(sem: &Sem) -> bool {
    let mut value = sem.value.load(Ordering::Acquire);
    while value > 0 {
        match sem.value.compare_exchange_weak(value, value - 1, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return true,
            Err(now) => value = now,
        }
    }
    false
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sem_init(sem: *mut Sem, _shared: c_int, value: u32) -> c_int {
    // SAFETY: the caller's semaphore.
    unsafe {
        (*sem).value.store(value, Ordering::Release);
        (*sem).waiters.store(0, Ordering::Release);
    }
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn sem_destroy(_sem: *mut Sem) -> c_int {
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sem_post(sem: *mut Sem) -> c_int {
    // SAFETY: the caller's semaphore.
    let sem = unsafe { &*sem };
    sem.value.fetch_add(1, Ordering::AcqRel);
    if sem.waiters.load(Ordering::Acquire) > 0 {
        futex_wake(&sem.value, 1);
    }
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sem_wait(sem: *mut Sem) -> c_int {
    // SAFETY: the caller's semaphore.
    let sem = unsafe { &*sem };
    loop {
        if sem_take(sem) {
            return 0;
        }
        sem.waiters.fetch_add(1, Ordering::AcqRel);
        futex_wait(&sem.value, 0);
        sem.waiters.fetch_sub(1, Ordering::AcqRel);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sem_trywait(sem: *mut Sem) -> c_int {
    // SAFETY: the caller's semaphore.
    if sem_take(unsafe { &*sem }) { 0 } else { -1 }
}

/// Deadline on the clock of [`gettimeofday`] (monotonic here).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sem_timedwait(sem: *mut Sem, deadline: *const TimeSpec) -> c_int {
    // SAFETY: the caller's semaphore and deadline.
    let (sem, deadline) = unsafe { (&*sem, &*deadline) };
    let until = (deadline.sec.max(0) as u64) * 1_000_000_000 + deadline.nsec.max(0) as u64;
    loop {
        if sem_take(sem) {
            return 0;
        }
        let now = now_ns();
        if now >= until {
            return -1;
        }
        sem.waiters.fetch_add(1, Ordering::AcqRel);
        futex_wait_for(&sem.value, 0, (until - now).div_ceil(1_000_000));
        sem.waiters.fetch_sub(1, Ordering::AcqRel);
    }
}
