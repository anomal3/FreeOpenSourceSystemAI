/* Проба узла DRM nouveau (18.11.2026, NVK поверх GSP-RM, веха M3): то, что
 * делают libdrm 2.4.124 (`drmGetDevices2`, `drmGetVersion`) и Mesa 25.0.7
 * (`nouveau_ws_device_new`, `nouveau_ws_context_create`) до первого буфера:
 *
 *   1. `/dev/dri`: в списке есть renderD128, это символьное устройство 226:128;
 *   2. sysfs как у libdrm: `device/drm`, ссылка `device/subsystem` на `.../pci`,
 *      `PCI_SLOT_NAME` в `uevent`, `vendor`, `device`, `subsystem_vendor`,
 *      `subsystem_device`, `revision`;
 *   3. `DRM_IOCTL_VERSION` в два вызова (длины, затем строки): `nouveau` >= 1.3.1;
 *   4. `GET_CAP` (PRIME, SYNCOBJ, SYNCOBJ_TIMELINE, TIMESTAMP_MONOTONIC);
 *   5. `VM_INIT` с окном ядра NVK, NVIF NEW NV_DEVICE + MTHD INFO, GETPARAM;
 *   6. канал: CHANNEL_ALLOC, NVIF SCLASS (ожидаемые классы GA106), NVIF NEW/DEL
 *      каждого объекта, пустой EXEC (проверка «канал не убит»), CHANNEL_FREE;
 *   7. syncobj: двоичный и timeline, ожидание по сроку.
 *
 * Запуск: `drmprobe [soft|hw]` (режим только печатается: значения устройства
 * у SoftGpu и у RTX 3060 разные, проверяется их согласованность). Общие
 * обёртки — `drmprobe.h`.
 */

#include "drmprobe.h"

/* Таблиц указателей в статической памяти здесь нет нарочно: программа —
 * статическая PIE без кода перемещений, такой указатель остался бы
 * непоправленным (проверка — `readelf -r` пуст). switch со строками clang
 * тоже превращает в такую таблицу, поэтому — одна строка с нулями внутри и
 * поиск i-го куска. */
static const char *nth(const char *packed, int i) {
    while (i-- > 0) packed += slen(packed) + 1;
    return packed;
}
static const char *sysfs_name(int i) { return nth("vendor\0device\0subsystem_vendor\0subsystem_device\0revision", i); }
static const char *object_name(int i) { return nth("copy\0" "2D\0" "3D\0" "M2MF\0compute", i); }

