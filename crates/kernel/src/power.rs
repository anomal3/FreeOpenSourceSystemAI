//! Питание: кто просит машину выключиться, кто это делает и в каком порядке.
//!
//! # Почему это не одна функция в оболочке
//!
//! Попросить выключиться могут трое: команда `shutdown`, пункт меню «Пуск» и
//! кнопка питания. Первые два — код задачи, у которой всё можно делать; третья
//! — обработчик прерывания, у которого нельзя почти ничего: перед снятием
//! питания надо сбросить файловую систему на диск, а это захват замков и
//! ожидание ответа контроллера. Обработчик, который это делает, встретит
//! занятый замок и остановит машину вместе с собой.
//!
//! Поэтому кнопка только **поднимает просьбу** и будит задачу, а выключает
//! система из задачи — [`service_task`]. Разница видна в журнале: между
//! строкой «power button» и строкой «shutting down» проходит планировщик.
//!
//! # Порядок выключения
//!
//! Сначала на диск уходит всё, что держится в памяти (счётчики ext2, кеш
//! записи диска, признак чистого размонтирования), и только потом машина
//! теряет питание. Обратный порядок означал бы том, который надо чинить после
//! каждого штатного выключения, — то есть ровно то, чего фаза 27 избегает.

use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};

use crate::{kprintln, sched};

/// Сказать то, что человек должен увидеть, **там, где он смотрит**.
///
/// Не `kprintln!`: когда поднят рабочий стол, загрузочная консоль экран уже
/// отдала, и сообщение о выключении ушло бы в серийную линию и в невидимую
/// консоль. [`crate::shell::print`] знает про оба случая — и про окно, и про
/// линию.
macro_rules! announce {
    // Один вызов, а не два: перевод строки, отправленный отдельно, оставляет
    // между собой и строкой щель, в которую попадает чужой вывод.
    ($($arg:tt)*) => {{
        $crate::shell::print(::core::format_args!("{}\n", ::core::format_args!($($arg)*)));
    }};
}

/// Ярлык источника прерывания для планировщика.
///
/// Число ничего не значит снаружи: [`sched::Wait::Irq`] источники различает, а
/// не адресует. Важно лишь, что просьба и задача договорились об одном и том
/// же — и что оно не совпадает с ярлыком xHCI.
const IRQ_SOURCE: u32 = crate::irq::source::POWER;

/// Кто попросил. В журнал уходит именно это: «машина выключилась» и «машину
/// выключили нажатием кнопки» — разные утверждения, и второе проверяемо.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Кнопка на корпусе (фиксированное событие ACPI).
    PowerButton,
    /// Подтверждённый пункт меню «Пуск».
    Desktop,
}

/// Просьба, ожидающая исполнения: признак «есть», признак «перезагрузка» и
/// признак источника, упакованные в один байт.
///
/// Атомик, а не замок, и один, а не три: поднимает просьбу обработчик
/// прерывания, которому ждать нельзя, а три отдельных атомика можно было бы
/// прочитать вразнобой — с чужим источником у своего действия.
static REQUEST: AtomicU8 = AtomicU8::new(0);

const PRESENT: u8 = 1 << 0;
const RESTART: u8 = 1 << 1;
const FROM_DESKTOP: u8 = 1 << 2;

/// Попросить систему выключиться (или перезагрузиться).
///
/// Можно звать откуда угодно, включая обработчик прерывания и код под замком
/// рабочего стола: функция только пишет атомик и будит задачу. Само выключение
/// — сброс тома на диск и ожидание ответа контроллера — делает задача, потому
/// что и то и другое требует замков и прерываний.
///
/// Повторные просьбы не копятся: «выключиться» дважды означает то же, что один
/// раз.
pub fn request(restart: bool, source: Source) {
    let mut code = PRESENT;
    if restart {
        code |= RESTART;
    }
    if source == Source::Desktop {
        code |= FROM_DESKTOP;
    }
    REQUEST.store(code, Ordering::Release);
    sched::wake_irq(IRQ_SOURCE);
}

