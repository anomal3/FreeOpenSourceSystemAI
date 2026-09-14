// Методы строк, форматирование, разбор чисел, StringBuilder и Math (фаза N4a).
//
// Культур у FreeOS нет: сравнение и смена регистра порядковые, как у .NET в
// режиме инвариантной глобализации (`DOTNET_SYSTEM_GLOBALIZATION_INVARIANT=1`,
// с ним `clr-check` и снимает эталон). Члены, которым нужны единицы UTF-16
// строки напрямую, — `InternalCall`; всё, что можно собрать из них, — здесь.

using System.Runtime.CompilerServices;
using System.Text;

namespace System
{
    public enum StringComparison
    {
        CurrentCulture = 0,
        CurrentCultureIgnoreCase = 1,
        InvariantCulture = 2,
        InvariantCultureIgnoreCase = 3,
        Ordinal = 4,
        OrdinalIgnoreCase = 5,
    }

    [Flags]
    public enum StringSplitOptions
    {
        None = 0,
        RemoveEmptyEntries = 1,
        TrimEntries = 2,
    }

    public sealed partial class String
    {
        [MethodImpl(MethodImplOptions.InternalCall)]
        public extern String(char c, int count);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public extern String(char[] value);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public extern String(char[] value, int startIndex, int length);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public extern char[] ToCharArray();

        [MethodImpl(MethodImplOptions.InternalCall)]
        public extern string Substring(int startIndex, int length);

        public string Substring(int startIndex) => Substring(startIndex, Length - startIndex);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public extern int IndexOf(char value, int startIndex);

        public int IndexOf(char value) => IndexOf(value, 0);

        // Порядковый поиск (см. начало файла).
        [MethodImpl(MethodImplOptions.InternalCall)]
        public extern int IndexOf(string value, int startIndex);

        public int IndexOf(string value) => IndexOf(value, 0);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public extern int LastIndexOf(char value);

        public bool Contains(string value) => IndexOf(value, 0) >= 0;

        public bool Contains(char value) => IndexOf(value, 0) >= 0;

        public bool StartsWith(string value)
        {
            if (value == null)
            {
                throw new ArgumentNullException("value");
            }
            return value.Length <= Length && Equals(Substring(0, value.Length), value);
        }

        public bool EndsWith(string value)
        {
            if (value == null)
            {
                throw new ArgumentNullException("value");
            }
            return value.Length <= Length && Equals(Substring(Length - value.Length), value);
        }

        public string Trim() => TrimCore(true, true);

        public string TrimStart() => TrimCore(true, false);

        public string TrimEnd() => TrimCore(false, true);

        private string TrimCore(bool start, bool end)
        {
            int first = 0;
            int last = Length;
            if (start)
            {
                while (first < last && char.IsWhiteSpace(this[first]))
                {
                    first++;
                }
            }
            if (end)
            {
                while (last > first && char.IsWhiteSpace(this[last - 1]))
                {
                    last--;
                }
            }
            return first == 0 && last == Length ? this : Substring(first, last - first);
        }

        [MethodImpl(MethodImplOptions.InternalCall)]
        public extern string ToUpper();

        [MethodImpl(MethodImplOptions.InternalCall)]
        public extern string ToLower();

        public string ToUpperInvariant() => ToUpper();

        public string ToLowerInvariant() => ToLower();

        [MethodImpl(MethodImplOptions.InternalCall)]
        public extern string Replace(string oldValue, string newValue);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public extern string Replace(char oldChar, char newChar);

        public string PadLeft(int totalWidth) => PadLeft(totalWidth, ' ');

        public string PadLeft(int totalWidth, char paddingChar) =>
            Length >= totalWidth ? this : Concat(new string(paddingChar, totalWidth - Length), this);

        public string PadRight(int totalWidth) => PadRight(totalWidth, ' ');

        public string PadRight(int totalWidth, char paddingChar) =>
            Length >= totalWidth ? this : Concat(this, new string(paddingChar, totalWidth - Length));

        public string Insert(int startIndex, string value) => Concat(Substring(0, startIndex), value, Substring(startIndex));

        public string Remove(int startIndex) => Substring(0, startIndex);

        public string Remove(int startIndex, int count) => Concat(Substring(0, startIndex), Substring(startIndex + count));

