//! Протокол fastboot: ровно столько, чтобы отдать журнал по кабелю.
//!
//! # Что это за протокол
//!
//! Текстовый и до неприличия простой. Хост шлёт команду одним пакетом, а
//! устройство отвечает пакетами, у каждого из которых первые четыре байта —
//! `OKAY`, `FAIL`, `DATA` или `INFO`. Последний и есть здесь главный: строку
//! после `INFO` программа `fastboot` печатает и продолжает ждать. То есть
//! готовый канал «много строк подряд, потом конец» существует в протоколе с
//! самого начала, и журнал ложится в него без единой натяжки.
//!
//! # Зачем это ядру
//!
//! Затем, что у аппарата нет линии наружу, а экран забирает рабочий стол.
//! Каждый вопрос к машине стоил прошивки и снимка экрана, сделанного руками, —
//! полтора десятка кругов за вечер. `fastboot oem log` печатает весь журнал на
//! ПК за секунду, и человек в этом больше не участвует.
//!
//! # Чего здесь нет
//!
//! Ни `download`, ни `flash`, ни `erase` — ничего, что пишет. Устройство,
//! отзывающееся на имя загрузчика, обязано быть безобидным: перепутать окно
//! `fastboot` с настоящим слишком легко, а цена ошибки — раздел.

use super::gadget::Bulk;
use crate::klog;

/// Длина пакета ответа. Больше протокол не читает.
const PACKET: usize = 64;
/// Сколько места остаётся под текст после четырёхбайтового тега.
const TEXT: usize = PACKET - 4;

/// Что устройство делает прямо сейчас.
enum State {
    /// Ждёт команду.
    Idle,
    /// Выдаёт карту буфера событий панели: с какого смещения продолжать.
    ///
    /// Существует ради вопроса, который нельзя задать иначе: где именно панель
    /// держит координаты. Отчёт по смещению, указанному в драйвере, читается
    /// пустым, а соседние смещения того же буфера — живыми, и увидеть разницу
    /// можно только осмотрев буфер целиком в тот миг, когда палец на экране.
    Dump { offset: u32 },
    /// Отдаёт журнал: откуда читать и где остановиться.
    ///
    /// Граница запоминается в тот миг, когда команда пришла, и это не мелочь:
    /// ядро печатает и во время выдачи — опрос устройств, сеть, оболочка. Без
    /// неё `oem log` не кончился бы никогда, потому что журнал прирастал бы
    /// ровно настолько, насколько его успели прочитать.
    Log { at: u64, until: u64 },
    /// Отдаёт заранее сложенные строки ответа: какую следующей.
    Lines { at: u8 },
}

/// Сколько строк помещается в один ответ.
///
/// Восемь — с запасом на самый длинный ответ: тридцать байт кристалла
/// шестнадцатеричными парами не влезают в один пакет и занимают две строки.
const LINES: usize = 8;

/// Сколько проходов подряд можно не суметь отдать ответ, прежде чем считать
/// собеседника ушедшим.
///
/// Проход занимает миллисекунду, так что это около двух секунд. Хост, который
/// спросил и слушает, забирает ответ за доли миллисекунды; две секунды молчания
/// означают не «медленно», а «некому».
const ABANDONED: u16 = 2000;

pub struct Fastboot {
    state: State,
    /// Готовый ответ, который ещё не удалось отдать.
    out: [u8; PACKET],
    out_len: usize,
    ready: bool,
    /// Сколько проходов подряд ответ не удаётся отдать.
    stalled: u16,
    /// Отдать ответ и перезагрузить машину.
    reboot_after_reply: bool,
    /// Сложенные строки ответа и сколько их.
    ///
    /// Складываются целиком **до** первой отправки, а не по мере надобности, и
    /// это не расточительность. Ответ на `oem tr` — это то, что кристалл сказал
    /// в один определённый миг; выдавая его частями и дочитывая между пакетами,
    /// мы получили бы срез, склеенный из разных мгновений, и не заметили бы
    /// этого никогда.
    lines: [[u8; TEXT]; LINES],
    line_len: [u8; LINES],
    line_count: u8,
}

