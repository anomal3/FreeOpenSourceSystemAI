# GSM / LTE на Redmi 9C (dandelion, MT6765): план подъёма звонков

Статус: план от 04.11, кода нет. Цель — «позвонить» из нашего ядра (Rust, без
Linux) на `dandelion`: MT6765 (Helio G35), PMIC MT6357, модем `md1` внутри SoC.
Правило проекта: телефон лишний раз не перезагружаем, всё проверяем через
`fastboot oem peek/poke/pmic` (см. `ROADMAP.md`, раздел «Телефон: батарея,
Wi-Fi, GSM»).

Пометки достоверности: **[dts]** — прочитано в `build/dandelion-stock.dtb`
(дерево заводского ядра, вырезанное из `bare-boot.img`; формат DTBO-обёртка
`0xd7b7ab1e`, сам FDT с смещения 0x40); **[src]** — прочитано в исходниках
MediaTek-драйвера `eccci` (ветка `10-alps` форка Mayuri-Chan, это ядро MT6765
Android 10, близкое к нашему `xiaomi-dandelion-q-oss`); **[?]** — не проверено,
гипотеза или запись по памяти.

Ссылки на источники:

- eccci, корень: <https://github.com/Mayuri-Chan/android_kernel_xiaomi_mt6765/tree/10-alps/drivers/misc/mediatek/eccci>
  (`modem_sys1.c`, `ccci_modem.c`, `fsm/ccci_fsm.c`, `port/port_cfg.c`,
  `inc/ccci_core.h`, `mt6765/md_sys1_platform.c|h`, `mt6765/modem_reg_base.h`,
  `mt6765/ccci_config.h`, `mt6765/cldma_reg.h`)
- загрузка/проверка образа: <https://github.com/Mayuri-Chan/android_kernel_xiaomi_mt6765/blob/10-alps/drivers/misc/mediatek/ccci_util/ccci_util_lib_load_img.c>
  и `ccci_util_lib_main.h` (структуры `md_check_header_v3..v6`)
- общий разбор LTE-модемов MediaTek: <https://github.com/cyrozap/mediatek-lte-baseband-re>
  (`General-Notes.adoc`), загрузчик образов для Ghidra:
  <https://github.com/nccgroup/ghidra-mtk-loader>, Comsecuris «mtk-baseband-sanctuary»
  (расшифровка/символы образа)
- старое дерево с тем же каркасом (CLDMA, runtime data):
  <https://android.googlesource.com/kernel/mediatek/+/android-mediatek-pike-3.10-marshmallow-mr1-wear-release/drivers/misc/mediatek/eccci/>
- локально (когда будет нужно): `android-kernels/xiaomi-dandelion-q-oss` —
  **на этом сетевом диске его нет**, клонировать отдельно; недостающие значения
  (см. §1.5) лежат в `eccci/mt6765/md_sys1_platform.c`.

---

## 1. Архитектура: модем MT6765 и CCCI

### 1.1 Что это

Модем — отдельный процессор (у G35 — Cortex-R4-класса/nanoMIPS-линия MediaTek
«MD gen 6293» [src: `MD_GENERATION 6293`, dts `mediatek,md_generation = 0x1895`
= 6293]), со своей прошивкой (`md1img`), своей памятью-окном в DRAM (его
«bank0» — образ, «bank4» — общая память с AP) и своим DSP/голосовым трактом.
Общий принцип у всех MTK: **AP грузит образ модема в зарезервированную DRAM,
включает питание и тактирование домена MD, отпускает его процессор
(boot slave), дальше общение по CCCI** (очереди через DMA CLDMA +
doorbell/SRAM CCIF). Это тот же жанр, что наш `mtk_consys.rs` (образ в EMI +
remap + отпустить MCU), только больше: общая память, MPU, длинный
рукопожатие-протокол и файловая служба для NVRAM.

### 1.2 Блоки и адреса AP-стороны [dts, узлы `mddriver`, `cldmadriver`, `ccifdriver`]

