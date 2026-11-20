/* Общее для проб узла DRM nouveau (`drmprobe.c`, `nvkprobe.c`, 18.11.2026,
 * NVK поверх GSP-RM): программы Linux x86-64 без libc, как `ntsyncprobe.c` —
 * только `syscall`, статическая PIE без перемещений.
 *
 * Структуры — копии `include/uapi/drm/{drm,nouveau_drm}.h` (Linux 6.12) и
 * NVIF (`nvif/ioctl.h`, `nvif/cl0080.h`) с размерами, сверенными по
 * компилированным заголовкам (спецификация nvk-spec.md, раздел 3.4); каждая
 * размерность проверяется `_Static_assert`, так что опечатка в поле ломает
 * сборку, а не ядро.
 *
 * Вывод: `<проба>: ok <что>` / `<проба>: FAILED <что>` / `<проба>: note ...`;
 * итог — `<проба>: 0 failed` или `<проба>: N checks failed` (строка с
 * ненулём не содержит подстроки «0 failed»), код выхода — число провалов.
 */

/* Не каждой пробе нужны все обёртки ниже. */
#pragma clang diagnostic ignored "-Wunused-function"

typedef unsigned long u64;
typedef unsigned int u32;
typedef unsigned short u16;
typedef unsigned char u8;
typedef long i64;
typedef int i32;

__asm__(".section .note.ABI-tag, \"a\", @note\n"
        ".balign 4\n"
        ".long 4, 16, 1\n"
        ".asciz \"GNU\"\n"
        ".long 0, 3, 2, 0\n"
        ".text\n");

enum {
    SYS_read = 0, SYS_write = 1, SYS_open = 2, SYS_close = 3, SYS_stat = 4, SYS_fstat = 5, SYS_mmap = 9,
    SYS_munmap = 11, SYS_ioctl = 16, SYS_nanosleep = 35, SYS_exit_group = 231, SYS_readlink = 89,
    SYS_getdents64 = 217, SYS_clock_gettime = 228,
};
enum { EPERM = 1, ENOENT = 2, EBADF = 9, EFAULT = 14, EINVAL = 22, ENOTTY = 25, ENOSPC = 28, ENOSYS = 38, ETIME = 62, ENODEV = 19 };
enum { O_RDONLY = 0, O_RDWR = 2, O_DIRECTORY = 0x10000, O_CLOEXEC = 0x80000 };
enum { PROT_READ = 1, PROT_WRITE = 2, MAP_SHARED = 1 };
enum { S_IFMT = 0170000, S_IFCHR = 0020000, S_IFDIR = 0040000 };

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

static u64 slen(const char *s) {
    u64 n = 0;
    while (s[n]) n++;
    return n;
}
static int seq(const char *a, const char *b) {
    while (*a && *a == *b) a++, b++;
    return *a == *b;
}
static int starts(const char *s, const char *head) {
    while (*head)
        if (*s++ != *head++) return 0;
    return 1;
}

/* ---- вывод ---- */

static const char *probe;
static int failed;

/* Строка вывода: накапливается кусками и уходит одним `write`. */
static char line[512];
static u64 line_len;
static void put(const char *s) {
    while (*s && line_len < sizeof line - 2) line[line_len++] = *s++;
}
static void put_dec(i64 v) {
    char d[24];
    int n = 0;
    if (v < 0) {
        put("-");
        v = -v;
    }
    do {
        d[n++] = (char)('0' + v % 10);
        v /= 10;
    } while (v);
    while (n && line_len < sizeof line - 2) line[line_len++] = d[--n];
}
static void put_hex(u64 v) {
    char d[20];
    int n = 0;
    put("0x");
    do {
        d[n++] = "0123456789abcdef"[v & 15];
        v >>= 4;
    } while (v);
    while (n && line_len < sizeof line - 2) line[line_len++] = d[--n];
}
static void begin(const char *kind) {
    line_len = 0;
    put(probe);
    put(": ");
    put(kind);
}
static void end(void) {
    line[line_len++] = '\n';
    sys3(SYS_write, 1, line, line_len);
    line_len = 0;
}
static void say(const char *s) {
    begin("");
    put(s);
    end();
}
static void check(int good, const char *what) {
    begin(good ? "ok " : "FAILED ");
    put(what);
    end();
    if (!good) failed++;
}
/* Провал с числом (код ошибки, значение) — чтобы по журналу было видно, что пришло. */
static void check_v(int good, const char *what, i64 got) {
    begin(good ? "ok " : "FAILED ");
    put(what);
    if (!good) {
        put(" (got ");
        put_dec(got);
        put(" / ");
        put_hex((u64)got);
        put(")");
    }
    end();
    if (!good) failed++;
}
static void note_hex(const char *what, u64 v) {
    begin("note ");
    put(what);
    put(" ");
    put_hex(v);
    end();
}
static void note_dec(const char *what, i64 v) {
    begin("note ");
    put(what);
    put(" ");
    put_dec(v);
    end();
}
static void finish(void) {
    begin("");
    if (failed) {
        put_dec(failed);
        put(" checks failed");
    } else {
        put("0 failed");
    }
    end();
    sys3(SYS_exit_group, failed, 0, 0);
    for (;;) {
    }
}

