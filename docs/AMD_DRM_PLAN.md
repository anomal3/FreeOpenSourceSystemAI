# Интеграция драйвера AMD (Polaris10) в DRM-слой Linux-совместимости — карта и план

Составлено 22.11.2026 по чтению `crates/kernel/src/linux/drm/`, `crates/nvgpu/`, `xtask/` и
`docs/AMD_A4_CALLS.md` (аналитик; код не запускался — выводы из чтения, места с пометкой
«не нашёл» требуют проверки). Контекст: «железная» часть драйвера AMD готова и проверена на
RX 590 (ROADMAP, раздел «Видеокарты AMD», этапы P1–P6): SMC, микрокод, кольцо GFX, GART,
VMID 1, IB, fence, compute-шейдер. Дальше — программная часть: DRM-слой, RADV, DXVK.

---

Всё прочитано по ветке `amd-gpu`, в ней есть всё из `origin/main` (merge-base проверен). Ничего не менялось и не запускалось: сборку и QEMU я не гонял, поэтому всё ниже про работу кода выведено из чтения. Рабочая копия: `/root/devices/DESKTOP-FGRTP5L/C:/web2027/FreeOpenSourceSystemAI`. Дальше `R` = эта папка.

Важная оговорка. `docs/AMD_A4_CALLS.md` написан для RDNA2 APU (GC 10.3.6), а целевая карта у нас Polaris10 (GFX8, RX 590). Список вызовов RADV из документа годится, числа DEV_INFO нет (это [I], не проверено):
- `family` = `AMDGPU_FAMILY_VI` (130);
- `hw_ip_version` 8.0, `ip_discovery_version=0`;
- флага FUSION нет (это dGPU);
- `READ_MMR_REG 0x263e` на GFX8 это `mmGB_ADDR_CONFIG`; в `amd.rs:534` константа уже есть.

---

## 1. Карта модулей

Пути ниже относительные: `K` = `R/crates/kernel/src/linux`, `NV` = `R/crates/nvgpu/src`.

### Слой DRM в ядре

| Файл | Что делает | Ключевое (путь:строка) |
|---|---|---|
| `K/drm/mod.rs` (549 стр.) | Точка входа `/dev/dri/renderD128`. Держит бэкенды, `DrmFile` (аналог `drm_file`) и общие ioctl. | см. список ниже |
| `K/drm/gem.rs` (361) | Буферы `Bo`, handle→BO, PRIME (dma-buf fd), mmap буфера, dma-buf ioctl. | `Bo:48`, `Bo::new:75`, `shared:116` (`SharedMem::from_frames`), `mmap_file:138`, `mmap_at:152`, `gem_new:169`, `close:233`, `prime_handle_to_fd:242`, `prime_fd_to_handle:264`, `DmaBuf:283`, `dmabuf_ioctl:313` |
| `K/drm/nouveau.rs` (440) | ioctl nouveau: GETPARAM, каналы, `VM_INIT`, `VM_BIND`, `EXEC`. | `ioctl:40`, `getparam:71`, `ChanCore:99` (fence-контекст), `Chan:150`, `channel_alloc:174`, `vm_init:245`, `wait_syncs:292`, `vm_bind:314`, `exec:386` |
| `K/drm/nvif.rs` (164) | Эмуляция NVIF, только nouveau. | `ioctl:35` |
| `K/drm/sync.rs` (493) | syncobj (binary и timeline), `SyncFile`, ожидание опросом раз в 1 мс. | реестр `CHANS:38`, `Syncobj:92`, `SyncobjFile:150`, `SyncFile:153`, `wait_until:201`, `ioctl:248` |
| `K/drm/sysfs.rs` (127) | Каталоги и файлы `/sys/dev/char/226:128/*` и `/sys/class/drm/renderD128/*` для libdrm. | `MINOR_DIRS:23`, `directory:42`, `text:71`, `exists:109`, `link:116` |
| `K/drm/vm.rs` (71) | VM файла: `VaSpace` + ссылки на BO + `apply` в бэкенд. | `Vm:22`, `new:32`, `bind:41`, `Drop:62` |
| `K/drm/soft.rs` (76) | `KernelHost: SoftHost` (кадры из `mm::frame`) и `new()` → `SoftGpu`. | `KernelHost:16`, `new:73` |
| `K/wayland_dmabuf.rs` (121) | `zwp_linux_dmabuf_v1`: dma-buf превращается в `Buffer` через `drm::dmabuf_memory` (только системная память, LINEAR, ARGB/XRGB). | `Params::add:~70`; неявной синхронизации нет (написано в шапке) |
| `R/crates/kernel/src/display/nvidia_gametest.rs` | Команда `gpu gametest [soft|force|kernel]`: GSP, мониторы, `nvk hw on`, затем сценарий Proton `fossai-gametest` (vkprobe, vulkaninfo, vkcube, d3d, tmnf). | Только шапка; `set_soft` вызывается на `:146`, `set_hw` на `:188` |

В `K/drm/mod.rs`:
- Константы узла: `RENDER_NODE:57`, `DRM_MAJOR:59`, `RENDER_MINOR:61`, `render_rdev:65`.
- Бэкенды: `Backends{real,soft}:73`, `set_backend:93`, `set_hw:102`, `set_soft:113`, `backend():139`, `node_visible:151`, `trace:161`.
- Файл: `DrmFile:177`, `FileState:185`, `add_bo:213`, `Drop FileState:234`, `impl Object for DrmFile:263`, `install:283`, `object_of:296`.
- Точки входа из `linux/mod.rs`: `open:303`, `rdev:318`, `object_rdev:325`, `readlink:330`, `mmap_object:341`, `dmabuf_memory:358`, `ioctl:365`.
- ioctl: `canonical:390`, `file_ioctl:464`, `version:512`, `get_unique:531`, `get_cap:537`.

### Крейт `nvgpu` (`no_std`, тесты на хосте: `cargo test -p nvgpu`)