| Блок | База (AP) | Прерывание | Примечание |
|---|---|---|---|
| CLDMA AP always-on | `0x10014000` (0x1000) | SPI 0xA5 = 165, level-high | `apcldmain_ao`/`out_ao`/`misc_ao` +0x000/+0x400/+0x800 |
| CLDMA AP power-down | `0x1021B000` (0x1000) | тот же | `apcldmain`/`out`/`misc` |
| CLDMA MD-сторона | `0x1021C000`(in) `0x1021C400`(out) `0x1021C800`(misc) | — | `mdcldma*`; AO-копия `0x10015000/5400/5800` |
| CCIF0 AP | `0x10209000` (0x1000) | SPI 0x94 = 148, level-low (8) | SRAM 512 байт (`mediatek,sram_size = 0x200`) |
| CCIF0 MD-вид | `0x1020A000` | — | |
| CCIF1 AP / MD | `0x1020B000` / `0x1020C000` | SPI 0x96 = 150 | запасной |
| CCIF2 MD / CCIF3 AP,MD | `0x1023D000`; `0x1023E000`(IRQ 0x92) / `0x1023F000` | — | для SCP/др. |
| MD WDT | — | SPI 0x105 = 261, edge-rising(2) | четвёртый IRQ узла `mddriver` |
| `MD1_CFG` (boot stats) | `0x1020E300` / `+4` [src] | — | счётчик стадий загрузки MD, читать `oem peek` |
| Отладочное окно MD | `0x0D0C0000` (0x40000) `dbg_mdsys1` | — | через него AP видит MD-регистры (`0x0D0D4000` PLL, `0x0D0D6000` CLKSW, `0x0D0D5000` GLOBALCON) [src+dts]. Окно `0x0C000000..0x0D000000` в `mtk_regs::WINDOWS` **кончается на `0x0D000000`** — расширить до `0x0D100000`. |
| infracfg_ao | `0x10001000` | — | MD-защита шины, SRCCLKENA, сброс CCIF (`+0x150` set/`+0x154` clr, бит 8) [src] |
| apmixedsys | `0x1000C000` | — | `AP_PLL_CON0`, `MDPLL1_CON0 = +0x3A0` |
| topckgen | `0x10000000` | — | бит 8 и 9 регистра `+0x0` — клок-гейт MD [src] |
| SPM / scpsys | `0x10006000` / `0x10001000`(syscon) | — | MTCMOS `MD1` (клок `scp-sys-md1-main`) |
| EMI | `0x10219000`,`0x10226000`,`0x1022D000`,`0x10235000` | SPI 0x90 | здесь же EMI MPU (защита окон MD) |
| MD-AUXADC | io-channel `<&auxadc 2>` | — | узел `md_auxadc`: каналы `md-channel`, `md-battery` |
| SIM hot-plug | узлы `md1_sim1_hot_plug_eint`, `md1_sim2_hot_plug_eint` | EINT | нужны для детекта SIM; номера EINT/GPIO — в `pinctrl`-узле (разобрать §2.3) |
| MD SIM-питание | PMIC | — | VSIM1/VSIM2 (LDO MT6357) — регистры взять из `pmic/mt6357/upmu_hw.h` |

Параметры узла `mddriver` [dts]: `mdhif_type = 3`, `md_id = 0`,
`ap_plat_info = 0x1A6D (=6765)`, `md_generation = 6293`,
`offset_apon_md1 = 0x1C24`, `cldma_capability = 6`; питание:
`_vmodem-supply` → `buck_vmodem` (0.5–1.2 В, 500 000…1 193 750 мкВ),
`_vcore-supply` → `buck_vcore`.

Каналы CLDMA/CCIF: **данные и управление идут по CLDMA**, **CCIF — сигнал
(doorbell) и SRAM для runtime data / аварийный выход** [src: `md_cd_send_runtime_data_v2`
шлёт `md_ccif_send(H2D_SRAM)`; первая загрузка — `md_ccif_ring_buf_init`].

### 1.3 Память модема — **в dts её нет** [dts]

В `reserved-memory` stock-дерева только `sspm_share`, `scp_share`,
`consys-reserve-memory`, `wifi-reserve-memory`, `ion-carveout-heap`,
`soter-shared-mem` (и т. п.). Резерв под MD **добавляет LK** (бутлоадер) уже при
передаче управления ядру: адрес/размер bank0, smem, CCB передаются в `chosen`
(теги `md1_*`/«lk_info») и читаются драйвером `get_md_resv_mem_info()`,
`get_md_resv_ccb_info()` и др. [src]. Выводы:

1. Наше ядро получает от LK живое FDT (с резервом) — **первое, что делаем:
   выгрузить `/chosen` и `/reserved-memory` из FDT, которое видит ядро**
   (`oem fdt`, §5), и найти там регионы MD. Без этого нельзя ни занять, ни
   защитить память от нашего аллокатора кадров (`mm::frame`).
2. MD-вид памяти: bank0 (образ) — по MD-адресу `0x00000000` + remap;
   bank4 (smem) «ремапится на ближайший 32 МБ-выровненный адрес»;
   cacheable-часть начинается с `0x40000000 + 224 МБ` (путь 6293) [src].
   **Регистр remap bank0 в AP-пространстве — не найден [?]**: взять из
   `ccci_modem.c`/`md_sys1_platform.c` (`set_ap_bank4_map`, `AP_BANK4_MAP0/1 = 0`
   в `ccci_platform.h`; `KERN_EMI_BASE = 0x40000000`).