/// Забрать просьбу, если она есть.
fn take() -> Option<(bool, Source)> {
    let code = REQUEST.swap(0, Ordering::AcqRel);
    if code & PRESENT == 0 {
        return None;
    }
    let source = if code & FROM_DESKTOP != 0 {
        Source::Desktop
    } else {
        Source::PowerButton
    };
    Some((code & RESTART != 0, source))
}

/// Служебная задача: ждёт просьбы и исполняет её.
///
/// Спит без срока — будить её больше некому, кроме [`request`], а опрашивать
/// кнопку, которая приходит прерыванием, значило бы работать на пустом месте.
pub fn service_task() {
    loop {
        sched::block_on_irq(IRQ_SOURCE, || {
            REQUEST.load(Ordering::Acquire) & PRESENT != 0
        });
        if let Some((restart, source)) = take() {
            match source {
                Source::PowerButton => announce!("  power       : power button pressed"),
                Source::Desktop => announce!("  power       : confirmed in the start menu"),
            }
            shut_down(restart);
        }
    }
}

/// Завести задачу, гасящую машину по просьбе.
///
/// Отдельная от оболочки и служебная: она не повод системе работать, а
/// оболочки на машине без ввода может не быть вовсе — кнопка питания при этом
/// обязана работать.
pub fn start() {
    if let Err(err) = sched::spawn_daemon("power", service_task) {
        kprintln!("  power       : the shutdown task did not start: {err}");
    }
}

/// Умеет ли машина выключиться сама.
///
/// Не умеет только телефон на MediaTek: ACPI и PSCI у него нет, а погасить
/// питание можно лишь через контроллер PMIC, порядок команд которого не описан
/// (см. `arch::aarch64::power::power_off`). Спрашивает лист питания телефона —
/// чтобы не предлагать кнопку, которая ничего не сделает.
#[must_use]
pub fn can_power_off() -> bool {
    crate::arch::can_power_off()
}

/// Погасить или перезагрузить машину.
///
/// Возвращается **только** если снять питание не удалось: сказать об этом
/// честнее, чем изобразить выключение остановкой процессора.
pub fn shut_down(restart: bool) {
    announce!("  {}", if restart { "restarting" } else { "shutting down" });

    // Счётчики ext2 живут в памяти редактора; без сброса том останется с
    // неверным числом свободных блоков — это чинится `fsck`, но чинить нечего,
    // если сбросить вовремя. `None` означает «корень не с диска» (живой ISO), и
    // это не отказ.
    //
    // Сбрасываются **все** тома, а не один корень: с фазы 32 корень смонтирован
    // только на чтение, а всё, что пишется, живёт на разделе состояния. Сброс
    // одного корня с этого момента сбрасывал бы ровно тот том, в который никто
    // не писал.
    for (where_at, result) in crate::fs::sync_all() {
        match result {
            Ok(()) => announce!("  volume      : {where_at} flushed and marked clean"),
            Err(err) => announce!("  volume      : {where_at} flush failed: {err}"),
        }
    }

    // Дальше процессор не отдаётся никому. Это и есть «задачи останавливаются»
    // в том объёме, в котором утверждение можно выполнить честно: сброс тома
    // ждёт замка, то есть чужая запись к этому моменту либо закончилась, либо
    // не начиналась, — но без этой строки следующий тик таймера мог бы отдать
    // квант программе, которая начнёт писать в том, уже помеченный чистым.
    //
    // Задачу, спящую внутри драйвера диска, это не будит и будить не должно:
    // питание снимается через несколько строк, и ждать ей всё равно нечего.
    // Видеокарта NVIDIA (18.11): выгрузить GSP-RM и снять WPR2 — иначе после
    // горячей перезагрузки драйвер не запустится до выключения питания. До
    // сброса журнала на флешку — чтобы шаги выгрузки попали в LOG.TXT. Без
    // карты или без запущенного GSP-RM — мгновенно; с ним — каждое ожидание
    // ограничено, неудача только пишется в журнал.
    let _ = crate::display::nvidia_gsp::unload(if restart { "restart" } else { "power off" });

    // Настройки живой системы на флешку (21.11) и журнал — последний раз,
    // пока USB ещё обслуживается.
    crate::usb::stick_settings::save_now();
    crate::usb::stick_log::flush_now();

    sched::set_preemption(false);

    // Запрет вытеснения держит только **этот** процессор. Соседние продолжали бы
    // исполнять свои задачи — в том числе писать в том, только что помеченный
    // чистым, — поэтому их останавливают до снятия питания.
    //
    // Цена названа: если питание снять не удастся, остановленные процессоры не
    // вернутся, и задачи, исполнявшиеся на них, останутся стоять. Машина,
    // отказавшаяся выключиться, — редкость, а том, записанный после сброса, —
    // порча, которую не видно до следующей загрузки.
    let stopped = crate::smp::stop_others();
    if stopped > 0 {
        kprintln!("  power       : {stopped} other processor(s) asked to stop");
    }

    let rsdp = crate::acpi::rsdp();
    // Очередь линии — до снятия питания: стенд узнаёт итог по этим строкам.
    // Не выключилась машина — следующий тик снова включит очередь.
    crate::serial::drain_and_stop_queue();
    // SAFETY: всё, что нужно сохранить, сохранено выше; возврат из удавшегося
    // выключения не предусмотрен, а из неудавшегося — обязателен, и он ниже.
    unsafe {
        if restart {
            crate::arch::reboot(rsdp);
        } else {
            crate::arch::power_off(rsdp);
        }
    }

    // Сюда попадаем, только если способ не сработал: машина осталась включённой,
    // и работать ей дальше — с вытеснением, как раньше.
    sched::set_preemption(true);
    announce!("  power       : the machine refused to go down");
}