static void test_sysfs(u32 *sysfs_device) {
    /* Список /dev/dri. */
    i64 dir = sys3(SYS_open, "/dev/dri", O_RDONLY | O_DIRECTORY | O_CLOEXEC, 0);
    int found = 0;
    if (dir >= 0) {
        static char buf[4096];
        for (;;) {
            i64 n = sys3(SYS_getdents64, dir, buf, sizeof buf);
            if (n <= 0) break;
            for (i64 at = 0; at < n;) {
                u16 reclen = *(u16 *)(buf + at + 16);
                const char *name = buf + at + 19;
                if (seq(name, "renderD128")) found = 1;
                else if (starts(name, "card") || starts(name, "renderD")) {
                    begin("note /dev/dri also has ");
                    put(name);
                    end();
                }
                if (!reclen) break;
                at += reclen;
            }
        }
        sys3(SYS_close, dir, 0, 0);
    }
    check(dir >= 0 && found, "/dev/dri lists renderD128");

    struct stat_ st;
    i64 r = sys3(SYS_stat, RENDER_NODE, &st, 0);
    check_v(r == 0 && (st.mode & S_IFMT) == S_IFCHR && dev_major(st.rdev) == 226 && dev_minor(st.rdev) == 128,
            "renderD128 is a character device 226:128", r ? r : (i64)st.rdev);

    r = sys3(SYS_stat, "/sys/dev/char/226:128/device/drm", &st, 0);
    check_v(r == 0, "sysfs device/drm exists (drmNodeIsDRM)", r);

    char link[256];
    r = sys3(SYS_readlink, "/sys/dev/char/226:128/device/subsystem", link, sizeof link - 1);
    if (r > 0) link[r] = 0;
    else link[0] = 0;
    const char *base = link;
    for (const char *s = link; *s; s++)
        if (*s == '/') base = s + 1;
    check_v(r > 0 && seq(base, "pci"), "device/subsystem links to .../pci", r);

    static char text[1024];
    r = read_file("/sys/dev/char/226:128/device/uevent", text, sizeof text);
    const char *slot = 0;
    for (char *s = text; r > 0 && *s; s++) {
        if ((s == text || s[-1] == '\n') && starts(s, "PCI_SLOT_NAME=")) {
            slot = s + 14;
            char *e = (char *)slot;
            while (*e && *e != '\n') e++;
            *e = 0;
            break;
        }
    }
    check(slot != 0 && slen(slot) == 12, "uevent has PCI_SLOT_NAME=dddd:bb:dd.f");
    if (slot) {
        begin("note PCI slot ");
        put(slot);
        end();
    }

    i64 vals[5];
    for (int i = 0; i < 5; i++) {
        char path[96];
        line_len = 0;
        put("/sys/dev/char/226:128/device/");
        put(sysfs_name(i));
        memcpy(path, line, line_len);
        path[line_len] = 0;
        line_len = 0;
        char v[32];
        vals[i] = read_file(path, v, sizeof v) > 0 ? parse_hex(v) : -1;
        begin("note sysfs ");
        put(sysfs_name(i));
        put(" ");
        put_hex((u64)vals[i]);
        end();
    }
    check_v(vals[0] == 0x10de, "sysfs vendor is 0x10de", vals[0]);
    check(vals[1] > 0 && vals[2] >= 0 && vals[3] >= 0 && vals[4] >= 0, "sysfs device, subsystem ids and revision parse as hex");
    *sysfs_device = (u32)vals[1];
}

static void test_version(void) {
    struct drm_version v;
    memset(&v, 0, sizeof v);
    i64 r = io(DRM_IOCTL_VERSION, &v);
    check_v(r == 0 && v.name_len > 0 && v.name_len < 64 && v.date_len < 64 && v.desc_len < 256,
            "VERSION first call gives the string lengths", r ? r : (i64)v.name_len);
    char name[64], date[64], desc[256];
    memset(name, 0, sizeof name);
    memset(date, 0, sizeof date);
    memset(desc, 0, sizeof desc);
    u64 nl = v.name_len, dl = v.date_len, sl = v.desc_len;
    v.name = (u64)name;
    v.date = (u64)date;
    v.desc = (u64)desc;
    v.name_len = nl < 63 ? nl : 63;
    v.date_len = dl < 63 ? dl : 63;
    v.desc_len = sl < 255 ? sl : 255;
    r = io(DRM_IOCTL_VERSION, &v);
    begin("note version ");
    put(name);
    put(" ");
    put_dec(v.major);
    put(".");
    put_dec(v.minor);
    put(".");
    put_dec(v.patch);
    put(" date ");
    put(date);
    put(" desc '");
    put(desc);
    put("'");
    end();
    u32 packed = ((u32)v.major << 24) | ((u32)v.minor << 8) | (u32)v.patch;
    check_v(r == 0 && starts(name, "nouveau"), "VERSION name is nouveau", r);
    check_v(packed >= 0x01000301u, "VERSION is at least 1.3.1 (Mesa NVK)", packed);
}

static void cap(u64 which, u64 want, const char *what) {
    struct drm_get_cap c = {which, 0xdead};
    i64 r = io(DRM_IOCTL_GET_CAP, &c);
    check_v(r == 0 && c.value == want, what, r ? r : (i64)c.value);
}
static void test_caps(void) {
    cap(DRM_CAP_PRIME, 3, "GET_CAP PRIME is import|export");
    cap(DRM_CAP_SYNCOBJ, 1, "GET_CAP SYNCOBJ is 1");
    cap(DRM_CAP_SYNCOBJ_TIMELINE, 1, "GET_CAP SYNCOBJ_TIMELINE is 1");
    cap(DRM_CAP_TIMESTAMP_MONOTONIC, 1, "GET_CAP TIMESTAMP_MONOTONIC is 1");
}