3. Раскладка smem [src: `md1_6293_noncacheable_fat`, `ccci_config.h`]:
   `RAW_MDCCCI_DBG @0 (2 КБ)`, `RAW_RUNTIME_DATA @58 КБ (4 КБ)` (AP-часть
   `0x800`, MD-часть `0x800`), `RAW_DBM @64К-DBM`, `CCISM_SCP @64 КБ (32 КБ)`,
   `CCISM_MCU @160 КБ (721 КБ)`, `RAW_DFD @1 МБ (448 КБ)`, затем cacheable:
   `CCB_DHL`, `RAW_DHL`, `RAW_MDM` по 20 МБ. Для минимальной загрузки хватает
   первых четырёх; остальное — как у LK.
4. Хвост SRAM CCIF (последние 12 байт, `0x7274626E`, адрес и размер smem
   debug) пишется AP перед стартом [src: `ccci_reset_ccif_hw`].

### 1.4 Образ модема

Раздел `md1img` (подпись MediaTek, формат md1img: набор частей с 512-байтным
`IMG_MAGIC 0x58881688` заголовком; части — `md1rom` (процессор MD),
`md1dsp`, `armv7`, возможно ramdisk). Проверка в драйвере [src]:
в **последних 0x4000 байтах** образа — `md_check_header`; `header_size`
читается словом **перед концом** образа (`end - 4`) и выбирает версию
(v3: 284 байта, v4: +регионы RMPU, v5: +`arm7_img_*`, v6: +`padding_blk`,
`ap_md_smem_size`, `md_to_md_smem_size`, `ramdisk_*`). Поля (v3, смещения от
начала структуры): `check_header[12]` @0 (магическая строка «CHECK_HEADER»,
сравнение `strncmp(…,12)`), `header_verno` @12, `product_ver` @16,
`image_type` @20 (1=2G, 2=3G…), `platform[16]` @24 (ожидаем «MT6765»),
`build_time[64]` @40, `build_ver[64]` @104, `bind_sys_id` @168 (= md_id+1),
`mem_size` @172 (сравнивается с размером резерва LK), `md_img_size` @176,
`dsp_img_offset/size` @184/@188. Эти же поля покажут, сколько памяти MD
реально требует — **сверка с резервом LK обязательна**.

Кто кладёт образ в DRAM — **открытый вопрос [?]**: в `md_cd_start` печатается
«modem image ready, bypass load» (ядро образ не грузит, только проверяет
заголовок через `ccci_get_md_check_hdr_inf`) — это намекает, что **LK уже
загрузил `md1img` в резерв**. Если так, к моменту старта нашего ядра образ уже
лежит в RAM, и раздел с eMMC читать не придётся вовсе (но для надёжности и
чтобы образ был «наш» — всё равно снять, M1). Проверка занимает минуты: найти
магию заголовка по адресам из `chosen` через `oem peek` (§3, M1a).

### 1.5 Включение питания и запуск (последовательность из драйвера) [src]

> **05.11: проверено на аппарате, точные значения — в
> `crates/kernel/src/arch/aarch64/mtk_md.rs`** (`oem md power/pll/go`, MD
> вышел на boot stats `54430007 53320000`). Поправки к тексту ниже:
> адреса `0x2xxxxxxx` — физические адреса AP (окно bank2), не «MD-вид»;
> `vmd1_pmic_setting_on` на MT6357 только ставит VMODEM 0,8 В (`0x15AA`),
> VSRAM_MD нет; boot vector/key на MT6765 не пишутся — только
> `MD_BOOT_VECTOR_EN = 1`. Смещения infracfg: `AP2MD_DUMMY` `0x370` б.0,
> `MD2PERI` — `PROTECTEN_1` (`0x250`, sta `0x258`, set/clr `0x2A8/0x2AC`) б.6,
> `PERI2MD` — `PROTECTEN` (`0x220`, sta `0x228`, set/clr `0x2A0/0x2A4`) б.7,
> `MD_SRCCLKENA` `0xF0C`. SPM: `MD1_PWR_CON` `0x320`, `PWR_STATUS` `0x180/184`
> б.0, `MD_SRAM_ISO_CON` `0x394`, `MD_EXTRA_PWR_CON` `0x398`.

`md_cd_start()`: (первый раз) remap MD-регистров, IRQ MD_WDT/CCIF0,
очистка smem → `ccci_md_hif_start(1)` (клоки CLDMA/CCIF, сброс CCIF HW,
сброс CLDMA) → **`md_cd_power_on`** → `md_cd_let_md_go` → включить IRQ WDT →
`ccci_md_hif_start(2)` (включить IRQ CCIF, `cldma_reset`, `cldma_start`).

`md_cd_power_on` по шагам:

1. `topckgen+0x0`: сбросить биты 8 и 9 (клок MD).
2. PMIC: `vmd1_pmic_setting_on` — включить `VMODEM` (BUCK) и `VSRAM_MD`
   (значения напряжений — из `mt6357` в Xiaomi-дереве; **наш код PMIC уже умеет**
   `oem pmic`, нужны только адреса регистров MT6357 [?]).
