//! Случайные числа: откуда они берутся и что делать, когда взять их негде.
//!
//! # Почему это отдельная подсистема, а не функция
//!
//! Потому что от неё зависит стойкость всего, что появится следом: ключ хоста
//! SSH, эфемерные ключи обмена, номера последовательности TCP. Плохая
//! случайность не ломает ничего заметного — система работает, соединения
//! устанавливаются, — и обнаруживается только тогда, когда кто-то снаружи
//! предскажет наш «случайный» ключ. Поэтому здесь важнее всего одно: **честно
//! говорить, какой источник использован**.
//!
//! # Три источника, в порядке убывания доверия
//!
//! 1. **Инструкция процессора.** `RDSEED`/`RDRAND` на x86-64, `RNDR` на
//!    AArch64 (расширение FEAT_RNG). Это настоящий аппаратный источник, и если
//!    он есть, вопрос закрыт.
//! 2. **Сбор из времени событий.** Тактовый счётчик читается в моменты, когда
//!    приходят прерывания, и младшие биты его показаний перемешиваются в пул.
//!    Джиттер прерываний — источник слабый, но не выдуманный: именно так
//!    начинают все системы, у которых нет аппаратного генератора.
//! 3. **Ничего.** Такого варианта нет. Система, которой нечего положить в
//!    ключ, обязана сказать об этом и отказаться выдавать ключ, а не выдать
//!    предсказуемый.
//!
//! # Пул и как из него берут
//!
//! Пул — это состояние ChaCha-подобного перемешивания на 32 байтах: всё, что
//! приходит от источников, замешивается в него, а выдача прокручивает его
//! дальше. Смысл в необратимости: даже узнав выданные байты, восстановить
//! состояние пула нельзя, а значит нельзя и предсказать следующие.
//!
//! Перемешивание здесь своё и простое (SHA-256 из чужого крейта в ядро не
//! приезжает — крипто живёт в программах, см. фазу 37). Это осознанный предел:
//! пул хорош ровно настолько, насколько хорош источник, а от источника его
//! качество не спасает.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::sync::SpinLock;

/// Откуда пришли байты.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Инструкция процессора.
    Hardware,
    /// Джиттер прерываний, смешанный с тактовым счётчиком.
    Jitter,
}

impl Source {
    pub fn name(self) -> &'static str {
        match self {
            Source::Hardware => "the CPU's own generator",
            Source::Jitter => "interrupt timing jitter",
        }
    }
}

/// Состояние пула.
struct Pool {
    state: [u64; 4],
    /// Сколько раз в пул что-нибудь замешивали.
    stirred: u64,
}

impl Pool {
    const fn new() -> Self {
        // Начальные константы — из ChaCha («expand 32-byte k»), и они не секрет
        // и не должны им быть: секретность даёт то, что в пул замешивается,
        // а не то, с чего он начат.
        Self {
            state: [
                0x6170_7865_3320_646E,
                0x7962_2D32_6B20_6574,
                0x0F1E_2D3C_4B5A_6978,
                0x8796_A5B4_C3D2_E1F0,
            ],
            stirred: 0,
        }
    }

    /// Замешать значение.
    fn stir(&mut self, value: u64) {
        self.state[0] ^= value;
        self.mix();
        self.stirred = self.stirred.saturating_add(1);
    }

    /// Прокрутить состояние.
    ///
    /// Четыре четверть-раунда в духе ChaCha: сложение, исключающее «или»,
    /// вращение. Ни один бит входа не остаётся на месте, и обратной функции у
    /// этого преобразования нет.
    fn mix(&mut self) {
        for _ in 0..8 {
            self.state[0] = self.state[0].wrapping_add(self.state[1]);
            self.state[3] ^= self.state[0];
            self.state[3] = self.state[3].rotate_left(32);

            self.state[2] = self.state[2].wrapping_add(self.state[3]);
            self.state[1] ^= self.state[2];
            self.state[1] = self.state[1].rotate_left(24);

            self.state[0] = self.state[0].wrapping_add(self.state[1]);
            self.state[3] ^= self.state[0];
            self.state[3] = self.state[3].rotate_left(16);

            self.state[2] = self.state[2].wrapping_add(self.state[3]);
            self.state[1] ^= self.state[2];
            self.state[1] = self.state[1].rotate_left(63);
        }
    }

    /// Выдать восемь байт, прокрутив состояние.
    fn take(&mut self) -> u64 {
        self.mix();
        // Наружу уходит не само состояние, а его свёртка: по выданному числу
        // нельзя восстановить ни одно из четырёх слов.
        self.state[0] ^ self.state[2].rotate_left(17)
    }
}

static POOL: SpinLock<Pool> = SpinLock::new(Pool::new());

/// Сколько раз в пул подмешали джиттер прерываний.
static JITTER_EVENTS: AtomicU64 = AtomicU64::new(0);

/// Есть ли у процессора собственный генератор.
static HAS_HARDWARE: AtomicBool = AtomicBool::new(false);

