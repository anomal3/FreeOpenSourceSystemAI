// Тип decimal (фаза N7g): 96-битная мантисса, масштаб от 0 до 28 и знак. Поля —
// те же четыре числа, что отдаёт decimal.GetBits; арифметика, печать и разбор —
// в Rust (`clr_vm::decimal`, `clr_vm::number`), здесь — операторы,
// преобразования и проверки аргументов.
//
// Литералов `0m` в этом файле нет намеренно: компилятор делает из них чтение
// decimal.Zero, и статический конструктор читал бы сам себя.

using System.Globalization;
using System.Runtime.CompilerServices;

namespace System.Globalization
{
    [Flags]
    public enum NumberStyles
    {
        None = 0,
        AllowLeadingWhite = 1,
        AllowTrailingWhite = 2,
        AllowLeadingSign = 4,
        AllowTrailingSign = 8,
        AllowParentheses = 16,
        AllowDecimalPoint = 32,
        AllowThousands = 64,
        AllowExponent = 128,
        AllowCurrencySymbol = 256,
        AllowHexSpecifier = 512,
        Integer = AllowLeadingWhite | AllowTrailingWhite | AllowLeadingSign,
        HexNumber = AllowLeadingWhite | AllowTrailingWhite | AllowHexSpecifier,
        Number = Integer | AllowTrailingSign | AllowDecimalPoint | AllowThousands,
        Float = Integer | AllowDecimalPoint | AllowExponent,
        Currency = Number | AllowParentheses | AllowCurrencySymbol,
        Any = Currency | AllowExponent,
    }
}

namespace System
{
    public struct Decimal : IComparable, IComparable<decimal>, IEquatable<decimal>, IFormattable
    {
        private const int SignMask = unchecked((int)0x80000000);
        private const int OpAdd = 0;
        private const int OpSubtract = 1;
        private const int OpMultiply = 2;
        private const int OpDivide = 3;
        private const int OpRemainder = 4;

        private readonly int lo;
        private readonly int mid;
        private readonly int hi;
        private readonly int flags;

        public static readonly decimal Zero = new decimal(0);
        public static readonly decimal One = new decimal(1);
        public static readonly decimal MinusOne = new decimal(-1);
        public static readonly decimal MaxValue = new decimal(-1, -1, -1, false, 0);
        public static readonly decimal MinValue = new decimal(-1, -1, -1, true, 0);

        private Decimal(int lo, int mid, int hi, int flags)
        {
            this.lo = lo;
            this.mid = mid;
            this.hi = hi;
            this.flags = flags;
        }

        public Decimal(int value)
        {
            long magnitude = value;
            flags = magnitude < 0 ? SignMask : 0;
            lo = (int)(magnitude < 0 ? -magnitude : magnitude);
            mid = 0;
            hi = 0;
        }

        public Decimal(uint value)
        {
            lo = (int)value;
            mid = 0;
            hi = 0;
            flags = 0;
        }

        public Decimal(long value)
        {
            ulong magnitude = value < 0 ? (ulong)(-(value + 1)) + 1 : (ulong)value;
            lo = (int)magnitude;
            mid = (int)(magnitude >> 32);
            hi = 0;
            flags = value < 0 ? SignMask : 0;
        }

        public Decimal(ulong value)
        {
            lo = (int)value;
            mid = (int)(value >> 32);
            hi = 0;
            flags = 0;
        }

        public Decimal(float value)
        {
            this = FromFloatChecked(value, true);
        }

        public Decimal(double value)
        {
            this = FromFloatChecked(value, false);
        }

        public Decimal(int[] bits)
        {
            if (bits == null)
            {
                throw new ArgumentNullException("bits");
            }
            if (bits.Length == 4)
            {
                int f = bits[3];
                if ((f & ~(SignMask | 0xFF0000)) == 0 && (f & 0xFF0000) <= (28 << 16))
                {
                    lo = bits[0];
                    mid = bits[1];
                    hi = bits[2];
                    flags = f;
                    return;
                }
            }
            throw new ArgumentException("Decimal byte array constructor requires an array of length four containing valid decimal bytes.");
        }

