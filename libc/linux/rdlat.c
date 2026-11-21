/* Linux x86-64 program without libc (21.11.2026, after hw-log14): does a long
 * read() of one thread stall the short calls of the others?
 *
 * The kernel used to copy a file under the process lock that every call takes
 * at least briefly, so one 2 ms exFAT read of D2R from the USB stick held all
 * of its threads. Here:
 *   1. two threads read 16 MiB from /dev/zero with read() on ONE shared
 *      descriptor, again and again; the main thread meanwhile does 3000
 *      getpid() + lseek(other fd) and times each - average, worst, how many
 *      took over 1 ms; and the average read.
 *      Check: the worst short call is well below one read.
 *   2. position semantics on a shared descriptor: four threads read the
 *      program's own file (/proc/self/exe) in 700-byte pieces through one fd
 *      until the end - together exactly the file's size and byte sum (no
 *      piece read twice or skipped).
 *   3. four threads write 64-byte records to one fd of a new file - the file
 *      ends up exactly 4 x 300 x 64 bytes, every record whole.
 *
 * `rdlat baseline` - numbers only for step 1 (the kernel is in `fileio off`).
 * `rdlat: ok <what>` / `rdlat: FAILED <what>`; total `rdlat: N failed`.
 */

typedef unsigned long u64;
typedef unsigned int u32;
typedef long i64;

__asm__(".section .note.ABI-tag, \"a\", @note\n"
        ".balign 4\n"
        ".long 4, 16, 1\n"
        ".asciz \"GNU\"\n"
        ".long 0, 3, 2, 0\n"
        ".text\n");

enum {
    SYS_read = 0,
    SYS_write = 1,
    SYS_close = 3,
    SYS_lseek = 8,
    SYS_mmap = 9,
    SYS_pread64 = 17,
    SYS_nanosleep = 35,
    SYS_getpid = 39,
    SYS_clock_gettime = 228,
    SYS_exit_group = 231,
    SYS_openat = 257,
    SYS_unlinkat = 263,
};
#define READERS 2
#define WORKERS 4
#define READ_BYTES (16L << 20)
#define SHORT_CALLS 3000
#define PIECE 700
#define RECORDS 300
#define RECORD 64

static i64 sys6(i64 n, i64 a, i64 b, i64 c, i64 d, i64 e, i64 f) {
    register i64 r10 __asm__("r10") = d;
    register i64 r8 __asm__("r8") = e;
    register i64 r9 __asm__("r9") = f;
    i64 ret;
    __asm__ volatile("syscall"
                     : "=a"(ret)
                     : "a"(n), "D"(a), "S"(b), "d"(c), "r"(r10), "r"(r8), "r"(r9)
                     : "rcx", "r11", "memory");
    return ret;
}
#define sys3(n, a, b, c) sys6(n, (i64)(a), (i64)(b), (i64)(c), 0, 0, 0)

void *memset(void *d, int c, u64 n) {
    unsigned char *p = d;
    while (n--) *p++ = (unsigned char)c;
    return d;
}
void *memcpy(void *d, const void *s, u64 n) {
    unsigned char *p = d;
    const unsigned char *q = s;
    while (n--) *p++ = *q++;
    return d;
}
static u64 strlen_(const char *s) {
    u64 n = 0;
    while (s[n]) n++;
    return n;
}
static void out(const char *s) { sys3(SYS_write, 1, s, strlen_(s)); }

static int failed;
static void check(int good, const char *what) {
    char line[160];
    const char *head = good ? "rdlat: ok " : "rdlat: FAILED ";
    u64 len = 0;
    for (const char *s = head; *s && len < sizeof line - 2; s++) line[len++] = *s;
    for (const char *s = what; *s && len < sizeof line - 2; s++) line[len++] = *s;
    line[len++] = '\n';
    sys3(SYS_write, 1, line, len);
    if (!good) failed++;
}
static void number(const char *label, i64 v, const char *tail) {
    char line[160];
    u64 len = 0;
    for (const char *s = label; *s && len < 100; s++) line[len++] = *s;
    char digits[24];
    int n = 0;
    if (v < 0) {
        line[len++] = '-';
        v = -v;
    }
    do {
        digits[n++] = (char)('0' + v % 10);
        v /= 10;
    } while (v && n < 20);
    while (n) line[len++] = digits[--n];
    for (const char *s = tail; *s && len < sizeof line - 2; s++) line[len++] = *s;
    line[len++] = '\n';
    sys3(SYS_write, 1, line, len);
}

struct timespec_ { i64 sec, nsec; };
static i64 ns(void) {
    struct timespec_ t;
    sys3(SYS_clock_gettime, 1, &t, 0);
    return t.sec * 1000000000L + t.nsec;
}
static void nap_ms(i64 ms) {
    struct timespec_ t = {ms / 1000, (ms % 1000) * 1000000L};
    sys3(SYS_nanosleep, &t, 0, 0);
}
static u32 load(volatile u32 *word) { return __atomic_load_n(word, __ATOMIC_ACQUIRE); }

