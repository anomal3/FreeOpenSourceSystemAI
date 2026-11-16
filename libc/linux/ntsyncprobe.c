/* Программа Linux x86-64 без libc (16.11.2026): `/dev/ntsync` — объекты
 * синхронизации Windows в ядре, на которых стоит Wine 11 / GE-Proton11
 * (`drivers/misc/ntsync.c`, `include/uapi/linux/ntsync.h`).
 *
 * Устроена как `futexv.c`: статическая PIE без перемещений, только `syscall`;
 * потоки — сырой `clone` (`CLONE_VM | CLONE_THREAD`) на своих стеках, TLS не
 * трогают. Сигнал — как в `sigasync.c`: обработчик `SIGUSR1` без
 * `SA_RESTART`, так что прерванное ожидание отвечает `EINTR`.
 *
 * Каждая проверка печатает `ntsyncprobe: ok <что>` или
 * `ntsyncprobe: FAILED <что>`; итог — `ntsyncprobe: N failed`, код выхода —
 * число проваленных.
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
    SYS_write = 1, SYS_open = 2, SYS_close = 3, SYS_stat = 4, SYS_rt_sigaction = 13, SYS_ioctl = 16,
    SYS_dup = 32, SYS_nanosleep = 35, SYS_getpid = 39, SYS_socketpair = 53, SYS_sendmsg = 46, SYS_recvmsg = 47,
    SYS_fork = 57, SYS_exit = 60, SYS_wait4 = 61, SYS_gettid = 186, SYS_clock_gettime = 228, SYS_exit_group = 231,
    SYS_tgkill = 234,
};
enum { EPERM = 1, EINTR = 4, EINVAL = 22, ENOTTY = 25, EOVERFLOW = 75, ETIMEDOUT = 110, EOWNERDEAD = 130 };
enum { SIGUSR1 = 10, SA_SIGINFO = 4, SA_RESTORER = 0x04000000 };
enum { O_RDONLY = 0, O_CLOEXEC = 0x80000 };

/* Номера ioctl (`_IOC(dir, 'N', nr, size)`). */
#define NTSYNC_IOC_CREATE_SEM 0x40084E80u
#define NTSYNC_IOC_SEM_RELEASE 0xC0044E81u
#define NTSYNC_IOC_WAIT_ANY 0xC0284E82u
#define NTSYNC_IOC_WAIT_ALL 0xC0284E83u
#define NTSYNC_IOC_CREATE_MUTEX 0x40084E84u
#define NTSYNC_IOC_MUTEX_UNLOCK 0xC0084E85u
#define NTSYNC_IOC_MUTEX_KILL 0x40044E86u
#define NTSYNC_IOC_CREATE_EVENT 0x40084E87u
#define NTSYNC_IOC_EVENT_SET 0x80044E88u
#define NTSYNC_IOC_EVENT_RESET 0x80044E89u
#define NTSYNC_IOC_EVENT_PULSE 0x80044E8Au
#define NTSYNC_IOC_SEM_READ 0x80084E8Bu
#define NTSYNC_IOC_MUTEX_READ 0x80084E8Cu
#define NTSYNC_IOC_EVENT_READ 0x80084E8Du
#define NTSYNC_WAIT_REALTIME 1u
#define INF (~0ul)

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
    char line[200];
    const char *head = good ? "ntsyncprobe: ok " : "ntsyncprobe: FAILED ";
    u64 len = 0;
    for (const char *s = head; *s && len < sizeof line - 2; s++) line[len++] = *s;
    for (const char *s = what; *s && len < sizeof line - 2; s++) line[len++] = *s;
    line[len++] = '\n';
    sys3(SYS_write, 1, line, len);
    if (!good) failed++;
}

/* Число в строку — для отладочных строк при провале. */
static void out_num(const char *what, i64 v) {
    char buf[96];
    u64 len = 0;
    const char *head = "ntsyncprobe: note ";
    for (const char *s = head; *s; s++) buf[len++] = *s;
    for (const char *s = what; *s && len < 60; s++) buf[len++] = *s;
    buf[len++] = ' ';
    if (v < 0) {
        buf[len++] = '-';
        v = -v;
    }
    char digits[24];
    int n = 0;
    do {
        digits[n++] = (char)('0' + v % 10);
        v /= 10;
    } while (v);
    while (n) buf[len++] = digits[--n];
    buf[len++] = '\n';
    sys3(SYS_write, 1, buf, len);
}

struct timespec_ { i64 sec, nsec; };
static u64 clock_ns(int clock) {
    struct timespec_ t;
    sys3(SYS_clock_gettime, clock, &t, 0);
    return (u64)t.sec * 1000000000ul + (u64)t.nsec;
}
static u64 mono(void) { return clock_ns(1); }
static void nap_ms(i64 ms) {
    struct timespec_ t = {ms / 1000, (ms % 1000) * 1000000L};
    sys3(SYS_nanosleep, &t, 0, 0);
}

