//! Длинная арифметика ровно под одну задачу: `s^e mod n` для проверки подписи
//! RSA.
//!
//! # Почему своя, а не крейт
//!
//! Потому что нужна одна операция, и та с крошечным показателем. Открытая
//! экспонента сертификата — это почти всегда 65537, то есть семнадцать бит:
//! возведение в степень стоит семнадцать умножений по модулю. Крейт `rsa`
//! приносит с собой кучу, `crypto-bigint` — обобщённую арифметику на все случаи;
//! здесь же двести строк, которые целиком помещаются в голову и проверяются
//! готовыми векторами.
//!
//! Второе соображение — то же, что у остального этого дерева: у программы нет
//! кучи. Числа живут в массивах фиксированного размера, а предел (4096 бит)
//! назван вслух: ключ длиннее не встречается в цепочках, которые нам нужны, а
//! бесконечный размер означал бы, что длину числа выбирает тот, кто прислал
//! сертификат.
//!
//! # Постоянное время исполнения здесь не нужно, и это не небрежность
//!
//! Мы **проверяем** чужую подпись чужим открытым ключом. Секрета в руках нет
//! вовсе: и число, и модуль, и показатель известны всем. Утечка по времени
//! рассказывает наблюдателю то, что он и так видел на проводе. Там, где секрет
//! есть, — обмен ключами X25519, — арифметика чужая и написана людьми, которые
//! этим занимаются (см. `Cargo.toml` крейта `ssh`).
//!
//! # Монтгомери, а не деление
//!
//! Взятие остатка от деления восьмикилобитного числа на четырёхкилобитное — это
//! длинное деление, которое ошибается на коррекции частного и ошибается редко,
//! то есть проходит тесты. Умножение Монтгомери обходится без деления вовсе:
//! вся редукция — это умножения и сложения, а единственная тонкость (`n0inv`)
//! считается пятью строками по Ньютону.

/// Наибольший модуль, который эта арифметика согласна взять.
///
/// Четыре килобита — это `ISRG Root X1`, корень Let's Encrypt, и он же самый
/// длинный ключ во всех цепочках, ради которых фаза написана. Ключ длиннее — не
/// «очень надёжный сертификат», а не тот файл.
pub const MAX_BITS: usize = 4096;

/// Столько же в машинных словах.
pub const MAX_LIMBS: usize = MAX_BITS / 64;

/// Модуль вместе со всем, что нужно, чтобы считать по нему быстро.
///
/// Считается один раз на ключ: `r2` стоит восемь тысяч удвоений, и делать их
/// заново на каждое умножение было бы вдвое дороже самой подписи.
#[derive(Clone)]
pub struct Modulus {
    /// Сам модуль, младшим словом вперёд.
    n: [u64; MAX_LIMBS],
    /// Сколько слов занято.
    limbs: usize,
    /// Длина модуля в байтах — та самая `k` из RFC 8017.
    bytes: usize,
    /// Длина модуля в битах: нужна PSS, чтобы знать, сколько бит старшего байта
    /// обязаны быть нулями.
    bits: usize,
    /// `-n^-1 mod 2^64`.
    n0inv: u64,
    /// `R^2 mod n`, где `R = 2^(64 * limbs)` — переводчик в форму Монтгомери.
    r2: [u64; MAX_LIMBS],
}