impl Fastboot {
    pub const fn new() -> Self {
        Self {
            state: State::Idle,
            out: [0; PACKET],
            out_len: 0,
            ready: false,
            stalled: 0,
            reboot_after_reply: false,
            lines: [[0; TEXT]; LINES],
            line_len: [0; LINES],
            line_count: 0,
        }
    }

    /// Забыть всё: шину сбросили или настройку сменили.
    pub fn reset(&mut self) {
        self.state = State::Idle;
        self.ready = false;
        self.out_len = 0;
        self.stalled = 0;
    }

    /// Собеседник ушёл: бросить недосказанное и вернуться к ожиданию команды.
    ///
    /// Без этого одна прерванная выдача запирает устройство навсегда — оно
    /// вечно предлагает ответ на вопрос, который больше некому услышать, и не
    /// слышит новых. Проверено ровно так: вывод `oem log` оборвали на середине,
    /// и следующая же команда получила «нет связи».
    fn abandon(&mut self, bulk: &mut Bulk) {
        bulk.flush();
        self.reset();
    }

    /// Один шаг обмена. За проход делается ровно одно дело — так очередь на
    /// передачу не обгоняет саму себя, а опрос остаётся коротким.
    pub fn poll(&mut self, bulk: &mut Bulk) {
        if self.ready {
            if bulk.send(&self.out[..self.out_len]) {
                self.ready = false;
                self.stalled = 0;
                // Перезагрузка — **после** того, как ответ ушёл. Сброс до
                // отправки означал бы для хоста оборванную команду: он не
                // отличает «выполнено и машина ушла» от «не ответили».
                if self.reboot_after_reply {
                    self.reboot_after_reply = false;
                    crate::arch::aarch64::mtk::reboot();
                }
                return;
            }
            // Хост не забирает ответ. Обычно это значит «ещё не успел», но
            // иногда — «ушёл»: программа на той стороне прервана, а мы остались
            // с недосказанной фразой и ждём слушателя, которого нет. Отличить
            // одно от другого можно только по времени.
            self.stalled = self.stalled.saturating_add(1);
            if self.stalled >= ABANDONED {
                self.abandon(bulk);
            }
            return;
        }

        if let State::Log { at, until } = self.state {
            self.continue_log(at, until);
            return;
        }

        if let State::Dump { offset } = self.state {
            self.continue_dump(offset);
            return;
        }

        if let State::Lines { at } = self.state {
            self.continue_lines(at);
            return;
        }

        let mut command = [0u8; PACKET];
        if let Some(len) = bulk.receive(&mut command) {
            self.handle(&command[..len]);
        }
    }

    /// Разобрать команду. Всё, что не узнали, честно отвергается: молчание
    /// хост читает как зависшее устройство и ждёт до предела ожидания.
    fn handle(&mut self, command: &[u8]) {
        let Ok(text) = core::str::from_utf8(command) else {
            self.respond(b"FAIL", "command is not text");
            return;
        };
        let text = text.trim_end_matches('\0').trim();

        if let Some(name) = text.strip_prefix("getvar:") {
            self.getvar(name);
        } else if text == "oem touchdump" {
            self.state = State::Dump { offset: 0 };
            self.continue_dump(0);
        } else if let Some(rest) = text.strip_prefix("oem t") {
            self.touch_command(rest);
        } else if text == "oem log" || text == "oem klog" {
            let (at, until) = (klog::oldest(), klog::written());
            self.state = State::Log { at, until };
            self.continue_log(at, until);
        } else if matches!(
            text,
            "reboot" | "reboot-bootloader" | "reboot-fastboot" | "oem reboot"
        ) {
            // Загрузчику нельзя сказать «открой fastboot»: это делается записью
            // в служебный раздел, а памятью мы управлять не умеем. Поэтому
            // машина уходит в обычную загрузку — оттуда её доводит `adb`. Имена
            // с «-bootloader» приняты всё равно: человек наберёт привычное, и
            // отказ на нём читался бы как неисправность, а не как разница между
            // «перезагружусь» и «перезагружусь именно туда».
            self.reboot_after_reply = true;
            self.respond(b"OKAY", "");
        } else {
            self.respond(b"FAIL", "unknown command");
        }
    }

