/* Проба узла DRM amdgpu (22.11.2026, `docs/AMD_DRM_PLAN.md`): то, что делают
 * libdrm 2.4.124 (`drmGetDevices2`, `drmGetVersion`, `amdgpu_device_initialize`)
 * и Mesa RADV 25.0.7 (`ac_query_gpu_info`) при перечислении устройства, до
 * первого буфера:
 *
 *   1. `/dev/dri`: в списке есть renderD128, это символьное устройство 226:128;
 *   2. sysfs как у libdrm: `device/drm`, ссылка `device/subsystem` на `.../pci`,
 *      `PCI_SLOT_NAME` и `DRIVER=amdgpu` в `uevent`, `vendor` 0x1002;
 *   3. `DRM_IOCTL_VERSION` в два вызова: `amdgpu` 3.27 или новее;
 *   4. `GET_CAP` (SYNCOBJ, SYNCOBJ_TIMELINE, PRIME, TIMESTAMP_MONOTONIC);
 *   5. `AMDGPU_INFO`: ACCEL_WORKING, DEV_INFO (семейство VI, согласие с sysfs),
 *      READ_MMR_REG 0x263e (GB_ADDR_CONFIG), HW_IP_COUNT/INFO, FW_VERSION ME/PFP/MEC,
 *      MEMORY, VRAM_GTT, неизвестный запрос — EINVAL.
 *
 * Запуск: `amdprobe [soft|hw]` (режим только печатается). Общие обёртки и
 * вывод — `drmprobe.h` (структуры nouveau в нём этой пробе не нужны).
 */

#include "drmprobe.h"

#define DRM_IOCTL_AMDGPU_INFO 0x40206445u /* DRM_IOW(DRM_COMMAND_BASE + 0x05, struct drm_amdgpu_info) */

struct amd_info {
    u64 return_pointer;
    u32 return_size;
    u32 query;
    u32 w[4]; /* объединение: id / type+ip_instance / dword_offset,count,instance,flags / fw_type... */
};
_Static_assert(sizeof(struct amd_info) == 32, "drm_amdgpu_info");

struct amd_heap { u64 total, usable, usage, max_alloc; };
struct amd_memory { struct amd_heap vram, cpu_vram, gtt; };
_Static_assert(sizeof(struct amd_memory) == 96, "drm_amdgpu_memory_info");

struct amd_hw_ip {
    u32 major, minor;
    u64 capabilities;
    u32 ib_start_alignment, ib_size_alignment, available_rings, ip_discovery_version;
};
_Static_assert(sizeof(struct amd_hw_ip) == 32, "drm_amdgpu_info_hw_ip");

struct amd_dev {
    u32 device_id, chip_rev, external_rev, pci_rev, family, num_shader_engines, num_shader_arrays_per_engine, gpu_counter_freq;
    u64 max_engine_clock, max_memory_clock;
    u32 cu_active_number, cu_ao_mask, cu_bitmap[4][4];
    u32 enabled_rb_pipes_mask, num_rb_pipes, num_hw_gfx_contexts, pcie_gen;
    u64 ids_flags, virtual_address_offset, virtual_address_max;
    u32 virtual_address_alignment, pte_fragment_size, gart_page_size, ce_ram_size, vram_type, vram_bit_width, vce_harvest_config,
        gc_double_offchip_lds_buf;
    u64 prim_buf_gpu_addr, pos_buf_gpu_addr, cntl_sb_buf_gpu_addr, param_buf_gpu_addr;
    u32 prim_buf_size, pos_buf_size, cntl_sb_buf_size, param_buf_size, wave_front_size, num_shader_visible_vgprs, num_cu_per_sh,
        num_tcc_blocks, gs_vgt_table_depth, gs_prim_buffer_depth, max_gs_waves_per_vgt, pcie_num_lanes, cu_ao_bitmap[4][4];
    u64 high_va_offset, high_va_max;
    u32 pa_sc_tile_steering_override;
    u64 tcc_disabled_mask, min_engine_clock, min_memory_clock;
    u32 tcp_cache_size, num_sqc_per_wgp, sqc_data_cache_size, sqc_inst_cache_size, gl1c_cache_size, gl2c_cache_size;
    u64 mall_size;
    u32 enabled_rb_pipes_mask_hi, shadow_size, shadow_alignment, csa_size, csa_alignment;
};
_Static_assert(sizeof(struct amd_dev) == 440, "drm_amdgpu_info_device");

#define INFO_ACCEL_WORKING 0x00
#define INFO_HW_IP_INFO 0x02
#define INFO_HW_IP_COUNT 0x03
#define INFO_FW_VERSION 0x0e
#define INFO_VRAM_GTT 0x14
#define INFO_READ_MMR_REG 0x15
#define INFO_DEV_INFO 0x16
#define INFO_MEMORY 0x19