| Файл | Что делает | Ключевое |
|---|---|---|
| `NV/backend.rs` | Договор `GpuBackend` и типы. | `Errno:29`, `Place:55`, `Push:81`, `GpuInfo:133` (+`ga106:167`), `GpuBackend:191` |
| `NV/softgpu.rs` (1707) | Программный GPU: VRAM-страницы по требованию, исполняет GPFIFO и методы через настоящий GMMU-обход. | `SoftHost:58`, `HeapHost:71`, `SoftGpu:362`, `new:374`, `with_info:379`, `read_va/write_va`, `bo_read/bo_write` |
| `NV/uapi.rs` (952) | Структуры и номера ioctl DRM/nouveau/NVIF с проверкой размеров на этапе компиляции. | `ioc:29`, DRM_IOCTL_*: `77-99`, nouveau: `120-130`, DRM_CAP_*: `173-179`, разбор NVIF |
| `NV/vaspace.rs` | Учёт VA и разбор `VM_BIND` по правилам nouveau. | `BindOp:49` (+`parse:59`), `Action:77`, `apply:84`, `VaSpace:102` |
| `NV/arena.rs` | `RangeAlloc` (первый подходящий, слияние соседей), `VramAlloc` (две арены). | `RangeAlloc:33`, `VramAlloc:190` |
| `NV/gpfifo.rs` | GPFIFO, заголовки методов и `PushBuilder`, специфичные для NVIDIA. | `GpEntry:55`, `Ring:76`, `PushBuilder:293` |
| `NV/gmmu.rs` | Таблицы страниц GP100+. | `Gmmu:253`, `PteAttrs:102`, `walk:468` |
| `NV/syncobj.rs` | Только учёт, без сна и замков. Забор — набор пар (ctx, seq). | `Fence:34`, `Syncobj:74`, `wait_poll:222`, `HandleTable:252` |
| `NV/lock.rs` | `SpinLock` для `no_std`. | `:~15` |
| `NV/lib.rs` | Состав модулей и реэкспорт. | `:1-41` |

### Пробы, сценарии, документы

- `R/libc/linux/drmprobe.c` (+`drmprobe.h`):
  - Проверяет `/dev/dri`, sysfs, `VERSION`, `GET_CAP`, `VM_INIT`, NVIF, канал, syncobj.
  - Привязана к nouveau; запуск `drmprobe [soft|hw]`.
- `R/libc/linux/nvkprobe.c`:
  - Проверяет GEM, mmap по `map_handle`, `VM_BIND`, копию через CE, семафор, PRIME, sparse.
  - Привязана к nouveau; запуск `nvkprobe [soft|hw]`.
- `R/libc/linux/vkprobe.c`:
  - Это генерик-клиент загрузчика Vulkan (glibc Debian). В прямом чтении он выбирает устройство по `argv[1]`, потом по DISCRETE, потом первое.
  - Делает vkCreateDevice, пустой submit с fence, fill+copy buffer.
  - К nouveau не привязан, подойдёт и для RADV.
- `R/docs/NVK.md` — краткая карта крейта `nvgpu`.
- `R/docs/AMD_PLAN.md` этап A4 — план слоя `linux/drm/drm_amdgpu.rs`.

---

## 2. Выбор бэкенда, регистрация, open, ioctl, mmap

**Трейт `GpuBackend`** (`NV/backend.rs:191`):
- `info`;
- `bo_alloc(size, Place)`, `bo_free`, `bo_frames`;
- `vm_create`, `vm_destroy`, `vm_map(kind)`, `vm_sparse`, `vm_unmap`, `vm_flush`;
- `chan_create`, `chan_obj`, `chan_obj_free`, `chan_destroy`, `chan_submit(&[Push]) -> seq`, `chan_seq_done`;
- `time_ns`, `vram_used`.

Он заточен под nouveau: классы объектов, GPFIFO-сегменты, `kind` PTE. Для amdgpu как есть не подходит.

**Регистрация устройства**:
- Бэкенд один из двух: `real` (`set_backend`, регистрирует `display/nvidia_rm_chan.rs:349`) или `soft` (команда `nvk soft on`, `shell.rs:1213`).
- Видимость узла: `backend()` (`mod.rs:139`) отдаёт `real`, только если `hw()==true` (`nvk hw on`), иначе `soft`. Без бэкенда узла нет вообще.
- Узел один и жёстко 226:128. Нигде нет `card0`, `renderD129` и множества устройств.
- Через `special.rs` и sysfs libdrm видит узел так:
  - `special.rs:221` зовёт `drm::sysfs::directory`, `:229` зовёт `sysfs::exists`, `:244` зовёт `sysfs::text`.
  - `linux/mod.rs:1086` зовёт `drm::rdev` и отдаёт `S_IFCHR` с `st_rdev`.
  - `linux/mod.rs:1134` зовёт `object_rdev` для fstat.
  - `linux/mod.rs:1354` зовёт `drm::readlink` для `device/subsystem`.
- `/sys/class/drm/renderD128` зеркалит `/sys/dev/char/226:128` (`sysfs.rs`, `MINOR_DIRS`). `/sys/class/drm/card0` нет.
- Для libdrm в `device/` лежат `uevent` (`PCI_SLOT_NAME`), `vendor`, `device`, `subsystem_*`, `revision`, `class`, ссылка `subsystem` → `.../pci`. Файла `config` нет (по документу нужен только как запасной путь).

**open** (`linux/mod.rs:839` → `drm::open`, `mod.rs:303`):
- Если путь не `RENDER_NODE` или бэкенда нет, возвращает `None`, и дальше идёт обычная цепочка.
- Создаёт `Arc<DrmFile>{gpu, st: Mutex<FileState>, pid}` и кладёт в таблицу fd как `Object` (`install:283`, флаги `O_NONBLOCK`/`O_CLOEXEC`).
- `DrmFile` держит свой бэкенд до закрытия, новые open видят новый.

**ioctl** (`linux/mod.rs:2104` → `drm::ioctl:365`):
1. Нормализация номера `canonical:390` по таблице `KNOWN`.
   - Нужна, потому что libdrm шлёт `DRM_IOW` там, где в заголовке `IOWR`.
   - Для amdgpu надо добавить номера.
2. `file_ioctl:464`:
   - Отдельно: `VERSION`, `GET_UNIQUE` (пусто), `GET_CAP`, `SET_CLIENT_CAP` (EOPNOTSUPP), `GET_MAGIC/AUTH/SET_MASTER/DROP_MASTER` (EACCES), `GEM_CLOSE`, `PRIME_*`.
   - Остальное: сначала `sync::ioctl`, затем `nouveau::ioctl`, иначе ENOTTY.
3. Для dma-buf: `gem::dmabuf_ioctl`. Для `SyncFile`/`SyncobjFile`: ENOTTY.