// ---------------------------------------------------------------------------
// Бездействие: экран, который гаснет сам, и машина, которая при этом не спит
// ---------------------------------------------------------------------------

/// Через сколько секунд бездействия гасить экран, если не сказано иначе.
///
/// Пять минут — то, что ставит своим умолчанием всякая настольная система.
/// Меньше — человек, читающий с экрана, остаётся в темноте; больше — машина,
/// которую забыли включённой, светит всю ночь.
pub const DEFAULT_SCREEN_OFF_SECONDS: u32 = 300;

/// Когда последний раз что-нибудь нажимали, двигали или касались, мс с загрузки.
///
/// Пишется из обработчика прерывания ([`crate::input::announce`]), поэтому это
/// атомик, а не поле под замком: замок в обработчике — это остановленная
/// машина, если он окажется занят.
static LAST_INPUT_MS: AtomicU64 = AtomicU64::new(0);

/// Через сколько секунд гасить. Ноль — не гасить вовсе.
static SCREEN_OFF_SECONDS: AtomicU32 = AtomicU32::new(DEFAULT_SCREEN_OFF_SECONDS);

/// Экран сейчас погашен.
static SCREEN_OFF: AtomicBool = AtomicBool::new(false);

/// Значение [`LAST_INPUT_MS`] на момент гашения.
///
/// По нему и отличается «ввода не было» от «ввод был»: сравнивать с текущим
/// временем бесполезно — спящая машина не знает, сколько она проспала, пока её
/// не разбудят.
static BLANKED_AT: AtomicU64 = AtomicU64::new(0);

/// Отметить, что человек что-то сделал.
///
/// Зовётся на **каждое** событие ввода, из обработчика прерывания. Экран здесь
/// не зажигается: зажечь его — значит собрать кадр, то есть взять замок стола,
/// а этого в обработчике делать нельзя. Отметки достаточно: она разбудит
/// задачу, а та посмотрит сюда же (см. [`idle_tick`]).
pub fn note_input() {
    LAST_INPUT_MS.store(crate::time::uptime_ms(), Ordering::Relaxed);
}