static i64 info_query(u32 query, u32 w0, u32 w1, void *out, u32 size) {
    struct amd_info q;
    memset(&q, 0, sizeof q);
    q.return_pointer = (u64)out;
    q.return_size = size;
    q.query = query;
    q.w[0] = w0;
    q.w[1] = w1;
    return io(DRM_IOCTL_AMDGPU_INFO, &q);
}

static void test_sysfs(u32 *sysfs_device, u32 *sysfs_rev) {
    struct stat_ st;
    i64 dir = sys3(SYS_open, "/dev/dri", O_RDONLY | O_DIRECTORY | O_CLOEXEC, 0);
    int found = 0;
    if (dir >= 0) {
        static char buf[4096];
        for (;;) {
            i64 n = sys3(SYS_getdents64, dir, buf, sizeof buf);
            if (n <= 0) break;
            for (i64 at = 0; at < n;) {
                u16 reclen = *(u16 *)(buf + at + 16);
                if (seq(buf + at + 19, "renderD128")) found = 1;
                if (!reclen) break;
                at += reclen;
            }
        }
        sys3(SYS_close, dir, 0, 0);
    }
    check(dir >= 0 && found, "/dev/dri lists renderD128");
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
    int has_driver = 0, has_slot = 0;
    for (char *s = text; r > 0 && *s; s++) {
        if (s == text || s[-1] == '\n') {
            if (starts(s, "DRIVER=amdgpu")) has_driver = 1;
            if (starts(s, "PCI_SLOT_NAME=")) has_slot = 1;
        }
    }
    check(has_driver, "uevent has DRIVER=amdgpu");
    check(has_slot, "uevent has PCI_SLOT_NAME");
    char v[32];
    i64 vendor = read_file("/sys/dev/char/226:128/device/vendor", v, sizeof v) > 0 ? parse_hex(v) : -1;
    i64 device = read_file("/sys/dev/char/226:128/device/device", v, sizeof v) > 0 ? parse_hex(v) : -1;
    i64 rev = read_file("/sys/dev/char/226:128/device/revision", v, sizeof v) > 0 ? parse_hex(v) : -1;
    note_hex("sysfs vendor", (u64)vendor);
    note_hex("sysfs device", (u64)device);
    check_v(vendor == 0x1002, "sysfs vendor is 0x1002 (AMD)", vendor);
    check(device > 0 && rev >= 0, "sysfs device and revision parse as hex");
    *sysfs_device = (u32)device;
    *sysfs_rev = (u32)rev;
}

static void test_version(void) {
    struct drm_version v;
    memset(&v, 0, sizeof v);
    i64 r = io(DRM_IOCTL_VERSION, &v);
    check_v(r == 0 && v.name_len > 0 && v.name_len < 64, "VERSION first call gives the string lengths", r ? r : (i64)v.name_len);
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
    end();
    check_v(r == 0 && seq(name, "amdgpu"), "VERSION name is amdgpu", r);
    check_v(v.major == 3 && v.minor >= 27, "VERSION is 3.27 or newer (libdrm, RADV)", (i64)v.minor);
}

static void cap(u64 which, u64 want, const char *what) {
    struct drm_get_cap c = {which, 0xdead};
    i64 r = io(DRM_IOCTL_GET_CAP, &c);
    check_v(r == 0 && c.value == want, what, r ? r : (i64)c.value);
}

