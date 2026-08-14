//! Запись о слотах: какая система грузится следующей и куда возвращаться.
//!
//! Файл лежит на ESP по пути [`PATH`] и читается тремя сторонами: установщик
//! его создаёт, загрузчик правит на каждой загрузке, система подтверждает
//! удачную. Формат поэтому живёт в отдельном крейте — три копии разбора одного
//! файла разъехались бы ровно в той мере, в какой их потом правили бы порознь.
//!
//! # Почему файл ровно два сектора и почему запись идёт на месте
//!
//! Дорожная карта требовала «записи во временный файл и переименования, а не
//! правки на месте». Здесь сделано иначе, и вот почему.
//!
//! Переименование на FAT32 — это не одна операция. Оно трогает запись каталога
//! (две, если считать удаление старой), таблицу FAT и FSInfo; выключение
//! посреди него оставляет том с потерянной цепочкой кластеров, то есть
//! **повреждает файловую систему**, а не только наш файл. Правка на месте не
//! трогает ни одной из этих структур: длина не меняется, цепочка кластеров не
//! меняется, запись каталога не меняется. Остаётся ровно одна запись сектора —
//! операция, которую носитель либо выполняет, либо нет.
//!
//! Против «либо нет» работает второй сектор. В файле лежат **две** копии
//! записи, каждая со своей контрольной суммой. Пишется сначала запасная, потом
//! основная; читается основная, а если её сумма не сошлась — запасная. Оборвись
//! питание в любой момент, целой останется хотя бы одна, и она описывает либо
//! прежнее состояние, либо новое — но не смесь.
//!
//! Требование дорожной карты этим удовлетворено по существу: обновление
//! состояния атомарно. Способ выбран другой, и сказано об этом здесь, а не
//! умолчано.
//!
//! # Что здесь не проверяется
//!
//! Подлинность. Файл на ESP может переписать кто угодно, у кого есть доступ к
//! диску, — как и ядро, лежащее рядом. Контрольная сумма ловит порчу, а не
//! подмену, и путать это нельзя.

#![no_std]

/// Путь к файлу на ESP, в записи прошивки (обратные косые).
pub const PATH: &str = "\\FREEOS\\SLOTS.CFG";

/// Тот же путь для тех, кто ходит по тому обычными путями.
pub const PATH_UNIX: &str = "FREEOS/SLOTS.CFG";

/// Размер одной записи — ровно сектор.
pub const RECORD_SIZE: usize = 512;

/// Размер файла: основная запись и запасная.
pub const FILE_SIZE: usize = 2 * RECORD_SIZE;

/// Сколько попыток даётся новой системе.
///
/// Три — не «на всякий случай». Одна попытка означала бы откат из-за
/// единственного сбоя, который мог быть случайным (не поднялся диск, не
/// ответила прошивка); десять означали бы, что заведомо сломанная система
/// десять раз подряд не пускает человека к работающей.
pub const DEFAULT_TRIES: u8 = 3;

/// Какой слот.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    A,
    B,
}

impl Slot {
    /// Второй слот.
    #[must_use]
    pub const fn other(self) -> Self {
        match self {
            Self::A => Self::B,
            Self::B => Self::A,
        }
    }

    /// Буква слота — она же часть имени файлов на ESP.
    #[must_use]
    pub const fn letter(self) -> &'static str {
        match self {
            Self::A => "a",
            Self::B => "b",
        }
    }

    /// Как слот выглядит в диагностике.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::A => "A",
            Self::B => "B",
        }
    }

    const fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::A),
            2 => Some(Self::B),
            _ => None,
        }
    }

    /// Числовое обозначение для передачи в hand-off.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::A => 1,
            Self::B => 2,
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "a" | "A" => Some(Self::A),
            "b" | "B" => Some(Self::B),
            _ => None,
        }
    }
}

/// Состояние слотов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct State {
    /// Слот, который грузится.
    pub active: Slot,
    /// Слот, к которому возвращаться, если активный не подтвердится.
    pub previous: Slot,
    /// Сколько попыток осталось у активного слота.
    pub tries: u8,
    /// Подтвердил ли активный слот, что он работает.
    pub confirmed: bool,
    /// Случился ли откат на прошлой загрузке.
    ///
    /// Живёт в файле, а не в памяти загрузчика, потому что сказать об этом
    /// должна **система**, а она стартует уже после него. Снимается тогда же,
    /// когда ставится подтверждение: откат, о котором рассказали, перестаёт
    /// быть новостью.
    pub rolled_back: bool,
}