Номера драйверных ioctl `0x40+` у nouveau и amdgpu пересекаются, поэтому диспетчер должен выбираться по бэкенду, а не по номеру.

**mmap** (`linux/mod.rs:1776`):
- `drm::mmap_object(fd, offset, len)` находит `DrmFile` или `DmaBuf` и возвращает `SharedMem` + смещение внутри.
- Для fd DRM это `gem::mmap_file:138`: ищется BO, у которого `map_handle ≤ offset < map_handle+size`. `map_handle` — поддельное смещение от `0x1_0000_0000`, генерирует `NEXT_MAP` (`gem.rs:45`). Для dma-buf смещение считается от начала буфера.
- `SharedMem::from_frames` (`user/shm.rs:117`) оборачивает кадры системной памяти, принадлежащие бэкенду, и держит BO живым, пока есть отображение.
- VRAM-буфер замапить нельзя (EINVAL, `gem.rs:157`). BAR/VRAM в user-space через слой Linux не маппится вообще.

**close / dup / fork / poll**:
- Всё через `Arc<dyn Object>`: `Slot::copy` (`user/files.rs:210`) клонирует `Arc`, `files.fork` (`files.rs:387`) копирует слоты. Значит `dup`, `F_DUPFD_CLOEXEC` и `fork` делят один `DrmFile` с общими VM, BO и syncobj. Для libdrm (`amdgpu_device_initialize`) это нужное поведение.
- `Drop for FileState` (`mod.rs:234`) задаёт порядок разбора: каналы (ждут заборы до 2 с), потом VM, потом BO, потом syncobj. Порядок критичен.
- poll: `readiness` у `DrmFile` — `(false, true, false)` (`mod.rs:271`), события не приходят. У `SyncFile` готовность считается опросом, `deadline()` даёт таймер 1 мс (`sync.rs:173-190`). У `SyncobjFile` готовности нет. `SYNCOBJ_EVENTFD` отвечает EOPNOTSUPP (`sync.rs:261`). Прерываний у GPU нет, всё на опросе.
- Про `lseek(SEEK_END)` на dma-buf (его делает `amdgpu_bo_import`): в `linux/mod.rs:549` LSEEK уходит в native `SYS_SEEK`. Как он обработает `Object`-fd, я не смотрел. Помечаю как риск, надо проверить.

---

## 3. Что общее, что nouveau-специфично

### Общее, берётся для amdgpu без изменений

- `canonical()` как механизм (`mod.rs:390`), но список `KNOWN` надо дополнить.
- `get_cap` (`mod.rs:537`): PRIME, TIMESTAMP_MONOTONIC, SYNCOBJ, SYNCOBJ_TIMELINE. Для RADV этого достаточно, `ADDFB2_MODIFIERS` отдаст EINVAL (документ считает допустимым).
- `get_unique`, `SET_CLIENT_CAP`, права узла рендера.
- Helpers `get`/`put`/`get_array`, `install`/`object_of`, `trace`.
- `Object`-слой, `DmaBuf`, `SyncFile`, `SyncobjFile`.
- Весь syncobj-стек: `nvgpu::syncobj` (Fence, wait_poll, HandleTable) и ioctl в `sync.rs`, включая `HANDLE_TO_FD` с флагом sync_file. Цикл ожидания `wait_until` общий.
- PRIME (`gem.rs:242-277`) и GEM_CLOSE: логика handle→`Arc<Bo>` общая.
- Таблица `FileState.bos` / `add_bo` / `syncobjs`.
- mmap-путь «fake offset → `SharedMem::from_frames`» (`gem.rs:138-159`, `mod.rs:341`).
- `RangeAlloc` / `VramAlloc` (`arena.rs`) годятся для аллокации VRAM и GART.
- `SoftHost`/`KernelHost` как поставщик кадров для софт-бэкенда.
- Зацепки `linux/mod.rs` (open/stat/fstat/readlink/mmap/ioctl) и `special.rs`: можно не трогать.

### Жёстко прописан nouveau, это надо обобщить (конкретные правки)

1. `mod.rs:72-147`: `Backends{real,soft: Option<Arc<dyn GpuBackend>>}` и `backend()`.
   - Правка: ввести `trait DrmDriver` и хранить `Arc<dyn DrmDriver>` для `real` и `soft`. Либо `enum Node{Nouveau(Arc<dyn GpuBackend>), Amd(Arc<dyn AmdBackend>)}`.
   - `set_backend`/`set_soft`/`hw()` оставить, добавить `set_amd_backend`, `amd soft on`, `amd hw on`. Команда `nvk` (`shell.rs:1203-1231`) станет `drm soft|hw|trace` с алиасом `nvk`.
2. `mod.rs:177-196`: `DrmFile.gpu: Arc<dyn GpuBackend>` и `FileState{chans,nvif,vm,vm_disabled}`.
   - Правка: общий `FileState{bos,next_bo,syncobjs,next_sync}` + `driver: Box<dyn DriverFile>` с `Drop`.
   - Порядок разбора в `Drop` (`mod.rs:234-248`) делегировать драйверу (сначала CS/ctx, потом VM).
3. `mod.rs:254` (лог «nvgpu: drm close ... chans vm»): вынести в драйвер.
4. `mod.rs:390-426`: `KNOWN` содержит `NOUVEAU_*`. Сделать `driver.known_ioctls()` или построить таблицу в драйвере.
5. `mod.rs:484`: `nouveau::ioctl(...)` в `file_ioctl`. Заменить на `file.driver.ioctl(file, nr, arg)`. Порядок «sync, затем драйвер» оставить.
6. `mod.rs:509-528` `version()`:
   - Здесь `"nouveau"`, дата `20120801`, описание «nVidia ...», версия `1.4.0`.
   - Для amdgpu: имя `"amdgpu"`, дата `20150101`, описание `"AMD GPU"`, версия `3.<minor>.0`.
   - libdrm требует major==3. RADV требует minor≥27 (по документу). Рекомендую 3.49 [I]. В документе реальный minor ядра 6.12 назван «не нашёл».
   - Данные вынести в `DeviceIdent`.