static void test_getparam(u32 sysfs_device) {
    u64 v;
    i64 r = getparam(NOUVEAU_GETPARAM_PCI_VENDOR, &v);
    check_v(r == 0 && v == 0x10de, "GETPARAM PCI_VENDOR is 0x10de", r ? r : (i64)v);
    r = getparam(NOUVEAU_GETPARAM_PCI_DEVICE, &v);
    note_hex("pci device", v);
    check_v(r == 0 && v == sysfs_device, "GETPARAM PCI_DEVICE matches sysfs", r ? r : (i64)v);
    r = getparam(NOUVEAU_GETPARAM_BUS_TYPE, &v);
    check_v(r == 0 && v == 2, "GETPARAM BUS_TYPE is PCIE", r ? r : (i64)v);
    r = getparam(NOUVEAU_GETPARAM_CHIPSET_ID, &v);
    note_hex("chipset", v);
    check_v(r == 0 && v >= 0x170 && v < 0x200, "GETPARAM CHIPSET_ID is an Ampere chip", r ? r : (i64)v);
    r = getparam(NOUVEAU_GETPARAM_FB_SIZE, &v);
    note_dec("fb MiB", (i64)(v >> 20));
    check_v(r == 0 && v > 0, "GETPARAM FB_SIZE is not zero", r);
    r = getparam(NOUVEAU_GETPARAM_GRAPH_UNITS, &v);
    note_dec("gpcs", (i64)(v & 0xff));
    note_dec("tpcs", (i64)((v >> 8) & 0xffff));
    check_v(r == 0 && (v & 0xff) > 0 && ((v >> 8) & 0xffff) > 0, "GETPARAM GRAPH_UNITS has gpcs and tpcs", r ? r : (i64)v);
    r = getparam(NOUVEAU_GETPARAM_EXEC_PUSH_MAX, &v);
    check_v(r == 0 && v >= 1 && v <= 512, "GETPARAM EXEC_PUSH_MAX is 1..512", r ? r : (i64)v);
    r = getparam(NOUVEAU_GETPARAM_VRAM_BAR_SIZE, &v);
    note_dec("vram bar MiB", (i64)(v >> 20));
    check_v(r == 0, "GETPARAM VRAM_BAR_SIZE", r);
    r = getparam(NOUVEAU_GETPARAM_VRAM_USED, &v);
    check_v(r == 0 && v > 0, "GETPARAM VRAM_USED is not zero", r ? r : (i64)v);
    r = getparam(NOUVEAU_GETPARAM_HAS_VMA_TILEMODE, &v);
    check_v(r == 0 && v == 1, "GETPARAM HAS_VMA_TILEMODE is 1", r ? r : (i64)v);
    u64 t0, t1;
    r = getparam(NOUVEAU_GETPARAM_PTIMER_TIME, &t0);
    nap_ms(5);
    i64 r1 = getparam(NOUVEAU_GETPARAM_PTIMER_TIME, &t1);
    check_v(r == 0 && r1 == 0 && t1 > t0, "GETPARAM PTIMER_TIME ticks", r ? r : (i64)(t1 - t0));
    r = getparam(99, &v);
    check_v(r == -EINVAL, "an unknown GETPARAM is EINVAL", r);
}

static void test_nvif_device(void) {
    i64 r = nvif_device_new();
    check_v(r == 0, "NVIF NEW NV_DEVICE (Mesa layout, DRM_IOW)", r);
    struct nv_device_info_v0 info;
    memset(&info, 0, sizeof info);
    r = nvif_device_info(&info);
    info.chip[15] = 0;
    info.name[63] = 0;
    begin("note device chipset ");
    put_hex(info.chipset);
    put(" platform ");
    put_dec(info.platform);
    put(" ram_user MiB ");
    put_dec((i64)(info.ram_user >> 20));
    put(" chip '");
    put(info.chip);
    put("' name '");
    put(info.name);
    put("'");
    end();
    u64 chipset;
    getparam(NOUVEAU_GETPARAM_CHIPSET_ID, &chipset);
    check_v(r == 0 && info.chipset == chipset, "NVIF MTHD NV_DEVICE_V0_INFO chipset matches GETPARAM", r ? r : (i64)info.chipset);
    check_v(info.platform == 3, "NV_DEVICE_V0_INFO platform is PCIE (discrete GPU)", info.platform);
    check(info.ram_user > 0 && info.chip[0] != 0 && info.name[0] != 0, "NV_DEVICE_V0_INFO has ram_user, chip and name");
}