        public static bool IsNullOrWhiteSpace(string value)
        {
            if (value == null)
            {
                return true;
            }
            for (int i = 0; i < value.Length; i++)
            {
                if (!char.IsWhiteSpace(value[i]))
                {
                    return false;
                }
            }
            return true;
        }

        public static bool Equals(string a, string b, StringComparison comparisonType)
        {
            bool ignoreCase = comparisonType == StringComparison.OrdinalIgnoreCase
                || comparisonType == StringComparison.CurrentCultureIgnoreCase
                || comparisonType == StringComparison.InvariantCultureIgnoreCase;
            if (!ignoreCase || a == null || b == null)
            {
                return ignoreCase ? (object)a == (object)b : Equals(a, b);
            }
            return a.Length == b.Length && CompareOrdinal(a.ToUpper(), b.ToUpper()) == 0;
        }

        // Разница первых несовпавших единиц или длин, как у .NET.
        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern int CompareOrdinal(string strA, string strB);

        public static int Compare(string strA, string strB)
        {
            int difference = CompareOrdinal(strA, strB);
            return difference < 0 ? -1 : difference > 0 ? 1 : 0;
        }

        public string[] Split(char separator, StringSplitOptions options = StringSplitOptions.None) =>
            SplitCore(new[] { separator }, options);

        public string[] Split(params char[] separator) => SplitCore(separator, StringSplitOptions.None);

        private string[] SplitCore(char[] separators, StringSplitOptions options)
        {
            int count = 1;
            for (int i = 0; i < Length; i++)
            {
                if (IsSeparator(this[i], separators))
                {
                    count++;
                }
            }
            string[] pieces = new string[count];
            int kept = 0;
            int start = 0;
            for (int i = 0; i <= Length; i++)
            {
                if (i < Length && !IsSeparator(this[i], separators))
                {
                    continue;
                }
                string piece = Substring(start, i - start);
                if ((options & StringSplitOptions.TrimEntries) != 0)
                {
                    piece = piece.Trim();
                }
                if ((options & StringSplitOptions.RemoveEmptyEntries) == 0 || piece.Length > 0)
                {
                    pieces[kept++] = piece;
                }
                start = i + 1;
            }
            if (kept == count)
            {
                return pieces;
            }
            string[] result = new string[kept];
            for (int i = 0; i < kept; i++)
            {
                result[i] = pieces[i];
            }
            return result;
        }

        private static bool IsSeparator(char c, char[] separators)
        {
            if (separators == null || separators.Length == 0)
            {
                return char.IsWhiteSpace(c);
            }
            for (int i = 0; i < separators.Length; i++)
            {
                if (separators[i] == c)
                {
                    return true;
                }
            }
            return false;
        }

        public static string Join(string separator, params string[] value)
        {
            var builder = new StringBuilder();
            for (int i = 0; i < value.Length; i++)
            {
                if (i > 0)
                {
                    builder.Append(separator);
                }
                builder.Append(value[i]);
            }
            return builder.ToString();
        }

        public static string Join(string separator, string[] value, int startIndex, int count)
        {
            if (value == null)
            {
                throw new ArgumentNullException("value");
            }
            if (startIndex < 0 || count < 0 || startIndex > value.Length - count)
            {
                throw new ArgumentOutOfRangeException("startIndex", "Index and count must refer to a location within the buffer.");
            }
            var builder = new StringBuilder();
            for (int i = 0; i < count; i++)
            {
                if (i > 0)
                {
                    builder.Append(separator);
                }
                builder.Append(value[startIndex + i]);
            }
            return builder.ToString();
        }

        public static string Join(string separator, Collections.Generic.IEnumerable<string> values)
        {
            var builder = new StringBuilder();
            bool first = true;
            foreach (string value in values)
            {
                if (!first)
                {
                    builder.Append(separator);
                }
                builder.Append(value);
                first = false;
            }
            return builder.ToString();
        }

        public static string Join<T>(string separator, Collections.Generic.IEnumerable<T> values)
        {
            var builder = new StringBuilder();
            bool first = true;
            foreach (T value in values)
            {
                if (!first)
                {
                    builder.Append(separator);
                }
                builder.Append(value?.ToString());
                first = false;
            }
            return builder.ToString();
        }

        public static string Format(string format, object arg0) => FormatCore(format, new[] { arg0 });

        public static string Format(string format, object arg0, object arg1) => FormatCore(format, new[] { arg0, arg1 });

        public static string Format(string format, object arg0, object arg1, object arg2) =>
            FormatCore(format, new[] { arg0, arg1, arg2 });

