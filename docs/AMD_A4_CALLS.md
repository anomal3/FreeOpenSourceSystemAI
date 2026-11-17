# AMD A4 / RDNA2 iGPU (GC 10.3.6): что делают libdrm, RADV и loader с ядром

Источники (открытые):
- Mesa 25.0.7 в Debian 13: `https://sources.debian.org/data/main/m/mesa/25.0.7-2+deb13u1/` (далее `mesa/`).
- libdrm 2.4.124-2 (Debian 13): `https://sources.debian.org/data/main/libd/libdrm/2.4.124-2/` (далее `libdrm/`).
- vulkan-loader 1.4.309.0-1 (Debian 13).
- Ядро Linux v6.12: `include/uapi/drm/{amdgpu_drm.h,drm.h}`, `include/uapi/linux/dma-buf.h`.

Обозначения: [V] проверено по тексту источника; [I] вывод/рекомендация (не из источника);
[?] не нашёл / не проверено.

Общее правило: любая цель `ioctl` идёт через `drmIoctl()`. Он повторяет вызов в цикле,
пока `errno` равен `EINTR` или `EAGAIN` [V, libdrm/xf86drm.c]. Ядро не должно
возвращать `EAGAIN` "просто так": будет бесконечный цикл.

---

## 1. Обнаружение устройства

### 1.1 Кто и как перечисляет

| Шаг | Где | Что делает |
|---|---|---|
| Loader | `loader/loader_linux.c` | DRM не трогает: нет `/dev/dri`, `/sys`, `opendir` [V]. Читает только `VK_LOADER_DEVICE_SELECT`. Поиск ICD-json (`icd.d`) в `loader.c` не разбирал [?]. |
| Instance | `mesa/src/vulkan/runtime/vk_instance.c`, `enumerate_drm_physical_devices_locked` | `drmGetDevices2(0, devices, 256)`: флаги 0, массив 256 [V]. Каждое устройство идёт в `try_create_for_drm`. `VK_ERROR_INCOMPATIBLE_DRIVER` значит "пропустить" [V]. |
| RADV | `radv_physical_device.c`, `create_drm_physical_device` | Требует `available_nodes & (1<<DRM_NODE_RENDER)`, `bustype == DRM_BUS_PCI`, `vendor_id == 0x1002` (ATI), иначе `INCOMPATIBLE_DRIVER` [V]. |
| RADV | `radv_physical_device_try_create` | `open(drm_device->nodes[DRM_NODE_RENDER], O_RDWR\|O_CLOEXEC)`. Затем `drmGetVersion(fd)`: имя должно быть `"amdgpu"` [V]. |
| RADV | то же | `stat()` по node-путям даёт `primary_devid`/`render_devid` из `st_rdev`. Ошибка stat даёт `VK_ERROR_INITIALIZATION_FAILED` [V]. Защищён ли stat primary проверкой `available_nodes`, не видно [?]. |
| RADV | то же | `pdev->bus_info = *drm_device->businfo.pci` (domain/bus/dev/func) [V]. `pci_id` берётся из `deviceinfo` [V]. |
| RADV | то же | Primary-нода открывается только при включённом `VK_KHR_display`: `open(nodes[PRIMARY])`, затем `AMDGPU_INFO_ACCEL_WORKING`. При неудаче `master_fd=-1`, без ошибки [V]. |
| ac | `ac_gpu_info.c`, `ac_query_pci_bus_info` | `drmGetDevice2(fd, 0, ...)`: `fstat`, `S_ISCHR`, полный повторный скан `/dev/dri`. При ошибке `pci.valid=false`, не фатально, т.к. RADV передаёт `require_pci_bus_info=true` [V]. |
| WSI | `wsi_common_drm.c`, `wsi_device_matches_drm_fd` | `drmGetDevice2` + `drmDevicesEqual`, сравнение PCI bus info [V]. |

### 1.2 Что читает `drmGetDevices2`/`drmGetDevice2` (libdrm/xf86drm.c) [V, кроме отмеченного]

1. `opendir("/dev/dri")` (`DRM_DIR_NAME`), `readdir`. Имя узла должно начинаться с `renderD` или `card`,
   иначе `drmGetNodeType` вернёт `-EINVAL`, а `controlD*` отвергается. Максимум 256 узлов.
2. Для каждого узла `stat("/dev/dri/<name>")`.
   - Нужен `S_ISCHR`.
   - `major/minor` берутся из `st_rdev`.
   - Проверка "major == 226" в Linux-ветке **не используется**: она только в fallback для других ОС.
   - Узел признаётся DRM, если существует каталог `/sys/dev/char/<maj>:<min>/device/drm`
     (`drmNodeIsDRM`, `stat`).
3. Тип шины: `readlink("/sys/dev/char/226:128/device/subsystem")`. Basename должен давать `/pci`
   (также `/usb /platform /spi /host1x /virtio`).
4. PCI slot (`drmParsePciBusInfo`):
   - `realpath("/sys/dev/char/226:128/device")`, срезается хвостовой `/virtio*`.
   - Читается `<pci_path>/uevent`, строка `PCI_SLOT_NAME=0000:BB:DD.F`, формат `%04x:%02x:%02x.%1u`.
   - Нет ключа даёт `-ENOENT`, битый формат `-EINVAL`.
5. PCI device info (`drmParsePciDeviceInfo`), при вызове с `flags=0` (случай Mesa/instance):
   `parse_separate_sysfs_files(skip_revision=true)`, `fscanf("%x")` из
   `<pci_path>/{vendor,device,subsystem_vendor,subsystem_device}`. Формат `0x1002\n` годится
   (`%x` принимает префикс `0x`). Значения маскируются до 16 бит.
6. `revision` и бинарный `config` читаются только при `flags & DRM_DEVICE_GET_PCI_REVISION`
   (это `drmGetDevice()`): сначала `parse_separate_sysfs_files(false)` (там же `revision`),
   при неудаче fallback в `config`: `read()` 64 байт, little-endian: vendor@0, device@2,
   revision@8, subsys_vendor@44, subsys_device@46.
   Значит `config` понадобится, только если файл `revision` отсутствует и у кого-то
   вызывается `drmGetDevice()`.
   В ходе vulkaninfo такой вызов не нашёл: у Mesa везде `flags=0` [V для vk_instance и
   `ac_query_pci_bus_info`, остальное [?]].
7. `drmFoldDuplicatedDevices`: узлы с одинаковой PCI-шиной (`drmDevicesEqual`, `memcmp` businfo) сливаются
   в один `drmDevice`, `available_nodes |= 1<<type`. Если есть и `card0`, и `renderD128`, их
   `PCI_SLOT_NAME` должен совпадать.