long raw_thread(u64 flags, void *stack_top, void (*fn)(void));
__asm__(".text\n"
        ".globl raw_thread\n"
        "raw_thread:\n"
        "  mov %rdx, %r9\n"
        "  xor %edx, %edx\n"
        "  xor %r10d, %r10d\n"
        "  xor %r8d, %r8d\n"
        "  mov $56, %eax\n"
        "  syscall\n"
        "  test %rax, %rax\n"
        "  jnz 1f\n"
        "  and $-16, %rsp\n"
        "  call *%r9\n"
        "  mov $60, %eax\n"
        "  xor %edi, %edi\n"
        "  syscall\n"
        "  hlt\n"
        "1:\n"
        "  ret\n");
#define THREAD_FLAGS (0x100 | 0x200 | 0x400 | 0x800 | 0x10000 | 0x40000)

static char stacks[WORKERS][32 * 1024] __attribute__((aligned(16)));
static int start_threads(void (*fn)(void), int count) {
    int started = 0;
    for (int t = 0; t < count; t++) {
        if (raw_thread(THREAD_FLAGS, stacks[t] + sizeof stacks[t], fn) > 0) started++;
    }
    return started;
}

/* ---- 1. long reads vs short calls ---- */
static volatile u32 stop, alive;
static int zero_fd;
static volatile i64 reads, read_ns;

void reader_main(void) {
    char *buf = (char *)sys6(SYS_mmap, 0, READ_BYTES, 3, 0x22, -1, 0);
    if ((i64)buf < 0) buf = 0;
    else memset(buf, 1, READ_BYTES);
    __atomic_add_fetch(&alive, 1, __ATOMIC_ACQ_REL);
    while (!load(&stop) && buf) {
        i64 t0 = ns();
        sys3(SYS_read, zero_fd, buf, READ_BYTES);
        __atomic_add_fetch(&read_ns, ns() - t0, __ATOMIC_ACQ_REL);
        __atomic_add_fetch(&reads, 1, __ATOMIC_ACQ_REL);
    }
    __atomic_sub_fetch(&alive, 1, __ATOMIC_ACQ_REL);
}

/* ---- 2. shared position, reads ---- */
static int file_fd;
static volatile i64 got_bytes, got_sum;

void piece_main(void) {
    unsigned char piece[PIECE];
    __atomic_add_fetch(&alive, 1, __ATOMIC_ACQ_REL);
    for (;;) {
        i64 got = sys3(SYS_read, file_fd, piece, PIECE);
        if (got <= 0) break;
        i64 sum = 0;
        for (i64 i = 0; i < got; i++) sum += piece[i];
        __atomic_add_fetch(&got_bytes, got, __ATOMIC_ACQ_REL);
        __atomic_add_fetch(&got_sum, sum, __ATOMIC_ACQ_REL);
    }
    __atomic_sub_fetch(&alive, 1, __ATOMIC_ACQ_REL);
}

/* ---- 3. shared position, writes ---- */
static int out_fd;
static volatile u32 writer_id;

void writer_main(void) {
    u32 me = __atomic_add_fetch(&writer_id, 1, __ATOMIC_ACQ_REL);
    char record[RECORD];
    memset(record, (int)('A' + me), RECORD);
    record[RECORD - 1] = '\n';
    __atomic_add_fetch(&alive, 1, __ATOMIC_ACQ_REL);
    for (int i = 0; i < RECORDS; i++) sys3(SYS_write, out_fd, record, RECORD);
    __atomic_sub_fetch(&alive, 1, __ATOMIC_ACQ_REL);
}

static void wait_gone(void) {
    for (int i = 0; i < 4000 && load(&alive) > 0; i++) nap_ms(5);
}