    /// Разговор с тачскрином по кабелю: `oem t<что> [аргументы]`.
    ///
    /// # Зачем целый разговор, а не пара переменных
    ///
    /// Затем, что каждый вопрос к панели, зашитый в ядро, стоит сборки,
    /// прошивки, перезагрузки и **двух нажатий питания живым человеком** — на
    /// логотипе загрузчика, куда изнутри ядра не дотянуться. За вечер это
    /// полтора десятка кругов, и на каждый круг приходится ровно одна догадка.
    ///
    /// Догадок же осталось много, и все они об одном: чего прошивка панели
    /// ждёт, чтобы начать складывать отчёт о касаниях туда, откуда его берёт
    /// родной драйвер. Проверять их по одной в круг — значит не проверить их
    /// никогда.
    ///
    /// Поэтому кристалл открывается целиком: читать по любому адресу, писать по
    /// любому адресу, пересбрасывать, менять предполагаемый адрес буфера
    /// событий и переключать опрос — всё это отсюда, не трогая аппарат руками.
    ///
    /// | команда | что делает |
    /// |---|---|
    /// | `tr <адрес> <длина>` | прочитать байты по полному адресу |
    /// | `tw <адрес> <байт>` | записать байт по полному адресу |
    /// | `trst [hard]` | пересбросить кристалл и дождаться прошивки |
    /// | `tmode <байт>` | `nvt_change_mode`: команда в `0x50` и рукопожатие |
    /// | `tbase <адрес>` | сменить адрес буфера событий |
    /// | `tsrc <0..3>` | кто опрашивает: 1 — USB, 2 — тач, 3 — обе |
    /// | `treport <0/1>` | слать ли события указателя наверх |
    /// | `tstat` | счётчики опроса и последний непустой отчёт |
    ///
    /// Числа читаются шестнадцатеричными без приставки: `oem tr 21c00 12`.
    fn touch_command(&mut self, rest: &str) {
        use crate::arch::aarch64::mtk_touch as touch;

        let mut words = rest.split_whitespace();
        let Some(what) = words.next() else {
            self.respond(b"FAIL", "say what to do with the touchscreen");
            return;
        };
        let first = words.next();
        let second = words.next();

        self.line_count = 0;
        match what {
            "r" => {
                let (Some(address), Some(len)) = (first.and_then(hex32), second.and_then(hex32))
                else {
                    self.respond(b"FAIL", "usage: oem tr <hex address> <hex length>");
                    return;
                };
                match touch::read_raw(address, len as usize) {
                    Some(probe) => {
                        // Число опросов печатается рядом с байтами, и это не
                        // украшение: «кристалл ответил единицами» и «посылка не
                        // вышла на провод» дают на руках одно и то же `ff`, а
                        // различает их только время, которое заняла передача.
                        let mut head = [0u8; TEXT];
                        let mut at = 0;
                        at += put(&mut head[at..], b"page ");
                        at += put_dec(&mut head[at..], probe.page_spins);
                        at += put(&mut head[at..], b" data ");
                        at += put_dec(&mut head[at..], probe.data_spins);
                        if probe.page_spins == 0 || probe.data_spins == 0 {
                            at += put(&mut head[at..], b" NEVER LEFT THE WIRE");
                        }
                        self.say(&head[..at]);
                        self.say_bytes(&probe.bytes[..probe.len]);
                    }
                    None => self.say(b"no touchscreen"),
                }
            }
            "w" => {
                let (Some(address), Some(value)) = (first.and_then(hex32), second.and_then(hex32))
                else {
                    self.respond(b"FAIL", "usage: oem tw <hex address> <hex byte>");
                    return;
                };
                if touch::write_raw(address, value as u8) {
                    self.say(b"written");
                } else {
                    self.say(b"no touchscreen");
                }
            }
            "rst" => {
                let hard = first == Some("hard");
                match touch::reset_chip(hard) {
                    Some((state, waited)) => {
                        let mut head = [0u8; TEXT];
                        let mut at = 0;
                        at += put(&mut head[at..], b"state after ");
                        at += put_dec(&mut head[at..], waited);
                        at += put(&mut head[at..], b" ms (0xa0..0xaf is up)");
                        self.say(&head[..at]);
                        self.say_bytes(&state[..6]);
                    }
                    None => self.say(b"no touchscreen"),
                }
            }
            "mode" => {
                let Some(mode) = first.and_then(hex32) else {
                    self.respond(b"FAIL", "usage: oem tmode <hex mode>");
                    return;
                };
                if touch::change_mode(mode as u8) {
                    self.say(b"mode written, host ready handshaken");
                } else {
                    self.say(b"no touchscreen");
                }
            }
            "base" => {
                let Some(address) = first.and_then(hex32) else {
                    self.respond(b"FAIL", "usage: oem tbase <hex address>");
                    return;
                };
                touch::set_event_base(address);
                self.say(b"event buffer moved");
            }
            "src" => {
                let Some(mask) = first.and_then(hex32) else {
                    self.respond(b"FAIL", "usage: oem tsrc <0..3>");
                    return;
                };
                touch::set_poll_source(mask as u8);
                self.say(b"polling source changed");
            }
            "report" => {
                let Some(on) = first.and_then(hex32) else {
                    self.respond(b"FAIL", "usage: oem treport <0|1>");
                    return;
                };
                touch::set_reporting(on != 0);
                self.say(b"reporting changed");
            }
            // Положить в кристалл один из вкомпилированных образов прошивки.
            //
            // Живёт здесь, а не только в загрузке, потому что панелей у этого
            // аппарата четыре разновидности, имя от загрузчика бывает не то, а
            // перебрать четыре образа по кабелю — это четыре команды против
            // четырёх сборок и восьми нажатий питания.
            "fw" => {
                let Some(index) = first.and_then(hex32) else {
                    self.respond(b"FAIL", "usage: oem tfw <image number>");
                    return;
                };
                match touch::load_firmware(index as usize) {
                    Some(outcome) => {
                        let mut head = [0u8; TEXT];
                        let mut at = 0;
                        match outcome {
                            touch::Outcome::Up(ms) => {
                                at += put(&mut head[at..], b"firmware up after ");
                                at += put_dec(&mut head[at..], ms);
                                at += put(&mut head[at..], b" ms");
                            }
                            touch::Outcome::Silent(state) => {
                                at += put(&mut head[at..], b"written but silent, state ");
                                head[at] = hex_digit(state >> 4);
                                head[at + 1] = hex_digit(state & 0x0f);
                                at += 2;
                            }
                            touch::Outcome::Broken => {
                                at += put(&mut head[at..], b"image did not parse");
                            }
                        }
                        self.say(&head[..at]);
                    }
                    None => {
                        let mut head = [0u8; TEXT];
                        let mut at = put(&mut head[0..], b"no such image; there are ");
                        at += put_dec(&mut head[at..], touch::firmware_count() as u32);
                        self.say(&head[..at]);
                    }
                }
            }
            "stat" => match touch::stats() {
                Some(stats) => {
                    let mut head = [0u8; TEXT];
                    let mut at = 0;
                    at += put(&mut head[at..], b"polls ");
                    at += put_dec(&mut head[at..], stats.polls);
                    at += put(&mut head[at..], b" stalled ");
                    at += put_dec(&mut head[at..], stats.stalled);
                    at += put(&mut head[at..], b" live ");
                    at += put_dec(&mut head[at..], stats.live);
                    at += put(&mut head[at..], b" irq-low ");
                    at += put_dec(&mut head[at..], stats.asserted);
                    self.say(&head[..at]);

                    let mut tail = [0u8; TEXT];
                    let mut at = 0;
                    at += put(&mut tail[at..], b"base ");
                    at += put_hex32(&mut tail[at..], stats.base);
                    at += put(&mut tail[at..], b" source ");
                    at += put_dec(&mut tail[at..], u32::from(stats.source));
                    at += put(
                        &mut tail[at..],
                        if stats.reporting { b" reporting" } else { b" quiet" },
                    );
                    self.say(&tail[..at]);
                    self.say_bytes(&stats.last);
                }
                None => self.say(b"no touchscreen"),
            },
            _ => {
                self.respond(b"FAIL", "unknown touch command");
                return;
            }
        }
        self.state = State::Lines { at: 0 };
        self.continue_lines(0);
    }