        public static string Format(string format, params object[] args) => FormatCore(format, args);

        private static string FormatCore(string format, object[] args)
        {
            if (format == null)
            {
                throw new ArgumentNullException("format");
            }
            var builder = new StringBuilder();
            int i = 0;
            int n = format.Length;
            while (i < n)
            {
                char c = format[i];
                if (c == '}')
                {
                    if (i + 1 < n && format[i + 1] == '}')
                    {
                        builder.Append('}');
                        i += 2;
                        continue;
                    }
                    throw BadFormat();
                }
                if (c != '{')
                {
                    builder.Append(c);
                    i++;
                    continue;
                }
                if (i + 1 < n && format[i + 1] == '{')
                {
                    builder.Append('{');
                    i += 2;
                    continue;
                }
                i++;
                int index = 0;
                int digits = 0;
                while (i < n && format[i] >= '0' && format[i] <= '9')
                {
                    index = index * 10 + (format[i] - '0');
                    i++;
                    digits++;
                }
                if (digits == 0)
                {
                    throw BadFormat();
                }
                int alignment = 0;
                if (i < n && format[i] == ',')
                {
                    i++;
                    bool left = i < n && format[i] == '-';
                    if (left)
                    {
                        i++;
                    }
                    while (i < n && format[i] >= '0' && format[i] <= '9')
                    {
                        alignment = alignment * 10 + (format[i] - '0');
                        i++;
                    }
                    if (left)
                    {
                        alignment = -alignment;
                    }
                }
                string itemFormat = null;
                if (i < n && format[i] == ':')
                {
                    int start = ++i;
                    while (i < n && format[i] != '}')
                    {
                        i++;
                    }
                    itemFormat = format.Substring(start, i - start);
                }
                if (i >= n || format[i] != '}')
                {
                    throw BadFormat();
                }
                i++;
                if (index >= args.Length)
                {
                    throw new FormatException("Index (zero based) must be greater than or equal to zero and less than the size of the argument list.");
                }
                builder.Append(Align(FormatItem(args[index], itemFormat), alignment));
            }
            return builder.ToString();
        }

        internal static string FormatItem(object value, string format) =>
            value is IFormattable formattable ? formattable.ToString(format, null) : value?.ToString();

        /// Положительное выравнивание — вправо, отрицательное — влево.
        internal static string Align(string text, int alignment)
        {
            text = text ?? Empty;
            if (alignment > text.Length)
            {
                return Concat(new string(' ', alignment - text.Length), text);
            }
            if (-alignment > text.Length)
            {
                return Concat(text, new string(' ', -alignment - text.Length));
            }
            return text;
        }

        private static FormatException BadFormat() => new FormatException("Input string was not in a correct format.");
    }

    internal static class Number
    {
        /// 0 — разобрано, 1 — не число, 2 — не помещается. Пробелы по краям
        /// допустимы, как у `NumberStyles.Integer`.
        internal static int TryParseInteger(string s, long min, long max, out long result)
        {
            result = 0;
            if (s == null)
            {
                return 1;
            }
            int i = 0;
            int end = s.Length;
            while (i < end && char.IsWhiteSpace(s[i]))
            {
                i++;
            }
            while (end > i && char.IsWhiteSpace(s[end - 1]))
            {
                end--;
            }
            bool negative = false;
            if (i < end && (s[i] == '-' || s[i] == '+'))
            {
                negative = s[i] == '-';
                i++;
            }
            if (i == end)
            {
                return 1;
            }
            ulong limit = negative ? (ulong)(-(min + 1)) + 1 : (ulong)max;
            ulong magnitude = 0;
            bool overflow = false;
            for (; i < end; i++)
            {
                char c = s[i];
                if (c < '0' || c > '9')
                {
                    return 1;
                }
                ulong digit = (ulong)(c - '0');
                if (overflow || magnitude > (limit - digit) / 10)
                {
                    overflow = true;
                    continue;
                }
                magnitude = magnitude * 10 + digit;
            }
            if (overflow)
            {
                return 2;
            }
            result = negative ? (long)(0 - magnitude) : (long)magnitude;
            return 0;
        }