/* ---- время ---- */

struct timespec_ { i64 sec, nsec; };
static u64 mono(void) {
    struct timespec_ t;
    sys3(SYS_clock_gettime, 1, &t, 0);
    return (u64)t.sec * 1000000000ul + (u64)t.nsec;
}
static void nap_ms(i64 ms) {
    struct timespec_ t = {ms / 1000, (ms % 1000) * 1000000L};
    sys3(SYS_nanosleep, &t, 0, 0);
}

/* ---- файлы ---- */

struct stat_ {
    u64 dev, ino, nlink;
    u32 mode, uid, gid, pad0;
    u64 rdev;
    i64 size, blksize, blocks;
    u64 times[6];
    i64 unused[3];
};
_Static_assert(sizeof(struct stat_) == 144, "struct stat x86-64");
static u32 dev_major(u64 d) { return (u32)(((d >> 8) & 0xfff) | ((d >> 32) & ~0xfffu)); }
static u32 dev_minor(u64 d) { return (u32)((d & 0xff) | ((d >> 12) & ~0xffu)); }

/* Прочитать маленький файл целиком (как sysfs-чтения libdrm); -errno при ошибке. */
static i64 read_file(const char *path, char *buf, u64 cap) {
    i64 fd = sys3(SYS_open, path, O_RDONLY | O_CLOEXEC, 0);
    if (fd < 0) return fd;
    u64 got = 0;
    for (;;) {
        i64 r = sys3(SYS_read, fd, buf + got, cap - 1 - got);
        if (r <= 0) break;
        got += (u64)r;
        if (got >= cap - 1) break;
    }
    buf[got] = 0;
    sys3(SYS_close, fd, 0, 0);
    return (i64)got;
}
/* `%x` как у libdrm: необязательный `0x`, шестнадцатеричные цифры. */
static i64 parse_hex(const char *s) {
    while (*s == ' ') s++;
    if (s[0] == '0' && (s[1] == 'x' || s[1] == 'X')) s += 2;
    i64 v = 0;
    int any = 0;
    for (;; s++) {
        int d;
        if (*s >= '0' && *s <= '9') d = *s - '0';
        else if (*s >= 'a' && *s <= 'f') d = *s - 'a' + 10;
        else if (*s >= 'A' && *s <= 'F') d = *s - 'A' + 10;
        else break;
        v = v * 16 + d;
        any = 1;
    }
    return any ? v : -1;
}

/* ---- uAPI DRM ---- */

#define DRM_IOCTL_VERSION 0xC0406400u
#define DRM_IOCTL_GET_CAP 0xC010640Cu
#define DRM_IOCTL_GEM_CLOSE 0x40086409u
#define DRM_IOCTL_PRIME_HANDLE_TO_FD 0xC00C642Du
#define DRM_IOCTL_PRIME_FD_TO_HANDLE 0xC00C642Eu
#define DRM_IOCTL_SYNCOBJ_CREATE 0xC00864BFu
#define DRM_IOCTL_SYNCOBJ_DESTROY 0xC00864C0u
#define DRM_IOCTL_SYNCOBJ_WAIT 0xC02864C3u
#define DRM_IOCTL_SYNCOBJ_RESET 0xC01064C4u
#define DRM_IOCTL_SYNCOBJ_SIGNAL 0xC01064C5u
#define DRM_IOCTL_SYNCOBJ_TIMELINE_WAIT 0xC03064CAu
#define DRM_IOCTL_SYNCOBJ_QUERY 0xC01864CBu
#define DRM_IOCTL_SYNCOBJ_TIMELINE_SIGNAL 0xC01864CDu

