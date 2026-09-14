// Примитивы. На стеке своей среды это не структуры с полем, а числа
// (`int32`, `int64`, `native int`, `F`), поэтому полей у них здесь нет, а
// печать написана в Rust: `ToString` получает `this` указателем на число или
// упакованным объектом, и среда достаёт значение сама.
//
// Разбор (`Parse`) — на C# (`Number`), ему число не нужно, только строка.

using System.Runtime.CompilerServices;

namespace System
{
    public interface IFormattable
    {
        string ToString(string format, IFormatProvider formatProvider);
    }

    public interface IFormatProvider
    {
        object GetFormat(Type formatType);
    }

    public struct Boolean
    {
        [MethodImpl(MethodImplOptions.InternalCall)]
        public override extern string ToString();
    }

    public struct Char
    {
        public const char MaxValue = '￿';
        public const char MinValue = '\0';

        [MethodImpl(MethodImplOptions.InternalCall)]
        public override extern string ToString();

        // Свойства символа — из таблиц Unicode, которые знает Rust. Суррогатная
        // половинка — не буква и не цифра, как и в .NET.
        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern bool IsLetter(char c);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern bool IsDigit(char c);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern bool IsLetterOrDigit(char c);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern bool IsWhiteSpace(char c);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern bool IsUpper(char c);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern bool IsLower(char c);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern char ToUpper(char c);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern char ToLower(char c);

        public static char ToUpperInvariant(char c) => ToUpper(c);

        public static char ToLowerInvariant(char c) => ToLower(c);
    }

    public struct SByte : IFormattable
    {
        public const sbyte MaxValue = 127;
        public const sbyte MinValue = -128;

        [MethodImpl(MethodImplOptions.InternalCall)]
        public override extern string ToString();

        [MethodImpl(MethodImplOptions.InternalCall)]
        public extern string ToString(string format);

        public string ToString(string format, IFormatProvider provider) => ToString(format);
    }

    public struct Byte : IFormattable
    {
        public const byte MaxValue = 255;
        public const byte MinValue = 0;

        [MethodImpl(MethodImplOptions.InternalCall)]
        public override extern string ToString();

        [MethodImpl(MethodImplOptions.InternalCall)]
        public extern string ToString(string format);

        public string ToString(string format, IFormatProvider provider) => ToString(format);

        public static byte Parse(string s) => (byte)Number.ParseInteger(s, MinValue, MaxValue, "Byte");
    }

    public struct Int16 : IFormattable
    {
        public const short MaxValue = 32767;
        public const short MinValue = -32768;

        [MethodImpl(MethodImplOptions.InternalCall)]
        public override extern string ToString();

        [MethodImpl(MethodImplOptions.InternalCall)]
        public extern string ToString(string format);

        public string ToString(string format, IFormatProvider provider) => ToString(format);
    }

    public struct UInt16 : IFormattable
    {
        public const ushort MaxValue = 65535;
        public const ushort MinValue = 0;

        [MethodImpl(MethodImplOptions.InternalCall)]
        public override extern string ToString();

        [MethodImpl(MethodImplOptions.InternalCall)]
        public extern string ToString(string format);

        public string ToString(string format, IFormatProvider provider) => ToString(format);
    }

    public struct Int32 : IComparable<int>, IFormattable
    {
        public const int MaxValue = 2147483647;
        public const int MinValue = -2147483648;

        [MethodImpl(MethodImplOptions.InternalCall)]
        public override extern string ToString();

        [MethodImpl(MethodImplOptions.InternalCall)]
        public extern string ToString(string format);

        public string ToString(string format, IFormatProvider provider) => ToString(format);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public extern int CompareTo(int value);

        public static int Parse(string s) => (int)Number.ParseInteger(s, MinValue, MaxValue, "Int32");