8. `drmGetDeviceNameFromFd2` (Linux): `fstat`, `S_ISCHR`, затем читается `DEVNAME` из
   `/sys/dev/char/<maj>:<min>` (по выжимке; конкретно файл `uevent` в этом каталоге [I]), результат `/dev/<DEVNAME>`.
9. `drmGetMinorNameForFD`: `fstat(fd)`, затем `opendir("/sys/dev/char/226:128/device/drm")`; первое имя с
   префиксом `renderD` становится `/dev/dri/renderD128`. Через неё работают
   `drmGetPrimaryDeviceNameFromFd`, `drmGetRenderDeviceNameFromFd`, `drmGetNodeTypeFromFd`
   (последний плюс `access(F_OK)` на `/dev/dri/<name>`).
   Пример: `amdgpu_get_auth` вызывает `drmGetNodeTypeFromFd` и ожидает `DRM_NODE_RENDER`.

### 1.3 Итоговая минимальная "картина устройства"

| Объект | Нужно |
|---|---|
| `/dev/dri/renderD128` | символьное, `st_rdev = makedev(226,128)`, `st_mode & S_IFMT == S_IFCHR`, доступ `O_RDWR` |
| `/dev/dri/card0` | [I] рекомендую иметь (226:0): нужен только при `VK_KHR_display`; без него `nodes[PRIMARY]=NULL`. Что делает RADV при `open(NULL)`, не проверял [?] |
| `/sys/dev/char/226:128/device/` | путь к PCI-каталогу (symlink допустим: берётся `realpath`) |
| `.../device/subsystem` | symlink, basename `pci` |
| `.../device/drm/renderD128` | каталог (его перечисляет `readdir`) |
| `.../device/uevent` | `PCI_SLOT_NAME=0000:00:08.1` (любой валидный, один на все узлы) |
| `.../device/vendor` | `0x1002` |
| `.../device/device` | PCI device ID GC 10.3.6 iGPU (см. п. 5, значение не нашёл) |
| `.../device/subsystem_vendor`, `subsystem_device` | любые hex, например `0x0000` |
| `.../device/revision` | hex, 1 байт. Нужен для `pci_rev` и для ссылки на `amdgpu.ids` |
| `.../device/config` | 64+ байта PCI config, на случай fallback (offsets выше) |
| `/sys/dev/char/226:128/uevent` | `DEVNAME=dri/renderD128` [I для имени файла] |

Не нашёл в libdrm 2.4.124 / mesa 25.0.7:
- `/sys/class/drm/*` (используется только `/sys/dev/char`);
- `boot_vga`;
- чтение `/sys/bus/pci/devices/*`;
- `DRM_IOCTL_GET_UNIQUE`: функция `drmGetBusid` есть в libdrm, но ни в winsys, ни в `ac_gpu_info.c`, ни в RADV её вызова не видел [V для просмотренных файлов].

### 1.4 Ещё файлы вне ядра, затрагиваемые при init [V, если не отмечено]

- `readlink("/proc/self/fd/<fd>")` в `ac_query_gpu_info` (заполняет `dev_filename`, результат игнорируется).
- libdrm `amdgpu_parse_asic_ids` открывает таблицу ASIC IDs (`AMDGPU_ASIC_ID_TABLE`, значение макроса в
  выжимке не видно [?], переопределяется переменной `AMDGPU_ASIC_ID_TABLE_PATH`).
  Формат: `devid_hex, revid_hex, Marketing Name`. Если файла нет, печатается в stderr и init идёт дальше
  с именем `"AMD Radeon Graphics"` (`amdgpu_get_marketing_name`).
- `disk_cache_create` (кэш шейдеров в `~/.cache`/XDG) — файловые операции, без ioctl [V по наличию вызова, пути [?]].

---

## 2. Последовательность вызовов при `vulkaninfo`

Параметры подсчитаны как `_IOC(dir, 'd', nr, size)`; `DRM_COMMAND_BASE=0x40`.

### 2.1 Таблица ioctl (в порядке выполнения)

Порядок внутри `ac_query_gpu_info` взят из выжимки по тексту функции; точный порядок строк 8–19 мог отличаться [?].