        public Decimal(int lo, int mid, int hi, bool isNegative, byte scale)
        {
            if (scale > 28)
            {
                throw new ArgumentOutOfRangeException("scale", "Decimal's scale value must be between 0 and 28, inclusive.");
            }
            this.lo = lo;
            this.mid = mid;
            this.hi = hi;
            flags = (scale << 16) | (isNegative ? SignMask : 0);
        }

        public byte Scale => (byte)(flags >> 16);

        public static int[] GetBits(decimal d) => new int[] { d.lo, d.mid, d.hi, d.flags };

        // Среда: `op` — OpAdd…OpRemainder; ответ 0 — число записано, 1 —
        // переполнение, 2 — деление на ноль.
        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern int Arith(int op, int lo1, int mid1, int hi1, int flags1, int lo2, int mid2, int hi2, int flags2, out int lo, out int mid, out int hi, out int flags);

        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern int Cmp(int lo1, int mid1, int hi1, int flags1, int lo2, int mid2, int hi2, int flags2);

        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern void RoundTo(int lo, int mid, int hi, int flags, int decimals, int mode, out int rlo, out int rmid, out int rhi, out int rflags);

        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern string Format(int lo, int mid, int hi, int flags, string format);

        // 0 — число, 1 — не число, 2 — слишком велико.
        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern int ParseText(string s, int style, out int lo, out int mid, out int hi, out int flags);

        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern double ToDoubleBits(int lo, int mid, int hi, int flags);

        // 0 — число, 1 — не помещается.
        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern int FromFloat(double value, bool single, out int lo, out int mid, out int hi, out int flags);

        private static OverflowException Overflow() => new OverflowException("Value was either too large or too small for a Decimal.");

        private static decimal Calc(int op, decimal a, decimal b)
        {
            int status = Arith(op, a.lo, a.mid, a.hi, a.flags, b.lo, b.mid, b.hi, b.flags, out int l, out int m, out int h, out int f);
            if (status == 1)
            {
                throw Overflow();
            }
            if (status == 2)
            {
                throw new DivideByZeroException();
            }
            return new decimal(l, m, h, f);
        }

        private static decimal FromFloatChecked(double value, bool single)
        {
            if (FromFloat(value, single, out int l, out int m, out int h, out int f) != 0)
            {
                throw Overflow();
            }
            return new decimal(l, m, h, f);
        }

        public static decimal Add(decimal d1, decimal d2) => Calc(OpAdd, d1, d2);

        public static decimal Subtract(decimal d1, decimal d2) => Calc(OpSubtract, d1, d2);

        public static decimal Multiply(decimal d1, decimal d2) => Calc(OpMultiply, d1, d2);

        public static decimal Divide(decimal d1, decimal d2) => Calc(OpDivide, d1, d2);

        public static decimal Remainder(decimal d1, decimal d2) => Calc(OpRemainder, d1, d2);

        public static decimal Negate(decimal d) => new decimal(d.lo, d.mid, d.hi, d.flags ^ SignMask);

        public static int Compare(decimal d1, decimal d2) => Cmp(d1.lo, d1.mid, d1.hi, d1.flags, d2.lo, d2.mid, d2.hi, d2.flags);

        public int CompareTo(decimal value) => Compare(this, value);

        public int CompareTo(object value)
        {
            if (value == null)
            {
                return 1;
            }
            if (value is decimal d)
            {
                return Compare(this, d);
            }
            throw new ArgumentException("Object must be of type Decimal.");
        }

        public bool Equals(decimal value) => Compare(this, value) == 0;

        public override bool Equals(object value) => value is decimal d && Compare(this, d) == 0;

        public static bool Equals(decimal d1, decimal d2) => Compare(d1, d2) == 0;