    /// Сложить строку в ответ. Лишние молча отбрасываются: обрезанный ответ
    /// лучше, чем отказ на команду, которая уже сделала свою работу.
    fn say(&mut self, text: &[u8]) {
        let slot = self.line_count as usize;
        if slot >= LINES {
            return;
        }
        let len = text.len().min(TEXT);
        self.lines[slot][..len].copy_from_slice(&text[..len]);
        self.line_len[slot] = len as u8;
        self.line_count += 1;
    }

    /// Сложить байты шестнадцатеричными парами, по столько на строку, сколько
    /// влезает в пакет.
    fn say_bytes(&mut self, bytes: &[u8]) {
        /// Три знака на байт, и надо оставить место под конец строки.
        const PER_LINE: usize = TEXT / 3;

        for chunk in bytes.chunks(PER_LINE) {
            let mut text = [0u8; TEXT];
            let mut at = 0;
            for byte in chunk {
                text[at] = hex_digit(byte >> 4);
                text[at + 1] = hex_digit(byte & 0x0f);
                text[at + 2] = b' ';
                at += 3;
            }
            self.say(&text[..at]);
        }
    }

    /// Отдать очередную сложенную строку — или закончить.
    fn continue_lines(&mut self, at: u8) {
        if at >= self.line_count {
            self.state = State::Idle;
            self.line_count = 0;
            self.respond(b"OKAY", "");
            return;
        }
        self.state = State::Lines { at: at + 1 };
        let len = self.line_len[at as usize] as usize;
        self.out[..4].copy_from_slice(b"INFO");
        self.out[4..4 + len].copy_from_slice(&self.lines[at as usize][..len]);
        self.out_len = 4 + len;
        self.ready = true;
    }

