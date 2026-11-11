# FossAi без установки — живая система

Живая система загружается с образа и **ничего не пишет на диски машины**: всё,
что вы делаете, живёт в памяти и исчезает при выключении. Удобно, чтобы
посмотреть систему, не трогая свой компьютер.

Файлы релиза (`x86_64`, только **UEFI** — BIOS не поддерживается):

| Файл | Для чего |
|---|---|
| `FossAi_<версия>_x86_64_release.iso` | VirtualBox, Hyper-V, QEMU — подключить как диск |
| `FossAi_<версия>_x86_64_release.img` | записать на флешку и загрузить настоящий компьютер |

Сколько памяти занимает система в простое: около **44 МиБ** в живом режиме
(окно «О системе» и «Параметры → Главная» показывают это сами).

---

## VirtualBox

1. **Создать** → тип *Other*, версия *Other/Unknown (64-bit)*. Диск не нужен
   (*Do not add a virtual hard disk*).
2. **Настроить** машину:
   - *Система → Материнская плата*: **Включить EFI** — обязательно.
   - *Система → Процессор*: 2–4 процессора.
   - *Память*: от 2 ГБ.
   - *Дисплей → Графический контроллер*: **VBoxVGA**.
   - *Сеть → Адаптер 1 → Дополнительно → Тип адаптера*:
     **Intel PRO/1000 MT Desktop (82540EM)** — адаптер по умолчанию
     (PCnet) системе не знаком, без этой смены сети не будет.
   - *USB*: **USB 3.0 (xHCI)** — через него работает мышь.
3. *Носители* → привод CD/DVD → выбрать `FossAi_<версия>_x86_64_release.iso`.
4. **Запустить.** Через 20–40 секунд — рабочий стол.

Hyper-V: машина *Поколения 2*, безопасную загрузку (Secure Boot) выключить.

QEMU: `qemu-system-x86_64 -machine q35 -m 2G -smp 4 -cpu max
-bios OVMF.fd -cdrom FossAi_<версия>_x86_64_release.iso`.

---

## Флешка (настоящий компьютер)

Нужна флешка от 256 МБ. **Всё, что на ней было, будет стёрто.**

Записывать надо **`.img`**, посекторно:

- **Windows — Rufus**: выбрать флешку и `.img`, нажать «Старт»; если Rufus
  спросит про режим — **DD**.
- **Windows / macOS / Linux — balenaEtcher** или **USBImager**: выбрать `.img`,
  выбрать флешку, «Flash».
- **Linux** (вместо `/dev/sdX` — ваша флешка, проверьте `lsblk`!):

  ```bash
  sudo dd if=FossAi_<версия>_x86_64_release.img of=/dev/sdX bs=4M status=progress conv=fsync
  ```

Загрузка: вставить флешку, при включении открыть меню загрузки
(обычно **F12**, **F11**, **F8** или **Esc** — зависит от производителя) и
выбрать флешку с пометкой **UEFI**. Если флешки в меню нет — в настройках
UEFI выключить **Secure Boot**.

---

## Что посмотреть

- **Пуск** (кнопка слева внизу, **Win** или **F1**) — все программы.
- **Файлы**, **Терминал**, **Параметры**, **Диспетчер задач**.
- **О системе** — версия, память, сочетания клавиш.
- В терминале: `help` — список команд.

Пакеты (браузер OBSOS, локальная языковая модель llama.cpp, Wine…) ставятся
в **установленную** систему — у живой корень только для чтения.

## Что известно

- Только `x86_64` и только UEFI.
- В VirtualBox с графическим контроллером *VMSVGA* в фоновом (headless) режиме
  снимок экрана бывает чёрным — выбирайте **VBoxVGA**.
- Сетевые карты: Intel e1000 (82540EM и родственные), virtio-net.

Сообщить о проблеме: [Issues](https://github.com/anomal3/FreeOpenSourceSystemAI/issues).

---

# FossAi live (English)

Boots from the image and **writes nothing to the machine's disks**.
`x86_64`, **UEFI only**.

- **VirtualBox**: *Other/Unknown (64-bit)*, no disk, **Enable EFI**, graphics
  **VBoxVGA**, network adapter **Intel PRO/1000 MT Desktop**, **USB 3.0 (xHCI)**,
  attach the `.iso`, start.
- **USB stick**: write the **`.img`** with Rufus (DD mode), balenaEtcher,
  USBImager or `dd`; boot the stick's **UEFI** entry from the boot menu
  (F12/F11/F8/Esc); turn Secure Boot off if it is not listed.
- Idle footprint: about **44 MiB** of RAM in live mode.
