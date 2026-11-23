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
 *      MEMORY, VRAM_GTT, неизвестный запрос — EINVAL;
 *   6. буферы: GEM_CREATE, GEM_MMAP + mmap по смещению (запись/чтение), GEM_METADATA,
 *      GEM_OP, GEM_WAIT_IDLE, GEM_CLOSE; адреса: GEM_VA MAP/UNMAP/REPLACE/CLEAR и их
 *      ошибки; контексты: CTX ALLOC/QUERY_STATE2/FREE;
 *   7. CS: буфер команд (WRITE_DATA, EVENT_WRITE_EOP, вложенный INDIRECT_BUFFER, DMA_DATA
 *      заливка и копия) исполняется на программной карте, пользовательский забор,
 *      syncobj-выход, timeline-сигнал, WAIT_CS, ошибки (чужой VA, неизвестный чанк, ctx).
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

/* ---- буферы, адреса, контексты (uAPI amdgpu_drm.h) ---- */

#define DRM_IOCTL_AMDGPU_GEM_CREATE 0xC0206440u
#define DRM_IOCTL_AMDGPU_GEM_MMAP 0xC0086441u
#define DRM_IOCTL_AMDGPU_CTX 0xC0106442u
#define DRM_IOCTL_AMDGPU_GEM_METADATA 0xC1206446u
#define DRM_IOCTL_AMDGPU_GEM_WAIT_IDLE 0xC0106447u
#define DRM_IOCTL_AMDGPU_GEM_VA 0x40286448u
#define DRM_IOCTL_AMDGPU_GEM_OP 0xC0106450u
#define DRM_IOCTL_GEM_CLOSE_ 0x40086409u

struct amd_gem_create { u64 bo_size, alignment, domains, domain_flags; };
_Static_assert(sizeof(struct amd_gem_create) == 32, "drm_amdgpu_gem_create");
struct amd_gem_va { u32 handle, pad, operation, flags; u64 va_address, offset_in_bo, map_size; };
_Static_assert(sizeof(struct amd_gem_va) == 40, "drm_amdgpu_gem_va");
struct amd_ctx { u32 op, flags, ctx_id; i32 priority; };
_Static_assert(sizeof(struct amd_ctx) == 16, "drm_amdgpu_ctx");
struct amd_meta { u32 handle, op; u64 flags, tiling_info; u32 data_size_bytes, data[64]; };
_Static_assert(sizeof(struct amd_meta) == 288, "drm_amdgpu_gem_metadata");
struct amd_gem_op { u32 handle, op; u64 value; };
struct amd_wait_idle { u32 handle, flags; u64 timeout; };

static i64 gem_create(u64 size, u64 domains, u32 *handle) {
    struct amd_gem_create c = {size, 4096, domains, 0};
    i64 r = io(DRM_IOCTL_AMDGPU_GEM_CREATE, &c);
    *handle = r == 0 ? (u32)c.bo_size : 0; /* выход — handle в первых 4 байтах union */
    return r;
}
static i64 gem_va(u32 handle, u32 op, u64 va, u64 off, u64 size, u32 flags) {
    struct amd_gem_va v = {handle, 0, op, flags, va, off, size};
    return io(DRM_IOCTL_AMDGPU_GEM_VA, &v);
}
static i64 ctx_op(u32 op, u32 id, u32 *out_id) {
    struct amd_ctx c = {op, 0, id, 0};
    i64 r = io(DRM_IOCTL_AMDGPU_CTX, &c);
    if (out_id) *out_id = c.op; /* alloc.ctx_id — первые 4 байта union */
    return r;
}

enum { VA_OP_MAP = 1, VA_OP_UNMAP = 2, VA_OP_CLEAR = 3, VA_OP_REPLACE = 4 };
enum { VM_RW = (1 << 1) | (1 << 2) | (1 << 3) };