        public static bool TryParse(string s, out int result)
        {
            bool parsed = Number.TryParseInteger(s, MinValue, MaxValue, out long value) == 0;
            result = parsed ? (int)value : 0;
            return parsed;
        }
    }

    public struct UInt32 : IFormattable
    {
        public const uint MaxValue = 4294967295;
        public const uint MinValue = 0;

        [MethodImpl(MethodImplOptions.InternalCall)]
        public override extern string ToString();

        [MethodImpl(MethodImplOptions.InternalCall)]
        public extern string ToString(string format);

        public string ToString(string format, IFormatProvider provider) => ToString(format);
    }

    public struct Int64 : IComparable<long>, IFormattable
    {
        public const long MaxValue = 9223372036854775807;
        public const long MinValue = -9223372036854775808;

        [MethodImpl(MethodImplOptions.InternalCall)]
        public override extern string ToString();

        [MethodImpl(MethodImplOptions.InternalCall)]
        public extern string ToString(string format);

        public string ToString(string format, IFormatProvider provider) => ToString(format);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public extern int CompareTo(long value);

        public static long Parse(string s) => Number.ParseInteger(s, MinValue, MaxValue, "Int64");

        public static bool TryParse(string s, out long result) => Number.TryParseInteger(s, MinValue, MaxValue, out result) == 0;
    }

    public struct UInt64 : IFormattable
    {
        public const ulong MaxValue = 18446744073709551615;
        public const ulong MinValue = 0;

        [MethodImpl(MethodImplOptions.InternalCall)]
        public override extern string ToString();

        [MethodImpl(MethodImplOptions.InternalCall)]
        public extern string ToString(string format);

        public string ToString(string format, IFormatProvider provider) => ToString(format);
    }

    public struct IntPtr
    {
    }

    public struct UIntPtr
    {
    }

    // Дробные числа (фаза N4c). Печать и разбор — в Rust (`clr_vm::number`):
    // им нужны биты числа и длинная арифметика, а правила повторяют .NET до
    // символа и сверяются с ним `clr-check`. Остальное — на C#: внутри
    // структуры `this` и есть само число.
    public struct Single : IComparable, IComparable<float>, IEquatable<float>, IFormattable
    {
        public const float MinValue = -3.40282347E+38f;
        public const float MaxValue = 3.40282347E+38f;
        public const float Epsilon = 1.401298E-45f;
        public const float PositiveInfinity = 1.0f / 0.0f;
        public const float NegativeInfinity = -1.0f / 0.0f;
        public const float NaN = 0.0f / 0.0f;
        public const float NegativeZero = -0.0f;

        public static bool IsNaN(float f) => f != f;

        public static bool IsInfinity(float f) => f == PositiveInfinity || f == NegativeInfinity;

        public static bool IsPositiveInfinity(float f) => f == PositiveInfinity;

        public static bool IsNegativeInfinity(float f) => f == NegativeInfinity;

        public static bool IsFinite(float f) => !IsNaN(f) && !IsInfinity(f);

        public static bool IsNegative(float f) => BitConverter.SingleToInt32Bits(f) < 0;

        [MethodImpl(MethodImplOptions.InternalCall)]
        public override extern string ToString();

        [MethodImpl(MethodImplOptions.InternalCall)]
        public extern string ToString(string format);

        public string ToString(IFormatProvider provider) => ToString();

        public string ToString(string format, IFormatProvider provider) => ToString(format);

        // NaN меньше любого числа и равен сам себе — порядок для сортировки, а
        // не для `<`.
        public int CompareTo(float value)
        {
            if (this < value)
            {
                return -1;
            }
            if (this > value)
            {
                return 1;
            }
            if (this == value)
            {
                return 0;
            }
            if (IsNaN(this))
            {
                return IsNaN(value) ? 0 : -1;
            }
            return 1;
        }

        public int CompareTo(object value)
        {
            if (value == null)
            {
                return 1;
            }
            if (value is float f)
            {
                return CompareTo(f);
            }
            throw new ArgumentException("Object must be of type Single.");
        }