impl Modulus {
    /// Разобрать модуль из байтов, старшим вперёд.
    ///
    /// Отказывает на чётном модуле, и это не формальность: у Монтгомери нет
    /// обратного к чётному числу по модулю `2^64`, а чётный модуль RSA — это не
    /// ключ, а мусор в поле ключа.
    #[must_use]
    pub fn new(be: &[u8]) -> Option<Self> {
        // Ведущие нули игнорируются: их мог оставить кодировщик, и длина ключа
        // от них не меняется.
        let be = {
            let mut trimmed = be;
            while let [0x00, rest @ ..] = trimmed {
                trimmed = rest;
            }
            trimmed
        };
        if be.is_empty() || be.len() > MAX_LIMBS * 8 {
            return None;
        }
        let mut n = [0u64; MAX_LIMBS];
        load(be, &mut n);
        if n[0] & 1 == 0 {
            return None;
        }
        let limbs = significant(&n);
        let bytes = be.len();
        let bits = 64 * limbs - n[limbs - 1].leading_zeros() as usize;

        let mut n0inv: u64 = 1;
        // Ньютон удваивает точность за шаг: единица верна по модулю 2, шесть
        // шагов дают 2^64. Проверять сходимость нечем и незачем — число шагов
        // здесь константа, выведенная на бумаге.
        for _ in 0..6 {
            n0inv = n0inv.wrapping_mul(2u64.wrapping_sub(n[0].wrapping_mul(n0inv)));
        }
        let n0inv = n0inv.wrapping_neg();

        // `R^2 mod n` удвоениями: начинаем с единицы и удваиваем 128 * limbs раз.
        // Деления при этом не происходит ни разу — только сдвиг и вычитание.
        let mut r2 = [0u64; MAX_LIMBS];
        r2[0] = 1;
        for _ in 0..(128 * limbs) {
            double_mod(&mut r2, &n, limbs);
        }

        Some(Self { n, limbs, bytes, bits, n0inv, r2 })
    }

    /// Длина модуля в байтах — `k` из RFC 8017.
    #[must_use]
    pub const fn bytes(&self) -> usize {
        self.bytes
    }

    /// Длина модуля в битах.
    #[must_use]
    pub const fn bits(&self) -> usize {
        self.bits
    }

    /// `base^exponent mod n`, обе стороны — байты старшим вперёд.
    ///
    /// Возвращает `false`, если основание не меньше модуля: у исправной подписи
    /// такого не бывает, а у подделанной бывает, и принимать её было бы
    /// приглашением. `out` заполняется целиком, с ведущими нулями, — длиной
    /// ровно [`Self::bytes`], потому что дальше по этим байтам считается
    /// заполнение, а оно позиционное.
    #[must_use]
    pub fn modexp(&self, base_be: &[u8], exponent_be: &[u8], out: &mut [u8]) -> bool {
        // Ведущие нули основания снимаются **до** проверки длины: иначе число,
        // записанное с запасом, выглядело бы слишком длинным, а число длиннее
        // массива молча теряло бы старшие байты и оказывалось «меньше модуля».
        let mut base_be = base_be;
        while let [0x00, rest @ ..] = base_be {
            base_be = rest;
        }
        if out.len() != self.bytes || base_be.len() > MAX_LIMBS * 8 {
            return false;
        }
        let mut base = [0u64; MAX_LIMBS];
        load(base_be, &mut base);
        if !less(&base, &self.n, self.limbs) {
            return false;
        }

        // Перевод в форму Монтгомери и единица в той же форме.
        let mut acc = [0u64; MAX_LIMBS];
        self.mont_mul(&base, &self.r2, &mut acc);
        let base_mont = acc;

        let mut one = [0u64; MAX_LIMBS];
        one[0] = 1;
        let mut result = [0u64; MAX_LIMBS];
        self.mont_mul(&one, &self.r2, &mut result);

        let mut scratch = [0u64; MAX_LIMBS];
        let mut started = false;
        for byte in exponent_be {
            for shift in (0..8).rev() {
                let bit = (byte >> shift) & 1 == 1;
                if !started {
                    // Ведущие нули показателя пропускаются: возводить единицу в
                    // квадрат безвредно, но лишние 4096 умножений на модуле в
                    // четыре килобита — это уже заметное время.
                    if !bit {
                        continue;
                    }
                    started = true;
                    result = base_mont;
                    continue;
                }
                self.mont_mul(&result, &result, &mut scratch);
                result = scratch;
                if bit {
                    self.mont_mul(&result, &base_mont, &mut scratch);
                    result = scratch;
                }
            }
        }
        if !started {
            // Показатель — ноль. Такого показателя у ключа не бывает; отвечать
            // единицей значило бы принимать любую подпись.
            return false;
        }

        // Обратно из формы Монтгомери.
        self.mont_mul(&result, &one, &mut scratch);
        store(&scratch, out);
        true
    }