static void test_gem(void) {
    u32 bo = 0;
    i64 r = gem_create(1 << 20, 2 /* GTT */, &bo);
    check_v(r == 0 && bo != 0, "GEM_CREATE 1 MiB in GTT gives a handle", r);
    u32 junk;
    check_v(gem_create(0, 2, &junk) == -EINVAL, "GEM_CREATE of size 0 is EINVAL", 0);
    check_v(gem_create(4096, 0, &junk) == -EINVAL, "GEM_CREATE with no domain is EINVAL", 0);
    check_v(gem_create(4096, 0x8 /* GDS */, &junk) == -EINVAL, "GEM_CREATE in GDS is EINVAL (no GDS here)", 0);

    /* mmap по смещению из GEM_MMAP: запись и чтение через отображение */
    u64 map_offset = bo; /* вход: handle; выход: addr_ptr (u64) */
    r = io(DRM_IOCTL_AMDGPU_GEM_MMAP, &map_offset);
    note_hex("mmap offset", map_offset);
    check_v(r == 0 && map_offset >= 0x100000000ull, "GEM_MMAP gives a fake mmap offset (>= 0x100000000)", r);
    u32 *p = (u32 *)sys6(SYS_mmap, 0, 1 << 20, PROT_READ | PROT_WRITE, MAP_SHARED, drm, (i64)map_offset);
    check_v((i64)p > 0 || (u64)p < (u64)-4096, "mmap of the render node at the buffer offset", (i64)p);
    if ((u64)p < (u64)-4096) {
        int bad = 0;
        for (u32 i = 0; i < (1 << 20) / 4; i += 1021) p[i] = 0x5a5a0000u + i;
        for (u32 i = 0; i < (1 << 20) / 4; i += 1021)
            if (p[i] != 0x5a5a0000u + i) bad++;
        check_v(bad == 0, "writes through the mapping read back (1 MiB, stride 1021 dwords)", bad);
        check_v(p[1] == 0, "a fresh buffer is zero-filled", p[1]);
    }
    u64 bad_offset = 0x1000;
    check(sys6(SYS_mmap, 0, 4096, PROT_READ, MAP_SHARED, drm, (i64)bad_offset) < 0, "mmap at an offset that belongs to no buffer fails");
    u32 other = 0;
    gem_create(2 << 20, 2, &other);

    /* GEM_VA: MAP, пересечение, UNMAP, REPLACE, CLEAR, ошибки */
    r = gem_va(bo, VA_OP_MAP, 0x100000, 0, 1 << 20, VM_RW);
    check_v(r == 0, "GEM_VA MAP 1 MiB at 0x100000", r);
    r = gem_va(bo, VA_OP_MAP, 0x180000, 0, 4096, VM_RW);
    check_v(r == -EINVAL, "GEM_VA MAP over an existing mapping is EINVAL", r);
    r = gem_va(bo, VA_OP_MAP, 0x1000, 0, 4096, VM_RW);
    check_v(r == -EINVAL, "GEM_VA MAP below the reserved bottom (64 KiB) is EINVAL", r);
    r = gem_va(bo, VA_OP_MAP, 0x300001, 0, 4096, VM_RW);
    check_v(r == -EINVAL, "GEM_VA MAP at an unaligned address is EINVAL", r);
    r = gem_va(bo, VA_OP_MAP, 0x400000, 1 << 20, 4096, VM_RW);
    check_v(r == -EINVAL, "GEM_VA MAP beyond the end of the buffer is EINVAL", r);
    r = gem_va(0xdead, VA_OP_MAP, 0x400000, 0, 4096, VM_RW);
    check_v(r == -ENOENT, "GEM_VA MAP of an unknown handle is ENOENT", r);
    r = gem_va(bo, VA_OP_MAP, 0x500000, 0, 4096, 0x8000);
    check_v(r == -EINVAL, "GEM_VA MAP with unknown flags is EINVAL", r);
    r = gem_va(bo, VA_OP_UNMAP, 0x100000, 0, 1 << 20, 0);
    check_v(r == 0, "GEM_VA UNMAP of the mapping", r);
    r = gem_va(bo, VA_OP_UNMAP, 0x100000, 0, 1 << 20, 0);
    check_v(r == -EINVAL, "a second GEM_VA UNMAP is EINVAL", r);
    gem_va(bo, VA_OP_MAP, 0x200000, 0, 8192, VM_RW);
    r = gem_va(other, VA_OP_REPLACE, 0x200000, 0, 8192, VM_RW);
    check_v(r == 0, "GEM_VA REPLACE over an existing mapping", r);
    r = gem_va(other, VA_OP_MAP, 0x200000, 0, 4096, VM_RW);
    check_v(r == -EINVAL, "after REPLACE the range is taken (MAP is EINVAL)", r);
    r = gem_va(0, VA_OP_CLEAR, 0x200000, 0, 8192, 0);
    check_v(r == 0, "GEM_VA CLEAR of the range", r);
    r = gem_va(other, VA_OP_MAP, 0x200000, 0, 8192, VM_RW);
    check_v(r == 0, "after CLEAR the range is free again", r);

    /* метаданные, GEM_OP, WAIT_IDLE */
    struct amd_meta m;
    memset(&m, 0, sizeof m);
    m.handle = bo;
    m.op = 1; /* SET */
    m.flags = 7;
    m.tiling_info = 0x1234abcdull;
    m.data_size_bytes = 8;
    m.data[0] = 0xdeadbeef;
    m.data[1] = 0xcafef00d;
    r = io(DRM_IOCTL_AMDGPU_GEM_METADATA, &m);
    check_v(r == 0, "GEM_METADATA SET", r);
    struct amd_meta g;
    memset(&g, 0, sizeof g);
    g.handle = bo;
    g.op = 2; /* GET */
    r = io(DRM_IOCTL_AMDGPU_GEM_METADATA, &g);
    check_v(r == 0 && g.flags == 7 && g.tiling_info == 0x1234abcdull && g.data_size_bytes == 8 && g.data[0] == 0xdeadbeef && g.data[1] == 0xcafef00d,
            "GEM_METADATA GET returns what SET stored", r);
    m.data_size_bytes = 300;
    r = io(DRM_IOCTL_AMDGPU_GEM_METADATA, &m);
    check_v(r == -EINVAL, "GEM_METADATA SET with more than 256 bytes is EINVAL", r);

    struct amd_gem_create info;
    memset(&info, 0, sizeof info);
    struct amd_gem_op op = {bo, 0 /* GET_GEM_CREATE_INFO */, (u64)&info};
    r = io(DRM_IOCTL_AMDGPU_GEM_OP, &op);
    check_v(r == 0 && info.bo_size == (1 << 20) && (info.domains & 2), "GEM_OP GET_GEM_CREATE_INFO returns size and domains", r);
    struct amd_wait_idle w = {bo, 0, 1000};
    r = io(DRM_IOCTL_AMDGPU_GEM_WAIT_IDLE, &w);
    check_v(r == 0, "GEM_WAIT_IDLE of an idle buffer is 0 (status 0)", r);

    /* контексты */
    u32 ctx = 0;
    r = ctx_op(1 /* ALLOC */, 0, &ctx);
    check_v(r == 0 && ctx != 0, "CTX ALLOC gives an id", r);
    r = ctx_op(4 /* QUERY_STATE2 */, ctx, 0);
    check_v(r == 0, "CTX QUERY_STATE2 of a live context is 0", r);
    r = ctx_op(2 /* FREE */, ctx, 0);
    check_v(r == 0, "CTX FREE", r);
    r = ctx_op(2 /* FREE */, ctx, 0);
    check_v(r == -EINVAL, "a second CTX FREE is EINVAL", r);
    r = ctx_op(4, ctx, 0);
    check_v(r == -EINVAL, "CTX QUERY_STATE2 of a freed context is EINVAL", r);

    /* закрытие: ручка уходит, отображение в память программы живёт */
    r = io(DRM_IOCTL_GEM_CLOSE_, &bo);
    check_v(r == 0, "GEM_CLOSE", r);
    r = io(DRM_IOCTL_GEM_CLOSE_, &bo);
    check_v(r == -EINVAL, "a second GEM_CLOSE is EINVAL", r);
    if ((u64)p < (u64)-4096) check_v(p[0] == 0x5a5a0000u, "the mapping outlives GEM_CLOSE", p[0]);
    r = io(DRM_IOCTL_AMDGPU_GEM_MMAP, &map_offset);
    check_v(r == -ENOENT, "GEM_MMAP of a closed handle is ENOENT", r);
}