/* ---- обёртки ntsync ---- */

struct wait_args { u64 timeout; u64 objs; u32 count, index, flags, owner, alert, pad; };

static int dev;
static i64 io(int fd, u32 req, void *arg) { return sys3(SYS_ioctl, fd, (u64)req, arg); }
static i64 create(u32 req, u32 a, u32 b) {
    u32 v[2] = {a, b};
    return io(dev, req, v);
}
static i64 sem(u32 count, u32 max) { return create(NTSYNC_IOC_CREATE_SEM, count, max); }
static i64 mutex(u32 owner, u32 count) { return create(NTSYNC_IOC_CREATE_MUTEX, owner, count); }
static i64 event(u32 manual, u32 signaled) { return create(NTSYNC_IOC_CREATE_EVENT, manual, signaled); }

/* Два поля объекта (`*_READ`); ответ ioctl — в `ret`. */
static u32 r0, r1;
static i64 rd(int fd, u32 req) {
    u32 v[2] = {0xdead, 0xdead};
    i64 ret = io(fd, req, v);
    r0 = v[0];
    r1 = v[1];
    return ret;
}
static i64 u32op(int fd, u32 req, u32 in, u32 *outv) {
    u32 v = in;
    i64 ret = io(fd, req, &v);
    if (outv) *outv = v;
    return ret;
}

/* Индекс победителя последнего ожидания главного потока; ждущие потоки
 * берут свой через `waitk`. */
static u32 last_index;
static i64 waitk(int d, u32 req, const u32 *objs, u32 count, u64 timeout, u32 owner, u32 alert, u32 flags, u32 *index) {
    struct wait_args a;
    a.timeout = timeout;
    a.objs = (u64)objs;
    a.count = count;
    a.index = 0xdeadbeef;
    a.flags = flags;
    a.owner = owner;
    a.alert = alert;
    a.pad = 0;
    i64 r = io(d, req, &a);
    *index = a.index;
    return r;
}
static i64 waitx(int d, u32 req, const u32 *objs, u32 count, u64 timeout, u32 owner, u32 alert, u32 flags) {
    return waitk(d, req, objs, count, timeout, owner, alert, flags, &last_index);
}
static i64 wany(const u32 *objs, u32 count, u64 timeout, u32 owner) {
    return waitx(dev, NTSYNC_IOC_WAIT_ANY, objs, count, timeout, owner, 0, 0);
}
static i64 wall(const u32 *objs, u32 count, u64 timeout, u32 owner) {
    return waitx(dev, NTSYNC_IOC_WAIT_ALL, objs, count, timeout, owner, 0, 0);
}

/* ---- потоки ---- */

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

/* Стек на каждый запуск — свой: закончивший поток ещё секунду может стоять
 * на своём, переиспользовать его рано. */
#define NSTACK 24
#define STACK 16384
static char stacks[NSTACK][STACK] __attribute__((aligned(16)));
static int next_stack;
static long spawn(void (*fn)(void)) {
    if (next_stack >= NSTACK) return -1;
    char *top = stacks[next_stack++] + STACK;
    /* CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND | CLONE_THREAD | CLONE_SYSVSEM */
    return raw_thread(0x100 | 0x200 | 0x400 | 0x800 | 0x10000 | 0x40000, top, fn);
}

static int wait_flag(volatile int *flag, int ms) {
    for (int i = 0; i < ms / 2 && !*flag; i++) nap_ms(2);
    return *flag;
}

/* Ждущий поток: параметры и ответ — в своей ячейке. */
struct wslot {
    volatile int started, done, tid;
    volatile i64 ret, first;
    volatile u32 index;
    u32 objs[4];
    u32 count, owner, alert, req;
    u64 timeout;
    int retry_eintr;
};
static struct wslot ws[4];

static void wbody(int k) {
    struct wslot *s = &ws[k];
    s->tid = (int)sys3(SYS_gettid, 0, 0, 0);
    s->started = 1;
    u32 index = 0xdeadbeef;
    i64 r = waitk(dev, s->req, s->objs, s->count, s->timeout, s->owner, s->alert, 0, &index);
    s->first = r;
    while (s->retry_eintr && r == -EINTR) r = waitk(dev, s->req, s->objs, s->count, s->timeout, s->owner, s->alert, 0, &index);
    s->index = index;
    s->ret = r;
    s->done = 1;
}
void w0(void) { wbody(0); }
void w1(void) { wbody(1); }
void w2(void) { wbody(2); }

