/* Проба буферов и отправки работы nouveau (18.11.2026, NVK поверх GSP-RM,
 * вехи M4 — SoftGpu в QEMU, M5 — RTX 3060): ровно те вызовы, которыми NVK
 * (Mesa 25.0.7 `nvkmd_nouveau_*`) копирует буфер, только поток команд собран
 * руками:
 *
 *   GEM_NEW: источник и приёмник в системной памяти (GART|MAPPABLE), середина
 *   в VRAM (без MAPPABLE), буфер команд (GART|MAPPABLE); mmap по map_handle;
 *   VM_BIND MAP всех четырёх (RUN_ASYNC, сигнал двоичного syncobj, ожидание);
 *   поток команд: SET_OBJECT копирования (NVC7B5/NVC6B5, подканал 4), две
 *   копии источник -> VRAM -> приёмник (NON_PIPELINED, PITCH, FLUSH), затем
 *   SEMAPHORE A..D канала (подканал 0) — освобождение семафора в буфере
 *   команд; EXEC с сигналом timeline syncobj (точка 1); TIMELINE_WAIT;
 *   сверка всех 64 КиБ и значения семафора.
 *
 * Дальше: GEM_INFO, GEM_CPU_PREP, PRIME туда-обратно (WSI NVK), ошибки
 * VM_BIND (невыровненный адрес, окно ядра), разреженная область (в режиме
 * `soft` — ещё и чтение нулей из неё копированием), UNMAP, GEM_CLOSE,
 * NVIF DEL, CHANNEL_FREE.
 *
 * Запуск: `nvkprobe [soft|hw]`. В `hw` разреженная область не читается
 * видеокартой (спецификация 4.3: не в первом проходе).
 */

#include "drmprobe.h"

#define SIZE 0x10000ul
#define VA_SRC 0x100000000ul
#define VA_VRAM 0x100100000ul
#define VA_DST 0x100200000ul
#define VA_PUSH 0x100300000ul
#define VA_SPARSE 0x200000000ul
#define SEM_OFFSET 0x800u
#define SEM_VALUE 0xc0ffee01u

/* Копирование Ampere (`clc7b5.h`, open-gpu-doc). */
enum {
    CE_LAUNCH_DMA = 0x300, CE_OFFSET_IN_UPPER = 0x400, CE_SET_SEMAPHORE_A = 0x240,
    LAUNCH_NON_PIPELINED = 2, LAUNCH_FLUSH = 1 << 2, LAUNCH_SRC_PITCH = 1 << 7, LAUNCH_DST_PITCH = 1 << 8,
};
/* Методы канала (NVC56F, как `nvc0_fence_emit32`): SEMAPHOREA..D. */
enum { HOST_SEMAPHOREA = 0x10, SEMD_RELEASE = 2 /* WFI_EN = 0, SIZE_16BYTE = 0 */ };
enum { SUBC_HOST = 0, SUBC_COPY = 4 };

struct bo { u32 handle; u64 map_handle, size; volatile u8 *cpu; };

static i64 gem_new(struct bo *b, u64 size, u32 domain) {
    struct drm_nouveau_gem_new g;
    memset(&g, 0, sizeof g);
    g.info.size = size;
    g.info.domain = domain;
    g.align = 4096;
    i64 r = io(DRM_IOCTL_NOUVEAU_GEM_NEW, &g);
    b->handle = g.info.handle;
    b->map_handle = g.info.map_handle;
    b->size = size;
    b->cpu = 0;
    return r;
}
static i64 gem_map(struct bo *b) {
    i64 p = sys6(SYS_mmap, 0, (i64)b->size, PROT_READ | PROT_WRITE, MAP_SHARED, drm, (i64)b->map_handle);
    if (p < 0 && p > -4096) return p;
    b->cpu = (volatile u8 *)p;
    return 0;
}
static i64 gem_close(u32 handle) {
    struct drm_gem_close c = {handle, 0};
    return io(DRM_IOCTL_GEM_CLOSE, &c);
}