    /// `a * b * R^-1 mod n` — умножение Монтгомери, разложение CIOS.
    ///
    /// «Coarsely Integrated Operand Scanning»: редукция вплетена в умножение, и
    /// промежуточное произведение никогда не занимает больше `limbs + 2` слов.
    /// Оба множителя обязаны быть меньше модуля.
    fn mont_mul(&self, a: &[u64; MAX_LIMBS], b: &[u64; MAX_LIMBS], out: &mut [u64; MAX_LIMBS]) {
        let len = self.limbs;
        let n = &self.n;
        let mut t = [0u64; MAX_LIMBS + 2];

        for i in 0..len {
            // t += a[i] * b
            let mut carry: u64 = 0;
            for j in 0..len {
                let s = u128::from(t[j]) + u128::from(a[i]) * u128::from(b[j]) + u128::from(carry);
                t[j] = s as u64;
                carry = (s >> 64) as u64;
            }
            let s = u128::from(t[len]) + u128::from(carry);
            t[len] = s as u64;
            t[len + 1] = (s >> 64) as u64;

            // m подобрано так, что младшее слово суммы обнуляется, — на этом
            // держится вся редукция: делить на 2^64 после этого можно сдвигом
            // массива, а не делением.
            let m = t[0].wrapping_mul(self.n0inv);
            let s = u128::from(t[0]) + u128::from(m) * u128::from(n[0]);
            debug_assert_eq!(s as u64, 0, "младшее слово обязано обнулиться");
            let mut carry = (s >> 64) as u64;
            for j in 1..len {
                let s = u128::from(t[j]) + u128::from(m) * u128::from(n[j]) + u128::from(carry);
                t[j - 1] = s as u64;
                carry = (s >> 64) as u64;
            }
            let s = u128::from(t[len]) + u128::from(carry);
            t[len - 1] = s as u64;
            t[len] = t[len + 1] + (s >> 64) as u64;
            t[len + 1] = 0;
        }

        // Итог меньше `2n`: одно условное вычитание доводит его до `n`.
        let mut value = [0u64; MAX_LIMBS];
        value[..len].copy_from_slice(&t[..len]);
        if t[len] != 0 || !less(&value, n, len) {
            subtract(&mut value, n, len);
        }
        *out = value;
    }
}

/// Уложить байты, старшим вперёд, в слова, младшим вперёд.
fn load(be: &[u8], out: &mut [u64; MAX_LIMBS]) {
    *out = [0; MAX_LIMBS];
    for (index, byte) in be.iter().rev().enumerate() {
        let limb = index / 8;
        if limb >= MAX_LIMBS {
            break;
        }
        out[limb] |= u64::from(*byte) << (8 * (index % 8));
    }
}

/// Выложить слова обратно в байты, старшим вперёд, с ведущими нулями.
fn store(value: &[u64; MAX_LIMBS], out: &mut [u8]) {
    for (index, byte) in out.iter_mut().rev().enumerate() {
        let limb = index / 8;
        *byte = if limb < MAX_LIMBS { (value[limb] >> (8 * (index % 8))) as u8 } else { 0 };
    }
}

/// Сколько слов занято под старшими нулями.
fn significant(value: &[u64; MAX_LIMBS]) -> usize {
    let mut len = MAX_LIMBS;
    while len > 0 && value[len - 1] == 0 {
        len -= 1;
    }
    len
}

/// `a < b` на первых `len` словах.
fn less(a: &[u64; MAX_LIMBS], b: &[u64; MAX_LIMBS], len: usize) -> bool {
    for index in (0..len).rev() {
        if a[index] != b[index] {
            return a[index] < b[index];
        }
    }
    false
}