void start_c(u64 *sp) {
    const char *arg = sp[0] > 1 ? (const char *)sp[2] : "";
    int baseline = arg[0] == 'b';
    out(baseline ? "rdlat: start (baseline)\n" : "rdlat: start\n");

    /* 1. */
    zero_fd = (int)sys6(SYS_openat, -100, (i64)"/dev/zero", 0, 0, 0, 0);
    int other_fd = (int)sys6(SYS_openat, -100, (i64)"/proc/self/exe", 0, 0, 0, 0);
    check(zero_fd >= 0 && other_fd >= 0, "/dev/zero and /proc/self/exe open");
    check(start_threads(reader_main, READERS) == READERS, "2 reader threads start");
    for (int i = 0; i < 400 && load(&alive) < READERS; i++) nap_ms(5);
    nap_ms(30);
    i64 sum = 0, worst = 0, over_ms = 0;
    for (int i = 0; i < SHORT_CALLS; i++) {
        i64 t0 = ns();
        sys3(SYS_getpid, 0, 0, 0);
        sys3(SYS_lseek, other_fd, 0, 1);
        i64 took = ns() - t0;
        sum += took;
        if (took > worst) worst = took;
        if (took > 1000000L) over_ms++;
    }
    __atomic_store_n(&stop, 1, __ATOMIC_RELEASE);
    wait_gone();
    check(load(&alive) == 0, "reader threads exit");
    i64 n = reads ? reads : 1;
    i64 read_avg = read_ns / n;
    number("rdlat: short calls (getpid+lseek) average ", sum / SHORT_CALLS / 1000, " us");
    number("rdlat: short calls worst ", worst / 1000, " us");
    number("rdlat: short calls over 1 ms ", over_ms, "");
    number("rdlat: one 16 MiB read() takes ", read_avg / 1000, " us on average");
    number("rdlat: reads done ", reads, "");
    if (baseline) {
        out("rdlat: baseline, short calls not checked\n");
    } else if (read_avg < 2000000L) {
        check(1, "short calls vs long read: inconclusive, the read is shorter than 2 ms");
    } else {
        check(worst < read_avg / 2, "short calls do not wait for a read on another thread");
    }

    /* 2. */
    file_fd = (int)sys6(SYS_openat, -100, (i64)"/proc/self/exe", 0, 0, 0, 0);
    check(file_fd >= 0, "own file opens");
    i64 size = sys3(SYS_lseek, file_fd, 0, 2);
    sys3(SYS_lseek, file_fd, 0, 0);
    i64 want_sum = 0;
    {
        static unsigned char whole[4096];
        for (i64 at = 0; at < size;) {
            i64 got = sys6(SYS_pread64, file_fd, (i64)whole, sizeof whole, at, 0, 0);
            if (got <= 0) break;
            for (i64 i = 0; i < got; i++) want_sum += whole[i];
            at += got;
        }
    }
    alive = 0;
    check(start_threads(piece_main, WORKERS) == WORKERS, "4 piece readers start");
    nap_ms(20);
    wait_gone();
    number("rdlat: shared fd read bytes ", got_bytes, "");
    number("rdlat: file size ", size, "");
    check(size > 0 && got_bytes == size, "four threads on one fd read the file exactly once (size)");
    check(got_sum == want_sum, "four threads on one fd read the file exactly once (byte sum)");

    /* 3. */
    /* A writable place: /tmp on a disk, the RAM home on a live stick. */
    static const char *const places[] = {"/tmp/rdlat.out", "/root/rdlat.out", "rdlat.out"};
    const char *place = places[0];
    out_fd = -1;
    for (int i = 0; i < 3 && out_fd < 0; i++) {
        place = places[i];
        out_fd = (int)sys6(SYS_openat, -100, (i64)place, 0100 | 01000 | 2, 0644, 0, 0);
    }
    if (out_fd < 0) {
        /* No writable volume (a live boot from the host folder): a memfd is
         * an open file just the same. */
        out_fd = (int)sys3(319, "rdlat", 0, 0);
        place = 0;
    }
    check(out_fd >= 0, "a scratch file opens for writing");
    if (out_fd >= 0) {
        alive = 0;
        check(start_threads(writer_main, WORKERS) == WORKERS, "4 writers start");
        nap_ms(20);
        wait_gone();
        i64 end = sys3(SYS_lseek, out_fd, 0, 2);
        number("rdlat: shared fd written bytes ", end, "");
        check(end == (i64)WORKERS * RECORDS * RECORD, "four writers on one fd: no record overwritten");
        static char record[RECORD];
        int whole = 1;
        for (i64 at = 0; at < end; at += RECORD) {
            if (sys6(SYS_pread64, out_fd, (i64)record, RECORD, at, 0, 0) != RECORD) {
                whole = 0;
                break;
            }
            for (int i = 1; i < RECORD - 1; i++) {
                if (record[i] != record[0]) whole = 0;
            }
            if (record[RECORD - 1] != '\n') whole = 0;
        }
        check(whole, "every record is whole");
        sys3(SYS_close, out_fd, 0, 0);
        if (place) sys3(SYS_unlinkat, -100, place, 0);
    }

    out(failed ? "rdlat: some failed\n" : "rdlat: 0 failed\n");
    sys3(SYS_exit_group, failed, 0, 0);
    for (;;) {
    }
}

__asm__(".text\n"
        ".globl _start\n"
        "_start:\n"
        "  xor %rbp, %rbp\n"
        "  mov %rsp, %rdi\n"
        "  and $-16, %rsp\n"
        "  call start_c\n"
        "  hlt\n");
