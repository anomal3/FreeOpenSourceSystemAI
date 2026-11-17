# План: видеокарты AMD (Radeon) в FossAi — задание для агента

Записано 17.11.2026 по просьбе Романа для соавтора (Coloborator): документ
самодостаточный — его можно целиком отдать агенту, и тот начнёт работу.
Цель вехи — **ускорение Vulkan видеокартой AMD** для Proton/DXVK (сейчас
весь Vulkan программный — lavapipe на процессоре) и своя картинка AMD.

---

## 0. Что это за проект (прочитать первым)

- FossAi (FreeOS) — своя ОС на Rust: ядро `crates/kernel`, программы
  `crates/user-progs`, сборка и тесты — `xtask`. Карта проекта —
  `PROJECT_MAP.md` (модули с описаниями), план и история — `ROADMAP.md`
  (**ROADMAP — это память проекта**: всё сделанное и решения пишутся туда).
- Программы Linux (glibc, Mesa, Wine/Proton из Debian 13) работают через
  **слой совместимости Linux** в ядре: `crates/kernel/src/linux/` (диспетчер
  вызовов `linux/mod.rs`, `ioctl` — `unsafe fn ioctl` в `linux/mod.rs`,
  особые файлы `/dev/*`, `/proc`, `/sys` — `linux/special.rs`; образцы
  «устройств» — `linux/snd.rs` (ALSA), `linux/ntsync.rs`, `linux/netlink.rs`).
  Это не эмуляция: код Linux исполняется процессором напрямую, ядро отвечает
  на системные вызовы Linux.
- Mesa и Vulkan едут в пакете Proton: `xtask/src/mesa.rs` (пакеты Debian 13,
  Mesa 25.0.7; `libdrm-amdgpu1` **уже** в списке; драйверы Vulkan, кроме
  lavapipe, сейчас **выкидываются** — строка с `libvulkan_lvp` в фильтре;
  ICD-файл пишется там же). Proton — `xtask/src/proton.rs`.
- Драйверы экрана ядра — `crates/kernel/src/display/`: `mod.rs` (выбор,
  смена режима), `outputs.rs` (реестр мониторов: `Backend`, EDID, раскладка
  стола), образцы: `intel.rs` (свой модсет Intel), `nvidia*.rs` (NVIDIA через
  GSP-RM: `nvidia.rs` — BAR0/VBIOS/FWSEC, `nvidia_gsp.rs` — прошивка и RPC,
  `nvidia_disp.rs` — модсет). EDID — крейт `crates/edid`.
- Команды оболочки ядра (терминал) — `crates/kernel/src/shell.rs` (образец:
  `gpu …` для NVIDIA).

### Сборка и проверка (на Windows-ПК, в корне проекта)

```
set PATH=%USERPROFILE%\.cargo\bin;%PATH%
cargo xtask image --release                      # живой образ build\FossAi_*_x86_64_release.img
set FOSSAI_WITH_PROTON=1&& cargo xtask image --release   # то же с Proton+Debian+Trackmania (≈2.9 ГБ)
cargo xtask test -r -a x86_64 -s linux -j 1      # сценарий QEMU (список — xtask/src/harness/scenarios.rs)
cargo xtask thirdparty --only proton             # пересобрать пакет Proton (после правки mesa.rs/proton.rs)
```

- QEMU **не эмулирует** видеокарты AMD. Ядро и слой Linux проверяются в
  QEMU сценариями; всё, что трогает железо AMD, — только на настоящей машине
  с живой флешки.
- Журнал на железе: живая флешка пишет журнал ядра в `\FREEOS\LOG.TXT` на
  разделе «FOSSAI LOGS» (в Windows — отдельная буква диска). После загрузки
  с флешки человек возвращается в Windows, агент читает этот файл.
- Тестовое железо Романа: Ryzen с **встроенной графикой AMD** `1002:13c0`
  (в журнале: `0000:08:00.0 1002:13c0 class 03:00:00 vga display -- NO
  DRIVER`; это RDNA2-iGPU семейства Raphael/Granite Ridge — **точную версию
  блоков (GC/DCN/PSP/SMU/SDMA) установить по IP discovery**, этап A1) и
  дискретная NVIDIA RTX 3060 (её не трогать). Мониторы подключены к NVIDIA:
  картинка с iGPU может быть не видна — для рендера (Vulkan) это не мешает.

### Правила (обязательно)

1. **Только открытые источники**: драйвер Linux `amdgpu`
   (`drivers/gpu/drm/amd/` — большей частью MIT), заголовки регистров
   `drivers/gpu/drm/amd/include/asic_reg/`, Mesa (RADV, `src/amd/`), libdrm
   (`amdgpu/`), документация AMD GPUOpen (RDNA ISA), linux-firmware
   (`amdgpu/*.bin` — закрытые, но свободно распространяемые). Никакого
   дизассемблирования/декомпиляции закрытых драйверов.