7. `sysfs.rs:85` (`DRIVER=nouveau`) заменить на `info.driver`. Класс жёстко `0x030000`. Слот и вендор уже берутся из `gpu.info()`. Имя `renderD128` и minor 128 прибиты константами; для второго узла или `card0` придётся параметризовать.
8. `sync.rs:28/38`: реестр `CHANS: BTreeMap<u64, Weak<nouveau::ChanCore>>`, `refresh`, `done`.
   - Правка: `trait FenceCtx { fn poll(&self)->u64; fn cached_done(&self, seq)->bool; }`, реестр на `Weak<dyn FenceCtx>`. `ChanCore` и новый `AmdFenceCtx` реализуют его.
   - Также `chan_fence`/`ordered_on` (`sync.rs:77-87`) завязаны на `ChanCore.ctx`.
9. `gem.rs:24-40`, `gem.rs:42-107`: `Bo` строится через `GpuBackend::bo_alloc(Place)` и флаги `NOUVEAU_GEM_DOMAIN_*`, `tile:(u32,u32)`.
   - Правка: вынести конструктор `Bo::from_backend(alloc fn)`, домены сделать полем драйвера. В `Bo` добавить `flags: u64` и `last_fence`.
   - `gem_new/gem_info/gem_cpu_prep` (`gem.rs:169-230`) — только nouveau, переехать в `nouveau.rs`.
10. `gem.rs:250` (`if bo.no_share { return EPERM }`) — аналог amdgpu `VM_ALWAYS_VALID` (`amdgpu_gem_prime_export` тоже даёт EPERM) [I]. Сделать `driver.can_export(bo)`.
11. `gem.rs:313-359`: `dmabuf_ioctl` для `EXPORT_SYNC_FILE` всегда отдаёт сигнальный забор. Нужен `bo.resv_fence()` у драйвера, иначе неявная синхронизация WSI не работает.
12. `vm.rs`: целиком nouveau (`VaSpace`/`BindOp::parse(NouveauVmBindOp)`/`apply` в `GpuBackend`). Для amdgpu это `AmdVm`, своя реализация, часть `VaSpace` (правила пересечений) можно взять, но `GEM_VA` проще писать отдельно.
13. `nvif.rs`, `nouveau.rs`: остаются как `NouveauDriver`.
14. `NV/uapi.rs`: добавить рядом amdgpu uAPI (лучше в новый крейт, см. §5), номера `DRM_COMMAND_BASE+nr`.
15. `soft.rs:72-76`: `new()` создаёт `SoftGpu` (RTX 3060). Добавить `soft_amd::new()`.
16. `special.rs`: правок не нужно, цепочка общая.
17. Прочее nouveau-специфичное:
    - `NV/backend.rs` `GpuInfo.chipset/classes/gpcs/tpcs`.
    - `gpfifo.rs`, `gmmu.rs` (GP100 PTE; у GFX8 формат другой, см. `amd.rs:1741-1746`).
    - `nvif.rs`, GETPARAM.
    - `nouveau_icd`-описатель в `xtask/src/mesa.rs:152`.

---

## 4. Программный бэкенд (`soft.rs` / `softgpu.rs`): зачем и как

- **Зачем.** Mesa NVK должна пройти всю цепочку DRM → GEM → mmap → VM_BIND → EXEC → fence в QEMU, где нет настоящей карты. `SoftGpu` реализует `GpuBackend` и исполняет то, что ему положили, через настоящие структуры: таблицы страниц пишет `Gmmu`, исполнение идёт обходом `walk`. Ошибки кодирования PDE/PTE видны сразу. Канал, GPFIFO и семафоры заборов эмулируются; шейдеры не исполняются (`docs/NVK.md`).
- **Как.** Системная память — настоящие кадры из пула ядра (`KernelHost`, `soft.rs:16-70`), та же память, что видит `mmap`. VRAM виртуальная (страницы по требованию). Исполнение синхронное: `chan_submit` сразу прогоняет кольцо.
- **Включение.** `nvk soft on` (`shell.rs:1213`) → `drm::set_soft(true)` → `soft::new()` → узел `/dev/dri/renderD128` появляется.
- **Какие сценарии это тестируют** (`R/xtask/src/harness/scenarios.rs`):
  - `nvk-soft` (`:7007`, Target::Live, x86_64, `-smp 2`): `nvk soft on`, затем `drmprobe soft`, `nvkprobe soft`, `nvkprobe-fixed soft`, и запрет строк `drmprobe: FAILED` / `nvkprobe: FAILED`.
  - `nvk-soft-vk` (`:7064`, Target::Installed, 8G, `-smp 4`): ставит Proton, затем `gpu gametest soft -- vk` (vkprobe), `-- d3d` (d3d9 DXVK на NVK). Тяжёлый, цепочка с установленным диском.
- **Как запустить.** `cargo xtask test -a x86_64 -s nvk-soft`, список — `cargo xtask test --list`. Вручную в оболочке ядра: `nvk soft on`, затем `run /compat/linux/bin/drmprobe soft`. Пробы собираются из списка `LINUX_PROGRAMS` (`xtask/src/cbuild.rs:736`). Новую пробу надо дописать туда (и поднять размер массива).
- **Можно ли сделать «софт-amdgpu»: да.** Рекомендуется.
  - Реализуется `SoftAmd: AmdBackend`: фиксированные `DevInfo` Polaris10, виртуальная VRAM, GTT на кадрах пула, таблица VA→BO, минимальный интерпретатор PM4.
  - Для `vulkaninfo` (перечисление, без `vkCreateDevice`) достаточно `VERSION`, `GET_CAP`, `AMDGPU_INFO` (семь запросов, `AMD_A4_CALLS.md` §2.1, §4.1) и sysfs.
  - Узкое место: пакет Proton сейчас вырезает RADV. `xtask/src/mesa.rs:78` (`skipped`) выкидывает все `libvulkan_*`, кроме `lvp` и `nouveau`, а ICD-описатель пишется только для lvp и nouveau (`:152`). `libdrm-amdgpu1` в PACKAGES есть (`:31`), `mesa-vulkan-drivers` есть (`:24`).
  - Надо: пропустить `libvulkan_radeon.so`, писать `radeon_icd.x86_64.json`, добавить ветку `FOSSAI_VULKAN=radv` и условие «есть узел» в `xtask/src/proton.rs:525-535`.
  - Проверки через вывод `vulkaninfo` и `vkprobe "AMD"` / `vkprobe RADV`. `vkprobe` менять не нужно, он выбирает по подстроке имени.
  - Шейдеры не исполнять. Для vkprobe (fill+copy buffer) этого достаточно только если софт-CP реализует `DMA_DATA` или CP-копию PM4. Fill/copy в RADV на GFX8 идёт через compute-шейдер или CP DMA, и это граница софт-модели: либо реализовать CP_DMA и WRITE_DATA, либо ограничить софт-проверку до CS+fence.