#define DRM_IOCTL_NOUVEAU_GETPARAM 0xC0106440u
#define DRM_IOCTL_NOUVEAU_CHANNEL_ALLOC 0xC0586442u
#define DRM_IOCTL_NOUVEAU_CHANNEL_FREE 0x40046443u
#define DRM_IOCTL_NOUVEAU_VM_INIT 0xC0106450u
#define DRM_IOCTL_NOUVEAU_VM_BIND 0xC0286451u
#define DRM_IOCTL_NOUVEAU_EXEC 0xC0286452u
#define DRM_IOCTL_NOUVEAU_GEM_NEW 0xC0306480u
#define DRM_IOCTL_NOUVEAU_GEM_CPU_PREP 0x40086482u
#define DRM_IOCTL_NOUVEAU_GEM_CPU_FINI 0x40046483u
#define DRM_IOCTL_NOUVEAU_GEM_INFO 0xC0286484u
/* NVIF (nr 0x47): libdrm `drmCommandWrite` = `DRM_IOW(0x47, size)` — направление
 * «только запись», хотя ядро пишет ответ в тот же буфер. Ядро обязано
 * разбирать только nr (спецификация 3.4) — проба нарочно шлёт именно так. */
#define DRM_NVIF_IOW(size) (0x40000000u | ((u32)(size) << 16) | 0x6447u)

enum {
    DRM_CAP_PRIME = 5, DRM_CAP_TIMESTAMP_MONOTONIC = 6, DRM_CAP_SYNCOBJ = 0x13, DRM_CAP_SYNCOBJ_TIMELINE = 0x14,
};
enum {
    NOUVEAU_GETPARAM_PCI_VENDOR = 3, NOUVEAU_GETPARAM_PCI_DEVICE = 4, NOUVEAU_GETPARAM_BUS_TYPE = 5,
    NOUVEAU_GETPARAM_FB_SIZE = 8, NOUVEAU_GETPARAM_AGP_SIZE = 9, NOUVEAU_GETPARAM_CHIPSET_ID = 11,
    NOUVEAU_GETPARAM_VM_VRAM_BASE = 12, NOUVEAU_GETPARAM_GRAPH_UNITS = 13, NOUVEAU_GETPARAM_PTIMER_TIME = 14,
    NOUVEAU_GETPARAM_HAS_BO_USAGE = 15, NOUVEAU_GETPARAM_HAS_PAGEFLIP = 16, NOUVEAU_GETPARAM_EXEC_PUSH_MAX = 17,
    NOUVEAU_GETPARAM_VRAM_BAR_SIZE = 18, NOUVEAU_GETPARAM_VRAM_USED = 19, NOUVEAU_GETPARAM_HAS_VMA_TILEMODE = 20,
};
enum {
    NOUVEAU_GEM_DOMAIN_VRAM = 2, NOUVEAU_GEM_DOMAIN_GART = 4, NOUVEAU_GEM_DOMAIN_MAPPABLE = 8,
    NOUVEAU_GEM_DOMAIN_NO_SHARE = 0x20,
};
enum { NOUVEAU_GEM_CPU_PREP_NOWAIT = 1, NOUVEAU_GEM_CPU_PREP_WRITE = 4 };
enum { VM_BIND_OP_MAP = 0, VM_BIND_OP_UNMAP = 1, VM_BIND_SPARSE = 1 << 8, VM_BIND_RUN_ASYNC = 1 };
enum { SYNC_BINARY = 0, SYNC_TIMELINE = 1 };
enum { SYNCOBJ_CREATE_SIGNALED = 1, WAIT_ALL = 1, WAIT_FOR_SUBMIT = 2, WAIT_AVAILABLE = 4 };
enum { DRM_CLOEXEC = O_CLOEXEC };

struct drm_version {
    i32 major, minor, patch, pad;
    u64 name_len, name;
    u64 date_len, date;
    u64 desc_len, desc;
};
struct drm_get_cap { u64 capability, value; };
struct drm_gem_close { u32 handle, pad; };
struct drm_prime_handle { u32 handle, flags; i32 fd; };
struct drm_syncobj_create { u32 handle, flags; };
struct drm_syncobj_destroy { u32 handle, pad; };
struct drm_syncobj_wait { u64 handles; i64 timeout_nsec; u32 count_handles, flags, first_signaled, pad; u64 deadline_nsec; };
struct drm_syncobj_timeline_wait {
    u64 handles, points;
    i64 timeout_nsec;
    u32 count_handles, flags, first_signaled, pad;
    u64 deadline_nsec;
};
struct drm_syncobj_array { u64 handles; u32 count_handles, pad; };
struct drm_syncobj_timeline_array { u64 handles, points; u32 count_handles, flags; };