2. Прошивки в репозиторий не коммитить: класть в `build/amd-fw/` (как
   `build/nvidia-fw/`) и возить на флешке (`\FREEOS\AMD\…`, имена 8.3 —
   см. как это сделано для NVIDIA в `xtask/src/image.rs`).
3. Работать в ветке `amd-gpu`, коммиты небольшие, по этапам; файлы NVIDIA,
   USB, `fs/ramvol.rs` не трогать без нужды. Комментарии в коде — по-русски,
   в стиле окружающего кода (объяснять «зачем», ссылаться на источник).
4. Каждый этап: сценарий QEMU, где это возможно (слой Linux, разбор таблиц),
   строки журнала с префиксом `amdgpu` для проверки на железе, запись в
   `ROADMAP.md` (раздел «Видеокарты AMD»).
5. Ничего не делать на железе без человека: этапы, требующие загрузки с
   флешки, заканчиваются сборкой образа и списком «что проверить».
6. Безопасность для пользователя: пока свой модсет не проверен на железе,
   картинка остаётся от прошивки (GOP); всё новое — за выключателем
   (команда оболочки и/или константа), как `AUTO_MODESET` в `nvidia_gsp.rs`.

---

## 1. Стратегия

Самый короткий путь к ускорению: **не писать свой Vulkan**, а дать готовому
RADV (Mesa из Debian) тот интерфейс ядра, которого он ждёт, — DRM-ioctl
`amdgpu` на `/dev/dri/renderD128`. Тем же путём мы уже запускаем готовые
Proton/Mesa. Под этим интерфейсом — свой драйвер в ядре, поднимающий
движки GPU (память, кольца команд, прошивки). Картинка (DCN) — отдельной
веткой и позже: пока рендер может показывать кадры через наш Wayland
(`wl_shm`, копия процессором).

Порядок: A1 разведка → A2 память и прерывания → A3 прошивки и кольца →
A4 DRM-интерфейс → A5 RADV/Vulkan → A6 DXVK/Proton → A7 свой дисплей (DCN).

---

## 2. Этапы

### A1. Разведка (только чтение, безопасно)
- Найти карту на PCI (класс 03, вендор 1002), BAR'ы (у iGPU: BAR0 — окно
  VRAM/«carve-out», BAR2 — doorbell, BAR5 — MMIO регистров; проверить по
  `amdgpu_device_init`/`amdgpu_doorbell`).
- Прочитать **IP discovery** (таблица версий блоков: GC, SDMA, MMHUB,
  GFXHUB, DCN, PSP/MP0, SMU/MP1, NBIO, IH, VCN): у iGPU таблица лежит в
  зарезервированной памяти в конце VRAM-окна, см.
  `amdgpu_discovery.c` (`amdgpu_discovery_read_binary_from_*`); запасной
  путь — файл `amdgpu/ip_discovery.bin` из linux-firmware.
- VBIOS у APU — из ACPI-таблицы **VFCT** (`amdgpu_bios.c`,
  `amdgpu_acpi_vfct_bios`), разобрать заголовок ATOM (ATOMBIOS).
- Команда оболочки `amd` (как `gpu` у NVIDIA): печать PCI, BAR, версий IP,
  VBIOS, размеров VRAM (carve-out) — и всё в журнал.
- Итог: точный список версий блоков и **точный список нужных файлов
  прошивки** (по коду `amdgpu`: `gfx_v10_0.c`/`gfx_v11_0.c`, `psp_v13_0.c`,
  `sdma_v5_2.c`, `dcn31*`, `smu_v13_0_*`) — записать в ROADMAP.
- Проверка: в QEMU — ничего не ломается без карты; на железе — строки
  `amdgpu : …` в журнале.

### A2. Память и прерывания
- GMC: окно VRAM (у APU — часть ОЗУ, отданная GPU), GART для системной
  памяти, таблицы страниц GPU VM (уровни PDB/PDE/PTE по `gmc_v10_0.c`/
  `amdgpu_vm*.c`), MMHUB/GFXHUB.
- IH (кольцо прерываний GPU) + MSI/MSI-X ядра (образцы — драйверы ядра
  с MSI-X, например `net/rtl8125.rs`, `usb/xhci`).
- Проверка: тест записи/чтения через GART, событие IH от самопроверки.

### A3. Прошивки и движки
- PSP (MP0): загрузка/проверка прошивок (TMR, `psp_v13_0.c`); на APU
  часть уже поднята прошивкой платформы — следовать коду `amdgpu`.