/* ---- CS ---- */

#define DRM_IOCTL_AMDGPU_CS 0xC0186444u
#define DRM_IOCTL_AMDGPU_WAIT_CS 0xC0206449u
#define DRM_IOCTL_SYNCOBJ_TIMELINE_SIGNAL_ 0xC01864CDu

struct amd_cs { u32 ctx_id, bo_list_handle, num_chunks, flags; u64 chunks; };
_Static_assert(sizeof(struct amd_cs) == 24, "drm_amdgpu_cs");
struct amd_chunk { u32 id, length_dw; u64 data; };
_Static_assert(sizeof(struct amd_chunk) == 16, "drm_amdgpu_cs_chunk");
struct amd_chunk_ib { u32 pad, flags; u64 va_start; u32 ib_bytes, ip_type, ip_instance, ring; };
_Static_assert(sizeof(struct amd_chunk_ib) == 32, "drm_amdgpu_cs_chunk_ib");
struct amd_chunk_fence { u32 handle, offset; };
struct amd_chunk_tl { u32 handle, flags; u64 point; };
struct amd_wait_cs { u64 handle, timeout; u32 ip_type, ip_instance, ring, ctx_id; };
_Static_assert(sizeof(struct amd_wait_cs) == 32, "drm_amdgpu_wait_cs");

