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

    // Печать дробных чисел — кратчайшее представление, читаемое обратно в то же
    // число, — фаза N4c. До неё `ToString` у них наследуется от ValueType и
    // печатает имя типа; образцы дробные числа не печатают.
    public struct Single
    {
    }

    public struct Double
    {
    }
}