static void wsetup(int k, u32 req, u32 a, u32 b, u32 count, u32 owner, u32 alert, u64 timeout) {
    struct wslot *s = &ws[k];
    s->started = s->done = 0;
    s->ret = s->first = 12345;
    s->index = 0xdeadbeef;
    s->req = req;
    s->objs[0] = a;
    s->objs[1] = b;
    s->count = count;
    s->owner = owner;
    s->alert = alert;
    s->timeout = timeout;
    s->retry_eintr = 0;
}

/* Поток-«ставящий»: через `set_delay` мс — ioctl `set_req` на `set_fd`. */
static volatile int set_fd, set_delay, set_done;
static volatile u32 set_req, set_val;
void setter(void) {
    nap_ms(set_delay);
    u32 v = set_val;
    io(set_fd, set_req, &v);
    set_done = 1;
}
static void later(int fd, u32 req, u32 val, int delay) {
    set_fd = fd;
    set_req = req;
    set_val = val;
    set_delay = delay;
    set_done = 0;
    spawn(setter);
}

/* Пинг-понг двух семафоров. */
static volatile int pp_a, pp_b, pp_n, pp_done;
void ponger(void) {
    u32 objs[1] = {(u32)pp_b};
    for (int i = 0; i < pp_n; i++) {
        if (wany(objs, 1, INF, 2) != 0) break;
        u32 one = 1;
        io(pp_a, NTSYNC_IOC_SEM_RELEASE, &one);
    }
    pp_done = 1;
}

/* Сигналы против захватов: шлёт RELEASE(1) и SIGUSR1. */
static volatile int race_sem, race_tid, race_n, race_done;
void racer(void) {
    i64 pid = sys3(SYS_getpid, 0, 0, 0);
    for (int i = 0; i < race_n; i++) {
        u32 one = 1;
        io(race_sem, NTSYNC_IOC_SEM_RELEASE, &one);
        sys3(SYS_tgkill, pid, race_tid, SIGUSR1);
        if (i % 16 == 0) nap_ms(1);
    }
    race_done = 1;
}

static volatile int handled;
void on_usr1(int sig, void *info, void *uc) {
    (void)sig;
    (void)info;
    (void)uc;
    handled++;
}
void restorer(void);
__asm__(".text\n"
        ".globl restorer\n"
        "restorer:\n"
        "  mov $15, %eax\n"
        "  syscall\n"
        "  hlt\n");

/* ---- SCM_RIGHTS ---- */

struct iovec_ { void *base; u64 len; };
struct msghdr_ { void *name; u32 namelen, pad0; struct iovec_ *iov; u64 iovlen; void *control; u64 controllen; int flags, pad1; };

static void test_scm_rights(int evfd) {
    int sv[2] = {-1, -1};
    check(sys6(SYS_socketpair, 1, 1, 0, (i64)sv, 0, 0) == 0, "socketpair AF_UNIX");
    u64 control[3]; /* cmsghdr (16) + два int */
    control[0] = 24;
    u32 *ctl = (u32 *)control;
    ctl[2] = 1; /* SOL_SOCKET */
    ctl[3] = 1; /* SCM_RIGHTS */
    ctl[4] = (u32)dev;
    ctl[5] = (u32)evfd;
    char byte = 'x';
    struct iovec_ iov = {&byte, 1};
    struct msghdr_ msg;
    memset(&msg, 0, sizeof msg);
    msg.iov = &iov;
    msg.iovlen = 1;
    msg.control = control;
    msg.controllen = 24;
    check(sys3(SYS_sendmsg, sv[0], &msg, 0) == 1, "sendmsg passes the device and an event");
    u64 incoming[3] = {0, 0, 0};
    memset(&msg, 0, sizeof msg);
    msg.iov = &iov;
    msg.iovlen = 1;
    msg.control = incoming;
    msg.controllen = 24;
    i64 r = sys3(SYS_recvmsg, sv[1], &msg, 0);
    u32 *in = (u32 *)incoming;
    int dev2 = (int)in[4], ev2 = (int)in[5];
    check(r == 1 && dev2 > 2 && ev2 > 2 && dev2 != dev && ev2 != evfd, "recvmsg gets two new fds");
    /* Как клиент Wine: ждать полученным устройством на полученном объекте. */
    u32 one;
    u32op(evfd, NTSYNC_IOC_EVENT_SET, 0, &one);
    u32 objs[1] = {(u32)ev2};
    i64 w = waitx(dev2, NTSYNC_IOC_WAIT_ANY, objs, 1, mono(), 1, 0, 0);
    check(w == 0 && last_index == 0, "the passed device waits on the passed object");
    objs[0] = (u32)evfd;
    w = waitx(dev2, NTSYNC_IOC_WAIT_ANY, objs, 1, mono(), 1, 0, 0);
    check(w == 0, "the passed device is the same device as the original");
    sys3(SYS_close, ev2, 0, 0);
    sys3(SYS_close, dev2, 0, 0);
    sys3(SYS_close, sv[0], 0, 0);
    sys3(SYS_close, sv[1], 0, 0);
}

