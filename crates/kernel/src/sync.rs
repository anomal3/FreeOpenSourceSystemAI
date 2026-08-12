//! Примитивы синхронизации ядра.
//!
//! # Почему обычного спинлока недостаточно
//!
//! Ядро само себя прерывает. Обработчик таймера исполняется в контексте того
//! кода, который он прервал, и если тот держал спинлок, а обработчику нужен
//! тот же лок — ждать освобождения будет некому: держатель не продолжится,
//! пока не завершится обработчик. Взаимная блокировка на одном процессоре, без
//! всякой многоядерности.
//!
//! Поэтому [`SpinLock::lock`] запрещает прерывания **до** захвата и возвращает
//! их в прежнее состояние при освобождении. Именно в прежнее, а не «включает»:
//! вложенный захват не должен разрешать прерывания раньше, чем отпустит внешний.

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

/// Глобальная переменная, к которой обращаются через сырой указатель.
///
/// Оставлен для состояния, к которому обращается только код запуска — до того,
/// как появились прерывания и задачи. Для всего остального нужен [`SpinLock`]:
/// допущение «исполнение однопоточное, прерывания запрещены» перестало быть
/// верным с Phase 3.
///
/// Потребитель остался один — арх-слой x86-64 (GDT, TSS и стеки IST), поэтому
/// на AArch64 тип целиком мёртв. Глушим предупреждение здесь: разводить его по
/// `cfg` значило бы вписать в общий примитив синхронизации знание о том, какая
/// архитектура им сегодня пользуется.
#[allow(dead_code)]
pub struct Racy<T> {
    cell: UnsafeCell<T>,
}

// SAFETY: тип не раздаёт `&mut` автоматически — наружу выходит только сырой
// указатель, разыменование которого уже требует `unsafe` и обоснования на
// месте использования. Обязанность не допускать гонок лежит на вызывающем.
unsafe impl<T> Sync for Racy<T> {}

impl<T> Racy<T> {
    pub const fn new(value: T) -> Self {
        Self { cell: UnsafeCell::new(value) }
    }

    /// Указатель на содержимое. Безопасен сам по себе; опасно разыменование.
    pub const fn get(&self) -> *mut T {
        self.cell.get()
    }
}

/// Взаимное исключение через активное ожидание, с запретом прерываний на время
/// удержания.
///
/// Ожидание активное намеренно: блокировать задачу негде — планировщик сам
/// защищён этим же типом, и попытка уснуть внутри лока была бы рекурсией.
/// Критические секции здесь короткие ровно поэтому.
pub struct SpinLock<T> {
    locked: AtomicBool,
    value: UnsafeCell<T>,
}

// SAFETY: доступ к `value` возможен только через `SpinGuard`, который выдаётся
// после успешного захвата `locked` и существует в единственном экземпляре.
unsafe impl<T: Send> Sync for SpinLock<T> {}
unsafe impl<T: Send> Send for SpinLock<T> {}

impl<T> SpinLock<T> {
    pub const fn new(value: T) -> Self {
        Self { locked: AtomicBool::new(false), value: UnsafeCell::new(value) }
    }

    /// Захватить лок, дождавшись освобождения.
    #[must_use = "лок освобождается при уничтожении охранника; проигнорировать его значит сразу же отпустить"]
    pub fn lock(&self) -> SpinGuard<'_, T> {
        // Прерывания запрещаются до захвата, а не после: иначе между этими
        // двумя действиями остаётся окно, в котором обработчик застаёт лок
        // уже занятым и упирается в вечное ожидание.
        let irq_was_enabled = crate::arch::interrupts::enabled();
        crate::arch::interrupts::disable();

        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            // Пока лок занят, читаем обычной загрузкой, не пытаясь занять шину
            // атомарной операцией на каждом витке.
            while self.locked.load(Ordering::Relaxed) {
                core::hint::spin_loop();
            }
        }

        SpinGuard { lock: self, irq_was_enabled }
    }

    /// Попытаться захватить лок, не дожидаясь освобождения.
    ///
    /// Нужен там, где ожидание недопустимо — например в обработчике отказа,
    /// который обязан напечатать диагностику даже если лок занят навсегда.
    pub fn try_lock(&self) -> Option<SpinGuard<'_, T>> {
        let irq_was_enabled = crate::arch::interrupts::enabled();
        crate::arch::interrupts::disable();

        if self
            .locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            Some(SpinGuard { lock: self, irq_was_enabled })
        } else {
            // Захват не удался — состояние прерываний надо вернуть, иначе
            // неудачная попытка молча оставила бы их запрещёнными.
            if irq_was_enabled {
                crate::arch::interrupts::enable();
            }
            None
        }
    }
}

/// Доступ к содержимому [`SpinLock`] на время удержания.
pub struct SpinGuard<'a, T> {
    lock: &'a SpinLock<T>,
    /// Были ли прерывания разрешены до захвата. Восстанавливается как было —
    /// вложенный захват не должен разрешать их раньше времени.
    irq_was_enabled: bool,
}

impl<T> Deref for SpinGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: охранник существует только при удержанном локе, поэтому
        // других ссылок на значение сейчас нет.
        unsafe { &*self.lock.value.get() }
    }
}

impl<T> DerefMut for SpinGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: см. `Deref`; `&mut self` охранника гарантирует, что и вторая
        // ссылка из этого же охранника не возникнет.
        unsafe { &mut *self.lock.value.get() }
    }
}

impl<T> Drop for SpinGuard<'_, T> {
    fn drop(&mut self) {
        // Порядок обратный захвату: сначала отпустить лок, потом вернуть
        // прерывания. Иначе обработчик успел бы прийти на ещё удерживаемый лок.
        self.lock.locked.store(false, Ordering::Release);
        if self.irq_was_enabled {
            crate::arch::interrupts::enable();
        }
    }
}