    fn getvar(&mut self, name: &str) {
        match name {
            "version" => self.respond(b"OKAY", "0.4"),
            "version-bootloader" => self.respond(b"OKAY", "freeos"),
            "product" => self.respond(b"OKAY", "freeos-dandelion"),
            "serialno" => self.respond(b"OKAY", "dandelion"),
            // Ноль — не заглушка, а отказ принимать образы: этому устройству
            // нечем и незачем их писать.
            "max-download-size" => self.respond(b"OKAY", "0"),
            "secure" => self.respond(b"OKAY", "no"),
            // Отчёт панели прямо сейчас, шестнадцатеричными парами. Живёт среди
            // переменных, а не команд, по прозаичной причине: значение
            // переменной программа `fastboot` печатает, а ответ на `oem` —
            // нет, и вопрос «что панель говорит, пока я держу палец» иначе
            // остаётся без видимого ответа.
            // Линия прерывания панели: ноль означает «кристаллу есть что
            // сказать». Вместе с пустым буфером событий это разные диагнозы.
            "touchirq" => match crate::arch::aarch64::mtk_touch::irq_level() {
                Some(true) => self.respond(b"OKAY", "high (nothing to report)"),
                Some(false) => self.respond(b"OKAY", "low (the chip has something)"),
                None => self.respond(b"FAIL", "no touchscreen"),
            },
            // Сведения о прошивке и её состояние — те же, что печатаются при
            // загрузке, но спрошенные сейчас, а не в прошлом.
            "touchinfo" => self.dump_touch(0x78, 12),
            "touchstate" => self.dump_touch(0x60, 8),
            "touch" => match crate::arch::aarch64::mtk_touch::raw_report() {
                Some(report) => {
                    let mut text = [0u8; TEXT];
                    let mut at = 0;
                    for byte in report.iter().take(13) {
                        text[at] = hex_digit(byte >> 4);
                        text[at + 1] = hex_digit(byte & 0x0f);
                        text[at + 2] = b' ';
                        at += 3;
                    }
                    self.out[..4].copy_from_slice(b"OKAY");
                    self.out[4..4 + at].copy_from_slice(&text[..at]);
                    self.out_len = 4 + at;
                    self.ready = true;
                }
                None => self.respond(b"FAIL", "no touchscreen"),
            },
            // Неизвестное имя — пустое значение, а не отказ: так отвечают
            // настоящие загрузчики, и `fastboot getvar all` не спотыкается.
            _ => self.respond(b"OKAY", ""),
        }
    }