---

## 5. Механизмы ядра для mmap устройств, eventfd/poll, sync_file

- **mmap буфера.** Только `SharedMem::from_frames` на кадрах из пула (`user/shm.rs:117`). Кадры чужого владельца в пул не возвращаются, владелец держится через `keep: Arc<dyn Any>`.
- **mmap MMIO/BAR/VRAM для программ.**
  - В слое Linux нет. Есть только драйверный API `user/driver.rs:162` `map_device(handle, bar)`.
  - Он ограничивает BAR до 16 МиБ (`:167`, «память видеокарты — не наш случай») и маппит через `space.map_frames(... PageFlags::DEVICE)` (`user/space.rs:522`, unsafe).
  - Для VRAM-BO с CPU-доступом нужна новая ветка (`SharedMem` на физическом диапазоне BAR с флагами DEVICE/WC). Сейчас такой ветки нет, а `from_frames` кладёт обычные флаги, то есть атрибуты кэша неверны для MMIO.
  - Фиксированных смещений для doorbell или MMIO в слое нет.
  - Поэтому первая версия: все CPU-видимые BO в GTT (системной памяти), VRAM без CPU-доступа.
- **MAP_FIXED поверх.** `linux_mmap` (`user/mod.rs:1126`, ветка в `linux/mod.rs:1758-1766`) поддерживает `Place::Fixed/Hint/NoReplace`. RADV делает замену мапа анонимным `MAP_FIXED` (документ §3.1), это работает.
- **eventfd/poll на fd устройства.** `EventFd` есть (`ipc.rs:1144`, `eventfd` на `:1190`), Epoll тоже. Но у DRM-fd нет ни событий, ни прерываний. `SYNCOBJ_EVENTFD` отвечает EOPNOTSUPP. RADV 25.0.7 его не использует (документ §3.2). IH (`MM_IH_RB_CNTL`) в `amd.rs` не инициализируется, всё на опросе.
- **sync_file.** `SyncFile(Fence)` (`sync.rs:153`): `poll`-готовность через `refresh()`+таймер 1 мс. Получается через `SYNCOBJ_HANDLE_TO_FD` с флагом EXPORT_SYNC_FILE (`sync.rs:297`), импорт через `FD_TO_HANDLE` с IMPORT_SYNC_FILE (`:322`). `SYNC_IOC_MERGE` не нашёл: `ioctl` на `SyncFile` отвечает ENOTTY (`mod.rs:378`). Но RADV вызывает `sync_accumulate` в `radv_amdgpu_cs_submit_zero` (документ §3.4), так что для пустого submit ioctl MERGE понадобится [I].
- **DMA_BUF_IOCTL_EXPORT/IMPORT_SYNC_FILE.** `gem.rs:313-359`:
  - `SYNC` — заглушка (память когерентна).
  - `EXPORT_SYNC_FILE` — всегда сигнальный забор.
  - `IMPORT_SYNC_FILE` — ждёт забор до 2 с.
  - Ни `resv` у BO, ни неявного отслеживания нет (там же признание в комментарии и в шапке `wayland_dmabuf.rs`: «ожидание заборов буфера не сделано»).
- **Wayland.** `zwp_linux_dmabuf_v1` v3, только LINEAR. RADV на dGPU рисует с модификаторами и тайлингом, поэтому по документу §3.3 на первом этапе нужен `MESA_VK_WSI_DEBUG=sw,noshm`. Тогда PRIME и DMA_BUF_IOCTL не нужны.

---

## 6. Архитектура `linux/drm/amdgpu.rs`

### Разбиение

- **Новый крейт `R/crates/amdgpu`** (`no_std`, тесты на хосте, по образцу `nvgpu`):
  - `uapi.rs`: структуры amdgpu_drm.h (INFO 32 Б, DEV_INFO 440, CS 24, GEM_CREATE 32, GEM_VA 40, GEM_MMAP 8, CTX 16, WAIT_CS 32, GEM_METADATA 288, GEM_OP 16 и т.д.), номера `DRM_COMMAND_BASE+nr`, проверка размеров `const _: () = assert!(...)`. Общую часть `drm.h`/syncobj брать из `nvgpu::uapi` (зависимость или вынос в общий `drmuapi`).
  - `backend.rs`: `trait AmdBackend`, `AmdInfo` (device_id, rev, family, VRAM, GTT, CU/SE, clocks, IDs прошивок), `Bo handle`, `VmHandle`, `Ring`.
  - `gmc8.rs`: PTE/PDE формата GFX8 (бит VALID, SYSTEM, SNOOPED, EXEC/READ/WRITE, как `amd.rs:1741-1746`; PD/PT, блок 10 бит) и построитель таблиц на `trait PtMem`, как `gmmu::PtMem`.
  - `va.rs`: учёт GEM_VA MAP/UNMAP/REPLACE/CLEAR.
  - `pm4.rs`: константы пакетов и сборщик (из `amd.rs:1508-1515, 1946-1949, 2200-2207`).
  - `softamd.rs`: `SoftAmd: AmdBackend` + интерпретатор PM4.
- **`R/crates/kernel/src/linux/drm/amdgpu.rs`** (ядро): `AmdDriver` / `AmdFile`, обработчики ioctl.
- **`R/crates/kernel/src/linux/drm/soft_amd.rs`**: `KernelHost`-обёртка (можно переиспользовать `soft.rs::KernelHost`).
- **`R/crates/kernel/src/display/amdgpu/`**: превращение `amd.rs` в драйвер (`mod.rs`, `smc.rs`, `ucode.rs`, `ring.rs`, `gmc.rs` (GART, VM), `cs.rs` (IB, fence), `shader.rs` для демо). `amd.rs` останется тонкой обёрткой для команд `amd ...` (разведка и отчёт A1).
- **`R/libc/linux/amdprobe.c`**: проба по образцу `drmprobe.c`.
- **Сценарии**: `amd-soft` рядом с `nvk-soft` в `scenarios.rs`.

### Подключение к слою DRM