| # | ioctl (значение) | struct (размер) | Откуда | Что заполнить / что вернуть |
|---|---|---|---|---|
| 1 | `DRM_IOCTL_VERSION` 0xC0406400 | `drm_version` (64) | `radv_physical_device_try_create` → `drmGetVersion` | Двухпроходный: 1-й вызов с нулевыми `*_len`, ядро пишет длины; 2-й с буферами (len+1). `name="amdgpu"`, `date`, `desc`. `version_major=3`, `minor`, `patchlevel` |
| 2 | то же | то же | `amdgpu_device_initialize` | Опять `drmGetVersion`: major строго 3, иначе `-EBADF` (libdrm). Минимум minor в libdrm нет |
| 3 | `AMDGPU_INFO` 0x40206445 (`ACCEL_WORKING`=0x00) | `drm_amdgpu_info` (32) | `amdgpu_device_initialize` | `return_pointer`/`return_size=4` → записать `1`. Ноль или ошибка → `-EBADF`, init падает |
| 4 | `AMDGPU_INFO` `DEV_INFO`=0x16 | `drm_amdgpu_info_device` (440) | `amdgpu_query_gpu_info_init` | См. §4. Размер: libdrm передаёт `sizeof` своей структуры [I: ядро копирует `min(size, ...)`, из этих источников не видно] |
| 5 | `AMDGPU_INFO` `READ_MMR_REG`=0x15, `dword_offset=0x263e`, `count=1`, `instance=0xffffffff`, `flags=0` | `read_mmr_reg` (в union) | `amdgpu_query_gpu_info_init` (libdrm) и `ac_drm_query_gpu_info` (Mesa) | Безусловно для всех семейств; ошибка фатальна (`if (r) return r`). Это `gb_addr_cfg`. Остальные MMR (0x263d, 0xa0d4/5, 0x2644, 0x2664, 0x9d8) читаются только для `family < AI` (141) — **для GC 10.3.6 не читаются** [V]. Значение для RDNA2 в источниках не нашёл [?] |
| 6 | `DRM_IOCTL_GET_CAP` 0xC010640C, cap=`DRM_CAP_SYNCOBJ` 0x13 | `drm_get_cap` (16) | `ac_query_gpu_info` | `value=1`, иначе "syncobj support is missing but is required" → FAIL [V] |
| 7 | `AMDGPU_INFO` `HW_IP_INFO`=0x02 (`query_hw_ip.type`, `ip_instance=0`) для каждого типа IP 0..9 | `drm_amdgpu_info_hw_ip` (32) | `ac_drm_query_hw_ip_info` | Для GFX/COMPUTE/DMA: версия, `available_rings`, выравнивания. Для остальных: `available_rings=0` (запись в `ip[]` создаётся только при `available_rings != 0` [V]) |
| 8 | `AMDGPU_INFO` `HW_IP_COUNT`=0x03 | `u32` | `ac_drm_query_hw_ip_count` | Число экземпляров на тип (1 для GFX/COMPUTE) |
| 9 | `AMDGPU_INFO` `FW_VERSION`=0x0e, `fw_type=FW_GFX_ME` (0x04) | `drm_amdgpu_info_firmware` (8) | `ac_query_gpu_info` | `ver`, `feature`. Безусловно, ошибка → FAIL [V]. Идёт в `has_load_ctx_reg_pkt` и `has_32bit_predication`; пороговые значения не видел [?] |
| 10 | то же, `fw_type=FW_GFX_PFP` (0x05) | то же | то же | безусловно, ошибка → FAIL |
| 11 | то же, `fw_type=FW_GFX_MEC` (0x08) | то же | то же | безусловно, ошибка → FAIL; идёт в `has_taskmesh_indirect0_bug` |
| 12 | `FW_VERSION` для VCN (0x0e)/VCE (0x01)/UVD (0x02) | то же | то же | **только** если у соответствующего IP `num_queues>0`. Для минимума: `available_rings=0` |
| 13 | `amdgpu_query_sw_info(address32_hi)` | — (в libdrm, без ioctl) | `ac_drm_query_sw_info` | Берётся из менеджера VA (нет ioctl). Ошибка → FAIL |
| 14 | `AMDGPU_INFO` `MEMORY`=0x19 | `drm_amdgpu_memory_info` (96: три `drm_amdgpu_heap_info` по 32) | `ac_query_gpu_info` | `vram`, `cpu_accessible_vram`, `gtt`: `total_heap_size`, `usable_heap_size`, `heap_usage`, `max_allocation`. VRAM округляется вверх до 256 МиБ (`fix_vram_size`) |
| 15 | `AMDGPU_INFO` `VIDEO_CAPS`=0x21 (DECODE=0, ENCODE=1) | `sensor_info.type` в union | `ac_drm_query_video_caps_info` | Только если `drm_minor >= 41`; **возвращаемое значение игнорируется** [V] |
| 16 | `GET_CAP` `SYNCOBJ_TIMELINE` 0x14 | `drm_get_cap` | `ac_query_gpu_info` | `1` → нативные timeline-syncobj. `0` → RADV эмулирует `vk_sync_timeline` в user-space [V, radv_amdgpu_winsys.c] |
| 17 | `GET_CAP` `ADDFB2_MODIFIERS` 0x10 | то же | то же | `kernel_has_modifiers`. Необязателен |
| 18 | `AMDGPU_INFO` `MAX_IBS`=0x22 | массив `u32[AMD_NUM_IP_TYPES]` | то же | Ошибка не фатальна, подставляются константы [V] |
| 19 | (условно) GEM_CREATE с `ENCRYPTED` | — | `has_tmz_support` | Только если нет `AMDGPU_IDS_FLAGS_TMZ`, minor в [36,40), gfx ≥ 9. Для minor вне диапазона не вызывается |
| 20 | `AMDGPU_INFO` `ACCEL_WORKING` на primary fd | — | `radv_physical_device_try_create` | Только при включённом `VK_KHR_display` |
| 21 | `AMDGPU_INFO` `VRAM_USAGE`=0x10 / `VIS_VRAM_USAGE`=0x17 / `GTT_USAGE`=0x11 (u64) | — | `ac_drm_query_heap_info` | Только `vkGetPhysicalDeviceMemoryProperties2` с `VK_EXT_memory_budget` (`radv_get_memory_budget_properties`) |

После вызова #14 `vulkaninfo` до `vkCreateDevice` ядро больше не трогает [V по логике `radv_physical_device_init`; остальное в этой функции после char 100000 не читал [?]].

Выводы:
- `GEM_*`, `CTX`, `CS`, `VA` при одном лишь перечислении устройств не вызываются.
- Если `vulkaninfo` создаёт `VkDevice` (зависит от версии утилиты, не проверял [?]), добавятся вызовы из §2.2.

### 2.2 Вызовы после `vkCreateDevice`/рендер (vkcube/DXVK)