    /// Отдать очередную строку карты буфера — или закончить.
    fn continue_dump(&mut self, offset: u32) {
        /// Докуда осматривать буфер. Больше сотни байт там уже не наше:
        /// отчёт о касаниях у этого кристалла занимает шестьдесят шесть.
        const END: u32 = 0x80;
        /// По сколько байт за строку. Двенадцать умещаются в пакет ответа
        /// вместе с подписью смещения, а тридцать ломают само чтение.
        const STEP: u32 = 12;

        if offset >= END {
            self.state = State::Idle;
            self.respond(b"OKAY", "");
            return;
        }
        self.state = State::Dump { offset: offset + STEP };

        match crate::arch::aarch64::mtk_touch::raw_at(offset, STEP as usize) {
            Some(bytes) => {
                let mut text = [0u8; TEXT];
                text[0] = b'+';
                text[1] = hex_digit((offset >> 4) as u8);
                text[2] = hex_digit((offset & 0x0f) as u8);
                text[3] = b' ';
                let mut at = 4;
                for byte in bytes.iter().take(STEP as usize) {
                    text[at] = hex_digit(byte >> 4);
                    text[at + 1] = hex_digit(byte & 0x0f);
                    text[at + 2] = b' ';
                    at += 3;
                }
                self.out[..4].copy_from_slice(b"INFO");
                self.out[4..4 + at].copy_from_slice(&text[..at]);
                self.out_len = 4 + at;
                self.ready = true;
            }
            None => {
                self.state = State::Idle;
                self.respond(b"FAIL", "no touchscreen");
            }
        }
    }

    /// Отдать кусок памяти кристалла шестнадцатеричными парами.
    fn dump_touch(&mut self, offset: u32, len: usize) {
        match crate::arch::aarch64::mtk_touch::raw_at(offset, len) {
            Some(bytes) => {
                let mut text = [0u8; TEXT];
                let mut at = 0;
                for byte in bytes.iter().take(len) {
                    text[at] = hex_digit(byte >> 4);
                    text[at + 1] = hex_digit(byte & 0x0f);
                    text[at + 2] = b' ';
                    at += 3;
                }
                self.out[..4].copy_from_slice(b"OKAY");
                self.out[4..4 + at].copy_from_slice(&text[..at]);
                self.out_len = 4 + at;
                self.ready = true;
            }
            None => self.respond(b"FAIL", "no touchscreen"),
        }
    }

    /// Отдать следующую строку журнала — или закончить выдачу.
    fn continue_log(&mut self, at: u64, until: u64) {
        if at >= until {
            self.state = State::Idle;
            self.respond(b"OKAY", "");
            return;
        }

        // Читать дальше границы нельзя: за ней лежат строки, напечатанные уже
        // во время самой выдачи, — в том числе те, которые печатает выдача.
        let mut line = [0u8; TEXT];
        let room = usize::try_from(until - at).unwrap_or(TEXT).min(TEXT);
        let (count, next) = klog::read(at, &mut line[..room]);
        if count == 0 {
            self.state = State::Idle;
            self.respond(b"OKAY", "");
            return;
        }

        // Откуда чтение началось на самом деле. Запрошенная позиция и
        // фактическая расходятся, когда кольцо успело прокрутиться под нами:
        // журнал тогда переносит читателя к самому старому уцелевшему байту.
        // Считать перевод строки от запрошенной значило бы вернуться назад — и
        // выдача пошла бы по кругу, ни разу не сдвинувшись.
        let start = next - count as u64;

        // Строка кончается переводом. Если его в прочитанном куске нет, строка
        // длиннее пакета — тогда отдаём кусок и продолжаем с того же места:
        // разорванная пополам строка читается, а потерянная нет.
        let (len, next) = match line[..count].iter().position(|&byte| byte == b'\n') {
            Some(end) => (end, start + end as u64 + 1),
            None => (count, next),
        };
        let len = trim_end(&line[..len]);

        self.state = State::Log { at: next, until };

        self.out[..4].copy_from_slice(b"INFO");
        self.out[4..4 + len].copy_from_slice(&line[..len]);
        self.out_len = 4 + len;
        self.ready = true;
    }

