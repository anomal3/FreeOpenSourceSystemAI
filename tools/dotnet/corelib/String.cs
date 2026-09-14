// Строка и консоль.

using System.Runtime.CompilerServices;
using System.Text;

namespace System
{
    public sealed class String
    {
        public static readonly string Empty = "";

        public extern int Length
        {
            [MethodImpl(MethodImplOptions.InternalCall)]
            get;
        }

        [IndexerName("Chars")]
        public extern char this[int index]
        {
            [MethodImpl(MethodImplOptions.InternalCall)]
            get;
        }

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern string Concat(string str0, string str1);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern string Concat(string str0, string str1, string str2);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern string Concat(string str0, string str1, string str2, string str3);

        // Пять частей и больше компилятор склеивает массивом.
        public static string Concat(params string[] values)
        {
            string result = Empty;
            for (int i = 0; i < values.Length; i++)
            {
                result = Concat(result, values[i]);
            }
            return result;
        }

        public static string Concat(object arg0, object arg1) => Concat(arg0?.ToString(), arg1?.ToString());

        public static string Concat(object arg0, object arg1, object arg2) =>
            Concat(arg0?.ToString(), arg1?.ToString(), arg2?.ToString());

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern bool Equals(string a, string b);

        public static bool operator ==(string a, string b) => Equals(a, b);

        public static bool operator !=(string a, string b) => !Equals(a, b);

        public static bool IsNullOrEmpty(string value) => value == null || value.Length == 0;

        public override bool Equals(object obj) => obj is string other && Equals(this, other);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public override extern int GetHashCode();

        public override string ToString() => this;
    }

    public static class Console
    {
        private static Encoding outputEncoding;

        // Вывод своей среды всегда UTF-8, так что кодировка только запоминается.
        // Смысл у свойства один: программа, написанная для настоящего dotnet на
        // русской Windows (там без него кириллица уходит в кодовой странице 866),
        // работает здесь без правки.
        public static Encoding OutputEncoding
        {
            get => outputEncoding ?? Encoding.UTF8;
            set => outputEncoding = value;
        }

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern void Write(string value);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern void WriteLine(string value);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern void WriteLine();

        public static void Write(object value) => Write(value?.ToString());

        public static void Write(bool value) => Write(value.ToString());

        public static void Write(char value) => Write(value.ToString());

        public static void Write(int value) => Write(value.ToString());

        public static void Write(uint value) => Write(value.ToString());

        public static void Write(long value) => Write(value.ToString());

        public static void Write(ulong value) => Write(value.ToString());

        public static void WriteLine(object value) => WriteLine(value?.ToString());

        public static void WriteLine(bool value) => WriteLine(value.ToString());

        public static void WriteLine(char value) => WriteLine(value.ToString());

        public static void WriteLine(int value) => WriteLine(value.ToString());

        public static void WriteLine(uint value) => WriteLine(value.ToString());

        public static void WriteLine(long value) => WriteLine(value.ToString());

        public static void WriteLine(ulong value) => WriteLine(value.ToString());
    }
}

namespace System.Text
{
    public abstract class Encoding
    {
        private static Encoding utf8;

        public static Encoding UTF8 => utf8 ??= new UTF8Encoding();

        public abstract string WebName { get; }
    }

    public sealed class UTF8Encoding : Encoding
    {
        public override string WebName => "utf-8";
    }
}