        internal static long ParseInteger(string s, long min, long max, string typeName)
        {
            if (s == null)
            {
                throw new ArgumentNullException("s");
            }
            int status = TryParseInteger(s, min, max, out long value);
            if (status == 1)
            {
                throw new FormatException("The input string '" + s + "' was not in a correct format.");
            }
            if (status == 2)
            {
                throw new OverflowException("Value was either too large or too small for an " + typeName + ".");
            }
            return value;
        }
    }

    public static class Math
    {
        public static int Max(int val1, int val2) => val1 >= val2 ? val1 : val2;

        public static long Max(long val1, long val2) => val1 >= val2 ? val1 : val2;

        public static int Min(int val1, int val2) => val1 <= val2 ? val1 : val2;

        public static long Min(long val1, long val2) => val1 <= val2 ? val1 : val2;

        public static int Abs(int value)
        {
            if (value >= 0)
            {
                return value;
            }
            if (value == int.MinValue)
            {
                throw new OverflowException("Negating the minimum value of a twos complement number is invalid.");
            }
            return -value;
        }

        public static long Abs(long value)
        {
            if (value >= 0)
            {
                return value;
            }
            if (value == long.MinValue)
            {
                throw new OverflowException("Negating the minimum value of a twos complement number is invalid.");
            }
            return -value;
        }

        public static int Clamp(int value, int min, int max)
        {
            if (min > max)
            {
                throw new ArgumentException("'" + min + "' cannot be greater than " + max + ".");
            }
            return value < min ? min : value > max ? max : value;
        }

        public static long Clamp(long value, long min, long max)
        {
            if (min > max)
            {
                throw new ArgumentException("'" + min + "' cannot be greater than " + max + ".");
            }
            return value < min ? min : value > max ? max : value;
        }

        public static int Sign(int value) => value < 0 ? -1 : value > 0 ? 1 : 0;

        public static int Sign(long value) => value < 0 ? -1 : value > 0 ? 1 : 0;

        // Дробная математика (фаза N4c). Функции — в Rust, из крейта `libm`
        // (порт musl). Точность у них та же, что у C-библиотек, но последний
        // бит синуса или степени у разных библиотек может различаться — и у
        // самого .NET на Windows и Linux тоже. Корень, округления и модуль
        // точны везде.
        public const double PI = 3.14159265358979323846;
        public const double E = 2.7182818284590452354;
        public const double Tau = 6.283185307179586476925;

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern double Sqrt(double d);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern double Cbrt(double d);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern double Sin(double a);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern double Cos(double d);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern double Tan(double a);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern double Asin(double d);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern double Acos(double d);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern double Atan(double d);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern double Atan2(double y, double x);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern double Sinh(double value);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern double Cosh(double value);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern double Tanh(double value);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern double Exp(double d);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern double Log(double d);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern double Log10(double d);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern double Log2(double x);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern double Pow(double x, double y);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern double Floor(double d);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern double Ceiling(double a);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern double Truncate(double d);

        /// Округление к ближайшему, половина — к чётному.
        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern double Round(double a);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern double Abs(double value);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern float Abs(float value);

        public static double Log(double a, double newBase)
        {
            if (double.IsNaN(a))
            {
                return a;
            }
            if (double.IsNaN(newBase))
            {
                return newBase;
            }
            if (newBase == 1)
            {
                return double.NaN;
            }
            if (a != 1 && (newBase == 0 || double.IsPositiveInfinity(newBase)))
            {
                return double.NaN;
            }
            return Log(a) / Log(newBase);
        }

        // Максимум и минимум по IEEE 754-2019: NaN побеждает, +0 больше -0.
        public static double Max(double val1, double val2)
        {
            if (val1 != val2)
            {
                if (!double.IsNaN(val1))
                {
                    return val2 < val1 ? val1 : val2;
                }
                return val1;
            }
            return double.IsNegative(val2) ? val1 : val2;
        }

        public static double Min(double val1, double val2)
        {
            if (val1 != val2)
            {
                if (!double.IsNaN(val1))
                {
                    return val1 < val2 ? val1 : val2;
                }
                return val1;
            }
            return double.IsNegative(val1) ? val1 : val2;
        }

        public static float Max(float val1, float val2)
        {
            if (val1 != val2)
            {
                if (!float.IsNaN(val1))
                {
                    return val2 < val1 ? val1 : val2;
                }
                return val1;
            }
            return float.IsNegative(val2) ? val1 : val2;
        }