        // Равные числа с разным масштабом дают один хэш: считается от double.
        public override int GetHashCode()
        {
            double value = ToDoubleBits(lo, mid, hi, flags);
            return value == 0 ? 0 : value.GetHashCode();
        }

        private static decimal RoundCore(decimal d, int decimals, MidpointRounding mode)
        {
            if ((uint)decimals > 28)
            {
                throw new ArgumentOutOfRangeException("decimals", "Decimal can only round to between 0 and 28 digits of precision.");
            }
            if (mode < MidpointRounding.ToEven || mode > MidpointRounding.ToPositiveInfinity)
            {
                throw new ArgumentException("The value '" + (int)mode + "' is not valid for this usage of the type MidpointRounding.", "mode");
            }
            RoundTo(d.lo, d.mid, d.hi, d.flags, decimals, (int)mode, out int l, out int m, out int h, out int f);
            return new decimal(l, m, h, f);
        }

        public static decimal Round(decimal d) => RoundCore(d, 0, MidpointRounding.ToEven);

        public static decimal Round(decimal d, int decimals) => RoundCore(d, decimals, MidpointRounding.ToEven);

        public static decimal Round(decimal d, MidpointRounding mode) => RoundCore(d, 0, mode);

        public static decimal Round(decimal d, int decimals, MidpointRounding mode) => RoundCore(d, decimals, mode);

        public static decimal Truncate(decimal d) => RoundCore(d, 0, MidpointRounding.ToZero);

        public static decimal Floor(decimal d) => RoundCore(d, 0, MidpointRounding.ToNegativeInfinity);

        public static decimal Ceiling(decimal d) => RoundCore(d, 0, MidpointRounding.ToPositiveInfinity);

        public override string ToString() => Format(lo, mid, hi, flags, null);

        public string ToString(string format) => Format(lo, mid, hi, flags, format);

        public string ToString(IFormatProvider provider) => ToString();

        public string ToString(string format, IFormatProvider provider) => ToString(format);

        public static decimal Parse(string s) => Parse(s, NumberStyles.Number);

        public static decimal Parse(string s, IFormatProvider provider) => Parse(s, NumberStyles.Number);

        public static decimal Parse(string s, NumberStyles style, IFormatProvider provider) => Parse(s, style);

        public static decimal Parse(string s, NumberStyles style)
        {
            if (s == null)
            {
                throw new ArgumentNullException("s");
            }
            int status = ParseText(s, (int)style, out int l, out int m, out int h, out int f);
            if (status == 1)
            {
                throw new FormatException("The input string '" + s + "' was not in a correct format.");
            }
            if (status == 2)
            {
                throw Overflow();
            }
            return new decimal(l, m, h, f);
        }

        public static bool TryParse(string s, out decimal result) => TryParse(s, NumberStyles.Number, null, out result);

        public static bool TryParse(string s, NumberStyles style, IFormatProvider provider, out decimal result)
        {
            if (s != null && ParseText(s, (int)style, out int l, out int m, out int h, out int f) == 0)
            {
                result = new decimal(l, m, h, f);
                return true;
            }
            result = new decimal(0, 0, 0, 0);
            return false;
        }

        // Целая часть к нулю; не влезает — OverflowException с именем типа.
        private static long ToWhole(decimal value, long min, long max, string typeName)
        {
            decimal whole = Truncate(value);
            if (whole.hi == 0 && whole.mid >= 0)
            {
                long magnitude = ((long)whole.mid << 32) | (uint)whole.lo;
                long signed = whole.flags < 0 ? -magnitude : magnitude;
                if (signed >= min && signed <= max)
                {
                    return signed;
                }
            }
            throw new OverflowException("Value was either too large or too small for " + typeName + ".");
        }

        public static int ToInt32(decimal d) => (int)ToWhole(d, int.MinValue, int.MaxValue, "an Int32");