void start_c(u64 *sp) {
    (void)sp;
    out("ntsyncprobe: start\n");
    i64 r;
    u32 v;

    /* 1. Устройство. */
    dev = (int)sys3(SYS_open, "/dev/ntsync", O_RDONLY | O_CLOEXEC, 0);
    check(dev >= 0, "open /dev/ntsync");
    if (dev < 0) {
        out("ntsyncprobe: 1 failed\n");
        sys3(SYS_exit_group, 1, 0, 0);
    }
    u64 st[18];
    memset(st, 0, sizeof st);
    r = sys3(SYS_stat, "/dev/ntsync", st, 0);
    u32 mode = ((u32 *)st)[6];
    check(r == 0 && (mode & 0170000) == 0020000, "stat /dev/ntsync is a character device");
    u32 junk[2] = {0, 0};
    check(io(dev, 0x40084E7Fu, junk) == -ENOTTY, "an unknown ioctl is ENOTTY");

    /* 2. Семафор. */
    check(sem(3, 2) == -EINVAL, "CREATE_SEM count > max is EINVAL");
    int s = (int)sem(1, 5);
    check(s > 2, "CREATE_SEM 1/5 gives an fd");
    check(rd(s, NTSYNC_IOC_SEM_READ) == 0 && r0 == 1 && r1 == 5, "SEM_READ is 1/5");
    check(u32op(s, NTSYNC_IOC_SEM_RELEASE, 4, &v) == 0 && v == 1, "SEM_RELEASE 4 returns the previous count 1");
    check(u32op(s, NTSYNC_IOC_SEM_RELEASE, 1, &v) == -EOVERFLOW, "SEM_RELEASE past max is EOVERFLOW");
    check(rd(s, NTSYNC_IOC_SEM_READ) == 0 && r0 == 5, "EOVERFLOW leaves the count at 5");
    check(io(s, NTSYNC_IOC_EVENT_SET, &v) == -EINVAL, "EVENT_SET on a semaphore is EINVAL");
    check(io(s, NTSYNC_IOC_CREATE_SEM, junk) == -ENOTTY, "a device ioctl on an object is ENOTTY");

    /* 3. Мьютекс. */
    check(mutex(0, 1) == -EINVAL && mutex(5, 0) == -EINVAL, "CREATE_MUTEX with owner xor count is EINVAL");
    int m = (int)mutex(0, 0);
    check(m > 2, "CREATE_MUTEX free gives an fd");
    u32 margs[2] = {7, 0};
    check(io(m, NTSYNC_IOC_MUTEX_UNLOCK, margs) == -EPERM, "MUTEX_UNLOCK of a free mutex is EPERM");
    u32 mo[1] = {(u32)m};
    check(wany(mo, 1, INF, 7) == 0 && last_index == 0, "WAIT_ANY takes the free mutex, index 0");
    check(rd(m, NTSYNC_IOC_MUTEX_READ) == 0 && r0 == 7 && r1 == 1, "MUTEX_READ is owner 7 count 1");
    check(wany(mo, 1, INF, 7) == 0 && rd(m, NTSYNC_IOC_MUTEX_READ) == 0 && r1 == 2, "the owner takes it again recursively");
    check(wany(mo, 1, mono(), 8) == -ETIMEDOUT, "another owner times out on it");
    margs[0] = 8;
    check(io(m, NTSYNC_IOC_MUTEX_UNLOCK, margs) == -EPERM, "MUTEX_UNLOCK by another owner is EPERM");
    margs[0] = 7;
    margs[1] = 99;
    check(io(m, NTSYNC_IOC_MUTEX_UNLOCK, margs) == 0 && margs[1] == 2, "MUTEX_UNLOCK returns the previous count 2");
    margs[0] = 0;
    check(io(m, NTSYNC_IOC_MUTEX_UNLOCK, margs) == -EINVAL, "MUTEX_UNLOCK owner 0 is EINVAL");
    check(u32op(m, NTSYNC_IOC_MUTEX_KILL, 9, 0) == -EPERM, "MUTEX_KILL by a non-owner is EPERM");
    check(u32op(m, NTSYNC_IOC_MUTEX_KILL, 7, 0) == 0, "MUTEX_KILL by the owner");
    check(rd(m, NTSYNC_IOC_MUTEX_READ) == -EOWNERDEAD && r0 == 0 && r1 == 0, "MUTEX_READ of a killed mutex is EOWNERDEAD 0/0");
    check(wany(mo, 1, INF, 8) == -EOWNERDEAD && last_index == 0, "WAIT_ANY on an abandoned mutex is EOWNERDEAD index 0");
    check(rd(m, NTSYNC_IOC_MUTEX_READ) == 0 && r0 == 8 && r1 == 1, "the abandoned flag is cleared, owner 8");

    /* 4. События. */
    int ea = (int)event(0, 1);
    u32 eo[1] = {(u32)ea};
    check(ea > 2 && wany(eo, 1, INF, 1) == 0, "WAIT_ANY on a signaled auto event");
    check(wany(eo, 1, mono(), 1) == -ETIMEDOUT, "the auto event was reset by the wait");
    check(u32op(ea, NTSYNC_IOC_EVENT_SET, 77, &v) == 0 && v == 0, "EVENT_SET returns previous 0");
    check(u32op(ea, NTSYNC_IOC_EVENT_SET, 77, &v) == 0 && v == 1, "EVENT_SET again returns previous 1");
    check(u32op(ea, NTSYNC_IOC_EVENT_RESET, 77, &v) == 0 && v == 1, "EVENT_RESET returns previous 1");
    check(rd(ea, NTSYNC_IOC_EVENT_READ) == 0 && r0 == 0 && r1 == 0, "EVENT_READ is auto, not signaled");
    int em = (int)event(5, 3);
    u32 emo[1] = {(u32)em};
    check(rd(em, NTSYNC_IOC_EVENT_READ) == 0 && r0 == 1 && r1 == 1, "nonzero manual/signaled read back as 1");
    check(wany(emo, 1, mono(), 1) == 0 && wany(emo, 1, mono(), 1) == 0, "a manual event satisfies two waits");
    check(u32op(em, NTSYNC_IOC_EVENT_PULSE, 0, &v) == 0 && v == 1 && rd(em, NTSYNC_IOC_EVENT_READ) == 0 && r1 == 0,
          "EVENT_PULSE without waiters leaves it reset");

    /* 5. Параметры ожидания. */
    u32 pair[2] = {(u32)s, (u32)ea};
    check(wany(pair, 1, mono(), 0) == -EINVAL, "WAIT owner 0 is EINVAL");
    {
        struct wait_args a = {mono(), (u64)pair, 1, 0, 0, 1, 0, 1};
        check(io(dev, NTSYNC_IOC_WAIT_ANY, &a) == -EINVAL, "WAIT pad != 0 is EINVAL");
        a.pad = 0;
        a.flags = 2;
        check(io(dev, NTSYNC_IOC_WAIT_ANY, &a) == -EINVAL, "WAIT unknown flag is EINVAL");
    }
    u32 many[65];
    for (int i = 0; i < 65; i++) many[i] = (u32)s;
    check(wany(many, 65, mono(), 1) == -EINVAL, "WAIT count 65 is EINVAL");
    u32 bad[2] = {1, 0xffffffffu};
    check(wany(bad, 1, mono(), 1) == -EINVAL && wany(bad + 1, 1, mono(), 1) == -EINVAL,
          "a non-ntsync fd or -1 in objs is EINVAL");
    u32 dup2[2] = {(u32)s, (u32)s};
    check(wall(dup2, 2, mono(), 1) == -EINVAL, "WAIT_ALL with a duplicate is EINVAL");
    check(wany(0, 0, mono() + 20000000ul, 1) == -ETIMEDOUT, "WAIT_ANY on nothing times out");
    int dev_b = (int)sys3(SYS_open, "/dev/ntsync", O_RDONLY | O_CLOEXEC, 0);
    {
        u32 vb[2] = {0, 1};
        int other = (int)io(dev_b, NTSYNC_IOC_CREATE_EVENT, vb);
        u32 oo[1] = {(u32)other};
        check(dev_b > 2 && other > 2 && wany(oo, 1, mono(), 1) == -EINVAL, "an object of another device is EINVAL");
        sys3(SYS_close, other, 0, 0);
        sys3(SYS_close, dev_b, 0, 0);
    }

    /* 6. Сроки. */
    u64 t0 = mono();
    r = wany(eo, 1, t0 + 50000000ul, 1);
    u64 took = mono() - t0;
    check(r == -ETIMEDOUT && took >= 50000000ul && took < 500000000ul, "absolute monotonic deadline: ETIMEDOUT after 50 ms");
    if (took < 50000000ul || took >= 500000000ul) out_num("waited ns", (i64)took);
    t0 = mono();
    r = waitx(dev, NTSYNC_IOC_WAIT_ANY, eo, 1, clock_ns(0) + 50000000ul, 1, 0, NTSYNC_WAIT_REALTIME);
    took = mono() - t0;
    check(r == -ETIMEDOUT && took >= 40000000ul && took < 500000000ul, "absolute realtime deadline: ETIMEDOUT after ~50 ms");
    if (r != -ETIMEDOUT || took < 40000000ul || took >= 500000000ul) out_num("realtime waited ns", (i64)took);
    check(wany(emo, 0, 1, 1) == -ETIMEDOUT, "a past deadline with nothing is ETIMEDOUT");
    u32op(em, NTSYNC_IOC_EVENT_SET, 0, 0);
    check(wany(emo, 1, 1, 1) == 0, "a past deadline still polls a signaled object");

    /* Пробуждение из другого потока, срок бесконечный. */
    later(ea, NTSYNC_IOC_EVENT_SET, 0, 20);
    t0 = mono();
    r = wany(eo, 1, INF, 1);
    check(r == 0 && last_index == 0 && set_done, "infinite WAIT_ANY wakes on EVENT_SET from another thread");
    wait_flag(&set_done, 2000);

    /* 7. Порядок и справедливость. */
    int s0 = (int)sem(1, 9), s1 = (int)sem(1, 9);
    u32 two[2] = {(u32)s0, (u32)s1};
    check(wany(two, 2, INF, 1) == 0 && last_index == 0 && rd(s1, NTSYNC_IOC_SEM_READ) == 0 && r0 == 1,
          "two signaled: index 0 wins, the second is untouched");
    int ex = (int)event(0, 0), ey = (int)event(0, 0);
    wsetup(0, NTSYNC_IOC_WAIT_ANY, (u32)ex, (u32)ey, 2, 1, 0, mono() + 10000000000ul);
    spawn(w0);
    wait_flag(&ws[0].started, 2000);
    nap_ms(50);
    u32op(ey, NTSYNC_IOC_EVENT_SET, 0, 0);
    check(wait_flag(&ws[0].done, 5000) && ws[0].ret == 0 && ws[0].index == 1, "a wake on the second object returns index 1");

    int sq = (int)sem(0, 9);
    for (int k = 0; k < 3; k++) wsetup(k, NTSYNC_IOC_WAIT_ANY, (u32)sq, 0, 1, (u32)(10 + k), 0, mono() + 10000000000ul);
    spawn(w0);
    spawn(w1);
    spawn(w2);
    wait_flag(&ws[0].started, 2000);
    wait_flag(&ws[1].started, 2000);
    wait_flag(&ws[2].started, 2000);
    nap_ms(100);
    u32op(sq, NTSYNC_IOC_SEM_RELEASE, 2, 0);
    nap_ms(150);
    int woke = ws[0].done + ws[1].done + ws[2].done;
    check(woke == 2 && rd(sq, NTSYNC_IOC_SEM_READ) == 0 && r0 == 0, "SEM_RELEASE 2 wakes exactly two of three waiters");
    if (woke != 2) out_num("woken", woke);
    u32op(sq, NTSYNC_IOC_SEM_RELEASE, 1, 0);
    check(wait_flag(&ws[0].done, 3000) && wait_flag(&ws[1].done, 3000) && wait_flag(&ws[2].done, 3000) &&
              ws[0].ret == 0 && ws[1].ret == 0 && ws[2].ret == 0,
          "SEM_RELEASE 1 wakes the third");

    /* 8. WAIT_ALL. */
    int e1 = (int)event(0, 0), e2 = (int)event(0, 0);
    wsetup(0, NTSYNC_IOC_WAIT_ALL, (u32)e1, (u32)e2, 2, 1, 0, mono() + 10000000000ul);
    spawn(w0);
    wait_flag(&ws[0].started, 2000);
    nap_ms(50);
    u32op(e1, NTSYNC_IOC_EVENT_SET, 0, 0);
    nap_ms(50);
    check(!ws[0].done && rd(e1, NTSYNC_IOC_EVENT_READ) == 0 && r1 == 1, "WAIT_ALL does not wake on one of two, nor take it");
    u32op(e2, NTSYNC_IOC_EVENT_SET, 0, 0);
    check(wait_flag(&ws[0].done, 5000) && ws[0].ret == 0 && ws[0].index == 0, "WAIT_ALL wakes when both are set, index 0");
    check(rd(e1, NTSYNC_IOC_EVENT_READ) == 0 && r1 == 0 && rd(e2, NTSYNC_IOC_EVENT_READ) == 0 && r1 == 0,
          "WAIT_ALL reset both auto events");
    int sa = (int)sem(2, 9), en = (int)event(1, 0);
    u32 sae[2] = {(u32)sa, (u32)en};
    check(wall(sae, 2, mono(), 1) == -ETIMEDOUT && rd(sa, NTSYNC_IOC_SEM_READ) == 0 && r0 == 2,
          "WAIT_ALL with one not ready takes nothing");
    u32op(en, NTSYNC_IOC_EVENT_SET, 0, 0);
    check(wall(sae, 2, mono(), 1) == 0 && rd(sa, NTSYNC_IOC_SEM_READ) == 0 && r0 == 1 && rd(en, NTSYNC_IOC_EVENT_READ) == 0 &&
              r1 == 1,
          "WAIT_ALL takes the semaphore once, a manual event stays set");
    int md = (int)mutex(3, 1);
    u32op(md, NTSYNC_IOC_MUTEX_KILL, 3, 0);
    u32 mde[2] = {(u32)md, (u32)en};
    check(wall(mde, 2, INF, 4) == -EOWNERDEAD && last_index == 0 && rd(md, NTSYNC_IOC_MUTEX_READ) == 0 && r0 == 4,
          "WAIT_ALL with an abandoned mutex is EOWNERDEAD and takes it");

    /* 9. Алерт. */
    int al = (int)event(1, 1), an = (int)event(0, 0);
    u32 ano[1] = {(u32)an};
    r = waitx(dev, NTSYNC_IOC_WAIT_ANY, ano, 1, INF, 1, (u32)al, 0);
    check(r == 0 && last_index == 1, "WAIT_ANY: a signaled alert gives index == count");
    u32op(an, NTSYNC_IOC_EVENT_SET, 0, 0);
    r = waitx(dev, NTSYNC_IOC_WAIT_ANY, ano, 1, INF, 1, (u32)al, 0);
    check(r == 0 && last_index == 0, "WAIT_ANY: a signaled object beats the alert");
    u32 ae[2] = {(u32)e1, (u32)e2};
    r = waitx(dev, NTSYNC_IOC_WAIT_ALL, ae, 2, INF, 1, (u32)al, 0);
    check(r == 0 && last_index == 2, "WAIT_ALL: the alert gives index == count");
    check(rd(al, NTSYNC_IOC_EVENT_READ) == 0 && r1 == 1, "the kernel does not reset the alert");
    u32op(al, NTSYNC_IOC_EVENT_RESET, 0, 0);
    wsetup(0, NTSYNC_IOC_WAIT_ANY, (u32)an, 0, 1, 1, (u32)al, mono() + 10000000000ul);
    spawn(w0);
    wait_flag(&ws[0].started, 2000);
    nap_ms(50);
    u32op(al, NTSYNC_IOC_EVENT_SET, 0, 0);
    check(wait_flag(&ws[0].done, 5000) && ws[0].ret == 0 && ws[0].index == 1, "setting the alert wakes a sleeping waiter");

    /* 10. PULSE. */
    int ep = (int)event(0, 0);
    wsetup(0, NTSYNC_IOC_WAIT_ANY, (u32)ep, 0, 1, 1, 0, mono() + 10000000000ul);
    spawn(w0);
    wait_flag(&ws[0].started, 2000);
    nap_ms(50);
    check(u32op(ep, NTSYNC_IOC_EVENT_PULSE, 0, &v) == 0 && v == 0, "EVENT_PULSE returns previous 0");
    check(wait_flag(&ws[0].done, 5000) && ws[0].ret == 0 && rd(ep, NTSYNC_IOC_EVENT_READ) == 0 && r1 == 0,
          "EVENT_PULSE wakes the auto waiter and leaves the event reset");
    int epm = (int)event(1, 0);
    wsetup(0, NTSYNC_IOC_WAIT_ANY, (u32)epm, 0, 1, 1, 0, mono() + 10000000000ul);
    wsetup(1, NTSYNC_IOC_WAIT_ANY, (u32)epm, 0, 1, 2, 0, mono() + 10000000000ul);
    spawn(w0);
    spawn(w1);
    wait_flag(&ws[0].started, 2000);
    wait_flag(&ws[1].started, 2000);
    nap_ms(80);
    u32op(epm, NTSYNC_IOC_EVENT_PULSE, 0, 0);
    check(wait_flag(&ws[0].done, 5000) && wait_flag(&ws[1].done, 5000) && ws[0].ret == 0 && ws[1].ret == 0 &&
              rd(epm, NTSYNC_IOC_EVENT_READ) == 0 && r1 == 0,
          "EVENT_PULSE on a manual event wakes both waiters, then resets");

    /* 11. Сигналы. */
    u64 handler, restorer_at;
    __asm__("lea on_usr1(%%rip), %0" : "=r"(handler));
    __asm__("lea restorer(%%rip), %0" : "=r"(restorer_at));
    u64 act[4];
    act[0] = handler;
    act[1] = SA_SIGINFO | SA_RESTORER;
    act[2] = restorer_at;
    act[3] = 0;
    check(sys6(SYS_rt_sigaction, SIGUSR1, (i64)act, 0, 8, 0, 0) == 0, "rt_sigaction SIGUSR1 without SA_RESTART");
    int es = (int)event(0, 0);
    wsetup(0, NTSYNC_IOC_WAIT_ANY, (u32)es, 0, 1, 1, 0, INF);
    ws[0].retry_eintr = 1;
    spawn(w0);
    wait_flag(&ws[0].started, 2000);
    nap_ms(50);
    i64 pid = sys3(SYS_getpid, 0, 0, 0);
    sys3(SYS_tgkill, pid, ws[0].tid, SIGUSR1);
    for (int i = 0; i < 500 && ws[0].first == 12345; i++) nap_ms(2);
    check(ws[0].first == -EINTR && handled >= 1, "tgkill SIGUSR1 interrupts WAIT_ANY with EINTR");
    nap_ms(50);
    check(!ws[0].done, "the retried wait keeps waiting");
    u32op(es, NTSYNC_IOC_EVENT_SET, 0, 0);
    check(wait_flag(&ws[0].done, 5000) && ws[0].ret == 0, "the retried wait wakes on EVENT_SET");

    race_sem = (int)sem(0, 1000000);
    race_tid = (int)sys3(SYS_gettid, 0, 0, 0);
    race_n = 300;
    race_done = 0;
    spawn(racer);
    u32 rs[1] = {(u32)race_sem};
    int got = 0, eintr = 0, other = 0;
    while (got < race_n) {
        r = wany(rs, 1, mono() + 5000000000ul, 1);
        if (r == 0) got++;
        else if (r == -EINTR) eintr++;
        else {
            other++;
            break;
        }
    }
    wait_flag(&race_done, 5000);
    check(got == race_n && other == 0 && rd(race_sem, NTSYNC_IOC_SEM_READ) == 0 && r0 == 0,
          "signals racing releases lose no acquisition");
    out_num("EINTR during the race", eintr);

    /* 12. Дескрипторы: dup, fork, SCM_RIGHTS. */
    int ed = (int)event(1, 0);
    int ed2 = (int)sys3(SYS_dup, ed, 0, 0);
    u32op(ed2, NTSYNC_IOC_EVENT_SET, 0, 0);
    check(ed2 > 2 && rd(ed, NTSYNC_IOC_EVENT_READ) == 0 && r1 == 1, "a dup fd is the same object");
    int ef = (int)event(0, 0);
    i64 child = sys3(SYS_fork, 0, 0, 0);
    if (child == 0) {
        nap_ms(30);
        u32op(ef, NTSYNC_IOC_EVENT_SET, 0, 0);
        sys3(SYS_exit_group, 0, 0, 0);
    }
    u32 efo[1] = {(u32)ef};
    check(child > 0 && wany(efo, 1, mono() + 5000000000ul, 1) == 0, "a forked child sets an event the parent waits on");
    if (child > 0) sys6(SYS_wait4, child, 0, 0, 0, 0, 0);
    test_scm_rights(ed);

    /* 13. Закрытие fd посреди ожидания. */
    int ec = (int)event(0, 0);
    int ec2 = (int)sys3(SYS_dup, ec, 0, 0);
    wsetup(0, NTSYNC_IOC_WAIT_ANY, (u32)ec, 0, 1, 1, 0, mono() + 10000000000ul);
    spawn(w0);
    wait_flag(&ws[0].started, 2000);
    nap_ms(50);
    sys3(SYS_close, ec, 0, 0);
    nap_ms(20);
    u32op(ec2, NTSYNC_IOC_EVENT_SET, 0, 0);
    check(wait_flag(&ws[0].done, 5000) && ws[0].ret == 0, "closing the fd during a wait keeps the object alive");

    /* 14. Нагрузка. */
    u32 sixty[64];
    for (int i = 0; i < 64; i++) sixty[i] = (u32)event(0, i == 63);
    r = wany(sixty, 64, INF, 1);
    check(r == 0 && last_index == 63, "WAIT_ANY on 64 objects finds the last one");
    for (int i = 0; i < 64; i++) sys3(SYS_close, sixty[i], 0, 0);
    pp_a = (int)sem(0, 1);
    pp_b = (int)sem(0, 1);
    pp_n = 20000;
    pp_done = 0;
    spawn(ponger);
    t0 = mono();
    u32 pa[1] = {(u32)pp_a};
    int rounds = 0;
    for (; rounds < pp_n; rounds++) {
        u32 one = 1;
        io(pp_b, NTSYNC_IOC_SEM_RELEASE, &one);
        if (wany(pa, 1, mono() + 5000000000ul, 1) != 0) break;
    }
    took = mono() - t0;
    wait_flag(&pp_done, 5000);
    check(rounds == pp_n && took < 20000000000ul, "20000 semaphore ping-pongs between two threads");
    out_num("ping-pong ms", (i64)(took / 1000000));

    out(failed ? "ntsyncprobe: some failed\n" : "ntsyncprobe: 0 failed\n");
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