/// Кнопка питания телефона (03.11): горит экран — заблокировать и погасить
/// вместе с подсветкой; погашен — зажечь, на экране блокировки.
///
/// Зовётся из задачи опроса кнопки (`mtk_regs::powerkey_task`), не из
/// прерывания: и блокировка, и гашение берут замок стола.
#[cfg_attr(not(target_arch = "aarch64"), allow(dead_code))]
pub fn power_button() {
    if SCREEN_OFF.load(Ordering::Relaxed) {
        SCREEN_OFF.store(false, Ordering::Relaxed);
        note_input();
        crate::display::set_power(true);
        crate::ui::set_screen_off(false);
        crate::kprintln!("  power       : screen on by the power button");
        return;
    }
    if crate::ui::saver::is_up() {
        crate::ui::saver::stop();
    }
    crate::ui::lock_now();
    if crate::ui::set_screen_off(true) {
        let last = LAST_INPUT_MS.load(Ordering::Relaxed);
        SCREEN_OFF.store(true, Ordering::Relaxed);
        BLANKED_AT.store(last, Ordering::Relaxed);
        crate::display::set_power(false);
        crate::kprintln!("  power       : screen off and locked by the power button");
    }
}

// --- Заставка и «не гасить» (22.11) ---------------------------------------

/// Значение [`LAST_INPUT_MS`] на момент появления заставки (`ui::saver`):
/// сменилось — ввод был.
static SAVER_AT: AtomicU64 = AtomicU64::new(0);
/// Показ заставки по кнопке «Просмотр»: когда (мс с загрузки), 0 — не просили.
static PREVIEW_AT: AtomicU64 = AtomicU64::new(0);
/// Живые `zwp_idle_inhibitor_v1` клиентов Wayland (видео, игры).
static INHIBITORS: AtomicU32 = AtomicU32::new(0);
/// О том, что заставку и гашение держат, в этот простой уже сказано.
static HELD_TOLD: AtomicBool = AtomicBool::new(false);

/// Показать заставку через мгновение («Просмотр»): щелчок, который её
/// попросил, успеет отпуститься и не уберёт её сразу.
pub fn request_saver_preview() {
    PREVIEW_AT.store(crate::time::uptime_ms() + 800, Ordering::Relaxed);
}

/// Клиент Wayland завёл (`true`) или убрал ингибитор простоя.
#[cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]
pub fn inhibit(on: bool) {
    if on {
        INHIBITORS.fetch_add(1, Ordering::Relaxed);
    } else {
        let mut n = INHIBITORS.load(Ordering::Relaxed);
        while n > 0 {
            match INHIBITORS.compare_exchange_weak(n, n - 1, Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => break,
                Err(now) => n = now,
            }
        }
    }
}

/// Сколько ингибиторов простоя живо.
#[must_use]
pub fn inhibitors() -> u32 {
    INHIBITORS.load(Ordering::Relaxed)
}

/// Через сколько секунд бездействия гаснет экран; ноль — никогда.
#[must_use]
pub fn screen_off_after() -> u32 {
    SCREEN_OFF_SECONDS.load(Ordering::Relaxed)
}

/// Задать срок. Ноль — не гасить.
pub fn set_screen_off_after(seconds: u32) {
    SCREEN_OFF_SECONDS.store(seconds, Ordering::Relaxed);
}

/// Погашен ли экран прямо сейчас.
#[must_use]
pub fn screen_is_off() -> bool {
    SCREEN_OFF.load(Ordering::Relaxed)
}

/// Сколько секунд никто ничего не делал.
#[must_use]
pub fn idle_seconds() -> u64 {
    let last = LAST_INPUT_MS.load(Ordering::Relaxed);
    crate::time::uptime_ms().saturating_sub(last) / 1000
}