enum { CHUNK_IB = 1, CHUNK_FENCE = 2, CHUNK_SYNCOBJ_IN = 4, CHUNK_SYNCOBJ_OUT = 5, CHUNK_BO_HANDLES = 6, CHUNK_TL_SIGNAL = 9 };

/* Подать CS: `ib_va`/`ib_bytes` — буфер команд; необязательно fence (BO, смещение), syncobj-выход, timeline-сигнал. */
static i64 submit(u32 ctx, u64 ib_va, u32 ib_bytes, u32 fence_bo, u32 fence_off, u32 out_syncobj, u32 tl_handle, u64 tl_point, u64 *seq) {
    struct amd_chunk_ib ib = {0, 0, ib_va, ib_bytes, 0, 0, 0};
    struct amd_chunk_fence fence = {fence_bo, fence_off};
    u32 out = out_syncobj;
    struct amd_chunk_tl tl = {tl_handle, 0, tl_point};
    struct amd_chunk chunks[4];
    u64 pointers[4];
    u32 n = 0;
    chunks[n] = (struct amd_chunk){CHUNK_IB, 8, (u64)&ib};
    n++;
    if (fence_bo) {
        chunks[n] = (struct amd_chunk){CHUNK_FENCE, 2, (u64)&fence};
        n++;
    }
    if (out_syncobj) {
        chunks[n] = (struct amd_chunk){CHUNK_SYNCOBJ_OUT, 1, (u64)&out};
        n++;
    }
    if (tl_handle) {
        chunks[n] = (struct amd_chunk){CHUNK_TL_SIGNAL, 4, (u64)&tl};
        n++;
    }
    for (u32 i = 0; i < n; i++) pointers[i] = (u64)&chunks[i];
    struct amd_cs cs = {ctx, 0, n, 0, (u64)pointers};
    i64 r = io(DRM_IOCTL_AMDGPU_CS, &cs);
    if (seq) *seq = r == 0 ? *(u64 *)&cs : 0;
    return r;
}
static i64 submit_bad_chunk(u32 ctx, u32 id) {
    struct amd_chunk c = {id, 0, 0};
    u64 pointer = (u64)&c;
    struct amd_cs cs = {ctx, 0, 1, 0, (u64)&pointer};
    return io(DRM_IOCTL_AMDGPU_CS, &cs);
}
static i64 wait_cs(u32 ctx, u64 seq, u64 *status) {
    struct amd_wait_cs w = {seq, 1000000000ull, 0, 0, 0, ctx};
    i64 r = io(DRM_IOCTL_AMDGPU_WAIT_CS, &w);
    *status = *(u64 *)&w; /* out.status — первые 8 байт union */
    return r;
}