3. MTCMOS `MD1` (клок `scp-sys-md1-main`): SPM `MD1_PWR_CON` + бит в
   `PWR_STATUS`, как у CONN в `mtk_consys.rs` (`power_on`: PWR_ON, PWR_ON_2ND,
   ждать статус, CLK_DIS/ISO/RST_B, снять защиту шины).
4. До обращения к регистрам MD: `INFRA_AP2MD_DUMMY` бит снять; `MD2PERI_PROT`
   бит поставить и ждать `RDY` (защита шины MD→периферия на время инициализации).
5. `INFRA_AO_MD_SRCCLKENA`: младший байт `= 0x21`.
6. `md1_pll_init` (адреса — `md_top_Pll` @ MD_PLL_MIXED, `md_top_clkSW`
   @ MD_CLKSW, `ap_mixed` = apmixedsys): ждать `Pll+0x00 != 0`; `apmixed+0x0 |= 2`,
   задержка 100 мкс; `Pll+0x004 = 0x02020E88`, `+0x060 = 0x801713B1`,
   `+0x058 = 0x80171400`, `+0x050 = 0x80229E00`, `+0x048 = 0x80204E00`,
   `+0x040 = 0x80213C00`; ждать бит14 регистра `+0xC00` = 0; `+0x064 &= ~0x80`;
   для MT6765 `+0x104 = 0x04C43100`; `+0x010 = 0x00100010` (цикл до равенства,
   перезапись каждые 5 итераций); ждать бит15 `CLKSW+0x084`;
   `CLKSW+0x024 |= 0x3`, затем `|= 0x058103FC`; `CLKSW+0x028 |= 0x10`;
   `CLKSW+0x020 = 1`; `Pll+0x314 = 0xFFFF`, `+0x318 = 0xFFFF`;
   `Pll+0xF00 = 0x62930000` (метка «PLL готов»).
7. MD RGU WDT: `MD_RGU_BASE + 0x0 (WDTCR) = WDT_MD_MODE_KEY = 0x55000030`.
   (`MD_RGU` = `0x200F0100` MD-вида; AP-вид найти в §1.2/окне `0x0D0xxxxx` [?])

`md_cd_let_md_go`: `md_boot_slave_En (MD_BOOT_VECTOR_EN = 0x20000024 MD-вида)
= 1`; затем защита шины: `PERI2MD_PROT` set + ждать RDY, `MD2PERI_PROT` clr +
ждать, `AP2MD_DUMMY` бит set.

**Не найдено в вытянутых файлах:** численные значения `INFRA_AP2MD_DUMMY_*`,
`INFRA_MD2PERI_PROT_*`, `INFRA_PERI2MD_PROT_*`, `INFRA_AO_MD_SRCCLKENA`
(смещения в infracfg_ao), регистры `md_boot_slave_Vector/Key` и AP-вид базы
`md_boot_slave_En`, настройка EMI MPU (на 4.9/4.14 для 64-бит — через SMC
`MTK_SIP_KERNEL_EMIMPU_SET`/`mpu_region` в ATF/TEE [?]; в `ccci_config.h`
`ENABLE_EMI_PROTECTION` закомментирован). Взять локально из
`md_sys1_platform.c` (шапка), `modem_sys1.c` и `ccci_modem.c` после клона
дерева Xiaomi.

### 1.6 Рукопожатие после старта [src: `fsm/ccci_fsm.c`]

Состояния: `GATED` → `STARTING` (в нём внутренние «ждём HS1/HS2») → `READY`,
плюс `EXCEPTION`, `STOPPING`. Команда START: проверить `GATED`, ждать
критичных пользователей, сбросить события, `ccci_md_pre_start`, объявить
`BOOT_WAITING_FOR_HS1`, `ccci_md_start`. Дальше сообщения приходят в
**канал CCCI_CONTROL_RX = 0**:

- `MD_INIT_START_BOOT` с `reserved == MD_INIT_CHK_ID` → событие **HS1**
  (в нём — `md_query_ap_feature`, возможности MD). AP отвечает **runtime data**
  (`ccci_md_send_runtime_data` → `md_cd_send_runtime_data_v2`): адрес AP-блока
  runtime data (`SMEM_USER_RAW_RUNTIME_DATA`), блок MD сразу за ним, MPU-диапазоны
  noncacheable, `share_memory_support = INTERNAL_MODEM`, список feature_set;
  на 6293+ — `md_ccif_send(H2D_SRAM)`.
- Затем MD присылает **HS2** → `READY`, порты оживают (`ccci_port_md_status_notify`).
- Тайминги: HS1/HS2 — в пределах `BOOT_TIMEOUT` (значение не вытянуто [?]),
  на практике секунды десятки.