impl State {
    /// Состояние свежеустановленной системы: слот A, подтверждён.
    ///
    /// Подтверждён сразу, и это не поблажка: систему, которую только что
    /// записал установщик, никто не проверял на этой машине — но и возвращаться
    /// ей некуда, второй слот пуст. Счётчик попыток без запасного слота
    /// означал бы обещание отката, которого не существует.
    #[must_use]
    pub const fn fresh() -> Self {
        Self {
            active: Slot::A,
            previous: Slot::A,
            tries: DEFAULT_TRIES,
            confirmed: true,
            rolled_back: false,
        }
    }

    /// Что делать с этой загрузкой.
    ///
    /// Меняет состояние так, как его надо записать **до** передачи управления
    /// ядру, и возвращает, что именно произошло. Порядок обязателен: счётчик,
    /// уменьшенный после запуска ядра, не уменьшится вовсе, если ядро не
    /// запустится — то есть ровно в том случае, ради которого он существует.
    pub fn begin_boot(&mut self) -> Attempt {
        if self.confirmed {
            // Подтверждённый слот не тратит попыток: он уже доказал, что
            // работает, и считать его загрузки незачем.
            self.tries = DEFAULT_TRIES;
            return Attempt::Confirmed(self.active);
        }

        if self.tries == 0 {
            // Попытки кончились. Возвращаемся на прежний слот и даём ему
            // полный счёт: он работал раньше, но доказывать это будет заново —
            // между тем разом и этим носитель могли и подменить.
            let failed = self.active;
            self.active = self.previous;
            self.previous = failed;
            self.tries = DEFAULT_TRIES;
            self.rolled_back = true;
            self.confirmed = false;
        }

        self.tries = self.tries.saturating_sub(1);
        if self.rolled_back {
            Attempt::RolledBack { to: self.active, from: self.previous, left: self.tries }
        } else {
            Attempt::Trying { slot: self.active, left: self.tries }
        }
    }

    /// Отметить, что активный слот работает.
    ///
    /// Возвращает `true`, если состояние изменилось и его надо записать: запись
    /// на ESP при каждой загрузке подтверждённой системы — это лишний износ
    /// носителя и лишний повод его испортить.
    pub fn confirm(&mut self) -> bool {
        if self.confirmed && self.tries == DEFAULT_TRIES && !self.rolled_back {
            return false;
        }
        self.confirmed = true;
        self.tries = DEFAULT_TRIES;
        self.rolled_back = false;
        true
    }

    /// Переключиться на неактивный слот после записи в него новой системы.
    pub fn switch_to_new(&mut self) {
        self.previous = self.active;
        self.active = self.active.other();
        self.tries = DEFAULT_TRIES;
        self.confirmed = false;
        self.rolled_back = false;
    }

    /// Разобрать файл: основную запись, а если она не сошлась — запасную.
    ///
    /// `None` означает, что не сошлись обе. Это не «слотов нет»: слотов нет
    /// тогда, когда нет файла, и различать эти два случая обязан вызывающий —
    /// испорченная запись требует внимания, а отсутствующая означает систему,
    /// установленную без слотов.
    #[must_use]
    pub fn parse(file: &[u8]) -> Option<Self> {
        Self::parse_record(file.get(..RECORD_SIZE)?)
            .or_else(|| Self::parse_record(file.get(RECORD_SIZE..FILE_SIZE)?))
    }

    fn parse_record(record: &[u8]) -> Option<Self> {
        // Хвост из нулей отбрасывается до разбора: запись занимает сектор, а
        // текста в ней сотня байт.
        let end = record.iter().position(|&byte| byte == 0).unwrap_or(record.len());
        let text = core::str::from_utf8(&record[..end]).ok()?;

        let mut active = None;
        let mut previous = None;
        let mut tries = None;
        let mut confirmed = None;
        let mut rolled_back = false;
        let mut stored_crc = None;
        let mut covered = 0usize;

        for line in text.lines() {
            let trimmed = line.trim();
            if let Some(value) = trimmed.strip_prefix("crc=") {
                stored_crc = u32::from_str_radix(value.trim(), 16).ok();
                break;
            }
            // Сумма считается по всему, что стоит **до** строки с ней, включая
            // перевод строки. Считать по всей записи было бы нельзя: сама сумма
            // в неё входит.
            covered += line.len() + 1;

            let Some((key, value)) = trimmed.split_once('=') else {
                continue;
            };
            match key.trim() {
                "active" => active = Slot::parse(value.trim()),
                "previous" => previous = Slot::parse(value.trim()),
                "tries" => tries = value.trim().parse::<u8>().ok(),
                "confirmed" => confirmed = Some(value.trim() == "1"),
                "rolledback" => rolled_back = value.trim() == "1",
                _ => {}
            }
        }

        let stored_crc = stored_crc?;
        if covered > text.len() || crc32(&record[..covered]) != stored_crc {
            return None;
        }

        Some(Self {
            active: active?,
            previous: previous?,
            tries: tries?,
            confirmed: confirmed?,
            rolled_back,
        })
    }