static void test_info(u32 sysfs_device, u32 sysfs_rev) {
    u32 one = 0;
    i64 r = info_query(INFO_ACCEL_WORKING, 0, 0, &one, 4);
    check_v(r == 0 && one == 1, "INFO ACCEL_WORKING is 1 (libdrm amdgpu_device_initialize)", r ? r : (i64)one);

    struct amd_dev d;
    memset(&d, 0xaa, sizeof d);
    r = info_query(INFO_DEV_INFO, 0, 0, &d, sizeof d);
    check_v(r == 0, "INFO DEV_INFO (440 bytes)", r);
    note_hex("dev device_id", d.device_id);
    note_hex("dev external_rev", d.external_rev);
    note_dec("dev family", d.family);
    note_dec("dev shader engines", d.num_shader_engines);
    note_dec("dev active CUs", d.cu_active_number);
    check_v(d.device_id == sysfs_device && d.pci_rev == sysfs_rev, "DEV_INFO device_id and pci_rev match sysfs", d.device_id);
    check_v(d.family == 130, "DEV_INFO family is AMDGPU_FAMILY_VI (130)", d.family);
    check_v(d.external_rev >= 0x50 && d.external_rev < 0x5a, "DEV_INFO external_rev is in the POLARIS10 range 0x50..0x59", d.external_rev);
    check(d.num_shader_engines > 0 && d.num_shader_arrays_per_engine > 0 && d.cu_active_number > 0, "DEV_INFO has shader engines and CUs");
    check(d.gpu_counter_freq > 0 && d.max_engine_clock > 0, "DEV_INFO has clocks");
    check(d.virtual_address_max > d.virtual_address_offset && d.virtual_address_alignment == 4096, "DEV_INFO has a VA range, 4 KiB alignment");
    check(d.wave_front_size == 64, "DEV_INFO wave_front_size is 64");
    /* ответ не длиннее return_size: короткий буфер получает начало структуры */
    u32 head[4] = {0, 0, 0, 0xdeadbeef};
    r = info_query(INFO_DEV_INFO, 0, 0, head, 12);
    check_v(r == 0 && head[0] == d.device_id && head[3] == 0xdeadbeef, "a short return_size gets only the head of DEV_INFO", r);

    u32 mmr = 0;
    r = info_query(INFO_READ_MMR_REG, 0x263e, 1, &mmr, 4);
    note_hex("mmr 0x263e (GB_ADDR_CONFIG)", mmr);
    check_v(r == 0 && mmr == 0x22011003u, "READ_MMR_REG 0x263e is GB_ADDR_CONFIG 0x22011003", r ? r : (i64)mmr);

    u32 count = 0xff;
    r = info_query(INFO_HW_IP_COUNT, 0 /* GFX */, 0, &count, 4);
    check_v(r == 0 && count == 1, "HW_IP_COUNT GFX is 1", r ? r : (i64)count);
    struct amd_hw_ip ip;
    memset(&ip, 0xaa, sizeof ip);
    r = info_query(INFO_HW_IP_INFO, 0, 0, &ip, sizeof ip);
    check_v(r == 0 && ip.major == 8 && ip.minor == 0 && ip.available_rings == 1, "HW_IP_INFO GFX is 8.0 with one ring", r);
    check(ip.ib_start_alignment > 0 && ip.ib_size_alignment > 0, "HW_IP_INFO GFX has IB alignments");

    for (int i = 0; i < 3; i++) {
        u32 fw[2] = {0, 0};
        u32 kind = i == 0 ? 0x04u : i == 1 ? 0x05u : 0x08u; /* ME, PFP, MEC */
        r = info_query(INFO_FW_VERSION, kind, 0, fw, 8);
        check_v(r == 0 && fw[0] != 0, i == 0 ? "FW_VERSION GFX_ME is not zero" : i == 1 ? "FW_VERSION GFX_PFP is not zero" : "FW_VERSION GFX_MEC is not zero", r ? r : (i64)fw[0]);
    }

    struct amd_memory m;
    memset(&m, 0, sizeof m);
    r = info_query(INFO_MEMORY, 0, 0, &m, sizeof m);
    note_dec("memory vram MiB", (i64)(m.vram.total >> 20));
    note_dec("memory visible vram MiB", (i64)(m.cpu_vram.total >> 20));
    note_dec("memory gtt MiB", (i64)(m.gtt.total >> 20));
    check_v(r == 0 && m.vram.total >= (1ull << 30) && m.gtt.total > 0, "MEMORY has VRAM and GTT", r);
    check(m.cpu_vram.total > 0 && m.cpu_vram.total <= m.vram.total, "MEMORY visible VRAM is within VRAM");
    u64 vg[3] = {0, 0, 0};
    r = info_query(INFO_VRAM_GTT, 0, 0, vg, 24);
    check_v(r == 0 && vg[0] == m.vram.total && vg[2] == m.gtt.total, "VRAM_GTT agrees with MEMORY", r);

    u64 junk = 0;
    r = info_query(0x7f, 0, 0, &junk, 8);
    check_v(r == -EINVAL, "an unknown INFO query is EINVAL", r);
}

__attribute__((used)) void start_c(u64 *sp) {
    int argc = (int)sp[0];
    char **argv = (char **)(sp + 1);
    probe = "amdprobe";
    begin("start mode ");
    put(argc > 1 ? argv[1] : "soft");
    end();

    u32 sysfs_device = 0, sysfs_rev = 0;
    test_sysfs(&sysfs_device, &sysfs_rev);
    i64 fd = open_render();
    check_v(fd >= 0, "open /dev/dri/renderD128 O_RDWR|O_CLOEXEC", fd);
    if (fd < 0) finish();
    drm = (int)fd;
    test_version();
    cap(DRM_CAP_PRIME, 3, "GET_CAP PRIME is import|export");
    cap(DRM_CAP_SYNCOBJ, 1, "GET_CAP SYNCOBJ is 1");
    cap(DRM_CAP_SYNCOBJ_TIMELINE, 1, "GET_CAP SYNCOBJ_TIMELINE is 1");
    cap(DRM_CAP_TIMESTAMP_MONOTONIC, 1, "GET_CAP TIMESTAMP_MONOTONIC is 1");
    test_info(sysfs_device, sysfs_rev);
    i64 r = sys3(SYS_close, drm, 0, 0);
    check_v(r == 0, "close the render node", r);
    say("done");
    finish();
}