/* Одна операция VM_BIND; sig — двоичный syncobj (0 — синхронно, без RUN_ASYNC). */
static i64 bind1(u32 op, u32 flags, u32 handle, u64 va, u64 bo_off, u64 range, u32 sig) {
    struct drm_nouveau_vm_bind_op o = {op, flags, handle, 0, va, bo_off, range};
    struct drm_nouveau_sync s = {SYNC_BINARY, sig, 0};
    struct drm_nouveau_vm_bind b;
    memset(&b, 0, sizeof b);
    b.op_count = 1;
    b.op_ptr = (u64)&o;
    if (sig) {
        b.flags = VM_BIND_RUN_ASYNC;
        b.sig_count = 1;
        b.sig_ptr = (u64)&s;
    }
    return io(DRM_IOCTL_NOUVEAU_VM_BIND, &b);
}
/* Как NVK: асинхронно с сигналом и ожиданием. */
static i64 bind_wait(u32 op, u32 flags, u32 handle, u64 va, u64 range) {
    u32 s;
    i64 r = syncobj_create(0, &s);
    if (r) return r;
    r = bind1(op, flags, handle, va, 0, range, s);
    if (r == 0) r = syncobj_wait(s, WAIT_ALL | WAIT_FOR_SUBMIT, 5000);
    syncobj_destroy(s);
    return r;
}

/* ---- поток команд ---- */

static volatile u32 *push;
static u32 push_n;
/* Заголовок метода Fermi+: SEC_OP INC_METHOD (1 << 29), число слов, подканал, адрес/4. */
static void mthd(u32 subc, u32 addr, u32 count) { push[push_n++] = 0x20000000u | (count << 16) | (subc << 13) | (addr >> 2); }
static void data(u32 v) { push[push_n++] = v; }

static void ce_copy(u64 src, u64 dst, u32 bytes) {
    mthd(SUBC_COPY, CE_OFFSET_IN_UPPER, 8);
    data((u32)(src >> 32));
    data((u32)src);
    data((u32)(dst >> 32));
    data((u32)dst);
    data(bytes); /* PITCH_IN */
    data(bytes); /* PITCH_OUT */
    data(bytes); /* LINE_LENGTH_IN */
    data(1);     /* LINE_COUNT */
    mthd(SUBC_COPY, CE_LAUNCH_DMA, 1);
    data(LAUNCH_NON_PIPELINED | LAUNCH_FLUSH | LAUNCH_SRC_PITCH | LAUNCH_DST_PITCH);
}
static void host_release(u64 va, u32 value) {
    mthd(SUBC_HOST, HOST_SEMAPHOREA, 4);
    data((u32)(va >> 32));
    data((u32)va);
    data(value);
    data(SEMD_RELEASE);
}

/* EXEC одного отрезка из буфера команд с сигналом точки timeline. */
static i64 exec_push(i32 ch, u32 bytes, u32 timeline, u64 point) {
    struct drm_nouveau_exec_push p = {VA_PUSH, bytes, 0};
    struct drm_nouveau_sync s = {SYNC_TIMELINE, timeline, point};
    struct drm_nouveau_exec x;
    memset(&x, 0, sizeof x);
    x.channel = (u32)ch;
    x.push_count = 1;
    x.push_ptr = (u64)&p;
    x.sig_count = 1;
    x.sig_ptr = (u64)&s;
    return io(DRM_IOCTL_NOUVEAU_EXEC, &x);
}

static u8 pattern(u64 i) { return (u8)((i * 131u + (i >> 8) * 7u + 0x5a) & 0xff); }