```text
trait DrmDriver: Send + Sync {
    fn ident(&self) -> &DeviceIdent;            // имя, версия, date, desc, pci ids, slot
    fn new_file(&self) -> Box<dyn DriverFile>;
    fn ioctl(&self, f: &DrmFile, nr: u32, arg: usize) -> Option<i64>;
    fn known_ioctls(&self) -> &[u32];
    fn bo_alloc(&self, size, domain, flags) -> Result<BoBacking, i64>;
    fn can_export(&self, bo: &Bo) -> bool;
    fn bo_resv_fence(&self, bo: &Bo) -> Option<Fence>;
}
trait DriverFile: Send { fn close(&mut self); }   // Drop-порядок внутри
```

`NouveauDriver` оборачивает `Arc<dyn GpuBackend>`. `AmdDriver` оборачивает `Arc<dyn AmdBackend>` (SoftAmd или HwAmd).

### Состояние на файл (`AmdFile`)

- **BO**: общий `FileState.bos` (handle→`Arc<Bo>`), `Bo.domain` ∈ {GTT, VRAM}, `Bo.flags` (CPU_ACCESS_REQUIRED, VM_ALWAYS_VALID, …), `map_handle`.
  - Политика размещения: если просили `VRAM|GTT` или `CPU_ACCESS_REQUIRED`, то в GTT (sysmem). Чистый VRAM без CPU-доступа — в VRAM.
- **VM процесса**: `AmdVm { gpu_vm: VmHandle, va: BTreeMap<u64, Mapping{bo, bo_off, size, flags}>, refs: BTreeMap<BoHandle, Arc<Bo>> }`.
  - Создаётся лениво при первом `GEM_VA` или `CTX`. Внутри: PD+PT в VRAM (как P5, `amd.rs:1953-1964`: PD, PT, блок 10 бит, `max_pfn=2^18`).
  - **VMID** выдаёт бэкенд при submit: простой менеджер 1..7 [I], при нехватке — переиспользование с flush через `VM_INVALIDATE_REQUEST` (`amd.rs:1735`) и перезапись `VM_CONTEXTn_PAGE_TABLE_BASE_ADDR` (из кольца WRITE_DATA, как `gfx_v8_0_ring_emit_vm_flush`, [I]).
  - `GEM_VA` ставит в VA-таблицу, `vm_map` в `AmdBackend` пишет PTE и сбрасывает TLB. BO держится ссылкой, пока отображён (как `vm.rs:22`).
- **Контексты CS**: `ctxs: BTreeMap<u32, AmdCtx{ id, priority, reset_count, last_seq: [u64; N_RINGS] }>`, `CTX` ops ALLOC_CTX, FREE_CTX, QUERY_STATE2, GET/SET_STABLE_PSTATE (в документе: только RGP).
- **Fence-таблица**: `AmdFenceCtx { ring, ctx_id, completed: AtomicU64 }`, реестр в `sync.rs` через `FenceCtx`.
  - Забор = `Fence::seq(ctx_key, seq)`, в `Syncobj` ложится через chunk SYNCOBJ_OUT / TIMELINE_SIGNAL.
  - `seq_no` — глобальный счётчик кольца (однопоточный кольцевой порядок). Это годится для WAIT_CS (`out.status` 0/1), `handle`=seq.
  - Выполнено ли: `backend.ring_seq_done(ring)` (чтение слота fence в VRAM/GTT). Опрос, как `ChanCore::poll`.
  - User-fence chunk (`AMDGPU_CHUNK_ID_FENCE`): ядро после IB добавляет `WRITE_DATA`/`EVENT_WRITE_EOP` со значением seq в `BO[handle]+offset`.
- **Дочерние зависимости**: `Drop` файла: дождаться заборов контекстов (до 2 с, как `nouveau.rs:156`) → `ctxs.clear()` → `vm` → `bos` → `syncobjs`.

### Обработчики ioctl (все по `canonical()`)

| ioctl | Реализация | Источник |
|---|---|---|
| `VERSION` | `DeviceIdent`: `amdgpu`, 3.x.0 | `mod.rs:512` обобщить |
| `GET_CAP` | уже есть | `mod.rs:537` |
| `AMDGPU_INFO` | ACCEL_WORKING=1, DEV_INFO, READ_MMR_REG (0x263e → значение из `GB_ADDR_CONFIG` железа или константа Polaris10), HW_IP_INFO/COUNT (GFX=1 кольцо; COMPUTE=0 пока), FW_VERSION (ME/PFP/MEC из `parse_ucode`), MEMORY (heap_info: vram/cpu_vram/gtt), VRAM/GTT_USAGE, TIMESTAMP, MAX_IBS (можно ENOTTY), остальное EINVAL | документ §2.1, §4 |
| `AMDGPU_CTX` | alloc/free/query_state2 | §2.2 |
| `GEM_CREATE` / `GEM_MMAP` / `GEM_METADATA` / `GEM_OP` / `GEM_WAIT_IDLE` | `gem.rs`-механизмы + `bo.map_handle` как `addr_ptr` | §2.2 |
| `GEM_VA` | MAP/UNMAP/REPLACE/CLEAR, флаги R/W/X, PRT (handle 0) = ENOTSUP на первом шаге | §3.1 |
| `AMDGPU_CS` | чанки IB(1), FENCE(2), BO_HANDLES(6), SYNCOBJ_IN/OUT(4/5), TIMELINE_WAIT/SIGNAL(8/9). Ожидание входных syncobj до submit | §2.3 |
| `WAIT_CS` | `timeout`, `ip_type/ring/ctx`, `out.status` | §2.2 |
| `SYNCOBJ_*`, `PRIME_*`, `GEM_CLOSE` | общие | — |
| `AMDGPU_VM`, `BO_LIST`, `WAIT_FENCES`, `FENCE_TO_HANDLE`, `USERQ` | ENOTTY/EINVAL | RADV 25.0.7 их не вызывает |
| `GEM_USERPTR` | отложить | нужен только external_memory_host |

Коды ошибок после CS (документ §2.3): `ENOMEM` — повтор 1 с, `ECANCELED/ENODATA/ETIME` — DEVICE_LOST.

### Трейт бэкенда (`AmdBackend`, ядровая сторона = `display::amdgpu`)