static const u32 expected_classes[6] = {0x902D, 0xA140, 0xC797, 0xC7C0, 0xC6B5, 0xC7B5};

static void test_channel(void) {
    i32 ch = -1;
    i64 r = channel_alloc(&ch);
    note_dec("channel", ch);
    check_v(r == 0 && ch >= 0, "CHANNEL_ALLOC (all-zero request, as NVK)", r);
    if (r != 0) return;

    struct nvif_sclass_entry e[16];
    int n = 0;
    r = nvif_sclass(ch, e, 16, &n);
    begin("note sclass");
    for (int i = 0; i < n && i < 16; i++) {
        put(" ");
        put_hex((u32)e[i].oclass);
    }
    end();
    check_v(r == 0 && n > 0 && n <= 16, "NVIF SCLASS on the channel", r ? r : n);
    int all = 1;
    for (int k = 0; k < 6; k++) {
        int have = 0;
        for (int i = 0; i < n && i < 16; i++)
            if ((u32)e[i].oclass == expected_classes[k]) have = 1;
        if (!have) {
            note_hex("missing class", expected_classes[k]);
            all = 0;
        }
    }
    check(all, "SCLASS lists 2D, M2MF, 3D, compute and both copy classes");

    /* Объекты в порядке и с ручками Mesa (`nouveau_context.c`). */
    u32 base = (u32)(0xbeef + ch) << 16;
    static const u32 lows[5] = {0xb5, 0x2d, 0x97, 0x40, 0xc0};
    u32 handles[5] = {0, base | 0x902d, base | 0x003d, base | 0x323f, base | 0x00c0};
    u64 cookies[5];
    int made[5] = {0, 0, 0, 0, 0};
    for (int i = 0; i < 5; i++) {
        u32 cls = pick_class(e, n, lows[i]);
        if (i == 3 && !cls) cls = pick_class(e, n, 0x39);
        cookies[i] = 0xc0de000000000000ul | ((u64)ch << 16) | (u64)i;
        r = cls ? nvif_obj_new(ch, handles[i], cls, cookies[i]) : -ENOENT;
        made[i] = r == 0;
        begin(r == 0 ? "ok " : "FAILED ");
        put("NVIF NEW ");
        put(object_name(i));
        put(" class ");
        put_hex(cls);
        if (r) {
            put(" -> ");
            put_dec(r);
        }
        end();
        if (r) failed++;
    }

    /* Пустой EXEC — так NVK узнаёт, что канал жив (0, а не ENODEV). */
    struct drm_nouveau_exec x;
    memset(&x, 0, sizeof x);
    x.channel = (u32)ch;
    r = io(DRM_IOCTL_NOUVEAU_EXEC, &x);
    check_v(r == 0, "an empty EXEC on a live channel is 0", r);

    static const int order[5] = {4, 2, 0, 3, 1}; /* compute, 3D, copy, M2MF, 2D */
    int dels = 1;
    for (int k = 0; k < 5; k++) {
        int i = order[k];
        if (made[i] && nvif_obj_del(ch, cookies[i]) != 0) dels = 0;
    }
    check(dels, "NVIF DEL of every object");
    r = channel_free(ch);
    check_v(r == 0, "CHANNEL_FREE", r);
    x.channel = (u32)ch;
    r = io(DRM_IOCTL_NOUVEAU_EXEC, &x);
    check_v(r != 0, "EXEC on a freed channel fails", r);
}

