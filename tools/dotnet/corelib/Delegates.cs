// Делегаты, события, сравнение и интерполяция строк (фаза N3c).
//
// Тела `Invoke` и конструкторов делегатов компилятор не пишет: они
// «runtime managed», и выполняет их среда (объект делегата — список пар
// «цель, метод»). Склейка и удаление из списка тоже в среде
// (`Delegate.Combine`/`Remove`): список внутри объекта, снаружи C# его не видит.

namespace System
{
    public delegate void Action();

    public delegate void Action<in T>(T obj);

    public delegate void Action<in T1, in T2>(T1 arg1, T2 arg2);

    public delegate TResult Func<out TResult>();

    public delegate TResult Func<in T, out TResult>(T arg);

    public delegate TResult Func<in T1, in T2, out TResult>(T1 arg1, T2 arg2);

    public delegate bool Predicate<in T>(T obj);

    public delegate int Comparison<in T>(T x, T y);

    public delegate void EventHandler(object sender, EventArgs e);

    public delegate void EventHandler<TEventArgs>(object sender, TEventArgs e);

    public class EventArgs
    {
        public static readonly EventArgs Empty = new EventArgs();
    }

    public interface IComparable
    {
        int CompareTo(object obj);
    }

    public interface IComparable<in T>
    {
        int CompareTo(T other);
    }

    public interface IEquatable<T>
    {
        bool Equals(T other);
    }
}

namespace System.Threading
{
    public static class Interlocked
    {
        // Поток один (веха v0.7c), и сравнить с записью можно без атомарности.
        // Этим вызовом компилятор C# пишет `event +=` и `event -=`.
        public static T CompareExchange<T>(ref T location1, T value, T comparand)
            where T : class
        {
            T current = location1;
            if ((object)current == (object)comparand)
            {
                location1 = value;
            }
            return current;
        }

        public static int Increment(ref int location) => ++location;

        public static int Decrement(ref int location) => --location;
    }
}

namespace System.Runtime.CompilerServices
{
    // Так компилятор C# 10+ собирает `$"..."`. Настоящий обработчик — `ref
    // struct`, пишущий в арендованный буфер; здесь обычная структура со
    // склейкой: программа ссылается на него по имени, и разница ей не видна, а
    // `ref struct` потребовал бы от библиотеки ещё два служебных атрибута.
    public struct DefaultInterpolatedStringHandler
    {
        private string text;

        public DefaultInterpolatedStringHandler(int literalLength, int formattedCount)
        {
            text = string.Empty;
        }

        public void AppendLiteral(string value) => text = string.Concat(text, value);

        public void AppendFormatted(string value) => text = string.Concat(text, value);

        public void AppendFormatted(object value) => text = string.Concat(text, value?.ToString());

        public void AppendFormatted<T>(T value) => text = string.Concat(text, string.FormatItem(value, null));

        public void AppendFormatted<T>(T value, string format) => text = string.Concat(text, string.FormatItem(value, format));

        public void AppendFormatted<T>(T value, int alignment) =>
            text = string.Concat(text, string.Align(string.FormatItem(value, null), alignment));

        public void AppendFormatted<T>(T value, int alignment, string format) =>
            text = string.Concat(text, string.Align(string.FormatItem(value, format), alignment));

        public string ToStringAndClear()
        {
            string result = text;
            text = string.Empty;
            return result;
        }
    }
}