```text
info() -> &AmdInfo
alloc_vram(size, align) -> Result<VramBlock>;  free_vram(blk)
bo_alloc(size, place{Vram,Gtt}) -> BoHandle;   bo_free;  bo_frames(bo) -> Option<Vec<u64>>
gtt_map(bo) -> GartRange;                       gtt_unmap(range)      // GART VMID0, как amd gart
vm_create() -> VmHandle;  vm_destroy;
vm_map(vm, va, bo, off, len, pte_flags);  vm_unmap(vm, va, len);  vm_flush(vm)
submit_ib(ring, vm, &[Ib{va,size_dw,flags}], fence_slot: (bo,off), user_seq) -> Result<seq>
ring_seq_done(ring) -> Result<u64>      // Err(ENODEV) = GPU hang/reset
fence_wait(ring, seq, timeout_ns) -> Result<bool>   // обёртка над опросом
fw_version(kind) -> (ver, feature);  mmr_read(dword_off) -> u32 (для READ_MMR_REG)
time_ns() -> u64;  vram_used() -> u64;  gtt_used()
```

---

## 7. План коммитов с критериями проверки

1. **«drm: выделить DrmDriver, перенести nouveau в NouveauDriver».** Без смены поведения.
   - Правки пунктов 1–5, 9, 12–13 из §3.
   - Проверка: `cargo test -p nvgpu`; `cargo xtask test -a x86_64 -s nvk-soft` — все строки `drmprobe:`/`nvkprobe:` как раньше, 0 failed.
2. **«drm: данные устройства (`DeviceIdent`) в `version()`, sysfs, `canonical`; `FenceCtx` вместо `ChanCore` в реестре `sync.rs`».**
   - Пункты 3, 4, 6, 7, 8, 10, 11.
   - Проверка: тот же `nvk-soft` + хост-юниттест на `DeviceIdent`.
3. **«crates/amdgpu: uapi + `AmdBackend` + PTE GFX8 + VA-менеджер + `SoftAmd` (INFO/GEM/VA без CS)».** Host-only.
   - Проверка: `cargo test -p amdgpu` (размеры структур через const-assert; сверка DEV_INFO 440; unit PTE по значениям из `amd.rs:1741-1746`; сверка VA MAP/UNMAP/REPLACE).
4. **«drm/amdgpu: VERSION, GET_CAP, AMDGPU_INFO на SoftAmd; `amd soft on`; `amdprobe.c`; сценарий `amd-soft`».**
   - Проба в QEMU: sysfs, VERSION=`amdgpu` 3.x, caps, ACCEL_WORKING, DEV_INFO, MMR 0x263e, HW_IP_*, FW_VERSION, MEMORY.
   - Проверка: `cargo xtask test -a x86_64 -s amd-soft` — `amdprobe: 0 failed`; `nvk-soft` не сломан.
   - (Попутно: в `xtask/src/cbuild.rs:736` добавить `amdprobe`, размер массива 14 → 15.)
5. **«Proton: RADV в пакете, `FOSSAI_VULKAN=radv`, ICD radeon».** `xtask/src/mesa.rs:78,152`, `proton.rs:525-535`.
   - Проверка: на `SoftAmd` `vulkaninfo`/`vkprobe` показывают `AMD Radeon ... (RADV POLARIS10)` и доходят до `vkEnumeratePhysicalDevices finds a device`. Если RADV отвергает DEV_INFO, на этом шаге правятся поля. Это первое реальное подтверждение документа §4.2.
6. **«drm/amdgpu: GEM_CREATE/MMAP/CLOSE, GEM_VA, CTX, PRIME на SoftAmd».**
   - Проверка: расширенный `amdprobe`: create → mmap по fake offset → запись/чтение; GEM_VA MAP/UNMAP/REPLACE; ошибки невыровненного адреса; PRIME туда-обратно возвращает тот же handle.
7. **«drm/amdgpu: AMDGPU_CS + WAIT_CS + заборы + syncobj-чанки + user-fence на SoftAmd (мини-PM4)».**
   - SoftAmd исполняет NOP, WRITE_DATA, EVENT_WRITE_EOP, INDIRECT_BUFFER, SET_*_REG (игнор).
   - Проверка: `amdprobe`: IB с WRITE_DATA в BO → значение видно в mmap; fence через user-fence chunk и WAIT_CS; syncobj signal/wait; timeline. `vkprobe` с пустым submit и fence проходит на RADV+SoftAmd. Fill/copy buffer только если реализован CP DMA (см. §4).
8. **«display/amdgpu: разложить `amd.rs` на модуль-драйвер без глобальных статиков».**
   - Структура `AmdGpu` (MMIO-окно, BAR0, RingState, VRAM-аллокатор, GART, VM-менеджер) вместо `POLARIS_MMIO`/`RING_STATE`/`POLARIS_BAR0`.
   - Цепочка `init(): smc → ucode → ring → gart` + API из §6. Команды `amd smc/ucode/ring/gart/cs/shader` оставить как обёртки.
   - Проверка железо: маркеры `START/UCODE/RING/GART/CS/SHADER.TXT` с `FOSSAI_AMD_*=1` дают те же результаты, что в ROADMAP P6 (шейдер пишет 42). QEMU: сборка обеих архитектур и `nvk-soft`/`amd-soft` без изменений.
9. **«HwAmd: `AmdBackend` поверх `display::amdgpu`, `amd hw on`».**
   - `alloc_vram`, GTT через GART VMID0, `vm_create` (PD/PT в VRAM), `vm_map`, `submit_ib` (IB + EOP-fence), `ring_seq_done`, VMID-менеджер.
   - Проверка железо (RX 590): `amdprobe hw` — IB `WRITE_DATA` из пользовательского VA под VMID≥1 пишет в GTT-BO и в VRAM-BO (читается через mmap и через `bo_read`). Негативный тест: обращение мимо отображения, сообщение о protection fault (регистры `MM_VM_CONTEXT1_PROTECTION_FAULT_*`, `amd.rs:1941-1944`), ring не зависает.
10. **«RADV на железе: vulkaninfo, vkprobe, `gpu gametest` для amd; трассировка».**
    - Проверка железо: `vulkaninfo` выводит устройство, `vkprobe` доходит до fill+copy и сверки данных; затем vkcube с `MESA_VK_WSI_DEBUG=sw,noshm`. Только потом dma-buf WSI (`GEM_METADATA`, `GEM_OP`, `EXPORT_SYNC_FILE` с настоящим `resv`) отдельным коммитом.