        public static float Min(float val1, float val2)
        {
            if (val1 != val2)
            {
                if (!float.IsNaN(val1))
                {
                    return val1 < val2 ? val1 : val2;
                }
                return val1;
            }
            return float.IsNegative(val1) ? val1 : val2;
        }

        public static int Sign(double value)
        {
            if (value < 0)
            {
                return -1;
            }
            if (value > 0)
            {
                return 1;
            }
            if (value == 0)
            {
                return 0;
            }
            throw new ArithmeticException("Function does not accept floating point Not-a-Number values.");
        }

        public static int Sign(float value) => Sign((double)value);

        public static double Clamp(double value, double min, double max)
        {
            if (min > max)
            {
                throw new ArgumentException("'" + min + "' cannot be greater than " + max + ".");
            }
            if (value < min)
            {
                return min;
            }
            if (value > max)
            {
                return max;
            }
            return value;
        }

        public static double CopySign(double x, double y)
        {
            long magnitude = BitConverter.DoubleToInt64Bits(x) & 0x7FFFFFFFFFFFFFFF;
            long sign = BitConverter.DoubleToInt64Bits(y) & unchecked((long)0x8000000000000000);
            return BitConverter.Int64BitsToDouble(magnitude | sign);
        }

        public static double Round(double value, int digits) => Round(value, digits, MidpointRounding.ToEven);

        public static double Round(double value, MidpointRounding mode)
        {
            switch (mode)
            {
                case MidpointRounding.ToEven:
                    return Round(value);
                case MidpointRounding.AwayFromZero:
                    // Как у .NET: через дробную часть, а не `Truncate(value + 0.5)`,
                    // которое ошибается у 0.49999999999999994.
                    double whole = Truncate(value);
                    double fraction = value - whole;
                    if (Abs(fraction) >= 0.5)
                    {
                        whole += Sign(fraction);
                    }
                    return whole;
                case MidpointRounding.ToZero:
                    return Truncate(value);
                case MidpointRounding.ToNegativeInfinity:
                    return Floor(value);
                case MidpointRounding.ToPositiveInfinity:
                    return Ceiling(value);
                default:
                    throw new ArgumentException("The value '" + (int)mode + "' is not valid for this usage of the type MidpointRounding.", "mode");
            }
        }

        // Как у .NET: умножить на степень десяти, округлить, разделить. От
        // 1e16 у числа дробной части уже нет.
        public static double Round(double value, int digits, MidpointRounding mode)
        {
            if ((uint)digits > 15)
            {
                throw new ArgumentOutOfRangeException("digits", "Rounding digits must be between 0 and 15, inclusive.");
            }
            if (mode < MidpointRounding.ToEven || mode > MidpointRounding.ToPositiveInfinity)
            {
                throw new ArgumentException("The value '" + (int)mode + "' is not valid for this usage of the type MidpointRounding.", "mode");
            }
            if (Abs(value) < 1e16)
            {
                double power10 = Power10(digits);
                value *= power10;
                value = Round(value, mode);
                value /= power10;
            }
            return value;
        }

        private static double Power10(int digits)
        {
            double power = 1;
            for (int i = 0; i < digits; i++)
            {
                power *= 10;
            }
            return power;
        }
    }

    public enum MidpointRounding
    {
        ToEven = 0,
        AwayFromZero = 1,
        ToZero = 2,
        ToNegativeInfinity = 3,
        ToPositiveInfinity = 4,
    }

    public static class MathF
    {
        public const float PI = 3.14159265f;
        public const float E = 2.71828183f;

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern float Sqrt(float x);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern float Sin(float x);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern float Cos(float x);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern float Tan(float x);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern float Atan2(float y, float x);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern float Pow(float x, float y);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern float Exp(float x);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern float Log(float x);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern float Floor(float x);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern float Ceiling(float x);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern float Truncate(float x);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern float Round(float x);

        public static float Abs(float x) => Math.Abs(x);

        public static float Max(float x, float y) => Math.Max(x, y);

        public static float Min(float x, float y) => Math.Min(x, y);
    }

    // Биты чисел. Порядок байт у FreeOS на обеих архитектурах — от младшего.
    public static class BitConverter
    {
        public static readonly bool IsLittleEndian = true;

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern long DoubleToInt64Bits(double value);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern double Int64BitsToDouble(long value);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern int SingleToInt32Bits(float value);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern float Int32BitsToSingle(int value);