    /// Записать обе копии в буфер размером [`FILE_SIZE`].
    pub fn write(&self, out: &mut [u8; FILE_SIZE]) {
        out.fill(0);
        let mut record = [0u8; RECORD_SIZE];
        self.write_record(&mut record);
        out[..RECORD_SIZE].copy_from_slice(&record);
        out[RECORD_SIZE..].copy_from_slice(&record);
    }

    /// Собрать одну запись.
    ///
    /// Текстом, а не двоичной структурой, и это выбор в пользу починки руками:
    /// файл, который человек может прочитать и поправить обычным редактором с
    /// любой Linux-машины, — это ещё один выход из положения «система не
    /// грузится», а их в этом проекте нарочно много.
    pub fn write_record(&self, out: &mut [u8; RECORD_SIZE]) {
        out.fill(0);
        let mut at = 0usize;

        let mut digits = [0u8; 3];
        // Сумма считается по тому, что стоит до строки `crc=`, поэтому текст
        // сначала укладывается целиком, а сумма дописывается следом. Замыкание
        // здесь было бы удобнее, но оно удержало бы `out` заимствованным ровно
        // до того места, где сумму надо посчитать.
        for piece in [
            "# FreeOS slot state; the bootloader rewrites this on every boot\n",
            "active=",
            self.active.letter(),
            "\nprevious=",
            self.previous.letter(),
            "\ntries=",
            decimal(self.tries, &mut digits),
            "\nconfirmed=",
            if self.confirmed { "1" } else { "0" },
            "\nrolledback=",
            if self.rolled_back { "1" } else { "0" },
            "\n",
        ] {
            at += put(out, at, piece);
        }

        let crc = crc32(&out[..at]);
        let mut hex = [0u8; 8];
        for piece in ["crc=", hex32(crc, &mut hex), "\n"] {
            at += put(out, at, piece);
        }
    }
}

/// Чем оказалась эта загрузка.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attempt {
    /// Слот уже подтверждён — обычная загрузка работающей системы.
    Confirmed(Slot),
    /// Слот новый и ещё не подтверждён; столько попыток осталось после этой.
    Trying { slot: Slot, left: u8 },
    /// Попытки кончились, вернулись на прежний слот.
    RolledBack { to: Slot, from: Slot, left: u8 },
}

impl Attempt {
    /// С какого слота грузиться.
    #[must_use]
    pub const fn slot(self) -> Slot {
        match self {
            Self::Confirmed(slot) | Self::Trying { slot, .. } => slot,
            Self::RolledBack { to, .. } => to,
        }
    }
}

/// Положить кусок текста в запись и вернуть, сколько байт занято.
///
/// Обрезает по концу сектора, а не переполняет: запись заведомо короче, но
/// полагаться на «заведомо» в коде, который пишет на диск, нельзя.
fn put(out: &mut [u8; RECORD_SIZE], at: usize, text: &str) -> usize {
    let len = text.len().min(RECORD_SIZE.saturating_sub(at));
    out[at..at + len].copy_from_slice(&text.as_bytes()[..len]);
    len
}