---

## 8. Риски

1. **Прошивка и MEC.** `amd.rs` запускает только GFX-кольцо; compute-кольцо MEC не поднято. Пока HW_IP_COUNT для COMPUTE=0. RADV с GFX-only работает, но vkprobe по умолчанию выбирает «графика или вычисления», проверить на железе.
2. **Нет прерываний.** Fence и WAIT_CS на опросе 1 мс; любая зависшая работа держит поток. Нужен watchdog (по образцу `nvidia_rm_chan.rs`, `watchdog ... killed`) и сброс контекста.
3. **VM на GFX8:**
   - Многопроцессность требует VMID-менеджера и flush на кольце. Сейчас проверен один VMID 1 (P5).
   - Свои PDE/PTE (4 КиБ, блок/fragment 10 бит), а не GP100 `gmmu`: переиспользования нет.
   - Ошибка в таблицах портит память (риск AMD_PLAN §4).
4. **Память VRAM для CPU.** Пока нет пути mmap BAR/VRAM для программ (`from_frames` кладёт обычные флаги кэша). Если RADV потребует `cpu_accessible_vram` > 0 для upload-куч, первая версия отдаёт 0/малое значение и всё через GTT. Реальные пороги RADV [I].
5. **Конфликт номеров ioctl** (nouveau 0x40+ и amdgpu 0x40+) решается только выбором драйвера по бэкенду. Сделать один активный узел на систему либо `renderD128` занимает только один драйвер за раз. Если на машине одновременно NVIDIA GSP и AMD, нужен второй узел (`renderD129`), сейчас не поддержан (`MINOR_DIRS`/`RENDER_NODE` прибиты).
6. **`lseek(SEEK_END)` на dma-buf** (нужен `amdgpu_bo_import`): `linux/mod.rs:549` отправляет LSEEK в `SYS_SEEK`; поведение на `Object` не проверено.
7. **Реальные значения Polaris10 для DEV_INFO** (clocks, CU bitmap, rb mask, VA-диапазоны `virtual_address_offset/max/alignment`, `ib_alignment`) документ не даёт: нужно снять с железа (`amd` отчёт, регистры, `AMD_DEBUG=info`).
8. **`sync_accumulate`/`SYNC_IOC_MERGE`** для пустого submit в RADV: сейчас ENOTTY на `SyncFile`.
9. **Старая документация не совпадает с железом.** `AMD_A4_CALLS.md` — GC 10.3.6; DXVK/Wine там не анализировались (раздел 7 п.2). Эмулировать ioctl «как Linux 6.12» не обязательно, версию minor взять консервативно (~49) и поправить по первому прогону `vulkaninfo`.
10. **Глобальные статики `amd.rs` не потокобезопасны под несколькими процессами.** Ring/MMIO должны жить в `AmdGpu` под замками. Запись VM-регистров через SRBM/GRBM-банкирование нельзя делать из нескольких потоков одновременно; безопаснее через WRITE_DATA на кольце.
11. **Не нашёл:**
    - единую таблицу «какие AMDGPU_INFO нужны сверх списка», кроме `AMD_A4_CALLS.md`;
    - наличие `card0`-узла и `/sys/class/drm/card0` в нашем слое;
    - как `xtask/src/harness/mod.rs` цепляет Installed-сценарии в цепочку (нужен ли предшествующий `proton-gl` для `-s nvk-soft-vk`) — не читал;
    - реальный `AMDGPU_KMS_DRIVER_MINOR` ядра 6.12 (в документе тоже помечен «не нашёл»).

## 9. Фактическое состояние (23.11.2026)

План из разделов 6–7 выполнен с отклонением: вместо трейта `AmdBackend` и крейта `crates/amdgpu`
сделано проще — `AmdFile` рядом с `DrmFile` (nouveau не тронут), без вынесения в крейт.

| Что | Где | Проверено |
| --- | --- | --- |
| Узел `renderD128` для AMD (`amdnode soft on`), sysfs, `VERSION`, `GET_CAP`, `AMDGPU_INFO` (Polaris10) | `linux/drm/amdgpu.rs`, `sysfs.rs` | QEMU: `amd-soft` |
| `GEM_CREATE/MMAP/CLOSE/METADATA/OP/WAIT_IDLE`, `GEM_VA` (MAP/UNMAP/REPLACE/CLEAR, PRT-резерв, 64 ГиБ), `CTX` | `amdgpu.rs` | QEMU: `amd-soft` |
| `CS` (интерпретатор PM4: `WRITE_DATA`, `EVENT_WRITE_EOP`, `INDIRECT_BUFFER`, `DMA_DATA`), `WAIT_CS`, syncobj/timeline (общий `sync.rs`, трейт `SyncHost`) | `amdgpu.rs`, `sync.rs` | QEMU: `amd-soft` |
| PRIME: dma-buf (`HANDLE_TO_FD`/`FD_TO_HANDLE`, `mmap`, `dmabuf_memory` для Wayland) | `amdgpu.rs` (`AmdDmaBuf`), `mod.rs` | QEMU: `amd-soft` |
| `sync_file`: `SYNC_IOC_MERGE` | `mod.rs` | QEMU: `amd-soft` |
| RADV из пакета Proton: создаёт устройство Polaris10, отправка, забор, `vulkaninfo` | `xtask/src/{mesa,proton}.rs`, сценарий `radv-soft-vk` | QEMU |
| Бэкенд на железе: VMID 1 на 64 ГиБ, PD + пул PT в VRAM, отправка IB кольцом GFX с fence, `SH_MEM_*` VMID 1..15, шейдер из пользовательского VA | `display/amd/hw.rs`, `amdnode hw on`, `amd hw` | железо (RX 590): `HW RESULT`, `HW SHADER RESULT` PASSED |

Не сделано: запуск RADV на железе (нужна установленная система с Proton), исполнение шейдеров на
программной карте, прерывания (IH) вместо опроса fence, SDMA и compute-очереди, clear-state для
графики (draw), VRAM-домен буферов (все буферы - системная память), PRT на железе (резерв
запоминается, но не отображается), `GEM_USERPTR`, DXVK поверх RADV. Образы для флешки:
`FOSSAI_AMD_{SMC_START,UCODE,RING,GART,CS,SHADER,HW}=1 cargo xtask image --release`.