static void test_syncobj(void) {
    u32 s;
    i64 r = syncobj_create(SYNCOBJ_CREATE_SIGNALED, &s);
    check_v(r == 0 && s != 0, "SYNCOBJ_CREATE signaled", r);
    r = syncobj_wait(s, WAIT_ALL, 1000);
    check_v(r == 0, "SYNCOBJ_WAIT on a signaled syncobj is 0", r);
    syncobj_destroy(s);

    u32 u;
    syncobj_create(0, &u);
    u64 t0 = mono();
    r = syncobj_wait(u, WAIT_ALL, 30);
    u64 took = (mono() - t0) / 1000000ul;
    /* Без WAIT_FOR_SUBMIT у syncobj без забора Linux отвечает EINVAL сразу;
     * с ним — ждёт до срока и отвечает ETIME. */
    check_v(r == -EINVAL, "SYNCOBJ_WAIT on an unsubmitted syncobj without WAIT_FOR_SUBMIT is EINVAL", r);
    t0 = mono();
    r = syncobj_wait(u, WAIT_ALL | WAIT_FOR_SUBMIT, 30);
    took = (mono() - t0) / 1000000ul;
    check_v(r == -ETIME && took >= 20, "WAIT_FOR_SUBMIT on an unsubmitted syncobj is ETIME after the deadline", r ? r : (i64)took);
    struct drm_syncobj_array a = {(u64)&u, 1, 0};
    r = io(DRM_IOCTL_SYNCOBJ_SIGNAL, &a);
    i64 w = syncobj_wait(u, WAIT_ALL, 1000);
    check_v(r == 0 && w == 0, "SYNCOBJ_SIGNAL then WAIT is 0", r ? r : w);
    r = io(DRM_IOCTL_SYNCOBJ_RESET, &a);
    w = syncobj_wait(u, WAIT_ALL | WAIT_FOR_SUBMIT, 10);
    check_v(r == 0 && w == -ETIME, "SYNCOBJ_RESET makes it unsignaled again", r ? r : w);
    syncobj_destroy(u);

    u32 t;
    syncobj_create(0, &t);
    u64 p5 = 5;
    struct drm_syncobj_timeline_array ta = {(u64)&t, (u64)&p5, 1, 0};
    r = io(DRM_IOCTL_SYNCOBJ_TIMELINE_SIGNAL, &ta);
    u64 point;
    i64 q = timeline_query(t, &point);
    check_v(r == 0 && q == 0 && point == 5, "TIMELINE_SIGNAL 5 then QUERY gives 5", r ? r : q ? q : (i64)point);
    r = timeline_wait(t, 5, WAIT_ALL, 1000);
    check_v(r == 0, "TIMELINE_WAIT for a signaled point is 0", r);
    r = timeline_wait(t, 6, WAIT_ALL | WAIT_FOR_SUBMIT, 20);
    check_v(r == -ETIME, "TIMELINE_WAIT for a future point is ETIME", r);
    r = syncobj_destroy(t);
    check_v(r == 0, "SYNCOBJ_DESTROY", r);
    r = syncobj_destroy(t);
    check_v(r == -EINVAL, "a second SYNCOBJ_DESTROY is EINVAL", r);
}

__attribute__((used)) void start_c(u64 *sp) {
    int argc = (int)sp[0];
    char **argv = (char **)(sp + 1);
    probe = "drmprobe";
    begin("start mode ");
    put(argc > 1 ? argv[1] : "soft");
    end();

    u32 sysfs_device = 0;
    test_sysfs(&sysfs_device);
    i64 fd = open_render();
    check_v(fd >= 0, "open /dev/dri/renderD128 O_RDWR|O_CLOEXEC", fd);
    if (fd < 0) finish();
    drm = (int)fd;
    test_version();
    test_caps();
    i64 r = vm_init();
    check_v(r == 0, "VM_INIT with the NVK kernel window [1<<39, 1<<40)", r);
    /* Повтор: у Linux — ошибка; спецификация её не требует — только в журнал. */
    note_dec("second VM_INIT ->", vm_init());
    test_nvif_device();
    test_getparam(sysfs_device);
    test_channel();
    test_syncobj();
    r = sys3(SYS_close, drm, 0, 0);
    check_v(r == 0, "close the render node", r);
    say("done");
    finish();
}
