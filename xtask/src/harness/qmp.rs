//! Клиент QMP — второй пульт стенда, нужный ровно одному устройству.
//!
//! # Почему монитора HMP оказалось мало
//!
//! Команда `mouse_move` шлёт **приращение**, и внутри QEMU оно доставляется
//! только тому устройству, которое объявило, что понимает относительный ввод.
//! Планшет такого не объявляет: он абсолютный, у него нет и не может быть
//! понятия «сдвинуться на десять точек вправо». Приращение, посланное машине, у
//! которой из указателей один планшет, не доходит ни до кого — молча, без
//! ошибки, и сценарий выглядит как «курсор не двигается».
//!
//! Абсолютное событие в HMP отправить нечем. Оно есть в QMP —
//! `input-send-event` с осью `abs`, — поэтому у стенда появился второй сокет.
//! Открывается он только для сценариев с планшетом: остальным он не нужен, а
//! лишний порт на каждый прогон — лишний способ отказать.
//!
//! # Почему JSON собирается строками, а не библиотекой
//!
//! Потому что весь разговор — это три вида сообщений: приветствие, `{"return":
//! {}}` и `{"error": ...}`. Зависимость ради разбора трёх форм означала бы, что
//! стенд, задача которого — ловить ошибки в системе, сам обзавёлся кодом,
//! который некому проверять.

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use super::monitor::accept_with_timeout;

/// Сколько ждать ответа на команду.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

/// Наибольшее значение абсолютной оси в QEMU (`INPUT_EVENT_ABS_MAX`).
///
/// Число из самого QEMU, а не из устройства: гипервизор принимает координату в
/// этой шкале и сам пересчитывает её в диапазон, который объявил планшет. Так
/// стенду не приходится знать, что у одного планшета 0..32767, а у другого
/// 0..4095 — он говорит долями экрана, как человек, ведущий пером.
const ABS_MAX: i64 = 0x7FFF;

/// Кнопки в терминах QMP.
pub const BUTTON_LEFT: &str = "left";
/// Правая кнопка — ею открывается меню рабочего стола.
pub const BUTTON_RIGHT: &str = "right";

pub struct Qmp {
    stream: TcpStream,
    /// Прочитанное, но ещё не разобранное.
    pending: String,
}

impl Qmp {
    /// Дождаться подключения QEMU и договориться о возможностях.
    ///
    /// Обмен обязателен: до `qmp_capabilities` сокет находится в режиме
    /// согласования и любую другую команду отвергает.
    pub fn accept(listener: &TcpListener, timeout: Duration) -> Result<Self> {
        let stream = accept_with_timeout(listener, timeout).context("QMP QEMU не подключился")?;
        let mut qmp = Self { stream, pending: String::new() };

        let greeting = qmp.read_message(COMMAND_TIMEOUT).context("QMP не поздоровался")?;
        if !greeting.contains("\"QMP\"") {
            bail!("вместо приветствия QMP пришло: {greeting}");
        }
        qmp.execute("{\"execute\":\"qmp_capabilities\"}")
            .context("QMP не принял qmp_capabilities")?;
        Ok(qmp)
    }

    /// Прочитать одно сообщение — QMP разделяет их переводом строки.
    fn read_message(&mut self, timeout: Duration) -> Result<String> {
        let deadline = Instant::now() + timeout;
        let mut buffer = [0u8; 4096];

        loop {
            if let Some(index) = self.pending.find('\n') {
                let message = self.pending[..index].trim().to_string();
                self.pending = self.pending[index + 1..].to_string();
                if message.is_empty() {
                    continue;
                }
                return Ok(message);
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                bail!("QMP молчит дольше {} с; получено: {:?}", timeout.as_secs(), self.pending);
            }
            self.stream
                .set_read_timeout(Some(remaining.min(Duration::from_millis(250))))
                .context("не удалось выставить таймаут чтения QMP")?;

            match self.stream.read(&mut buffer) {
                Ok(0) => bail!("QMP закрыл соединение (QEMU завершился?)"),
                Ok(count) => self.pending.push_str(&String::from_utf8_lossy(&buffer[..count])),
                Err(err)
                    if err.kind() == ErrorKind::WouldBlock || err.kind() == ErrorKind::TimedOut => {}
                Err(err) => return Err(err).context("не удалось прочитать ответ QMP"),
            }
        }
    }

    /// Отправить команду и дождаться её результата.
    ///
    /// Асинхронные события (`{"event": ...}`) приходят в тот же сокет и в любой
    /// момент — их надо пропускать, а не принимать за ответ. Первое сообщение с
    /// `return` или `error` и есть результат: команды отправляются по одной.
    fn execute(&mut self, command: &str) -> Result<()> {
        self.stream
            .write_all(format!("{command}\n").as_bytes())
            .with_context(|| format!("не удалось отправить QMP '{command}'"))?;
        self.stream.flush().ok();

        let deadline = Instant::now() + COMMAND_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                bail!("нет ответа QMP на '{command}'");
            }
            let message = self.read_message(remaining)?;
            if message.contains("\"error\"") {
                bail!("QMP ответил ошибкой на '{command}': {message}");
            }
            if message.contains("\"return\"") {
                return Ok(());
            }
        }
    }

    /// Поставить указатель в точку экрана.
    ///
    /// Координаты — в точках гостевого экрана, как их видит сценарий; перевод в
    /// шкалу QEMU делается здесь, потому что это её свойство, а не свойство
    /// проверки.
    pub fn move_to(&mut self, x: i32, y: i32, width: i32, height: i32) -> Result<()> {
        let ax = scale(x, width);
        let ay = scale(y, height);
        // Обе оси — одним сообщением: гипервизор отдаёт устройству отчёт по
        // концу пачки, и разделив их, мы получили бы два отчёта, первый из
        // которых ставит курсор в угол по одной оси.
        self.execute(&format!(
            "{{\"execute\":\"input-send-event\",\"arguments\":{{\"events\":[\
             {{\"type\":\"abs\",\"data\":{{\"axis\":\"x\",\"value\":{ax}}}}},\
             {{\"type\":\"abs\",\"data\":{{\"axis\":\"y\",\"value\":{ay}}}}}]}}}}"
        ))
    }

    /// Нажать или отпустить кнопку.
    pub fn button(&mut self, name: &str, down: bool) -> Result<()> {
        self.execute(&format!(
            "{{\"execute\":\"input-send-event\",\"arguments\":{{\"events\":[\
             {{\"type\":\"btn\",\"data\":{{\"down\":{down},\"button\":\"{name}\"}}}}]}}}}"
        ))
    }
}

/// Точка экрана → доля шкалы QEMU.
fn scale(value: i32, extent: i32) -> i64 {
    let extent = i64::from(extent.max(2) - 1);
    let value = i64::from(value).clamp(0, extent);
    value * ABS_MAX / extent
}
