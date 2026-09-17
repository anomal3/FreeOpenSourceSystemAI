// Строка и консоль.

using System.Runtime.CompilerServices;
using System.Text;

namespace System
{
    public sealed partial class String : IComparable<string>
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

        public static string Concat(Collections.Generic.IEnumerable<string> values)
        {
            if (values == null)
            {
                throw new ArgumentNullException("values");
            }
            var builder = new Text.StringBuilder();
            foreach (string value in values)
            {
                builder.Append(value);
            }
            return builder.ToString();
        }

        public static string Concat(object arg0, object arg1) => Concat(arg0?.ToString(), arg1?.ToString());

        public static string Concat(object arg0, object arg1, object arg2) =>
            Concat(arg0?.ToString(), arg1?.ToString(), arg2?.ToString());

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern bool Equals(string a, string b);

        public static bool operator ==(string a, string b) => Equals(a, b);

        public static bool operator !=(string a, string b) => !Equals(a, b);

        public static bool IsNullOrEmpty(string value) => value == null || value.Length == 0;

        // Порядковое сравнение единиц UTF-16. У .NET `CompareTo` культурное, и
        // на смешанном регистре и диакритике порядок разойдётся — это фаза N4.
        [MethodImpl(MethodImplOptions.InternalCall)]
        public extern int CompareTo(string strB);

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

        // Куда пишет программа, если она сама это назначила (`SetOut`). Пока
        // `null`, текст идёт прямо в стандартный вывод задачи, мимо писателя:
        // обёртка над выводом нужна только тем, кто её подменяет, а платить за
        // лишний вызов на каждой строке пришлось бы всем.
        private static System.IO.TextWriter redirected;

        public static System.IO.TextWriter Out => redirected ??= new StdoutWriter();

        public static void SetOut(System.IO.TextWriter newOut)
        {
            if (newOut == null)
            {
                throw new ArgumentNullException("newOut");
            }
            redirected = newOut is StdoutWriter ? null : newOut;
        }

        public static void Write(string value)
        {
            if (redirected != null && !(redirected is StdoutWriter))
            {
                redirected.Write(value);
                return;
            }
            StdoutWrite(value);
        }

        public static void WriteLine(string value)
        {
            if (redirected != null && !(redirected is StdoutWriter))
            {
                redirected.WriteLine(value);
                return;
            }
            StdoutWriteLine(value);
        }

        public static void WriteLine()
        {
            if (redirected != null && !(redirected is StdoutWriter))
            {
                redirected.WriteLine();
                return;
            }
            StdoutWriteLine();
        }

        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern void StdoutWrite(string value);

        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern void StdoutWriteLine(string value);

        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern void StdoutWriteLine();

        // Писатель над стандартным выводом — то, что отдаёт `Console.Out`, пока
        // его не подменили.
        private sealed class StdoutWriter : System.IO.TextWriter
        {
            public override void Write(char value) => StdoutWrite(value.ToString());

            public override void Write(string value) => StdoutWrite(value);

            public override void WriteLine(string value) => StdoutWriteLine(value);

            public override void WriteLine() => StdoutWriteLine();
        }

        public static void Write(object value) => Write(value?.ToString());

        public static void Write(bool value) => Write(value.ToString());

        public static void Write(char value) => Write(value.ToString());

        public static void Write(int value) => Write(value.ToString());

        public static void Write(uint value) => Write(value.ToString());

        public static void Write(long value) => Write(value.ToString());

        public static void Write(ulong value) => Write(value.ToString());

        public static void Write(float value) => Write(value.ToString());

        public static void Write(double value) => Write(value.ToString());

        public static void WriteLine(float value) => WriteLine(value.ToString());

        public static void WriteLine(double value) => WriteLine(value.ToString());

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

        // Кодировка у своей среды одна — UTF-8 (фаза N5a), и перекодирует её Rust:
        // неверные последовательности становятся U+FFFD так же, как у .NET.
        public virtual string GetString(byte[] bytes)
        {
            if (bytes == null)
            {
                throw new ArgumentNullException("bytes");
            }
            return DecodeUtf8(bytes, 0, bytes.Length);
        }

        public virtual string GetString(byte[] bytes, int index, int count)
        {
            if (bytes == null)
            {
                throw new ArgumentNullException("bytes");
            }
            if (index < 0 || count < 0 || index > bytes.Length - count)
            {
                throw new ArgumentOutOfRangeException(index < 0 ? "index" : "count");
            }
            return DecodeUtf8(bytes, index, count);
        }

        public virtual byte[] GetBytes(string s)
        {
            if (s == null)
            {
                throw new ArgumentNullException("s");
            }
            return EncodeUtf8(s);
        }

        public virtual int GetByteCount(string s) => GetBytes(s).Length;

        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern string DecodeUtf8(byte[] bytes, int index, int count);

        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern byte[] EncodeUtf8(string s);
    }

    public sealed class UTF8Encoding : Encoding
    {
        public override string WebName => "utf-8";
    }
}