- По ходу MD требует **файловую службу `ccci_fs`** (канал `CCCI_FS_*`, 14/15) —
  читает/пишет NVRAM, и **RPC** (`CCCI_RPC_*` 32/33) — запросы к AP (SIM,
  GPIO, время, пр.). Без ответов на FS MD не выходит на HS2 «чисто» или
  стартует без IMEI/калибровок.

Формат `struct ccci_header` — 16 байт [?, по памяти]:
`data[0]` (адрес/служебное), `data[1]` (длина пакета, либо id управл. сообщения),
затем слово `channel:16 | seq_num:15 | assert_bit:1`, затем `reserved`.
Сверить по `ccci_core.h`/`ccci_hif.h`.

Каналы, нужные для звонка [src: `inc/ccci_core.h`, `port/port_cfg.c`]:

| Канал | RX/TX | Узел Linux | Для чего |
|---|---|---|---|
| CONTROL | 0 / 1 | `ccci_ctrl` | рукопожатие, статусы |
| SYSTEM | 2 / 3 | `ccci_sys` | системные сообщения, MD_BOOT_STAGE и т. п. |
| FS | 14 / 15 | `ccci_fs` | NVRAM-файлы |
| RPC | 32 / 33 | `ccci_rpc` | запросы MD→AP |
| **UART2 (AT)** | **10 / 12** (+ack 11/13) | **`ttyC0`** (minor 3) | **AT-команды RIL** |
| UART1 | 6 / 8 | `ccci_md_log_ctrl` | управление логом MD (не AT!) |
| PCM | 4 / 5 | `ccci_aud` | голос/аудио-управление |
| IPC | 34–41 | `ccci_ipc_*` | IMS, прочее |
| STATUS | 67/68 | `ccci_poll` | поллер состояния |
| CCMNI | 20.. | `ccmni0..` | пакетные данные (не нужны для звонка) |

Очередь AT-порта: `DATA_AT_CMD_Q = 5` для `MD_GENERATION > 6292` (у нас 6293).

---

## 2. Что нужно снять с телефона и как

### 2.1 Список

| Раздел | Зачем | Заметка |
|---|---|---|
| `md1img` | образ модема (+ `md1dsp` внутри/отдельно [?]) | обязательно; около 40–100 МБ [?] |
| `nvdata` (ext4, `/mnt/vendor/nvdata`) | NVRAM модема: **IMEI, калибровка РЧ, настройки** | обязательно для штатной работы; читать **только** |
| `nvram` (raw, у некоторых) | исходные NVRAM | проверить таблицу разделов |
| `protect1`, `protect2` | резервные копии NVRAM/IMEI (ext4) | копия «на случай» |
| `proinfo`, `para`, `seccfg` | серийники/секфлаги | `proinfo` читаем, остальное не трогаем |
| `md1dsp`, `md1arm7` | DSP/ARM7 образы, если отдельные разделы | `ls /dev/block/by-name` в Android |

Откуда узнать таблицу: GPT/PMT eMMC (нужен драйвер, §2.3) либо
`adb shell ls -l /dev/block/platform/*/by-name` на заводской системе
(быстрее для плана, ничего не пишет).

### 2.2 Три способа снять

1. **Из заводского Android через adb, пока он есть** — без наших драйверов:
   `adb pull` через `su` невозможен (рут нет); `adb shell` без рута `md1img`
   не прочитает. На локед-загрузчике — **не вариант**, кроме `/vendor/etc` и
   части `/mnt/vendor/nvdata` (не читается).