/* PRIME: так Mesa отдаёт изображения WSI (dma-buf в Wayland) и принимает чужие. */
static void test_prime(void) {
    u32 bo = 0;
    i64 r = gem_create(1 << 16, 2 /* GTT */, &bo);
    check_v(r == 0 && bo != 0, "PRIME: GEM_CREATE of the buffer to share", r);
    u64 map_offset = bo;
    io(DRM_IOCTL_AMDGPU_GEM_MMAP, &map_offset);
    u32 *p = (u32 *)sys6(SYS_mmap, 0, 1 << 16, PROT_READ | PROT_WRITE, MAP_SHARED, drm, (i64)map_offset);
    if ((u64)p < (u64)-4096) p[7] = 0xa5a5c3c3u;

    struct drm_prime_handle ph = {bo, DRM_CLOEXEC | O_RDWR, -1};
    r = io(DRM_IOCTL_PRIME_HANDLE_TO_FD, &ph);
    check_v(r == 0 && ph.fd >= 0, "PRIME_HANDLE_TO_FD gives a dma-buf fd", r);
    struct drm_prime_handle bad = {bo, 0x7000, -1};
    r = io(DRM_IOCTL_PRIME_HANDLE_TO_FD, &bad);
    check_v(r == -EINVAL, "PRIME_HANDLE_TO_FD with unknown flags is EINVAL", r);
    struct drm_prime_handle none = {0xdead, DRM_CLOEXEC, -1};
    r = io(DRM_IOCTL_PRIME_HANDLE_TO_FD, &none);
    check_v(r == -ENOENT, "PRIME_HANDLE_TO_FD of an unknown handle is ENOENT", r);
    if (ph.fd < 0) return;

    /* mmap самого dma-buf: те же страницы, что у узла */
    u32 *q = (u32 *)sys6(SYS_mmap, 0, 1 << 16, PROT_READ | PROT_WRITE, MAP_SHARED, ph.fd, 0);
    check_v((u64)q < (u64)-4096, "mmap of the dma-buf fd", (i64)q);
    if ((u64)q < (u64)-4096) {
        check_v(q[7] == 0xa5a5c3c3u, "the dma-buf shows what was written through the node mapping", q[7]);
        q[9] = 0x1badf00du;
        if ((u64)p < (u64)-4096) check_v(p[9] == 0x1badf00du, "a write through the dma-buf shows in the node mapping", p[9]);
    }
    check((i64)sys6(SYS_mmap, 0, 1 << 20, PROT_READ, MAP_SHARED, ph.fd, 0) < 0, "mmap past the end of the dma-buf is refused");

    /* импорт в тот же файл: та же ручка; в другой файл узла: новая ручка и те же данные */
    struct drm_prime_handle back = {0, 0, ph.fd};
    r = io(DRM_IOCTL_PRIME_FD_TO_HANDLE, &back);
    check_v(r == 0 && back.handle == bo, "PRIME_FD_TO_HANDLE in the same file gives the same handle", r);
    struct drm_prime_handle wrong = {0, 0, drm};
    r = io(DRM_IOCTL_PRIME_FD_TO_HANDLE, &wrong);
    check_v(r == -EINVAL, "PRIME_FD_TO_HANDLE of a render node fd (not a dma-buf) is EINVAL", r);
    struct drm_prime_handle badfd = {0, 0, 999};
    r = io(DRM_IOCTL_PRIME_FD_TO_HANDLE, &badfd);
    check_v(r == -EBADF, "PRIME_FD_TO_HANDLE of a closed fd is EBADF", r);

    i64 second = open_render();
    if (second >= 0) {
        int first = drm;
        drm = (int)second;
        struct drm_prime_handle other = {0, 0, ph.fd};
        r = io(DRM_IOCTL_PRIME_FD_TO_HANDLE, &other);
        check_v(r == 0 && other.handle != 0, "PRIME_FD_TO_HANDLE in a second file of the node", r);
        if (r == 0) {
            u64 off2 = other.handle;
            io(DRM_IOCTL_AMDGPU_GEM_MMAP, &off2);
            u32 *s = (u32 *)sys6(SYS_mmap, 0, 1 << 16, PROT_READ | PROT_WRITE, MAP_SHARED, drm, (i64)off2);
            if ((u64)s < (u64)-4096) check_v(s[7] == 0xa5a5c3c3u, "the importing file sees the same memory", s[7]);
            io(DRM_IOCTL_GEM_CLOSE_, &other.handle);
        }
        drm = first;
        sys3(SYS_close, second, 0, 0);
    }
    sys3(SYS_close, ph.fd, 0, 0);
    /* буфер живёт, пока его держит хоть что-то: ручка и отображения остались */
    struct drm_prime_handle again = {bo, DRM_CLOEXEC | O_RDWR, -1};
    r = io(DRM_IOCTL_PRIME_HANDLE_TO_FD, &again);
    check_v(r == 0 && again.fd >= 0, "after closing the dma-buf the handle still exports", r);
    if (again.fd >= 0) sys3(SYS_close, again.fd, 0, 0);
    io(DRM_IOCTL_GEM_CLOSE_, &bo);
}