- SMU (MP1): минимум сообщений для включения блоков/частот.
- RLC, CP (ME/PFP/MEC) — микрокод GFX; SDMA — микрокод копирования.
- Кольца: GFX ring, compute ring, SDMA ring; тест — пакет `WRITE_DATA`/
  `NOP` + fence в память (PM4: `amdgpu/*/nvd.h`, `soc15d.h`), SDMA copy.
- Команда `amd test`: прогон тестов колец, вывод в журнал.

### A4. DRM-интерфейс для программ Linux (слой Linux)
Новый модуль `crates/kernel/src/linux/drm.rs` (+ `drm_amdgpu.rs`):
- Устройства `/dev/dri/card0`, `/dev/dri/renderD128` (major 226), `stat`
  с правильным `st_rdev`; `/sys/dev/char/226:128/device/{vendor,device,
  subsystem_vendor,subsystem_device,revision,…}`, `/sys/class/drm/…` —
  libdrm (`drmGetDevice2`) находит устройство через sysfs.
- Общие DRM-ioctl: `VERSION` (имя «amdgpu», версия как у ядра Linux 6.x с
  нужной `minor` для RADV), `GET_CAP`, `GET_UNIQUE`, `SET_CLIENT_CAP`,
  `PRIME_HANDLE_TO_FD/FD_TO_HANDLE`, `GEM_CLOSE`, **syncobj**
  (`SYNCOBJ_CREATE/DESTROY/WAIT/RESET/SIGNAL/TIMELINE_*/QUERY/TRANSFER`,
  `HANDLE_TO_FD`).
- amdgpu-ioctl (`include/uapi/drm/amdgpu_drm.h`): `AMDGPU_INFO` (все
  запросы, которые делает RADV при старте: `DEV_INFO`, `MEMORY`,
  `HW_IP_INFO/COUNT`, `FW_VERSION`, `VRAM_GTT`, `READ_MMR_REG`…),
  `GEM_CREATE`, `GEM_MMAP` (+ `mmap` по смещению на fd), `GEM_VA`,
  `GEM_WAIT_IDLE`, `GEM_METADATA`, `CTX`, `BO_LIST`, `CS` (сабмит IB в
  кольца A3), `WAIT_CS`, `WAIT_FENCES`, `FENCE_TO_HANDLE`.
- Порядок: сначала собрать **точный список** вызовов, которые делают
  `vulkaninfo`/RADV при старте (прочитать `src/amd/vulkan/winsys/amdgpu/`
  и `src/amd/common/ac_gpu_info.c` в Mesa 25.0, libdrm `amdgpu/`), затем
  реализовывать по нему.
- Проверка в QEMU: без карты `/dev/dri/*` нет (или пустой ответ), ничего не
  ломается; своя проба на C в `libc/linux/` (как `ntsyncprobe.c`), которая
  открывает устройство и делает `VERSION`/`INFO` — на железе печатает
  сведения о GPU.

### A5. RADV
- В `xtask/src/mesa.rs` перестать выкидывать `libvulkan_radeon.so`, добавить
  ICD-файл `radeon_icd.x86_64.json` рядом с `lvp_icd`; при необходимости —
  `libdrm-amdgpu1`, `libelf`, `libLLVM` (RADV компилирует шейдеры своим ACO,
  LLVM ему не обязателен).
- Проверка на железе: `vulkaninfo` (из пакета Proton) показывает
  «AMD Radeon …» рядом с llvmpipe; затем `vkcube` (Wayland через `wl_shm`).

### A6. DXVK/Proton
- `tools/d3dprobe` (своя проба D3D9/D3D11 в пакете Proton) на RADV —
  кадры в секунду против lavapipe; затем Trackmania (`run
  /opt/debian/bin/bash /opt/tmnf/bin/tmnf`). Выбор устройства DXVK —
  `DXVK_FILTER_DEVICE_NAME` или порядок ICD.

### A7. Свой дисплей (позже)
- DCN (`drivers/gpu/drm/amd/display/`) + DMCUB-прошивка: модсет, EDID,
  подключение к реестру мониторов `display/outputs.rs` (новый
  `Backend::Amd`) и «Параметры → Дисплей». Образец по смыслу —
  `nvidia_disp.rs`.

---

## 3. Что вернуть человеку после каждого этапа
- Коммиты в ветке `amd-gpu`, запись в `ROADMAP.md`.
- Если нужен прогон на железе: имя собранного образа, что в нём включено,
  и короткий список «что проверить», плюс какие строки журнала прочитать
  (`amdgpu`, `drm`).

## 4. Известные риски
- APU делит память с процессором: карта памяти (что отдано GPU) — из
  прошивки (UEFI/ACPI) и регистров; ошибка здесь портит память ОС.
- PSP/SMU на APU частично принадлежат платформе: неверное сообщение может
  повесить машину — только с выключателем и после разведки A1.
- Синхронизация (syncobj, fences) — главный источник зависаний DXVK.