/// Десятичная запись без кучи и без `core::fmt`.
fn decimal(mut value: u8, buffer: &mut [u8; 3]) -> &str {
    let mut at = buffer.len();
    loop {
        at -= 1;
        buffer[at] = b'0' + value % 10;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    // SAFETY: в буфер записаны только цифры ASCII.
    unsafe { core::str::from_utf8_unchecked(&buffer[at..]) }
}

/// Шестнадцатеричная запись фиксированной ширины.
fn hex32(value: u32, buffer: &mut [u8; 8]) -> &str {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    for (index, byte) in buffer.iter_mut().enumerate() {
        let shift = 28 - index * 4;
        *byte = DIGITS[((value >> shift) & 0xF) as usize];
    }
    // SAFETY: в буфер записаны только шестнадцатеричные цифры ASCII.
    unsafe { core::str::from_utf8_unchecked(buffer) }
}

/// CRC-32 (полином IEEE 802.3, отражённый).
///
/// Своя, а не заимствованная: крейт читают и загрузчик, и ядро, и установщик, а
/// тянуть ради двадцати строк зависимость, которая где-то из них требует кучи,
/// значило бы обменять понятность на удобство.
#[must_use]
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// Числовое обозначение слота из hand-off.
#[must_use]
pub fn slot_from_code(code: u8) -> Option<Slot> {
    Slot::from_code(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(state: State) -> State {
        let mut file = [0u8; FILE_SIZE];
        state.write(&mut file);
        State::parse(&file).expect("запись обязана читаться обратно")
    }

    #[test]
    fn a_record_survives_a_round_trip() {
        let state = State {
            active: Slot::B,
            previous: Slot::A,
            tries: 2,
            confirmed: false,
            rolled_back: true,
        };
        assert_eq!(roundtrip(state), state);
    }

    /// Порча основной записи не должна стоить состояния: ради этого второй
    /// сектор и существует.
    #[test]
    fn a_torn_primary_falls_back_to_the_spare() {
        let state = State::fresh();
        let mut file = [0u8; FILE_SIZE];
        state.write(&mut file);
        file[..RECORD_SIZE].fill(0xAA);
        assert_eq!(State::parse(&file), Some(state));
    }

    /// Обе копии испорчены — честный отказ, а не выдуманное состояние.
    #[test]
    fn both_copies_broken_means_no_state() {
        let mut file = [0xEEu8; FILE_SIZE];
        assert_eq!(State::parse(&file), None);
        file.fill(0);
        assert_eq!(State::parse(&file), None);
    }

    /// Изменение одного байта обязано ломать сумму — иначе она ничего не ловит.
    #[test]
    fn a_single_changed_byte_is_caught() {
        let state = State::fresh();
        let mut file = [0u8; FILE_SIZE];
        state.write(&mut file);
        // Правится цифра счётчика попыток в основной записи; запасная остаётся
        // целой, поэтому разбор обязан вернуть **исходное** состояние.
        let at = file[..RECORD_SIZE]
            .windows(7)
            .position(|window| window == b"tries=3")
            .expect("строка со счётчиком");
        file[at + 6] = b'1';
        assert_eq!(State::parse(&file), Some(state));
    }

    /// Три неудачные попытки и откат — главный сценарий фазы, проверенный без
    /// единого запуска эмулятора.
    #[test]
    fn three_failures_roll_back_to_the_previous_slot() {
        let mut state = State::fresh();
        state.switch_to_new();
        assert_eq!(state.active, Slot::B);
        assert_eq!(state.previous, Slot::A);

        // Три загрузки, ни одна не подтвердилась.
        for expected_left in [2, 1, 0] {
            let attempt = state.begin_boot();
            assert_eq!(attempt, Attempt::Trying { slot: Slot::B, left: expected_left });
        }

        // Четвёртая приходит на старый слот и говорит об этом.
        let attempt = state.begin_boot();
        assert_eq!(
            attempt,
            Attempt::RolledBack { to: Slot::A, from: Slot::B, left: DEFAULT_TRIES - 1 }
        );
        assert_eq!(state.active, Slot::A);

        // Старая система поднимается и подтверждает себя; о том, что был
        // откат, она успела рассказать — и признак снимается.
        assert!(state.confirm());
        assert!(!state.rolled_back);
        assert_eq!(state.begin_boot(), Attempt::Confirmed(Slot::A));
    }

    /// Подтверждённая система не переписывает файл на каждой загрузке: лишняя
    /// запись на ESP — это лишний повод его испортить.
    #[test]
    fn a_confirmed_slot_needs_no_write() {
        let mut state = State::fresh();
        assert!(!state.confirm());
    }

    /// Новая система, подтвердившаяся с первого раза, больше не оглядывается на
    /// счётчик.
    #[test]
    fn a_successful_update_stops_counting() {
        let mut state = State::fresh();
        state.switch_to_new();
        assert_eq!(state.begin_boot(), Attempt::Trying { slot: Slot::B, left: 2 });
        assert!(state.confirm());
        assert_eq!(state.begin_boot(), Attempt::Confirmed(Slot::B));
        assert_eq!(state.tries, DEFAULT_TRIES);
    }
}