/* sync_file: забор syncobj наружу и слияние двух (SYNC_IOC_MERGE), как у Mesa WSI и libdrm. */
struct amd_syncobj_handle { u32 handle, flags; i32 fd; u32 pad; };
struct amd_pollfd { i32 fd; u16 events, revents; };
struct amd_sync_merge { char name[32]; i32 fd2, fence; u32 flags, pad; };
_Static_assert(sizeof(struct amd_syncobj_handle) == 16, "drm_syncobj_handle");
_Static_assert(sizeof(struct amd_sync_merge) == 48, "sync_merge_data");
static int sync_ready(int fd) {
    struct amd_pollfd p = {fd, 1 /* POLLIN */, 0};
    sys3(7 /* poll */, (u64)&p, 1, 0);
    return (p.revents & 1) != 0;
}
static void test_syncfile(void) {
    u32 a = 0, b = 0, empty = 0;
    i64 r = syncobj_create(SYNCOBJ_CREATE_SIGNALED, &a);
    check_v(r == 0, "sync_file: a signaled syncobj", r);
    r = syncobj_create(SYNCOBJ_CREATE_SIGNALED, &b);
    check_v(r == 0, "sync_file: a second signaled syncobj", r);
    syncobj_create(0, &empty);
    /* Linux: у syncobj без забора экспортировать нечего (drm_syncobj_export_sync_file) */
    struct amd_syncobj_handle none = {empty, 1 /* EXPORT_SYNC_FILE */, -1, 0};
    r = io(0xC01064C1u /* SYNCOBJ_HANDLE_TO_FD */, &none);
    check_v(r == -EINVAL, "sync_file: a syncobj without a fence cannot be exported (EINVAL)", r);
    struct amd_syncobj_handle ea = {a, 1, -1, 0};
    struct amd_syncobj_handle eb = {b, 1, -1, 0};
    i64 ra = io(0xC01064C1u, &ea);
    i64 rb = io(0xC01064C1u, &eb);
    check_v(ra == 0 && rb == 0 && ea.fd >= 0 && eb.fd >= 0, "sync_file: SYNCOBJ_HANDLE_TO_FD exports a sync_file", ra ? ra : rb);
    if (ra != 0 || rb != 0) return;
    check_v(sync_ready(ea.fd) && sync_ready(eb.fd), "sync_file: poll shows the exported fences signaled", 0);
    struct amd_sync_merge m = {"amdprobe", eb.fd, -1, 0, 0};
    r = sys3(SYS_ioctl, ea.fd, 0xC0303E03u /* SYNC_IOC_MERGE */, &m);
    check_v(r == 0 && m.fence >= 0 && m.fence != ea.fd && m.fence != eb.fd, "sync_file: SYNC_IOC_MERGE gives a new sync_file", r);
    if (r == 0) {
        check_v(sync_ready(m.fence), "sync_file: the merge of two signaled is signaled", 0);
        sys3(SYS_close, m.fence, 0, 0);
    }
    struct amd_sync_merge bad = {"amdprobe", 999, -1, 0, 0};
    r = sys3(SYS_ioctl, ea.fd, 0xC0303E03u, &bad);
    check_v(r == -EBADF, "sync_file: SYNC_IOC_MERGE with a closed fd2 is EBADF", r);
    struct amd_sync_merge flagged = {"amdprobe", eb.fd, -1, 1, 0};
    r = sys3(SYS_ioctl, ea.fd, 0xC0303E03u, &flagged);
    check_v(r == -EINVAL, "sync_file: SYNC_IOC_MERGE with flags is EINVAL", r);
    struct amd_sync_merge notsync = {"amdprobe", drm, -1, 0, 0};
    r = sys3(SYS_ioctl, ea.fd, 0xC0303E03u, &notsync);
    check_v(r == -EINVAL, "sync_file: SYNC_IOC_MERGE with a render node as fd2 is EINVAL", r);
    sys3(SYS_close, ea.fd, 0, 0);
    sys3(SYS_close, eb.fd, 0, 0);
}