| ioctl | struct (размер) | Откуда | Поля / смысл |
|---|---|---|---|
| `AMDGPU_CTX` 0xC0106442, `op=ALLOC_CTX`(1) | `drm_amdgpu_ctx` (16) | `radv_amdgpu_ctx_create` → `ac_drm_cs_ctx_create2` | `in.priority`: NORMAL=0 (мап: REALTIME→VERY_HIGH 1023, HIGH→512, MEDIUM→0, LOW→-512); `out.alloc.ctx_id`. `-EACCES` → `VK_ERROR_NOT_PERMITTED` |
| `AMDGPU_CTX` `FREE_CTX`(2) | то же | `radv_amdgpu_ctx_destroy` | |
| `AMDGPU_CTX` `GET/SET_STABLE_PSTATE` (5/6) | то же | `radv_amdgpu_ctx_set_pstate` | только RGP-трассировка [I] |
| `AMDGPU_CTX` `QUERY_STATE2` (4) | то же | есть `ac_drm_cs_query_reset_state2` | в `radv_amdgpu_cs.c` вызов не нашёл [V] |
| `AMDGPU_GEM_CREATE` 0xC0206440 | `drm_amdgpu_gem_create` (32) | `radv_amdgpu_winsys_bo_create` → `amdgpu_bo_alloc` (libdrm) | `in.bo_size`, `in.alignment`, `in.domains`, `in.domain_flags`; `out.handle` |
| `DRM_IOCTL_GEM_CLOSE` 0x40086409 | `drm_gem_close` (8) | `amdgpu_bo_free` → `drmCloseBufferHandle` | |
| `AMDGPU_GEM_VA` 0x40286448 | `drm_amdgpu_gem_va` (40) | `radv_amdgpu_bo_va_op` → `ac_drm_bo_va_op_raw` | `handle`, `operation` (MAP=1, UNMAP=2, CLEAR=3, REPLACE=4), `flags`, `va_address`, `offset_in_bo`, `map_size` (выровнен по странице, `align64(size, getpagesize())`) |
| `AMDGPU_GEM_MMAP` 0xC0086441 | `drm_amdgpu_gem_mmap` (8) | `radv_amdgpu_winsys_bo_map`, `amdgpu_bo_cpu_map` | `in.handle`; `out.addr_ptr` = fake offset для `mmap` на fd устройства |
| `AMDGPU_CS` 0xC0186444 | `drm_amdgpu_cs` (24) | `radv_amdgpu_cs_submit` → `ac_drm_cs_submit_raw2` | `in.ctx_id`, `bo_list_handle=0` (**BO-list ioctl не используется**), `num_chunks`, `chunks` (указатель на массив указателей), `flags=0`; `out.handle` = seq_no. Цикл повтора при `-ENOMEM` до 1 с |
| `AMDGPU_WAIT_CS` 0xC0206449 | `drm_amdgpu_wait_cs` (32) | `ac_drm_cs_query_fence_status` (static `amdgpu_ioctl_wait_cs`); `radv_amdgpu_ctx_wait_idle` с таймаутом 1 с | `in.handle=seq_no`, `timeout`, `ip_type`, `ip_instance`, `ring`, `ctx_id`; `out.status` (0 = завершено, иначе занято) |
| `DRM_IOCTL_SYNCOBJ_CREATE` 0xC00864BF | `drm_syncobj_create` (8) | `radv_amdgpu_ctx_queue_syncobj` | с `DRM_SYNCOBJ_CREATE_SIGNALED` (0x1) |
| `SYNCOBJ_DESTROY` 0xC00864C0 | (8) | `radv_amdgpu_ctx_destroy`, WSI | |
| `SYNCOBJ_HANDLE_TO_FD`/`FD_TO_HANDLE` 0xC01064C1/C2 | `drm_syncobj_handle` (16) | `ac_drm_cs_syncobj_import/export_sync_file(2)`, `wsi_create_image_explicit_sync_drm` | флаги 0x1 = sync_file |
| `SYNCOBJ_WAIT` 0xC02864C3, `TIMELINE_WAIT` 0xC03064CA | (40)/(48) | `drmSyncobjWait`, `wsi_drm_wait_for_explicit_sync_release` | флаги: WAIT_ALL 1, WAIT_FOR_SUBMIT 2, WAIT_AVAILABLE 4, WAIT_DEADLINE 8 |
| `SYNCOBJ_SIGNAL` 0xC01064C5, `RESET` C4, `TIMELINE_SIGNAL` 0xC01864CD, `QUERY` 0xC01864CB, `TRANSFER` 0xC02064CC | (16)/(16)/(24)/(24)/(32) | `radv_amdgpu_cs.c` (`transfer`, `query2`), WSI | |
| `AMDGPU_INFO` `TIMESTAMP`=0x05 | u64 | `radv_amdgpu_winsys_query_value` (`vkGetCalibratedTimestamps`) | GPU-счётчик |
| `AMDGPU_INFO` `NUM_BYTES_MOVED`=0x0f, `NUM_EVICTIONS`=0x18, `NUM_VRAM_CPU_PAGE_FAULTS`=0x1e | u64 | то же | только по запросу [I: диагностика] |
| `AMDGPU_INFO` `SENSOR`=0x1d (GFX_SCLK 1, GFX_MCLK 2, GPU_TEMP 3) | u32 | то же | только по запросу |
| `AMDGPU_INFO` `GPUVM_FAULT`=0x23 | — | `query_gpuvm_fault` | только отладка |
| `AMDGPU_GEM_METADATA` 0xC1206446 | `drm_amdgpu_gem_metadata` (288) | `radv_amdgpu_winsys_bo_set_metadata` / `get_metadata` | `op` SET=1/GET=2; `data.tiling_info`, `data.data[64]`. Нужен для WSI-экспорта изображения |
| `AMDGPU_GEM_OP` 0xC0106450, `op=GET_GEM_CREATE_INFO`(0) | `drm_amdgpu_gem_op` (16) | `ac_drm_bo_query_info` (из `bo_from_fd`, `get_flags_from_fd`) | `value` — указатель на `drm_amdgpu_gem_create_in`. Только при импорте dma-buf |
| `DRM_IOCTL_PRIME_HANDLE_TO_FD` 0xC00C642D | `drm_prime_handle` (12) | `radv_amdgpu_winsys_get_fd` → `ac_drm_bo_export` (dma_buf_fd) | `fd` возвращается |
| `DRM_IOCTL_PRIME_FD_TO_HANDLE` 0xC00C642E | то же | `radv_amdgpu_winsys_bo_from_fd` | + `lseek(fd, 0, SEEK_END)` для размера dma-buf (libdrm `amdgpu_bo_import`) |
| `AMDGPU_GEM_USERPTR` 0xC0186451 | `drm_amdgpu_gem_userptr` (24) | `radv_amdgpu_winsys_bo_from_ptr` → `amdgpu_create_bo_from_user_mem` | флаги ANONONLY(2)\|REGISTER(8)\|VALIDATE(4); домен GTT. Нужен только для `VK_EXT_external_memory_host`/GPU_SHM (§3) |
| `AMDGPU_VM` 0xC0086453 | `drm_amdgpu_vm` (8) | `ac_drm_vm_reserve_vmid` | только `reserve_vmid` (RGP) |
| `AMDGPU_BO_LIST` 0xC0186443 | (24) | в RADV 25.0.7 не используется: список BO передаётся chunk'ом `BO_HANDLES` [V] | |
| `AMDGPU_WAIT_FENCES` 0xC0186452 | (24) | RADV не использует (`ac_drm_cs_wait_fences` не существует) [V] | |
| `AMDGPU_GEM_WAIT_IDLE` 0xC0106447 | (16) | есть `ac_drm_bo_wait_for_idle`, из `radv_amdgpu_bo.c` не вызывается [V] | |
| `AMDGPU_FENCE_TO_HANDLE` 0xC0206454, `AMDGPU_SCHED` 0x40106455 | (32)/(16) | в просмотренных файлах RADV не нашёл [?] | |
| `AMDGPU_USERQ*` | — | `ac_drm_create_userqueue`, только `AMD_USERQ` и gfx ≥ 11 | не нужен для GFX10.3 [V] |

Старший ряд ioctl DRM_IOCTL_* (общий слой DRM):
`GET_UNIQUE` 0xC0106401, `GET_CLIENT` 0xC0286405, `GEM_FLINK` 0xC008640A, `GEM_OPEN` 0xC010640B, `SET_CLIENT_CAP` 0x4010640D.
В просмотренных путях Mesa/libdrm для рендер-ноды не вызываются: `GET_CLIENT` только при primary-ноде
(`amdgpu_get_auth`), flink/open только при импорте по flink-имени, `SET_CLIENT_CAP` вызова не нашёл. Реализовывать можно как ENOTTY/EINVAL.