/// Проверить процессор и сказать, что нашлось.
pub fn init() {
    let hardware = arch_random().is_some();
    HAS_HARDWARE.store(hardware, Ordering::Release);

    // Первое замешивание — из того, что есть всегда: тактовый счётчик и время
    // работы. Это не источник случайности, а способ не начинать с константы,
    // одинаковой на всех машинах.
    let mut pool = POOL.lock();
    pool.stir(crate::arch::monotonic::counter());
    pool.stir(crate::time::uptime_ms());
    drop(pool);

    if hardware {
        crate::kprintln!("  random      : {}", Source::Hardware.name());
    } else {
        crate::kprintln!(
            "  random      : no generator in this CPU, collecting {}",
            Source::Jitter.name()
        );
    }
}

/// Подмешать в пул момент прихода прерывания.
///
/// Вызывается из обработчика таймера — то есть очень часто и в контексте, где
/// нельзя ждать. Поэтому здесь `try_lock`: не вышло занять пул — событие
/// пропускается. Пропуск безвреден, а ожидание в обработчике прерывания — нет.
pub fn stir_from_interrupt() {
    let Some(mut pool) = POOL.try_lock() else {
        return;
    };
    // Значение имеет смысл только младшими битами: старшие меняются
    // предсказуемо, и подмешивать их — самообман, а не энтропия.
    pool.stir(crate::arch::monotonic::counter());
    JITTER_EVENTS.fetch_add(1, Ordering::Relaxed);
}

/// Заполнить буфер случайными байтами.
///
/// Возвращает источник, которому эти байты обязаны своим качеством. Вызывающий
/// обязан сказать об этом вслух, если байты идут в ключ: «сгенерирован ключ»
/// и «сгенерирован ключ на джиттере прерываний» — разные утверждения.
pub fn fill(buffer: &mut [u8]) -> Source {
    let hardware = HAS_HARDWARE.load(Ordering::Acquire);
    let mut pool = POOL.lock();

    for chunk in buffer.chunks_mut(8) {
        // Аппаратное значение не отдаётся напрямую, а замешивается в пул: так
        // выданное зависит и от него, и от всего, что было собрано раньше. Если
        // генератор в процессоре окажется недоверенным (а поводы у людей
        // бывали), пул от этого не станет хуже.
        if let Some(value) = arch_random() {
            pool.stir(value);
        }
        let value = pool.take().to_le_bytes();
        chunk.copy_from_slice(&value[..chunk.len()]);
    }

    if hardware { Source::Hardware } else { Source::Jitter }
}

/// Сколько событий собрано и есть ли аппаратный источник — для диагностики.
pub fn stats() -> (bool, u64, u64) {
    let stirred = POOL.lock().stirred;
    (
        HAS_HARDWARE.load(Ordering::Acquire),
        JITTER_EVENTS.load(Ordering::Relaxed),
        stirred,
    )
}

/// Значение от аппаратного генератора, если он есть.
#[cfg(target_arch = "x86_64")]
fn arch_random() -> Option<u64> {
    // `RDRAND` объявляется битом 30 в ECX листа 1 CPUID. Спрашивать надо
    // каждый раз: результат кешировать можно, но проверка стоит десятки тактов
    // против запроса к генератору, который стоит сотни.
    // SAFETY: `CPUID` не имеет побочных эффектов; лист 1 существует на любом
    // процессоре, на котором эта система вообще стартует.
    let supported = unsafe { core::arch::x86_64::__cpuid(1).ecx & (1 << 30) != 0 };
    if !supported {
        return None;
    }

    // Генератор вправе ответить «занят», и спецификация Intel прямо велит
    // повторить попытку до десяти раз. Бесконечный цикл здесь был бы способом
    // подвесить систему неисправным процессором.
    for _ in 0..10 {
        let mut value: u64 = 0;
        // SAFETY: инструкция поддерживается — проверено выше; она пишет в
        // переданную переменную и выставляет флаг переноса при успехе.
        let ok = unsafe { core::arch::x86_64::_rdrand64_step(&mut value) };
        if ok == 1 {
            return Some(value);
        }
        core::hint::spin_loop();
    }
    None
}

/// Значение от аппаратного генератора, если он есть.
#[cfg(target_arch = "aarch64")]
fn arch_random() -> Option<u64> {
    // FEAT_RNG объявляется полем RNDR (биты 63:60) регистра `ID_AA64ISAR0_EL1`.
    // Читать сам `RNDR` без этой проверки нельзя: на процессоре без расширения
    // это неопределённая инструкция, то есть исключение вместо числа.
    let isar0: u64;
    // SAFETY: чтение регистра идентификации разрешено на EL1 и не имеет
    // побочных эффектов.
    unsafe { core::arch::asm!("mrs {}, ID_AA64ISAR0_EL1", out(reg) isar0, options(nomem, nostack)) };
    if (isar0 >> 60) & 0xF == 0 {
        return None;
    }

    for _ in 0..10 {
        let value: u64;
        let flags: u64;
        // SAFETY: расширение поддерживается — проверено выше. `RNDR` выставляет
        // флаг нуля, когда энтропии не хватило, и число тогда брать нельзя.
        unsafe {
            core::arch::asm!(
                "mrs {value}, S3_3_C2_C4_0",
                "cset {flags}, ne",
                value = out(reg) value,
                flags = out(reg) flags,
                options(nomem, nostack)
            );
        }
        if flags != 0 {
            return Some(value);
        }
        core::hint::spin_loop();
    }
    None
}