2. **BROM/Preloader-режим MediaTek** (VCOM `0E8D:0003`/`2000`, Windows COM
   уже есть): скачивание разделов — инструменты `mtkclient` (Python,
   <https://github.com/bkerler/mtkclient>) делают `r md1img` и т. п.
   Для MT6765 обход SLA/DAA в BROM известен (экслоит `kamakiri`/mtkclient);
   **для Redmi 9C с Xiaomi-авторизацией проверить**, нужен ли аккаунт. Это
   самый быстрый путь для M1 и не требует нашего eMMC-драйвера. Выполнять
   на Windows-ПК (`pc_shell`), без записи в телефон.
3. **Наш fastboot + свой eMMC-драйвер** — нужен и дальше (rootfs на eMMC,
   `ccci_fs` читает `nvdata`), поэтому всё равно делать, §2.3.

Рекомендация: **для M1 — способ 2**, параллельно строить способ 3.
Образы класть в `firmware/modem/` (не в хранилище, как `firmware/connsys/`).

### 2.3 Минимальный eMMC-драйвер (MSDC0) под `oem emmc`

Наш fastboot сегодня читает только оперативку/регистры. Раздел можно отдать
хосту через уже существующий механизм `upload`/`get_staged`
(`fastboot.rs`: `State::Upload`, `staged`) — он умеет отдавать буфер по
`DATA`; расширить его до потокового чтения (не держать 100 МБ в RAM).

Из dts [dts]: **MSDC0 = eMMC**, `reg = 0x11230000 (0x10000)`, SPI `0x4F` = 79
(level-low), 8-bit, HS400/HS200/DDR 1.8 В, `max-frequency = 0xBEBC200` =
200 МГц, `non-removable`, `bootable`, `vmmc-supply` (VEMC), клоки
`msdc0-clock`, `msdc0-hclock`, `msdc0-aes-clock` (infra gate, индексы
0x4C, 0x1C, 0x44), `msdc0_top @ 0x11CD0000`, `msdc0_pad_macro @ 0x11CD0000`.
MSDC1 (SD) = `0x11240000`, не нужен.

**Ключевой приём: LK уже поднял eMMC** (LK грузит нас с раздела recovery из
eMMC), контроллер остаётся настроенным (HS400 или HS200 — как LK оставил).
Значит минимальный драйвер:

1. `oem peek 11230000 40` — прочитать `MSDC_CFG`, `MSDC_STA`, `SDC_CFG`,
   `MSDC_CLK_CTL`(`+0x04 …`); убедиться, что клок включён и режим не сброшен.
   Окно `mtk_regs::allowed` (`0x1000_0000..0x2000_0000`) уже покрывает.
2. PIO-чтение: `SDC_CMD` (CMD17/CMD18 + длина блока 512 + режим DMA off),
   `SDC_ARG` = LBA, ждать `MSDC_INT` (бит CMDRDY/XFER_COMPL), вычитывать
   `MSDC_RXDATA` словами при `MSDC_FIFOCS.RXCNT`; без DMA ≈ 1–3 МБ/с
   (хватит: `md1img` ~ минуты, ядро и так тяжелее).
3. Адресация/раздел: eMMC в LBA, **GPT** читать с LBA 1 (раздел user), либо
   MTK PMT-таблица [?]; имена `md1img`, `nvdata`… — из GPT. Boot-разделы
   (`boot0/boot1`: `preloader`) выбирать `CMD6 PARTITION_CONFIG` — **не
   писать никогда**.
4. Только чтение. Запись в eMMC не включаем, пока не будет отдельного решения.
5. Позже: ADMA/BASIC DMA, прерывание SPI 79, сброс/переключение режимов,
   кэш — для rootfs на eMMC.

Альтернатива без регистров: SCSI-подобного нет, UFS нет — только MSDC.

Команды `oem` (добавить в `fastboot.rs`): `oem emmc info` (CID/CSD/EXT_CSD),
`oem emmc gpt`, `oem emmc read <lba> <n>` (в `staged` → `fastboot get_staged`),
`oem emmc part <имя>` (стримом).

---

## 3. Вехи (каждая проверяется через `oem peek` / `oem log`)

Правило: один шаг — один коммит, один «заход» на телефон; MD-питание
**выключаемо** (как `consys` `power_off`), чтобы не перезагружать аппарат.

**M1a. Разведка (без записей).**
`oem fdt` → `chosen`/`reserved-memory`: адреса/размеры MD (bank0, smem, CCB).
Найти в RAM по этим адресам магию md1img/`CHECK_HEADER` (`oem mem`/`oem peek`).
Расширить окно `mtk_regs::WINDOWS` до `0x0D100000`.
Если образ уже в RAM (гипотеза §1.4) — M1b/M2 можно сделать с телефона без eMMC.
Критерий: известны база/размер bank0 и smem, найден (или нет) образ.

**M1b. Снять образы.** `md1img`, `nvdata`, `protect1/2`, `proinfo`: mtkclient
(PC) либо `oem emmc`. Критерий: файлы в `firmware/modem/`, `sha256` записан.
**IMEI не менять и нигде не публиковать** (юридически: правка IMEI запрещена
во многих странах; наш код NVRAM только читает и отдаёт MD как есть).

**M2. Разбор `md1img` на ПК** (Python/Rust-утилита в `xtask`, без ядра):
части, 512-байтные заголовки `0x58881688`, хвост `md_check_header`
(версия, `mem_size`, `platform`, `bind_sys_id`, `dsp_img_*`, `arm7_*`).
Критерий: таблица частей, ожидаемый размер bank0 совпал с резервом LK.

**M3. Положить образ в резерв и включить домен MD.**
Из `/lib/firmware/modem/` (RAM-диск; образ большой — грузить в целевой
резерв потоком, не держа копии) в bank0, `dc civac`, затем `md_cd_power_on`
(§1.5) и `let_md_go`. Проверка: `oem peek 1020E300 2` (`MD1_CFG_BOOT_STATS0/1`
меняются), `oem peek` PLL-регистров MD (метка `0x62930000` в `Pll+0xF00`),
`MD WDT` не срабатывает (статус `WDT_MD_STA`), EMI-нарушений нет. Первые
шаги — холодный отдельный `oem md on`/`oem md off`.
Риск: EMI MPU (§4), MD-тактирование от `SRCCLKENA`/26 МГц.

**M4. Рукопожатие CCCI.**
CCIF0 (`0x10209000`) + CLDMA (`0x10014000`/`0x1021B000`): кольца GPD/BD,
очереди 0..7, прерывание SPI 165/148. Ждём HS1 в `CCCI_CONTROL_RX`, отвечаем
runtime data, ждём HS2. Параллельно `ccci_fs` (минимум: open/read/write/stat,
атомы вокруг `nvdata`) и `ccci_rpc`. Критерий: состояние READY в нашем
журнале, `oem peek` smem показывает ответ MD.

**M5. AT-канал.** Открыть UART2 (RX 10 / TX 12, очередь 5), послать `AT\r`,
получить `OK`. Затем `ATI`, `AT+CGMM`, `AT+EGMR=0,7` (читать IMEI),
`AT+CFUN?`. Критерий: `OK` в `oem log`.

**M6. SIM и сеть.**
SIM1: питание SIM (VSIM1 в PMIC), hot-plug EINT (`md1_sim1_hot_plug_eint`)
сообщить MD через RPC; `AT+ESIMS`/`AT+CPIN?`, `AT+CFUN=1`, `AT+COPS?`,
`AT+CREG?` → зарегистрированы (значения 1/5). RIL MediaTek использует
расширения `AT+E*` (`AT+EFUN`, `AT+ECFG`, `AT+EPBSE`…) — на практике
достаточно `CFUN`, `CPIN`, `CREG`, `COPS` [?]. Антенна/РЧ-калибровка из
`nvdata` обязательна.

**M7. Звонок.** `ATD<номер>;`, мониторинг `AT+CLCC`, `ATH`. Критерий: на
другой телефон идёт вызов; входящий `RING`/`+CRING` на UART2.

**M8. Голос.** Речь идёт в самом модеме (DSP MD); AP настраивает тракт:
`mt_soc_pcm_voice_md1` (+ `_bt`) в dts [dts] — PCM-интерфейс MD↔AFE
(MT6765: MD-PCM → AFE `MD1` интерфейс → DAC MT6357 → наушник/динамик),
микрофон: ADC MT6357 → AFE → PCM → MD. Нужно: драйвер AFE (`0x11220000`
`audio`), аналоговая часть MT6357 (через PWRAP, у нас есть), включить
маршрут и команду MD (канал PCM `4/5` и/или `AT+ESPEECH`-подобные) [?].
Первая цель — «вызов идёт, звук позже», потом наушник, потом громкая связь.

---

## 4. Риски и неизвестные

1. **EMI MPU / TEE.** Память MD защищена: регионы и домены задаёт AP (по
   заголовку v4+ `region_info`, `domain_attr`). На 64-бит MTK это SMC в ATF
   (`MTK_SIP_*`), либо ничего, если LK уже выставил и не заперт. Мы работаем
   как EL1-«ядро» под LK/ATF, SMC доступен; номера вызовов — из Xiaomi-дерева
   `drivers/misc/mediatek/emi_mpu` [?]. Самый вероятный источник «зависания».
2. **Где образ** (LK или ядро) и **подпись**: если грузит LK, у нас может не
   быть нужды в eMMC для M3; если грузим мы — подпись Xiaomi проверит само
   MD (secure boot MD ROM) или TEE; штатный образ проходит.
3. **Значения infracfg/SPM** (см. §1.5) — вытащить из Xiaomi-дерева.
4. **MD ждёт `ccci_fs` и `rpc`**: без них нет HS2 или нет IMEI; это отдельная
   работа (ext4 read-only для `nvdata`, формат NVRAM-файлов MTK — «LID»-файлы
   в `/mnt/vendor/nvdata/md/NVRAM/NVD_IMEI/…`).
5. **Без NVRAM**: MD обычно поднимается, но IMEI пустой/дефолтный,
   РЧ-калибровка не загружена → регистрация в сети невозможна или
   только экстренные вызовы с плохим уровнем; на ряде прошивок MD уходит в
   EE/assert. Поэтому M1b снимает `nvdata`; **не писать, не клонировать,
   не менять IMEI**.
6. **Радио-закон/сертификация**: передача в эфир — только на легальных
   частотах оператора, с родной калибровкой; без неё не пробовать.
7. **PMIC**: `VMODEM`/`VSRAM_MD` — неверное напряжение сожжёт модем;
   значения брать только из Xiaomi-дерева, шагать вверх, измерять батарею
   (`oem battery`) до и после.
8. **Питание/батарея**: включённый модем + Wi-Fi тянут ток; не оставлять
   включённым без нужды.
9. **Размер образа**: RAM-диск лимит ~25 МиБ — модем из RAM-диска не
   положить, пока нет eMMC; значит до eMMC-драйвера образ грузим потоком
   по USB/`fastboot` (команда `oem md load` с `download:`) либо читаем
   прямо с eMMC.
10. **IRQ**: GIC SPI 165/148/261 — проверить, что наш `gic.rs` пропускает
    эти линии; CLDMA — level-high, CCIF — level-low.
11. Голос (M8) — самое неопределённое: набор команд MD для речи, AFE-интерфейс
    и аналоговая часть не изучены.

### Оценка трудозатрат (рабочие дни одного разработчика, грубо)

| Веха | Дни | Комментарий |
|---|---|---|
| M1a разведка | 0.5–1 | `oem fdt`, окно регистров |
| M1b снять образы | 1 (mtkclient) / 3–5 (`oem emmc`) | |
| M2 разбор | 1 | |
| M3 питание+загрузка | 4–8 | MPU и infracfg — главные риски |
| M4 CCCI/CLDMA | 8–15 | CLDMA, HS1/HS2, `ccci_fs`, `ccci_rpc` |
| M5 AT | 1–2 | |
| M6 SIM+сеть | 3–6 | RPC, SIM, `nvdata` |
| M7 звонок | 1–2 | |
| M8 голос | 8–20 | AFE, MT6357 аналог, маршрут |

Итого: до `AT → OK` — около 3–5 недель; до исходящего вызова — 5–7;
со звуком — 8–10 (неопределённость высокая).

---

## 5. Файлы, которые будут добавлены

В `crates/kernel/src/arch/aarch64/`:

| Файл | Ответственность |
|---|---|
| `mtk_msdc.rs` | MSDC0/eMMC: инициализация-«усыновление» состояния LK, PIO CMD17/18, `emmc_info`, чтение блоков, GPT-парсер (RO) |
| `mtk_md_power.rs` | домен MD: PMIC (VMODEM/VSRAM_MD), SPM MTCMOS, infracfg protect, PLL init, boot slave, `power_on/off/status` |
| `mtk_md_image.rs` | заголовки md1img, проверка `md_check_header`, раскладка по bank0, `dc civac`, сверка с резервом LK |
| `mtk_ccci_hw.rs` | CLDMA (кольца GPD/BD, очереди, IRQ) и CCIF (doorbell, SRAM) |
| `mtk_ccci.rs` | протокол: `ccci_header`, каналы, FSM `GATED→STARTING→READY`, HS1/HS2, runtime data, порты |
| `mtk_ccci_fs.rs` | служба `CCCI_FS`: чтение NVRAM из `nvdata` (RO), просмотр путей |
| `mtk_ccci_rpc.rs` | служба `CCCI_RPC`: запросы MD→AP (SIM, GPIO, время) |
| `mtk_modem.rs` | AT-канал `ttyC0`: очередь команд, парсер ответов, `/dev/modem`-подобный узел, звонок/статус для `/bin/phone` |
| `mtk_audio_md.rs` | (M8) AFE + MT6357 аналог, маршрут голоса |

Правки в существующем:

- `mtk_regs.rs` — расширить `WINDOWS` до `0x0D100000`; при желании `oem fdt`.
- `mtk_usb/fastboot.rs` — команды `oem fdt`, `oem emmc …`, `oem md [on|off|load|status|ccci|at <команда>]`; потоковая выдача раздела.
- `mod.rs` (arch) — `pub mod` для новых файлов, служба `modem` (фоновая, как `battery`).
- `xtask` — `phone-md1img` (разбор образа на ПК), `phone-pull` (выкачка из `staged`).
- `crates/.../phone` (`/bin/phone`) — убрать «модем ещё не подключён», подключить `mtk_modem`.
- `docs/PHONE.md` — ссылка сюда.

Образы: `firmware/modem/{md1img.bin,nvdata.img,…}` (в хранилище не кладём).

---

## 6. Первые три шага

1. **`oem fdt`** (+ расширение окна регистров до `0x0D100000`): выгрузить
   `/chosen` и `/reserved-memory` из FDT, которое LK передал ядру; найти
   регион(ы) MD и проверить, нет ли md1img уже в RAM. Ничего не пишет.
2. **Снять `md1img`, `nvdata`, `protect1/2`, GPT** через mtkclient на ПК
   (или `adb shell ls by-name` для карты разделов) → `firmware/modem/`;
   параллельно клонировать `xiaomi-dandelion-q-oss` и выписать значения
   infracfg/SPM/EMI-MPU из `md_sys1_platform.c`, `modem_sys1.c`,
   `ccci_modem.c`, `emi_mpu`.
3. **Разобрать образ на ПК** (`xtask phone-md1img`): части, `md_check_header`,
   требуемый `mem_size`; сверить с резервом LK. После этого — `oem md on`
   (M3) с выключаемым питанием.