struct drm_nouveau_getparam { u64 param, value; };
struct drm_nouveau_channel_alloc {
    u32 fb_ctxdma_handle, tt_ctxdma_handle;
    i32 channel;
    u32 pushbuf_domains, notifier_handle;
    struct { u32 handle, grclass; } subchan[8];
    u32 nr_subchan;
};
struct drm_nouveau_channel_free { i32 channel; };
struct drm_nouveau_gem_info { u32 handle, domain; u64 size, offset, map_handle; u32 tile_mode, tile_flags; };
struct drm_nouveau_gem_new { struct drm_nouveau_gem_info info; u32 channel_hint, align; };
struct drm_nouveau_gem_cpu_prep { u32 handle, flags; };
struct drm_nouveau_sync { u32 flags, handle; u64 timeline_value; };
struct drm_nouveau_vm_init { u64 kernel_managed_addr, kernel_managed_size; };
struct drm_nouveau_vm_bind_op { u32 op, flags, handle, pad; u64 addr, bo_offset, range; };
struct drm_nouveau_vm_bind { u32 op_count, flags, wait_count, sig_count; u64 wait_ptr, sig_ptr, op_ptr; };
struct drm_nouveau_exec_push { u64 va; u32 va_len, flags; };
struct drm_nouveau_exec { u32 channel, push_count, wait_count, sig_count; u64 wait_ptr, sig_ptr, push_ptr; };

/* NVIF */
enum { NVIF_SCLASS = 1, NVIF_NEW = 2, NVIF_DEL = 3, NVIF_MTHD = 4 };
enum { NVIF_OWNER_ANY = 0xff, NVIF_ROUTE_NVIF = 0, NVIF_ROUTE_HIDDEN = 0xff };
enum { NV_DEVICE = 0x0080, NV_DEVICE_V0_INFO = 0 };
struct nvif_ioctl_v0 { u8 version, type, pad02[4], owner, route; u64 token, object; };
struct nvif_ioctl_new_v0 { u8 version, pad01[6], route; u64 token, object; u32 handle; i32 oclass; };
struct nvif_ioctl_mthd_v0 { u8 version, method, pad02[6]; };
struct nvif_ioctl_sclass_v0 { u8 version, count, pad02[6]; };
struct nvif_sclass_entry { i32 oclass; short minver, maxver; };
struct nv_device_v0 { u8 version, pad01[7]; u64 device; };
struct nv_device_info_v0 {
    u8 version, platform;
    u16 chipset;
    u8 revision, family, pad06[2];
    u64 ram_size, ram_user;
    char chip[16];
    char name[64];
};

_Static_assert(sizeof(struct drm_version) == 64, "drm_version");
_Static_assert(sizeof(struct drm_nouveau_getparam) == 16, "getparam");
_Static_assert(sizeof(struct drm_nouveau_channel_alloc) == 88, "channel_alloc");
_Static_assert(sizeof(struct drm_nouveau_gem_new) == 48, "gem_new");
_Static_assert(sizeof(struct drm_nouveau_gem_info) == 40, "gem_info");
_Static_assert(sizeof(struct drm_nouveau_vm_init) == 16, "vm_init");
_Static_assert(sizeof(struct drm_nouveau_vm_bind) == 40, "vm_bind");
_Static_assert(sizeof(struct drm_nouveau_vm_bind_op) == 40, "vm_bind_op");
_Static_assert(sizeof(struct drm_nouveau_exec) == 40, "exec");
_Static_assert(sizeof(struct drm_nouveau_exec_push) == 16, "exec_push");
_Static_assert(sizeof(struct drm_nouveau_sync) == 16, "sync");
_Static_assert(sizeof(struct drm_syncobj_wait) == 40, "syncobj_wait");
_Static_assert(sizeof(struct drm_syncobj_timeline_wait) == 48, "syncobj_timeline_wait");
_Static_assert(sizeof(struct drm_syncobj_timeline_array) == 24, "syncobj_timeline_array");
_Static_assert(sizeof(struct drm_syncobj_array) == 16, "syncobj_array");
_Static_assert(sizeof(struct nvif_ioctl_v0) == 24, "nvif_ioctl_v0");
_Static_assert(sizeof(struct nvif_ioctl_new_v0) == 32, "nvif_ioctl_new_v0");
_Static_assert(__builtin_offsetof(struct nvif_ioctl_new_v0, handle) == 24, "new.handle");
_Static_assert(sizeof(struct nvif_ioctl_mthd_v0) == 8, "nvif_ioctl_mthd_v0");
_Static_assert(sizeof(struct nvif_ioctl_sclass_v0) == 8, "nvif_ioctl_sclass_v0");
_Static_assert(sizeof(struct nvif_sclass_entry) == 8, "sclass entry");
_Static_assert(sizeof(struct nv_device_v0) == 16, "nv_device_v0");
_Static_assert(sizeof(struct nv_device_info_v0) == 104, "nv_device_info_v0");