        public bool Equals(float obj) => this == obj || (IsNaN(obj) && IsNaN(this));

        public override bool Equals(object obj) => obj is float f && Equals(f);

        // Все NaN и оба нуля — один хэш, как у .NET.
        public override int GetHashCode()
        {
            int bits = BitConverter.SingleToInt32Bits(this);
            if (IsNaN(this) || this == 0)
            {
                bits &= 0x7F800000;
            }
            return bits;
        }

        public static float Parse(string s) => (float)double.ParseFloat(s, true);

        public static bool TryParse(string s, out float result)
        {
            bool parsed = double.TryParseFloat(s, true, out double value);
            result = (float)value;
            return parsed;
        }
    }

    public struct Double : IComparable, IComparable<double>, IEquatable<double>, IFormattable
    {
        public const double MinValue = -1.7976931348623157E+308;
        public const double MaxValue = 1.7976931348623157E+308;
        public const double Epsilon = 4.9406564584124654E-324;
        public const double NegativeInfinity = -1.0 / 0.0;
        public const double PositiveInfinity = 1.0 / 0.0;
        public const double NaN = 0.0 / 0.0;
        public const double NegativeZero = -0.0;

        public static bool IsNaN(double d) => d != d;

        public static bool IsInfinity(double d) => d == PositiveInfinity || d == NegativeInfinity;

        public static bool IsPositiveInfinity(double d) => d == PositiveInfinity;

        public static bool IsNegativeInfinity(double d) => d == NegativeInfinity;

        public static bool IsFinite(double d) => !IsNaN(d) && !IsInfinity(d);

        public static bool IsNegative(double d) => BitConverter.DoubleToInt64Bits(d) < 0;

        [MethodImpl(MethodImplOptions.InternalCall)]
        public override extern string ToString();

        [MethodImpl(MethodImplOptions.InternalCall)]
        public extern string ToString(string format);

        public string ToString(IFormatProvider provider) => ToString();

        public string ToString(string format, IFormatProvider provider) => ToString(format);

        public int CompareTo(double value)
        {
            if (this < value)
            {
                return -1;
            }
            if (this > value)
            {
                return 1;
            }
            if (this == value)
            {
                return 0;
            }
            if (IsNaN(this))
            {
                return IsNaN(value) ? 0 : -1;
            }
            return 1;
        }

        public int CompareTo(object value)
        {
            if (value == null)
            {
                return 1;
            }
            if (value is double d)
            {
                return CompareTo(d);
            }
            throw new ArgumentException("Object must be of type Double.");
        }

        public bool Equals(double obj) => this == obj || (IsNaN(obj) && IsNaN(this));

        public override bool Equals(object obj) => obj is double d && Equals(d);

        public override int GetHashCode()
        {
            long bits = BitConverter.DoubleToInt64Bits(this);
            if (IsNaN(this) || this == 0)
            {
                bits &= 0x7FF0000000000000;
            }
            return (int)bits ^ (int)(bits >> 32);
        }

        public static double Parse(string s) => ParseFloat(s, false);

        public static bool TryParse(string s, out double result)
        {
            if (s == null)
            {
                result = 0;
                return false;
            }
            return TryParseFloat(s, false, out result);
        }

        internal static double ParseFloat(string s, bool single)
        {
            if (s == null)
            {
                throw new ArgumentNullException("s");
            }
            if (!TryParseFloat(s, single, out double result))
            {
                throw new FormatException("The input string '" + s + "' was not in a correct format.");
            }
            return result;
        }

        // Разбор по правилам .NET с инвариантной культурой: пробелы, знак,
        // разряды через запятую, экспонента, NaN и Infinity. `single` — к
        // ближайшему float, а не к ближайшему double.
        [MethodImpl(MethodImplOptions.InternalCall)]
        internal static extern bool TryParseFloat(string s, bool single, out double result);
    }
}