### 2.3 Chunks в `AMDGPU_CS` [V, radv_amdgpu_cs.c]

| chunk_id | Значение | Когда |
|---|---|---|
| `IB` | 0x01 | Один на каждый IB: `ip_type`, `ip_instance=0`, `ring`, `flags` (опц. `AMDGPU_IB_FLAG_PREEMPT`), `ib_bytes = size*4`, `va_start`. Адрес IB кратен `info.ip[type].ib_alignment` (assert) |
| `FENCE` | 0x02 | Если IP не UVD/VCE/VCN: `handle` = GEM-хэндл BO с user-fence (4096 байт GTT с CPU-доступом), `offset = (ip_type*MAX_RINGS_PER_TYPE + ring)*4` (байт, `uint32` слот) |
| `SYNCOBJ_IN`/`OUT` | 0x04/0x05 | при `has_timeline_syncobj == false` |
| `SYNCOBJ_TIMELINE_WAIT`/`SIGNAL` | 0x08/0x09 | при `has_timeline_syncobj == true` |
| `BO_HANDLES` | 0x06 | всегда; список BO (хэндл + приоритет) прямо в chunk |
| `DEPENDENCIES`, `SCHEDULED_DEPENDENCIES`, `CP_GFX_SHADOW` | 0x03, 0x07, 0x0a | в просмотренной части `radv_amdgpu_cs.c` не нашёл [?]; `CP_GFX_SHADOW` связан с `uses_shadow_regs` только как флаг preempt |

Коды ошибок после CS, которые вернёт ядро: `-ENOMEM` (повтор до 1 с), `-ECANCELED`/`-ENODATA`/`-ETIME` → `DEVICE_LOST`, прочее → `UNKNOWN` [V].

Ядро-сторона user-fence: при завершении IB ядро должно записать `seq_no` в слот (4 байта) `BO[handle] + offset`; RADV сам опрашивает слоты fence BO? — в просмотренных файлах опрос слота не нашёл, видел только `WAIT_CS` [?].

---

## 3. mmap, sync_file, dma-buf, present

### 3.1 mmap на fd устройства

| Что | Вызов | Права/флаги |
|---|---|---|
| CPU-мап BO | `ioctl(GEM_MMAP)` → `off`; `mmap(addr, size, PROT_READ\|PROT_WRITE, MAP_SHARED \| (fixed ? MAP_FIXED : 0), fd, off)` [V, `radv_amdgpu_bo.c`; libdrm `drm_mmap` с теми же PROT/FLAGS] | `PROT_READ\|PROT_WRITE`, `MAP_SHARED`; **offset — 64-бит значение из `out.addr_ptr`**, ядро должно сопоставить его BO. Ядро игнорирует offset/size запроса |
| Замена мапа (unmap с replace) | `mmap(cpu_map, size, PROT_NONE, MAP_PRIVATE\|MAP_ANONYMOUS\|MAP_FIXED, -1, 0)` | обычный анонимный `MAP_FIXED` |
| Размонтирование | `munmap(ptr, size)` | |

Doorbell-мап, VRAM/GTT BAR-мап и мап `GEM_MMAP` другого вида в просмотренных путях для GFX10.3 не нашёл [?].
`AMDGPU_GEM_DOMAIN_DOORBELL` (0x40) определён в заголовке, но в `radv_amdgpu_bo.c` не используется (домены VRAM/GTT/GDS/OA) [V].

Домены и флаги, которые приходят в `GEM_CREATE` [V]:
- `preferred_heap`: VRAM → `VRAM|GTT` (0x6); GTT → 0x2; GDS 0x8; OA 0x20.
- `domain_flags`: `CPU_ACCESS_REQUIRED` 0x1, `NO_CPU_ACCESS` 0x2, `CPU_GTT_USWC` 0x4, `VRAM_CLEARED` 0x8,
  `VM_ALWAYS_VALID` 0x40, `EXPLICIT_SYNC` 0x80 (когда нет `IMPLICIT_SYNC`),
  `DISCARDABLE` 0x1000 (minor ≥ 47), `GFX12_DCC` 0x10000 (minor ≥ 58), `ENCRYPTED` 0x400 (только TMZ-проба).
- Хэш/выравнивание: `phys_alignment` из запроса.

Флаги `GEM_VA` [V]:
- обычный BO: `PAGE_READABLE|PAGE_EXECUTABLE` (0xA), плюс `PAGE_WRITEABLE` (0x4);
- `MTYPE_UC` 0x80 при `RADEON_FLAG_VA_UNCACHED` на gfx9+;
- PRT: `PAGE_PRT` 0x10 без BO (handle 0).

VA-менеджер (libdrm) [V]:
- `amdgpu_va_range_alloc(AMDGPU_VA_RANGE_HIGH [| 32_BIT | REPLAYABLE])`. Диапазоны берутся из `dev_info`:
  `virtual_address_offset/max/alignment` (low), `high_va_offset/high_va_max` (high).
- Если high-менеджер не инициализирован, флаг HIGH сбрасывается, и аллокатор работает по low.
  Для минимума можно отдавать только low-диапазон [I из этого поведения]. Точные числа для GFX10.3 в источниках не нашёл [?].
- VA для BO-импорта: выравнивание 1 МиБ; для обычных: `max(alignment, pte_fragment_size)` при `size ≥ pte_fragment_size`;
  gfx9+: выравнивание поднимается до MSB размера.
- Размеры и выравнивания: `pte_fragment_size`, `gart_page_size` читаются Mesa напрямую (копируются в `info`).

### 3.2 Прочие системные вызовы вокруг устройства [V, если не отмечено]

| Вызов | Где |
|---|---|
| `open(.., O_RDWR\|O_CLOEXEC)` на render-ноду | `radv_physical_device_try_create` |
| `fcntl(fd, F_DUPFD_CLOEXEC, 0)` на fd устройства | `amdgpu_device_initialize` (libdrm). Поэтому у устройства может быть несколько fd на одно открытие; ядро должно уметь делить один контекст/VM между dup-fd'ами [I] |
| `fstat`, `stat`, `access(F_OK)` на `/dev/dri/*` | libdrm |
| `lseek(dmabuf_fd, 0, SEEK_END)` | `amdgpu_bo_import` (размер dma-buf) |
| `close(fd)` на хэндл syncobj/sync_file/dma-buf | WSI, winsys |
| `poll`/`epoll`/`eventfd` на fd устройства | **не нашёл** в просмотренных `radv_amdgpu_*`, `ac_linux_drm.c`, `wsi_common_drm.c` [V]. Есть `drmSyncobjEventfd` (`DRM_IOCTL_SYNCOBJ_EVENTFD` 0xC01864CF) в libdrm, использование Mesa не нашёл |
| `futex` | в libdrm/Mesa — `pthread_mutex` и т.п. (user-space) [I] |
| `memfd_create`/`ftruncate` для wl_shm | `os_create_anonymous_file` — реализацию не читал [?] |

