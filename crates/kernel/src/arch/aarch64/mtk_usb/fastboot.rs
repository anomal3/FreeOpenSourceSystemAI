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
    /// Отдаёт журнал: откуда читать и где остановиться.
    ///
    /// Граница запоминается в тот миг, когда команда пришла, и это не мелочь:
    /// ядро печатает и во время выдачи — опрос устройств, сеть, оболочка. Без
    /// неё `oem log` не кончился бы никогда, потому что журнал прирастал бы
    /// ровно настолько, насколько его успели прочитать.
    Log { at: u64, until: u64 },
}

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
            // Неизвестное имя — пустое значение, а не отказ: так отвечают
            // настоящие загрузчики, и `fastboot getvar all` не спотыкается.
            _ => self.respond(b"OKAY", ""),
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