__attribute__((used)) void start_c(u64 *sp) {
    int argc = (int)sp[0];
    char **argv = (char **)(sp + 1);
    int hw = argc > 1 && seq(argv[1], "hw");
    probe = "nvkprobe";
    begin("start mode ");
    put(hw ? "hw" : "soft");
    end();

    i64 fd = open_render();
    check_v(fd >= 0, "open /dev/dri/renderD128", fd);
    if (fd < 0) finish();
    drm = (int)fd;
    i64 r = vm_init();
    check_v(r == 0, "VM_INIT", r);
    r = nvif_device_new();
    check_v(r == 0, "NVIF NEW NV_DEVICE", r);

    /* Канал и объект копирования. */
    i32 ch = -1;
    r = channel_alloc(&ch);
    check_v(r == 0 && ch >= 0, "CHANNEL_ALLOC", r);
    if (r) finish();
    struct nvif_sclass_entry e[16];
    int n = 0;
    nvif_sclass(ch, e, 16, &n);
    u32 copy = pick_class(e, n, 0xb5);
    note_hex("copy class", copy);
    const u64 copy_cookie = 0xc0de0000000000b5ul;
    r = copy ? nvif_obj_new(ch, 0, copy, copy_cookie) : -ENOENT;
    check_v(r == 0, "NVIF NEW copy object (handle 0)", r);

    /* Буферы. */
    struct bo src, vram, dst, pb;
    const u32 sysmem = NOUVEAU_GEM_DOMAIN_GART | NOUVEAU_GEM_DOMAIN_MAPPABLE | NOUVEAU_GEM_DOMAIN_NO_SHARE;
    i64 r1 = gem_new(&src, SIZE, NOUVEAU_GEM_DOMAIN_GART | NOUVEAU_GEM_DOMAIN_MAPPABLE);
    i64 r2 = gem_new(&vram, SIZE, NOUVEAU_GEM_DOMAIN_VRAM | NOUVEAU_GEM_DOMAIN_NO_SHARE);
    i64 r3 = gem_new(&dst, SIZE, sysmem);
    i64 r4 = gem_new(&pb, 4096, sysmem);
    check_v(r1 == 0 && r2 == 0 && r3 == 0 && r4 == 0, "GEM_NEW 3 sysmem BOs and one VRAM BO", r1 ? r1 : r2 ? r2 : r3 ? r3 : r4);
    check(src.handle && vram.handle && dst.handle && pb.handle && src.handle != dst.handle && src.map_handle != dst.map_handle,
          "GEM_NEW gives distinct handles and map handles");
    r1 = gem_map(&src);
    r2 = gem_map(&dst);
    r3 = gem_map(&pb);
    check_v(r1 == 0 && r2 == 0 && r3 == 0, "mmap the mappable BOs by map_handle", r1 ? r1 : r2 ? r2 : r3);
    if (r1 || r2 || r3) finish();
    int zero = 1;
    for (u64 i = 0; i < SIZE; i++)
        if (src.cpu[i] || dst.cpu[i]) zero = 0;
    check(zero, "new BOs read as zeros");
    for (u64 i = 0; i < SIZE; i++) {
        src.cpu[i] = pattern(i);
        dst.cpu[i] = 0xaa;
    }

    struct drm_nouveau_gem_info gi;
    memset(&gi, 0, sizeof gi);
    gi.handle = src.handle;
    r = io(DRM_IOCTL_NOUVEAU_GEM_INFO, &gi);
    check_v(r == 0 && gi.size == SIZE && gi.map_handle == src.map_handle, "GEM_INFO gives the size and map handle", r ? r : (i64)gi.size);

    /* Адреса. */
    r1 = bind_wait(VM_BIND_OP_MAP, 0, src.handle, VA_SRC, SIZE);
    r2 = bind_wait(VM_BIND_OP_MAP, 0, vram.handle, VA_VRAM, SIZE);
    r3 = bind_wait(VM_BIND_OP_MAP, 0, dst.handle, VA_DST, SIZE);
    r4 = bind_wait(VM_BIND_OP_MAP, 0, pb.handle, VA_PUSH, 4096);
    check_v(r1 == 0 && r2 == 0 && r3 == 0 && r4 == 0, "VM_BIND MAP x4 (RUN_ASYNC, signal syncobj, wait)", r1 ? r1 : r2 ? r2 : r3 ? r3 : r4);
    r = bind1(VM_BIND_OP_MAP, 0, src.handle, VA_SRC + 0x40000 + 0x10, 0, 4096, 0);
    check_v(r == -EINVAL, "VM_BIND at an unaligned address is EINVAL", r);
    r = bind1(VM_BIND_OP_MAP, 0, src.handle, KERNEL_VA + 0x100000, 0, 4096, 0);
    check_v(r == -EINVAL, "VM_BIND inside the kernel-managed window is EINVAL", r);
    r = bind1(VM_BIND_OP_MAP, 0, src.handle, VA_SRC + 0x80000, SIZE, 4096, 0);
    check_v(r == -EINVAL, "VM_BIND past the end of the BO is EINVAL", r);

    /* Копия sysmem -> VRAM -> sysmem и семафор, одним EXEC. */
    push = (volatile u32 *)pb.cpu;
    push_n = 0;
    volatile u32 *sem = (volatile u32 *)(pb.cpu + SEM_OFFSET);
    *sem = 0;
    mthd(SUBC_COPY, 0, 1);
    data(copy); /* SET_OBJECT */
    ce_copy(VA_SRC, VA_VRAM, SIZE);
    ce_copy(VA_VRAM, VA_DST, SIZE);
    host_release(VA_PUSH + SEM_OFFSET, SEM_VALUE);
    u32 tl;
    syncobj_create(0, &tl);
    u64 t0 = mono();
    r = exec_push(ch, push_n * 4, tl, 1);
    check_v(r == 0, "EXEC one push with a timeline signal", r);
    r = timeline_wait(tl, 1, WAIT_ALL | WAIT_FOR_SUBMIT, 5000);
    u64 us = (mono() - t0) / 1000;
    check_v(r == 0, "TIMELINE_WAIT point 1 after EXEC", r);
    note_dec("exec -> signal us", (i64)us);
    u64 bad = 0, first_bad = ~0ul;
    for (u64 i = 0; i < SIZE; i++)
        if (dst.cpu[i] != pattern(i)) {
            if (first_bad == ~0ul) first_bad = i;
            bad++;
        }
    if (bad) {
        note_hex("first mismatch at", first_bad);
        note_hex("its byte", dst.cpu[first_bad]);
    }
    check_v(bad == 0, "copy 64 KiB sysmem -> VRAM -> sysmem through the copy engine (0 mismatches)", (i64)bad);
    check_v(*sem == SEM_VALUE, "the channel semaphore release wrote its payload", *sem);
    u64 point;
    r = timeline_query(tl, &point);
    check_v(r == 0 && point >= 1, "SYNCOBJ_QUERY shows point 1", r ? r : (i64)point);

    struct drm_nouveau_gem_cpu_prep prep = {dst.handle, NOUVEAU_GEM_CPU_PREP_WRITE};
    r = io(DRM_IOCTL_NOUVEAU_GEM_CPU_PREP, &prep);
    check_v(r == 0, "GEM_CPU_PREP on an idle BO", r);

    /* Второй EXEC в тот же канал — следующая точка. */
    push_n = 0;
    *sem = 0;
    host_release(VA_PUSH + SEM_OFFSET, SEM_VALUE + 1);
    r = exec_push(ch, push_n * 4, tl, 2);
    i64 w = timeline_wait(tl, 2, WAIT_ALL | WAIT_FOR_SUBMIT, 5000);
    check_v(r == 0 && w == 0 && *sem == SEM_VALUE + 1, "a second EXEC signals point 2", r ? r : w ? w : (i64)*sem);

    /* Пустой EXEC: канал жив. */
    struct drm_nouveau_exec x;
    memset(&x, 0, sizeof x);
    x.channel = (u32)ch;
    r = io(DRM_IOCTL_NOUVEAU_EXEC, &x);
    check_v(r == 0, "an empty EXEC after work is 0 (channel alive)", r);

    /* PRIME: так NVK отдаёт изображения WSI. Отдаётся только BO без
     * NO_SHARE (Linux `nouveau_gem_prime_export`: иначе EPERM) — `src`. */
    struct drm_prime_handle ns = {dst.handle, DRM_CLOEXEC | O_RDWR, -1};
    r = io(DRM_IOCTL_PRIME_HANDLE_TO_FD, &ns);
    check_v(r == -EPERM, "PRIME_HANDLE_TO_FD of a NO_SHARE BO is EPERM", r);
    if (r == 0) sys3(SYS_close, ns.fd, 0, 0);
    struct drm_prime_handle ph = {src.handle, DRM_CLOEXEC | O_RDWR, -1};
    r = io(DRM_IOCTL_PRIME_HANDLE_TO_FD, &ph);
    check_v(r == 0 && ph.fd >= 0, "PRIME_HANDLE_TO_FD of a shareable BO", r);
    if (r == 0) {
        struct drm_prime_handle back = {0, 0, ph.fd};
        r = io(DRM_IOCTL_PRIME_FD_TO_HANDLE, &back);
        check_v(r == 0 && back.handle == src.handle, "PRIME_FD_TO_HANDLE in the same file gives the same handle", r ? r : (i64)back.handle);
        memset(&gi, 0, sizeof gi);
        gi.handle = back.handle;
        r = io(DRM_IOCTL_NOUVEAU_GEM_INFO, &gi);
        check_v(r == 0 && gi.size == SIZE, "GEM_INFO on the imported handle", r);
        sys3(SYS_close, ph.fd, 0, 0);
    }

    /* Разреженная область. */
    r = bind_wait(VM_BIND_OP_MAP, VM_BIND_SPARSE, 0, VA_SPARSE, SIZE);
    check_v(r == 0, "VM_BIND sparse region", r);
    if (!hw && r == 0) {
        for (u64 i = 0; i < SIZE; i++) dst.cpu[i] = 0xaa;
        push_n = 0;
        ce_copy(VA_SPARSE, VA_DST, SIZE);
        r = exec_push(ch, push_n * 4, tl, 3);
        w = timeline_wait(tl, 3, WAIT_ALL | WAIT_FOR_SUBMIT, 5000);
        u64 nz = 0;
        for (u64 i = 0; i < SIZE; i++) nz += dst.cpu[i] != 0;
        check_v(r == 0 && w == 0 && nz == 0, "reading a sparse region gives zeros", r ? r : w ? w : (i64)nz);
    }
    r1 = bind_wait(VM_BIND_OP_UNMAP, 0, 0, VA_SPARSE, SIZE);
    r2 = bind_wait(VM_BIND_OP_UNMAP, VM_BIND_SPARSE, 0, VA_SPARSE, SIZE);
    check_v(r1 == 0 && r2 == 0, "UNMAP then UNMAP SPARSE frees the sparse region", r1 ? r1 : r2);

    /* Разбор. */
    r1 = bind_wait(VM_BIND_OP_UNMAP, 0, 0, VA_SRC, SIZE);
    r2 = bind_wait(VM_BIND_OP_UNMAP, 0, 0, VA_VRAM, SIZE);
    r3 = bind_wait(VM_BIND_OP_UNMAP, 0, 0, VA_DST, SIZE);
    r4 = bind_wait(VM_BIND_OP_UNMAP, 0, 0, VA_PUSH, 4096);
    check_v(r1 == 0 && r2 == 0 && r3 == 0 && r4 == 0, "VM_BIND UNMAP x4", r1 ? r1 : r2 ? r2 : r3 ? r3 : r4);
    r = bind_wait(VM_BIND_OP_MAP, 0, src.handle, VA_SRC, SIZE);
    check_v(r == 0, "the same VA can be mapped again after UNMAP", r);
    bind_wait(VM_BIND_OP_UNMAP, 0, 0, VA_SRC, SIZE);
    sys3(SYS_munmap, src.cpu, SIZE, 0);
    sys3(SYS_munmap, dst.cpu, SIZE, 0);
    sys3(SYS_munmap, pb.cpu, 4096, 0);
    r1 = gem_close(src.handle);
    r2 = gem_close(vram.handle);
    r3 = gem_close(dst.handle);
    r4 = gem_close(pb.handle);
    check_v(r1 == 0 && r2 == 0 && r3 == 0 && r4 == 0, "GEM_CLOSE x4", r1 ? r1 : r2 ? r2 : r3 ? r3 : r4);
    r = gem_close(src.handle);
    check_v(r == -EINVAL, "a second GEM_CLOSE is EINVAL", r);
    syncobj_destroy(tl);
    r1 = nvif_obj_del(ch, copy_cookie);
    r2 = channel_free(ch);
    check_v(r1 == 0 && r2 == 0, "NVIF DEL copy object and CHANNEL_FREE", r1 ? r1 : r2);
    r = sys3(SYS_close, drm, 0, 0);
    check_v(r == 0, "close the render node", r);
    say("done");
    finish();
}