        public static long ToInt64(decimal d) => ToWhole(d, long.MinValue, long.MaxValue, "an Int64");

        public static double ToDouble(decimal d) => ToDoubleBits(d.lo, d.mid, d.hi, d.flags);

        public static float ToSingle(decimal d) => (float)ToDouble(d);

        public static decimal operator +(decimal d1, decimal d2) => Calc(OpAdd, d1, d2);

        public static decimal operator -(decimal d1, decimal d2) => Calc(OpSubtract, d1, d2);

        public static decimal operator *(decimal d1, decimal d2) => Calc(OpMultiply, d1, d2);

        public static decimal operator /(decimal d1, decimal d2) => Calc(OpDivide, d1, d2);

        public static decimal operator %(decimal d1, decimal d2) => Calc(OpRemainder, d1, d2);

        public static decimal operator -(decimal d) => Negate(d);

        public static decimal operator +(decimal d) => d;

        public static decimal operator ++(decimal d) => Calc(OpAdd, d, One);

        public static decimal operator --(decimal d) => Calc(OpSubtract, d, One);

        public static bool operator ==(decimal d1, decimal d2) => Compare(d1, d2) == 0;

        public static bool operator !=(decimal d1, decimal d2) => Compare(d1, d2) != 0;

        public static bool operator <(decimal d1, decimal d2) => Compare(d1, d2) < 0;

        public static bool operator <=(decimal d1, decimal d2) => Compare(d1, d2) <= 0;

        public static bool operator >(decimal d1, decimal d2) => Compare(d1, d2) > 0;

        public static bool operator >=(decimal d1, decimal d2) => Compare(d1, d2) >= 0;

        public static implicit operator decimal(byte value) => new decimal((int)value);

        public static implicit operator decimal(sbyte value) => new decimal((int)value);

        public static implicit operator decimal(short value) => new decimal((int)value);

        public static implicit operator decimal(ushort value) => new decimal((int)value);

        public static implicit operator decimal(char value) => new decimal((int)value);

        public static implicit operator decimal(int value) => new decimal(value);

        public static implicit operator decimal(uint value) => new decimal(value);

        public static implicit operator decimal(long value) => new decimal(value);

        public static implicit operator decimal(ulong value) => new decimal(value);

        public static explicit operator decimal(float value) => FromFloatChecked(value, true);

        public static explicit operator decimal(double value) => FromFloatChecked(value, false);

        public static explicit operator byte(decimal value) => (byte)ToWhole(value, byte.MinValue, byte.MaxValue, "an unsigned byte");

        public static explicit operator sbyte(decimal value) => (sbyte)ToWhole(value, sbyte.MinValue, sbyte.MaxValue, "a signed byte");

        public static explicit operator short(decimal value) => (short)ToWhole(value, short.MinValue, short.MaxValue, "an Int16");

        public static explicit operator ushort(decimal value) => (ushort)ToWhole(value, ushort.MinValue, ushort.MaxValue, "a UInt16");

        public static explicit operator char(decimal value) => (char)ToWhole(value, char.MinValue, char.MaxValue, "a character");

        public static explicit operator int(decimal value) => (int)ToWhole(value, int.MinValue, int.MaxValue, "an Int32");

        public static explicit operator uint(decimal value) => (uint)ToWhole(value, uint.MinValue, uint.MaxValue, "a UInt32");

        public static explicit operator long(decimal value) => ToWhole(value, long.MinValue, long.MaxValue, "an Int64");

        public static explicit operator ulong(decimal value)
        {
            decimal whole = Truncate(value);
            if (whole.hi == 0 && (whole.flags >= 0 || (whole.lo | whole.mid) == 0))
            {
                return ((ulong)(uint)whole.mid << 32) | (uint)whole.lo;
            }
            throw new OverflowException("Value was either too large or too small for a UInt64.");
        }

        public static explicit operator double(decimal value) => ToDouble(value);

        public static explicit operator float(decimal value) => ToSingle(value);
    }
}