### 3.3 Present через Wayland [V, wsi_common*.c, radv_wsi.c]

**Важно:** RADV вызывает `wsi_device_init(..., &(struct wsi_device_options){.sw_device = false})`.
Поэтому по умолчанию `wl_shm`-ветка WSI **не используется**. Используется dma-buf (`WSI_WL_BUFFER_NATIVE`).

| Режим | Как включить | Что нужно от ядра |
|---|---|---|
| Native dma-buf (по умолчанию) | — | `PRIME_HANDLE_TO_FD`, `GEM_METADATA`, `GEM_OP`, `PRIME_FD_TO_HANDLE`; `vk_image` с `VK_EXT_image_drm_format_modifier` (`supports_modifiers = gfx_level >= GFX9`), `VK_KHR_external_memory_fd`; `GetMemoryFdKHR` → экспорт dma_buf_fd. Wayland: `zwp_linux_dmabuf_v1` **обязателен**: в `wsi_wl_image_init` для NATIVE стоит `assert(display->wl_dmabuf)`. Если dmabuf-протокола нет, fallback на shm в коде не видел [V] |
| Sync для native | implicit: `DMA_BUF_IOCTL_EXPORT_SYNC_FILE` (_IOWR('b',2), `{u32 flags; s32 fd}`) и `DMA_BUF_IOCTL_IMPORT_SYNC_FILE` (_IOW('b',3)). При `ENOTTY/EBADF/ENOSYS` Mesa ставит флаг и возвращает `FEATURE_NOT_PRESENT`; затем сигнал через memory object chain. explicit sync (`wp_linux_drm_syncobj_v1`): timeline-syncobj + `drmSyncobjFDToHandle/Query/TimelineWait/Transfer/ExportSyncFile`. Нужны `device->has_timeline_semaphore` и OPAQUE_FD-экспорт семафоров |
| Принудительный sw | переменная окружения **`MESA_VK_WSI_DEBUG=sw`** (имена: `buffer, sw, noshm, linear, dxgi, nowlts`) | `wsi->sw = sw_device \|\| (WSI_DEBUG & WSI_DEBUG_SW)`. При sw `GetMemoryFdKHR` не загружается; образ рисуется в host-visible линейный буфер, при present `vkMapMemory` + копия в shm |
| sw + GPU_SHM | sw и `has_import_memory_host` без `noshm` | shm-память импортируется в GPU как host pointer → `GEM_USERPTR` с `ANONONLY` [I: shm из memfd не анонимная, ядро с ANONONLY такой мап отвергнет. Не проверено] |
| sw + SHM_MEMCPY | `MESA_VK_WSI_DEBUG=sw,noshm` | **Рекомендуемый минимальный режим.** Копия: `memcpy(image->shm_ptr, image->base.cpu_map, row_pitch*height)`. shm: `os_create_anonymous_file` + `mmap(MAP_SHARED)` + `wl_shm_create_pool`. После submit `WaitForFences` блокирует до конца кадра |

Ядерные вызовы в sw/shm-режиме: `GEM_CREATE` + `GEM_VA` + `GEM_MMAP` + `mmap` (для линейного буфера),
`CS`, `WAIT_CS` или syncobj-wait (для `vkWaitForFences`). Без `PRIME_*`, без `DMA_BUF_IOCTL_*`, без sync_file.
Сокет Wayland передаёт fd shm по SCM_RIGHTS (это уже не ядро DRM).

### 3.4 sync_file вне WSI [V]

- `radv_amdgpu_cs_submit_zero` (пустой submit без IB): `ac_drm_cs_syncobj_export_sync_file(2)` → `sync_accumulate("radv", ...)` (libsync, ioctl `SYNC_IOC_MERGE` на sync_file fd — константу в источниках не открывал [?]) → `ac_drm_cs_syncobj_import_sync_file`.
- Нужно, чтобы fd, возвращаемый `SYNCOBJ_HANDLE_TO_FD` с флагом `EXPORT_SYNC_FILE`, был sync_file (`poll`-able, `SYNC_IOC_MERGE`-able) [I].

---

## 4. Минимальный сценарий "vulkaninfo печатает AMD Radeon"

### 4.1 Набор ioctl (достаточный по анализу выше) [I как вывод]

1. `VERSION` (2 прохода), `name="amdgpu"`, `3.<minor>.0`.
2. `AMDGPU_INFO`: `ACCEL_WORKING`, `DEV_INFO`, `READ_MMR_REG(0x263e)`, `HW_IP_INFO` и `HW_IP_COUNT` (GFX+COMPUTE, остальное 0),
   `FW_VERSION` (ME, PFP, MEC), `MEMORY`.
3. `GET_CAP`: `SYNCOBJ=1`, желательно `SYNCOBJ_TIMELINE=1`.
4. `MAX_IBS`, `VIDEO_CAPS`, `ADDFB2_MODIFIERS`: могут вернуть любую ошибку (ENOTTY/EINVAL) без последствий.

Всё остальное неизвестными кодами: ENOTTY/EINVAL.

### 4.2 Поля `drm_amdgpu_info_device` (v6.12, 440 байт) и что с ними делает Mesa [V]