/* ---- обёртки ---- */

static int drm;
static i64 io(u32 req, void *arg) { return sys3(SYS_ioctl, drm, (u64)req, arg); }

static i64 getparam(u64 param, u64 *value) {
    struct drm_nouveau_getparam g = {param, 0};
    i64 r = io(DRM_IOCTL_NOUVEAU_GETPARAM, &g);
    *value = g.value;
    return r;
}

/* Окно ядра у NVK: [1<<39, 1<<40) (`nvkmd_nouveau_dev.c`). */
#define KERNEL_VA (1ul << 39)
#define KERNEL_VA_SIZE ((1ul << 40) - (1ul << 39))
static i64 vm_init(void) {
    struct drm_nouveau_vm_init v = {KERNEL_VA, KERNEL_VA_SIZE};
    return io(DRM_IOCTL_NOUVEAU_VM_INIT, &v);
}

/* Устройство NVIF так, как его создаёт Mesa (`nouveau_ws_device_new`). */
#define DEVICE_COOKIE 0xdec0de0000000001ul
static i64 nvif_device_new(void) {
    struct {
        struct nvif_ioctl_v0 ioctl;
        struct nvif_ioctl_new_v0 new;
        struct nv_device_v0 dev;
    } a;
    memset(&a, 0, sizeof a);
    a.ioctl.type = NVIF_NEW;
    a.ioctl.owner = NVIF_OWNER_ANY;
    a.ioctl.route = NVIF_ROUTE_NVIF;
    a.new.oclass = NV_DEVICE;
    a.new.object = DEVICE_COOKIE;
    a.new.token = DEVICE_COOKIE;
    a.dev.device = ~0ul;
    return io(DRM_NVIF_IOW(sizeof a), &a);
}
static i64 nvif_device_info(struct nv_device_info_v0 *info) {
    struct {
        struct nvif_ioctl_v0 ioctl;
        struct nvif_ioctl_mthd_v0 mthd;
        struct nv_device_info_v0 info;
    } a;
    memset(&a, 0, sizeof a);
    a.ioctl.type = NVIF_MTHD;
    a.ioctl.owner = NVIF_OWNER_ANY;
    a.ioctl.object = DEVICE_COOKIE;
    a.mthd.method = NV_DEVICE_V0_INFO;
    i64 r = io(DRM_NVIF_IOW(sizeof a), &a);
    *info = a.info;
    return r;
}
/* Классы канала: `count` на входе — сколько мест, ядро пишет сколько есть. */
static i64 nvif_sclass(i32 channel, struct nvif_sclass_entry *out, int cap, int *count) {
    struct {
        struct nvif_ioctl_v0 ioctl;
        struct nvif_ioctl_sclass_v0 sclass;
        struct nvif_sclass_entry e[16];
    } a;
    memset(&a, 0, sizeof a);
    a.ioctl.type = NVIF_SCLASS;
    a.ioctl.owner = NVIF_OWNER_ANY;
    a.ioctl.route = NVIF_ROUTE_HIDDEN;
    a.ioctl.token = (u64)channel;
    a.sclass.count = 16;
    i64 r = io(DRM_NVIF_IOW(sizeof a), &a);
    *count = a.sclass.count;
    for (int i = 0; i < cap && i < 16; i++) out[i] = a.e[i];
    return r;
}
static i64 nvif_obj_new(i32 channel, u32 handle, u32 oclass, u64 cookie) {
    struct {
        struct nvif_ioctl_v0 ioctl;
        struct nvif_ioctl_new_v0 new;
    } a;
    memset(&a, 0, sizeof a);
    a.ioctl.type = NVIF_NEW;
    a.ioctl.owner = NVIF_OWNER_ANY;
    a.ioctl.route = NVIF_ROUTE_HIDDEN;
    a.ioctl.token = (u64)channel;
    a.new.route = NVIF_ROUTE_NVIF;
    a.new.handle = handle;
    a.new.oclass = (i32)(oclass & 0xffff);
    a.new.object = cookie;
    a.new.token = cookie;
    return io(DRM_NVIF_IOW(sizeof a), &a);
}
static i64 nvif_obj_del(i32 channel, u64 cookie) {
    struct nvif_ioctl_v0 a;
    memset(&a, 0, sizeof a);
    a.type = NVIF_DEL;
    a.owner = NVIF_OWNER_ANY;
    a.route = NVIF_ROUTE_HIDDEN;
    a.token = (u64)channel;
    a.object = cookie;
    return io(DRM_NVIF_IOW(sizeof a), &a);
}
static i64 channel_alloc(i32 *channel) {
    struct drm_nouveau_channel_alloc a;
    memset(&a, 0, sizeof a);
    a.channel = -1;
    i64 r = io(DRM_IOCTL_NOUVEAU_CHANNEL_ALLOC, &a);
    *channel = a.channel;
    return r;
}
static i64 channel_free(i32 channel) {
    struct drm_nouveau_channel_free f = {channel};
    return io(DRM_IOCTL_NOUVEAU_CHANNEL_FREE, &f);
}
/* Лучший класс из списка с таким младшим байтом (правило Mesa `nouveau_context.c`). */
static u32 pick_class(const struct nvif_sclass_entry *e, int n, u32 low) {
    u32 best = 0;
    for (int i = 0; i < n && i < 16; i++)
        if (((u32)e[i].oclass & 0xff) == low && (u32)e[i].oclass > best) best = (u32)e[i].oclass;
    return best;
}