    /// Сложить ответ. Текст длиннее пакета обрезается — протокол другого не
    /// предусматривает, а разрезать ответ на два значило бы отдать хосту два
    /// ответа на один вопрос.
    fn respond(&mut self, tag: &[u8; 4], text: &str) {
        let len = text.len().min(TEXT);
        self.out[..4].copy_from_slice(tag);
        self.out[4..4 + len].copy_from_slice(&text.as_bytes()[..len]);
        self.out_len = 4 + len;
        self.ready = true;
    }
}

/// Отрезать возврат каретки и пробелы в конце.
///
/// Возврат каретки в журнале есть: экранная консоль ставит его перед переводом
/// строки. На хосте он превратил бы каждую строку в затирание предыдущей.
fn trim_end(line: &[u8]) -> usize {
    let mut len = line.len();
    while len > 0 && matches!(line[len - 1], b'\r' | b' ' | b'\t') {
        len -= 1;
    }
    len
}

/// Одна шестнадцатеричная цифра.
///
/// Своя, потому что форматирование ядра выделяет память, а этот код работает
/// внутри опроса, где выделять нельзя и незачем.
const fn hex_digit(value: u8) -> u8 {
    match value {
        0..=9 => b'0' + value,
        _ => b'a' + (value - 10),
    }
}

/// Разобрать шестнадцатеричное число без приставки.
///
/// Без приставки — потому что её пришлось бы набирать в каждой команде, а все
/// числа здесь до единого шестнадцатеричные: адреса внутри кристалла, значения
/// его регистров, длины. Десятичное среди них было бы исключением, а не
/// правилом.
fn hex32(text: &str) -> Option<u32> {
    let text = text.strip_prefix("0x").unwrap_or(text);
    if text.is_empty() || text.len() > 8 {
        return None;
    }
    let mut value = 0u32;
    for byte in text.bytes() {
        let digit = match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            b'A'..=b'F' => byte - b'A' + 10,
            _ => return None,
        };
        value = (value << 4) | u32::from(digit);
    }
    Some(value)
}

/// Положить строку в буфер, вернув, сколько заняла. Не влезло — не положила
/// ничего: полуобрезанное слово в ответе читается как неисправность.
fn put(out: &mut [u8], text: &[u8]) -> usize {
    if text.len() > out.len() {
        return 0;
    }
    out[..text.len()].copy_from_slice(text);
    text.len()
}

/// То же для десятичного числа. Десятичного — потому что это счётчики, а их
/// читает человек.
fn put_dec(out: &mut [u8], mut value: u32) -> usize {
    let mut digits = [0u8; 10];
    let mut len = 0;
    loop {
        digits[len] = b'0' + (value % 10) as u8;
        len += 1;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    if len > out.len() {
        return 0;
    }
    for (at, digit) in digits[..len].iter().rev().enumerate() {
        out[at] = *digit;
    }
    len
}

/// И для адреса — шестнадцатеричным, как его набирают в команде.
fn put_hex32(out: &mut [u8], value: u32) -> usize {
    let mut digits = [0u8; 8];
    for (at, slot) in digits.iter_mut().enumerate() {
        *slot = hex_digit(((value >> (28 - at * 4)) & 0x0f) as u8);
    }
    let start = digits.iter().position(|d| *d != b'0').unwrap_or(7);
    put(out, &digits[start..])
}