| Поле | Использование |
|---|---|
| `family` | **главное для распознавания:** 149 (`AMDGPU_FAMILY_GC_10_3_6`) = `FAMILY_RPL` 0x95 в Mesa → `CHIP_RAPHAEL_MENDOCINO`, имя `"RAPHAEL_MENDOCINO"`. Нет совпадения → `UNIMPLEMENTED_HW` + "unknown (family_id, chip_external_rev)" |
| `external_rev`, `chip_rev` | сохраняются (`chip_external_rev`/`chip_rev`); в `identify_chip` **не сравниваются** [V]. В addrlib: `AMDGPU_RAPHAEL_RANGE 0x01,0xFF` [V]. Используйте `external_rev` из 0x01..0xFF [I] |
| `device_id`, `pci_rev` | `pci_id`, `pci_rev_id`; libdrm ищет по ним имя в `amdgpu.ids` |
| `num_shader_engines`, `num_shader_arrays_per_engine` | `max_se`, `max_sa_per_se`; границы цикла по `cu_bitmap` |
| `cu_bitmap[4][4]`, `num_cu_per_sh` | `cu_mask`, `num_cu`, `max_good_cu_per_sa`/`min_good_cu_per_sa` |
| `enabled_rb_pipes_mask` (+`_hi` при minor ≥ 52), `num_rb_pipes` | `enabled_rb_mask`, `max_render_backends` |
| `num_tcc_blocks`, `tcc_disabled_mask` | GFX10+: при minor ≥ 35 используются; `l2_cache_size = num_tcc_blocks * 128 KiB` для RAPHAEL_MENDOCINO |
| `max_engine_clock`, `max_memory_clock` | кГц → МГц; `max_gflops`, пропускная способность |
| `gpu_counter_freq` | `clock_crystal_freq`; 0 → предупреждение и принудительно 1 (не фатально) |
| `vram_type`, `vram_bit_width` | типы и ширина шины; для iGPU: `LPDDR5`=12 или `DDR5`=10 [I] |
| `ids_flags` | `FUSION`(0x1) → `has_dedicated_vram=false` (APU). `PREEMPTION`(0x2) → `register_shadowing_required` (только gfx < 11). `TMZ`(0x4) — см. п.19 таблицы §2.1. `CONFORMANT_TRUNC_COORD`(0x8) — только gfx11+ |
| `pte_fragment_size`, `gart_page_size` | копируются |
| `pcie_gen`, `pcie_num_lanes` | только при minor ≥ 51 |
| `num_shader_visible_vgprs` | при minor ≥ 45: `/2` для gfx10+; иначе 512 (gfx10+) |
| `vce_harvest_config` | копируется |
| `tcp_cache_size`, `gl1c/gl2c`, `sqc_*`, `mall_size`, `shadow_*`, `csa_*` | только gfx11+ / minor ≥ 52. Для GFX10.3 используются константы Mesa (TCP 16 KiB и т.д.) |
| Не читаются в `ac_query_gpu_info` | `num_hw_gfx_contexts`, `wave_front_size`, `gc_double_offchip_lds_buf`, `virtual_address_*`, `high_va_*`, `cu_ao_*`, `min_*_clock` (но `virtual_address_*`/`high_va_*`/`virtual_address_alignment` читает libdrm для VA-менеджера, а `num_hw_gfx_contexts`, `cu_ao_mask`, `cu_active_number` копируются в `amdgpu_gpu_info`) |

### 4.3 Версия GFX и IP [V]

- `gfx_level` выводится не из `family`, а из версии `ip[GFX]`: нужно `ver_major==10`, `ver_minor==3` → `GFX10_3`.
- При `drm_minor >= 48` и `ip_discovery_version != 0` версия распаковывается из него побайтово
  (major/minor/rev; точные сдвиги не видел [?]). Иначе берётся `hw_ip_version_major/minor`.
  Для `FAMILY_RPL` минор принудительно 3 только в ветке без discovery.
  Рекомендация [I]: `ip_discovery_version=0`, `hw_ip_version_major=10`, `hw_ip_version_minor=3`.
- `available_rings` — битовая маска; `num_queues` — число бит (для GFX: 0x1 → 1 кольцо).
- Требование: `GFX.num_queues > 0 || COMPUTE.num_queues > 0` ("failed to find gfx or compute").
  Для vkcube нужен GFX (`has_graphics`).
- `ib_start_alignment`/`ib_size_alignment`: RADV проверяет `ib_mc_address % ib_alignment == 0`.
  Реальных чисел для GFX не нашёл [?].

### 4.4 Память для iGPU [I по логике RADV, V для формул]

- `FUSION` → APU: RADV делит кучи 2/3 VRAM + 1/3 GTT от суммы (`radv_physical_device_init_mem_types`).
- VRAM-куча создаётся только если `vram_size*9 >= visible_vram_size`.
- `MEMORY.vram.total_heap_size` округляется вверх до 256 МиБ.
- `all_vram_visible = vram*0.9 < vram_vis`. Для iGPU отдавайте `cpu_accessible_vram == vram`.
- `address32_hi`: из VA-менеджера (libdrm), отдельного ioctl нет.

---

## 5. Требования Mesa/libdrm к ядру (сводка)

| Требование | Источник | Эффект при невыполнении |
|---|---|---|
| `drm_major == 3` | libdrm `amdgpu_device_initialize` (`-EBADF`); `assert` в `ac_gpu_info.c` | init падает |
| `drm_minor >= 27` ("3.27.0, kernel 4.20+") | `ac_gpu_info.c` | `AC_QUERY_GPU_INFO_FAIL` → `VK_ERROR_INITIALIZATION_FAILED` |
| Гейты по minor (не фатальны, включают поля) | `ac_gpu_info.c`: ≥35 tcc, ≥36..39 tmz-проба, ≥41 video caps, ≥45 vgprs, ≥47 discardable (radv_amdgpu_bo.c), ≥48 ip_discovery, ≥49, ≥51 pcie, ≥52 rb_hi/кэши/`CONFORMANT_TRUNC_COORD`, ≥55, ≥58 GFX12_DCC | поля игнорируются или подставляются значения по умолчанию |
| Рекомендация [I] | | minor ≈ 49..52: тогда `ip_discovery_version`, `pcie_*`, `enabled_rb_pipes_mask_hi` должны быть осмысленны (дают 0 — безопасно). Какой minor у ядра 6.12 (Debian 13), в источниках не нашёл [?] |
| `ACCEL_WORKING != 0` | libdrm | `-EBADF` |
| `DRM_CAP_SYNCOBJ == 1` | `ac_gpu_info.c` | FAIL |
| `DRM_CAP_SYNCOBJ_TIMELINE` | RADV | при 0 — эмуляция, работает |
| `DEV_INFO`, `read_mmr(0x263e)`, `FW(ME/PFP/MEC)`, `MEMORY` возвращают 0 | ac_gpu_info / libdrm | любая ошибка = FAIL |
| `family` известна Mesa | `identify_chip` | `UNIMPLEMENTED_HW` |
| GFX ≥ 10 → `GFX10_3`; `gfx_level <= GFX12` | `radv_is_gpu_supported` | `INCOMPATIBLE_DRIVER` |
| `has_graphics` (`GFX.num_queues > 0`) | `radv_is_gpu_supported`, не-CDNA | иначе устройство отвергается для gfx9; для gfx10 условие в выжимке только для GFX9 [V ограниченно] |
| `drmGetVersion().name == "amdgpu"` | `radv_physical_device_try_create` | `INCOMPATIBLE_DRIVER` |
| PCI vendor `0x1002`, bustype PCI | `create_drm_physical_device` | `INCOMPATIBLE_DRIVER` |
| `ac_addrlib_create` (addrlib по `family`, `chip_external_rev`, `gb_addr_cfg`) | `radv_physical_device_try_create` | `VK_ERROR_INITIALIZATION_FAILED`. Какие значения addrlib отвергает, не проверял [?] |
| `radv_init_wsi` | там же | без него устройство не появится |