static i64 syncobj_create(u32 flags, u32 *handle) {
    struct drm_syncobj_create c = {0, flags};
    i64 r = io(DRM_IOCTL_SYNCOBJ_CREATE, &c);
    *handle = c.handle;
    return r;
}
static i64 syncobj_destroy(u32 handle) {
    struct drm_syncobj_destroy d = {handle, 0};
    return io(DRM_IOCTL_SYNCOBJ_DESTROY, &d);
}
/* Срок у syncobj — абсолютный по CLOCK_MONOTONIC. */
static i64 syncobj_wait(u32 handle, u32 flags, u64 timeout_ms) {
    struct drm_syncobj_wait w;
    memset(&w, 0, sizeof w);
    w.handles = (u64)&handle;
    w.count_handles = 1;
    w.flags = flags;
    w.timeout_nsec = (i64)(mono() + timeout_ms * 1000000ul);
    return io(DRM_IOCTL_SYNCOBJ_WAIT, &w);
}
static i64 timeline_wait(u32 handle, u64 point, u32 flags, u64 timeout_ms) {
    struct drm_syncobj_timeline_wait w;
    memset(&w, 0, sizeof w);
    w.handles = (u64)&handle;
    w.points = (u64)&point;
    w.count_handles = 1;
    w.flags = flags;
    w.timeout_nsec = (i64)(mono() + timeout_ms * 1000000ul);
    return io(DRM_IOCTL_SYNCOBJ_TIMELINE_WAIT, &w);
}
static i64 timeline_query(u32 handle, u64 *point) {
    struct drm_syncobj_timeline_array a;
    *point = ~0ul;
    a.handles = (u64)&handle;
    a.points = (u64)point;
    a.count_handles = 1;
    a.flags = 0;
    return io(DRM_IOCTL_SYNCOBJ_QUERY, &a);
}

#define RENDER_NODE "/dev/dri/renderD128"
static i64 open_render(void) { return sys3(SYS_open, RENDER_NODE, O_RDWR | O_CLOEXEC, 0); }

__asm__(".text\n"
        ".globl _start\n"
        "_start:\n"
        "  xor %rbp, %rbp\n"
        "  mov %rsp, %rdi\n"
        "  and $-16, %rsp\n"
        "  call start_c\n"
        "  hlt\n");