/// `a -= b` на первых `len` словах; заём за пределы `len` не проверяется, потому
/// что вызывающий уже убедился, что `a >= b`.
fn subtract(a: &mut [u64; MAX_LIMBS], b: &[u64; MAX_LIMBS], len: usize) {
    let mut borrow = 0u64;
    for index in 0..len {
        let (difference, first) = a[index].overflowing_sub(b[index]);
        let (difference, second) = difference.overflowing_sub(borrow);
        a[index] = difference;
        borrow = u64::from(first || second);
    }
}

/// `a = 2a mod n`.
///
/// Перенос из старшего слова учитывается отдельно: `2a` может не поместиться в
/// те же `len` слов, и забытый бит превратил бы удвоение в взятие по модулю
/// `2^(64*len)` — ошибка, которая проявится только на модуле с единицей в
/// старшем бите, то есть на любом настоящем ключе RSA.
fn double_mod(a: &mut [u64; MAX_LIMBS], n: &[u64; MAX_LIMBS], len: usize) {
    let mut carry = 0u64;
    for index in 0..len {
        let next = a[index] >> 63;
        a[index] = (a[index] << 1) | carry;
        carry = next;
    }
    if carry != 0 || !less(a, n, len) {
        subtract(a, n, len);
    }
}

#[cfg(test)]
mod tests {
    use super::Modulus;

    /// Маленькие числа, посчитанные на бумаге: `7^5 mod 33 = 10`.
    ///
    /// Ведущие нули модуля снимаются, и длина ответа равна длине модуля без
    /// них: `k` из RFC 8017 — это не «сколько байт прислали», а «сколько байт в
    /// числе».
    #[test]
    fn small_modexp_matches_paper() {
        let modulus = Modulus::new(&[0x00, 0x00, 0x21]).expect("33 нечётно");
        assert_eq!(modulus.bytes(), 1);
        assert_eq!(modulus.bits(), 6);
        let mut out = [0u8; 1];
        assert!(modulus.modexp(&[7], &[5], &mut out));
        assert_eq!(out, [10]);
    }

    /// Чётный модуль отвергается: у Монтгомери нет обратного к нему.
    #[test]
    fn an_even_modulus_is_refused() {
        assert!(Modulus::new(&[0x01, 0x00]).is_none());
    }

    /// Основание не меньше модуля — отказ, а не молчаливое взятие остатка.
    #[test]
    fn a_base_that_is_not_smaller_than_the_modulus_is_refused() {
        let modulus = Modulus::new(&[0x01, 0x00, 0x01]).expect("65537 нечётно");
        let mut out = [0u8; 3];
        assert!(!modulus.modexp(&[0x01, 0x00, 0x01], &[3], &mut out));
        assert!(modulus.modexp(&[0x01, 0x00, 0x00], &[1], &mut out));
        assert_eq!(out, [0x01, 0x00, 0x00]);
    }

    /// Показатель ноль — отказ: иначе любая подпись «сходится» с единицей.
    #[test]
    fn a_zero_exponent_is_refused() {
        let modulus = Modulus::new(&[0x01, 0x00, 0x01]).expect("65537 нечётно");
        let mut out = [0u8; 3];
        assert!(!modulus.modexp(&[2], &[0], &mut out));
    }

    /// Малая теорема Ферма на простом модуле: `a^(p-1) = 1 mod p`.
    ///
    /// Проверка ценна тем, что задействует всю длину: 2^61 - 1 — простое число
    /// Мерсенна, показатель занимает восемь байт, и умножений выходит под
    /// шесть десятков.
    #[test]
    fn fermat_holds_on_a_mersenne_prime() {
        let p: u64 = (1 << 61) - 1;
        let modulus = Modulus::new(&p.to_be_bytes()).expect("простое число нечётно");
        assert_eq!(modulus.bits(), 61);
        let mut out = [0u8; 8];
        let exponent = (p - 1).to_be_bytes();
        assert!(modulus.modexp(&[0x12, 0x34, 0x56, 0x78], &exponent, &mut out));
        let mut want = [0u8; 8];
        want[7] = 1;
        assert_eq!(out, want);
    }
}