        public static ulong DoubleToUInt64Bits(double value) => (ulong)DoubleToInt64Bits(value);

        public static double UInt64BitsToDouble(ulong value) => Int64BitsToDouble((long)value);
    }
}

namespace System.Text
{
    public sealed class StringBuilder
    {
        private char[] chars;
        private int length;

        public StringBuilder()
        {
            chars = new char[16];
        }

        public StringBuilder(int capacity)
        {
            chars = new char[capacity < 1 ? 1 : capacity];
        }

        public StringBuilder(string value)
            : this()
        {
            Append(value);
        }

        public int Length
        {
            get => length;
            set
            {
                if (value < 0)
                {
                    throw new ArgumentOutOfRangeException("value");
                }
                Reserve(value);
                for (int i = length; i < value; i++)
                {
                    chars[i] = '\0';
                }
                length = value;
            }
        }

        [IndexerName("Chars")]
        public char this[int index]
        {
            get
            {
                if ((uint)index >= (uint)length)
                {
                    throw new IndexOutOfRangeException();
                }
                return chars[index];
            }
            set
            {
                if ((uint)index >= (uint)length)
                {
                    throw new ArgumentOutOfRangeException("index");
                }
                chars[index] = value;
            }
        }

        private void Reserve(int needed)
        {
            if (needed <= chars.Length)
            {
                return;
            }
            int size = chars.Length * 2;
            if (size < needed)
            {
                size = needed;
            }
            char[] bigger = new char[size];
            for (int i = 0; i < length; i++)
            {
                bigger[i] = chars[i];
            }
            chars = bigger;
        }

        public StringBuilder Append(char value)
        {
            Reserve(length + 1);
            chars[length++] = value;
            return this;
        }

        public StringBuilder Append(char value, int repeatCount)
        {
            Reserve(length + repeatCount);
            for (int i = 0; i < repeatCount; i++)
            {
                chars[length++] = value;
            }
            return this;
        }

        public StringBuilder Append(string value)
        {
            if (value == null)
            {
                return this;
            }
            char[] source = value.ToCharArray();
            Reserve(length + source.Length);
            for (int i = 0; i < source.Length; i++)
            {
                chars[length + i] = source[i];
            }
            length += source.Length;
            return this;
        }

        public StringBuilder Append(object value) => Append(value?.ToString());

        public StringBuilder Append(bool value) => Append(value.ToString());

        public StringBuilder Append(int value) => Append(value.ToString());

        public StringBuilder Append(uint value) => Append(value.ToString());

        public StringBuilder Append(long value) => Append(value.ToString());

        public StringBuilder Append(ulong value) => Append(value.ToString());

        public StringBuilder Append(float value) => Append(value.ToString());

        public StringBuilder Append(double value) => Append(value.ToString());

        // Перевод строки FreeOS — один LF; у .NET на Windows здесь CR LF.
        public StringBuilder AppendLine() => Append('\n');

        public StringBuilder AppendLine(string value) => Append(value).Append('\n');

        public StringBuilder Insert(int index, string value)
        {
            if ((uint)index > (uint)length)
            {
                throw new ArgumentOutOfRangeException("index");
            }
            if (value == null)
            {
                return this;
            }
            char[] source = value.ToCharArray();
            Reserve(length + source.Length);
            for (int i = length - 1; i >= index; i--)
            {
                chars[i + source.Length] = chars[i];
            }
            for (int i = 0; i < source.Length; i++)
            {
                chars[index + i] = source[i];
            }
            length += source.Length;
            return this;
        }

        public StringBuilder Remove(int startIndex, int length)
        {
            if (startIndex < 0 || length < 0 || startIndex + length > this.length)
            {
                throw new ArgumentOutOfRangeException("length");
            }
            for (int i = startIndex + length; i < this.length; i++)
            {
                chars[i - length] = chars[i];
            }
            this.length -= length;
            return this;
        }

        public StringBuilder Replace(string oldValue, string newValue)
        {
            string replaced = ToString().Replace(oldValue, newValue);
            length = 0;
            return Append(replaced);
        }

        public StringBuilder Replace(char oldChar, char newChar)
        {
            for (int i = 0; i < length; i++)
            {
                if (chars[i] == oldChar)
                {
                    chars[i] = newChar;
                }
            }
            return this;
        }

        public StringBuilder Clear()
        {
            length = 0;
            return this;
        }

        public override string ToString() => new string(chars, 0, length);
    }
}