/// Посмотреть, не пора ли погасить экран или зажечь его обратно.
///
/// Зовётся из цикла оболочки — то есть из задачи, где можно брать замки и
/// собирать кадры. Возвращает `true`, если состояние изменилось: по этому
/// признаку цикл понимает, что пора пересчитать, как долго ему спать.
///
/// # Почему это не «сон» в смысле ACPI
///
/// Потому что настоящий S3 — это сохранение состояния всей машины и вектор
/// пробуждения в таблицах прошивки, и сделано это **не** здесь (сказано в
/// ROADMAP). Здесь то, что даёт почти всю экономию и ничем не рискует: экран
/// чёрный, кадры не собираются, а оболочка перестаёт просыпаться десять раз в
/// секунду ради часов, которых никто не видит.
pub fn idle_tick() -> bool {
    let last = LAST_INPUT_MS.load(Ordering::Relaxed);
    if SCREEN_OFF.load(Ordering::Relaxed) {
        if last == BLANKED_AT.load(Ordering::Relaxed) {
            return false;
        }
        // Ввод был — значит человек вернулся. Мониторы — раньше кадра (22.11):
        // зажжённый монитор показывает чёрное, пока стол не соберёт кадр.
        let slept = last.saturating_sub(BLANKED_AT.load(Ordering::Relaxed)) / 1000;
        SCREEN_OFF.store(false, Ordering::Relaxed);
        let started = crate::time::uptime_ms();
        let lit = crate::display::set_power(true);
        crate::ui::swallow_wake_touch();
        crate::ui::set_screen_off(false);
        crate::kprintln!(
            "  power       : awake, the screen was off for {slept} s ({lit}, {} ms)",
            crate::time::uptime_ms().saturating_sub(started)
        );
        return true;
    }

    let now = crate::time::uptime_ms();
    // Заставка (22.11): любой ввод её убирает (клавиши и мышь стола — сразу,
    // сами; здесь — серийная линия и остальное); иначе — её виток.
    let saver_up = crate::ui::saver::is_up();
    if saver_up {
        if last != SAVER_AT.load(Ordering::Relaxed) {
            crate::ui::saver::stop();
            return true;
        }
        crate::ui::saver::tick();
    }
    let preview = PREVIEW_AT.load(Ordering::Relaxed);
    if preview != 0 && now >= preview {
        PREVIEW_AT.store(0, Ordering::Relaxed);
        if !saver_up {
            SAVER_AT.store(LAST_INPUT_MS.load(Ordering::Relaxed), Ordering::Relaxed);
            crate::ui::saver::start(true);
        }
        return true;
    }

    let idle_ms = now.saturating_sub(last);
    if idle_ms < 1000 {
        HELD_TOLD.store(false, Ordering::Relaxed);
    }
    let limit = SCREEN_OFF_SECONDS.load(Ordering::Relaxed);
    let off_due = limit != 0 && idle_ms >= u64::from(limit) * 1000;
    let saver_after = crate::ui::saver::after();
    let saver_due = !off_due && !saver_up && saver_after != 0 && idle_ms >= u64::from(saver_after) * 1000;
    if !off_due && !saver_due {
        return false;
    }
    // Игра или видео во весь экран, ингибитор Wayland — не мешать (22.11).
    // Заставку, показанную по «Просмотру», это не касается.
    if !saver_up {
        match crate::ui::saver::inhibited() {
            None => return false,
            Some(Some(why)) => {
                if !HELD_TOLD.swap(true, Ordering::Relaxed) {
                    crate::kprintln!("  power       : screensaver and screen off held back: {why}");
                }
                return false;
            }
            Some(None) => {}
        }
    }
    if saver_due {
        SAVER_AT.store(last, Ordering::Relaxed);
        return crate::ui::saver::start(false);
    }
    // Срок отключения: заставка уходит (с экраном блокировки, если выбран),
    // мониторы гаснут.
    if saver_up && !crate::ui::saver::stop() {
        return false;
    }
    // Гасить нечего, если рисовать некуда: на машине в серийной консоли стол не
    // поднят, и «экран погашен» было бы неправдой.
    if !crate::ui::set_screen_off(true) {
        return false;
    }
    SCREEN_OFF.store(true, Ordering::Relaxed);
    BLANKED_AT.store(last, Ordering::Relaxed);
    // Настоящее отключение (22.11): DisplayPort-мониторы NVIDIA — в D3 и без
    // сигнала, у virtio-gpu — выходы без картинки; у экрана прошивки — только
    // чёрное, как раньше.
    let dark = crate::display::set_power(false);
    crate::kprintln!("  power       : screen off after {limit} s without input ({dark})");
    true
}