---

## 6. Что нужно от ядра сверх ioctl (общий список, без утверждений о нашем ядре)

| Область | Что требуется (по анализу выше) |
|---|---|
| devfs/sysfs | `/dev/dri/renderD128` (226:128, `S_IFCHR`); `/sys/dev/char/226:128/...` (§1.3); `opendir`/`readdir`/`readlink`/`realpath`/`stat` на `/sys/dev/char/...` |
| procfs | `/proc/self/fd/N` для `readlink` |
| mmap | файл устройства: `PROT_READ\|PROT_WRITE`, `MAP_SHARED`, 64-бит offset из `GEM_MMAP`; `MAP_FIXED` по уже существующему мапу (замена на `PROT_NONE` анонимный); `munmap` |
| fd | `open O_RDWR\|O_CLOEXEC`, `fcntl F_DUPFD_CLOEXEC`, `close`, `lseek(SEEK_END)` на dma-buf; `dup` fd'ов с общим VM/контекстом |
| ioctl-обёртка | возврат ошибок в `errno`; `EINTR`/`EAGAIN` приводят к повтору; `ENOTTY` для неизвестных кодов |
| syncobj | объекты с `CREATE_SIGNALED`, timeline-точки, wait/signal/reset/query/transfer; экспорт/импорт как sync_file (poll-able fd) |
| sync_file | fd с `poll` и merge-ioctl (`sync_accumulate` в `radv_amdgpu_cs_submit_zero`) |
| dma-buf | `PRIME_HANDLE_TO_FD` (flags `DRM_CLOEXEC\|DRM_RDWR`), `PRIME_FD_TO_HANDLE`, `DMA_BUF_IOCTL_{EXPORT,IMPORT}_SYNC_FILE`, `DMA_BUF_IOCTL_SYNC` (_IOW('b',0,u64)), mmap dma-buf — нужны **только для native WSI** (§3.3) |
| shm (Wayland) | memfd/анонимный файл, `mmap MAP_SHARED`, `SCM_RIGHTS` — только при `MESA_VK_WSI_DEBUG=sw` |
| userptr | `GEM_USERPTR` нужен для `VK_EXT_external_memory_host` и GPU_SHM; при `noshm` — не нужен [I] |
| потоки | `pthread`/futex на стороне libc, не специфично для DRM |
| eventfd/epoll | со стороны драйверов не нашёл [?]; Wayland-цикл Mesa использует `wl_display_*` |

Ранжирование "обязательно для vulkaninfo / для рендера" (по таблицам §2) [I]:

| Нужно для | ioctl |
|---|---|
| vulkaninfo (перечисление) | VERSION, GET_CAP(SYNCOBJ), AMDGPU_INFO(ACCEL_WORKING, DEV_INFO, READ_MMR_REG 0x263e, HW_IP_INFO, HW_IP_COUNT, FW_VERSION×3, MEMORY) |
| vkCreateDevice + рендер в sw-режиме | + CTX, GEM_CREATE, GEM_CLOSE, GEM_VA, GEM_MMAP, CS (chunks IB/FENCE/BO_HANDLES/SYNCOBJ_*), WAIT_CS, SYNCOBJ_* |
| native-present | + PRIME_*, GEM_METADATA, GEM_OP, DMA_BUF_IOCTL_*_SYNC_FILE |
| опционально | GEM_USERPTR, VM, AMDGPU_INFO(TIMESTAMP/SENSOR/…) |
| не использует RADV 25.0.7 для GFX10.3 | BO_LIST, WAIT_FENCES, GEM_WAIT_IDLE, FENCE_TO_HANDLE, SCHED, USERQ |

---

## 7. Не нашёл / неуверенно

1. **Реальные значения GC 10.3.6 iGPU**: PCI `device`/`revision`, `chip_rev`/`external_rev`, тактовые, `gb_addr_cfg`,
   VA-диапазоны, `ib_*_alignment`, пороги версий ME/PFP/MEC. В просмотренных файлах их нет. Нужен дамп с живой машины
   (`vulkaninfo`, `umr`, `AMD_DEBUG=info`/`RADV_DEBUG=info` — флаг `RADV_DEBUG_INFO` печатает `ac_print_gpu_info`).
2. **DXVK и Wine**: исходники DXVK/Wine не читал; требования к Vulkan-расширениям/фичам для DXVK и путь present через winewayland/winex11
   не анализировались.
3. **Порядок ioctl внутри `ac_query_gpu_info`** взят из выжимки, не из построчного чтения.
4. Что делает RADV, если `nodes[DRM_NODE_PRIMARY] == NULL` при `VK_KHR_display`, и защищён ли `stat(primary)` — не проверено.
5. Включает ли `vulkaninfo` `VK_KHR_display` в instance — не проверено.
6. `ac_gpu_info.c` читался на ~97%: хвост (2347 символов) не читал; `radv_physical_device.c` читался на ~84%; `xf86drm.c` целиком не читал
   (середина 60k–84k символов не покрыта), `drmGetMinorType`/`drmFoldDuplicatedDevices` — только по пересказу.
7. Реализация `os_create_anonymous_file` (memfd vs tmpfile) и `sync_accumulate`/`SYNC_IOC_MERGE` (libsync) — не открывал.
8. Семантика user-fence (`AMDGPU_CHUNK_ID_FENCE`) со стороны ядра: RADV передаёт BO и `offset`, но использует ли RADV
   опрос слота вместо `WAIT_CS` — не видел.
9. `DEPENDENCIES`/`SCHEDULED_DEPENDENCIES`/`CP_GFX_SHADOW` в просмотренной части `radv_amdgpu_cs.c` не нашёл (файл читался выжимкой).
10. Версия `amdgpu` kernel DRM у ядра 6.12 (3.x.0) — не выясняла; `AMDGPU_VERSION_*` из `amdgpu_drv.c`, которого я не читал.
11. Утверждения о ядерном поведении (`min(size)` в `AMDGPU_INFO`, `ANONONLY` на shm, и т.п.) — [I], из заданных источников не подтверждены.
12. Значения `DMA_BUF_BASE='b'` (0x62) и номера SYNC_IOC_* — из памяти/заголовка `dma-buf.h` без числового значения базы; макросы подтверждены, база не процитирована.