static void test_cs(void) {
    u32 ib_bo = 0, data_bo = 0, ctx = 0, so = 0, tl = 0;
    gem_create(1 << 16, 2, &ib_bo);
    gem_create(1 << 16, 2, &data_bo);
    u64 ib_off = ib_bo, data_off = data_bo;
    io(DRM_IOCTL_AMDGPU_GEM_MMAP, &ib_off);
    io(DRM_IOCTL_AMDGPU_GEM_MMAP, &data_off);
    u32 *ib = (u32 *)sys6(SYS_mmap, 0, 1 << 16, PROT_READ | PROT_WRITE, MAP_SHARED, drm, (i64)ib_off);
    u32 *data = (u32 *)sys6(SYS_mmap, 0, 1 << 16, PROT_READ | PROT_WRITE, MAP_SHARED, drm, (i64)data_off);
    check((u64)ib < (u64)-4096 && (u64)data < (u64)-4096, "CS: both buffers mapped into the program");
    if ((u64)ib >= (u64)-4096 || (u64)data >= (u64)-4096) return;
    /* Далеко от адресов test_gem (0x100000..0x400000): их отображения остаются. */
    const u64 IB_VA = 0x4000000, DATA_VA = 0x5000000;
    check_v(gem_va(ib_bo, VA_OP_MAP, IB_VA, 0, 1 << 16, VM_RW) == 0 && gem_va(data_bo, VA_OP_MAP, DATA_VA, 0, 1 << 16, VM_RW) == 0, "CS: GEM_VA MAP of the IB and data buffers", 0);
    check_v(ctx_op(1, 0, &ctx) == 0 && ctx != 0, "CS: CTX ALLOC", ctx);
    check_v(syncobj_create(0, &so) == 0, "CS: SYNCOBJ_CREATE (not signaled)", 0);

    /* вложенный буфер (на 0x100800): WRITE_DATA 0xbeef по DATA_VA + 0x30 */
    u32 *nested = ib + 0x800 / 4;
    nested[0] = 0xC0033700u; nested[1] = 0x00100500u; nested[2] = (u32)(DATA_VA + 0x30); nested[3] = 0; nested[4] = 0xbeef;

    u32 n = 0;
    /* WRITE_DATA одно слово */
    ib[n++] = 0xC0033700u; ib[n++] = 0x00100500u; ib[n++] = (u32)DATA_VA; ib[n++] = 0; ib[n++] = 0xa5a5a5a5u;
    /* WRITE_DATA три слова подряд */
    ib[n++] = 0xC0053700u; ib[n++] = 0x00100500u; ib[n++] = (u32)(DATA_VA + 0x10); ib[n++] = 0; ib[n++] = 1; ib[n++] = 2; ib[n++] = 3;
    /* EVENT_WRITE_EOP: 32 бита 0x1234 (DATA_SEL 1) */
    ib[n++] = 0xC0044700u; ib[n++] = 0x38514u; ib[n++] = (u32)(DATA_VA + 0x20); ib[n++] = 1u << 29; ib[n++] = 0x1234; ib[n++] = 0;
    /* вложенный INDIRECT_BUFFER, 5 слов */
    ib[n++] = 0xC0023F00u; ib[n++] = (u32)(IB_VA + 0x800); ib[n++] = 0; ib[n++] = 5;
    /* DMA_DATA: залить 16 байт значением 0x77777777 по DATA_VA + 0x40 */
    ib[n++] = 0xC0055000u; ib[n++] = 2u << 29; ib[n++] = 0x77777777u; ib[n++] = 0; ib[n++] = (u32)(DATA_VA + 0x40); ib[n++] = 0; ib[n++] = 16;
    /* DMA_DATA: скопировать эти 16 байт на DATA_VA + 0x50 */
    ib[n++] = 0xC0055000u; ib[n++] = 0; ib[n++] = (u32)(DATA_VA + 0x40); ib[n++] = 0; ib[n++] = (u32)(DATA_VA + 0x50); ib[n++] = 0; ib[n++] = 16;
    /* пропускаемые пакеты: SET_SH_REG и тип 2 */
    ib[n++] = 0xC0017600u; ib[n++] = 0x20c; ib[n++] = 0; ib[n++] = 0x80000000u;

    u64 seq = 0;
    i64 r = submit(ctx, IB_VA, n * 4, data_bo, 0x8000, so, 0, 0, &seq);
    check_v(r == 0 && seq == 1, "CS: first submission gets sequence number 1", r ? r : (i64)seq);
    check_v(data[0] == 0xa5a5a5a5u, "CS: WRITE_DATA wrote one dword at the mapped VA", data[0]);
    check_v(data[4] == 1 && data[5] == 2 && data[6] == 3, "CS: WRITE_DATA wrote three consecutive dwords", data[4]);
    check_v(data[8] == 0x1234, "CS: EVENT_WRITE_EOP wrote the 32-bit fence value", data[8]);
    check_v(data[12] == 0xbeef, "CS: a nested INDIRECT_BUFFER ran", data[12]);
    check_v(data[16] == 0x77777777u && data[19] == 0x77777777u, "CS: DMA_DATA fill wrote exactly 16 bytes", data[16]);
    check_v(data[20] == 0x77777777u && data[23] == 0x77777777u && data[24] == 0, "CS: DMA_DATA copy wrote exactly 16 bytes", data[20]);
    u64 status = 9;
    r = wait_cs(ctx, seq, &status);
    check_v(r == 0 && status == 0, "CS: WAIT_CS of the first submission says done (status 0)", r ? r : (i64)status);
    check_v(data[0x8000 / 4] == 1, "CS: the user fence chunk stored the sequence number in the buffer", data[0x8000 / 4]);
    check_v(syncobj_wait(so, 0, 100) == 0, "CS: the SYNCOBJ_OUT syncobj is signaled after the submission", 0);

    /* второй CS: номер растёт, ожидание будущего номера — ошибка */
    r = submit(ctx, IB_VA, n * 4, 0, 0, 0, 0, 0, &seq);
    check_v(r == 0 && seq == 2, "CS: the second submission gets number 2", r ? r : (i64)seq);
    r = wait_cs(ctx, 3, &status);
    check_v(r == -EINVAL, "CS: WAIT_CS of a sequence number not submitted yet is EINVAL", r);

    /* timeline: сигнал точки 5 через чанк */
    check_v(syncobj_create(0, &tl) == 0, "CS: SYNCOBJ_CREATE for the timeline", 0);
    r = submit(ctx, IB_VA, n * 4, 0, 0, 0, tl, 5, &seq);
    u64 point = 0;
    i64 q = timeline_query(tl, &point);
    check_v(r == 0 && q == 0 && point == 5, "CS: SYNCOBJ_TIMELINE_SIGNAL chunk puts point 5 on the timeline", r ? r : q ? q : (i64)point);

    /* ошибки */
    r = submit(ctx, 0x900000, 16, 0, 0, 0, 0, 0, 0);
    check_v(r == -EINVAL, "CS: an IB at an unmapped VA is EINVAL", r);
    r = submit(ctx + 100, IB_VA, n * 4, 0, 0, 0, 0, 0, 0);
    check_v(r == -EINVAL, "CS: an unknown context is EINVAL", r);
    r = submit_bad_chunk(ctx, 0x77);
    check_v(r == -EINVAL, "CS: an unknown chunk id is EINVAL", r);
    u32 fresh = 0;
    syncobj_create(0, &fresh);
    struct amd_chunk_ib ibc = {0, 0, IB_VA, 16, 0, 0, 0};
    u32 wait_handle = fresh;
    struct amd_chunk cc[2] = {{CHUNK_IB, 8, (u64)&ibc}, {CHUNK_SYNCOBJ_IN, 1, (u64)&wait_handle}};
    u64 cps[2] = {(u64)&cc[0], (u64)&cc[1]};
    struct amd_cs bad = {ctx, 0, 2, 0, (u64)cps};
    r = io(DRM_IOCTL_AMDGPU_CS, &bad);
    check_v(r == -EINVAL, "CS: waiting on a syncobj that has no fence is EINVAL", r);

    /* шлюз контекста: после FREE подать нельзя */
    ctx_op(2, ctx, 0);
    r = submit(ctx, IB_VA, n * 4, 0, 0, 0, 0, 0, 0);
    check_v(r == -EINVAL, "CS: a submission to a freed context is EINVAL", r);
    io(DRM_IOCTL_GEM_CLOSE_, &ib_bo);
    io(DRM_IOCTL_GEM_CLOSE_, &data_bo);
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
    test_gem();
    test_prime();
    test_syncfile();
    test_cs();
    i64 r = sys3(SYS_close, drm, 0, 0);
    check_v(r == 0, "close the render node", r);
    say("done");
    finish();
}
