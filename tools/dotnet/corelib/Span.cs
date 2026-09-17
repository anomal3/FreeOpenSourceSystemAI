// Срезы (фаза N4d, пока только то, что уже нужно программам).
//
// Компилятор C# 14 пишет `array.Contains(x)` не через Linq, а через
// `MemoryExtensions.Contains(ReadOnlySpan<T>, T)` с неявным приведением
// массива к срезу. У .NET срез — `ref struct` поверх указателя; здесь — обычная
// структура поверх массива: программа видит только имена и сигнатуры.

using System.Collections.Generic;

namespace System
{
    public readonly struct ReadOnlySpan<T>
    {
        private readonly T[] array;
        private readonly int start;
        private readonly int length;

        public ReadOnlySpan(T[] array)
        {
            this.array = array;
            start = 0;
            length = array == null ? 0 : array.Length;
        }

        public ReadOnlySpan(T[] array, int start, int length)
        {
            if (array == null ? start != 0 || length != 0 : (uint)start > (uint)array.Length || (uint)length > (uint)(array.Length - start))
            {
                throw new ArgumentOutOfRangeException();
            }
            this.array = array;
            this.start = start;
            this.length = length;
        }

        public int Length => length;

        public bool IsEmpty => length == 0;

        public ReadOnlySpan<T> Slice(int start) => Slice(start, length - start);

        public ReadOnlySpan<T> Slice(int start, int length)
        {
            if ((uint)start > (uint)this.length || (uint)length > (uint)(this.length - start))
            {
                throw new ArgumentOutOfRangeException();
            }
            return new ReadOnlySpan<T>(array, this.start + start, length);
        }

        public Enumerator GetEnumerator() => new Enumerator(this);

        // Перечислитель среза для `foreach` (фаза N10c, ReadOnlySet из
        // dotnet/runtime). У .NET он ref struct; здесь срез — обычная структура.
        public struct Enumerator
        {
            private readonly ReadOnlySpan<T> span;
            private int index;

            internal Enumerator(ReadOnlySpan<T> span)
            {
                this.span = span;
                index = -1;
            }

            public bool MoveNext()
            {
                int next = index + 1;
                if (next < span.Length)
                {
                    index = next;
                    return true;
                }
                return false;
            }

            public T Current => span.ItemAt(index);
        }

        internal T ItemAt(int index)
        {
            if ((uint)index >= (uint)length)
            {
                throw new IndexOutOfRangeException();
            }
            return array[start + index];
        }

        // Фаза N10: `nodes[i].Element` в PriorityQueue читает поле элемента
        // прямо по ссылке, без копии всей пары.
        public ref readonly T this[int index]
        {
            get
            {
                if ((uint)index >= (uint)length)
                {
                    throw new IndexOutOfRangeException();
                }
                return ref array[start + index];
            }
        }

        public T[] ToArray()
        {
            T[] copy = new T[length];
            for (int i = 0; i < length; i++)
            {
                copy[i] = array[start + i];
            }
            return copy;
        }

        public static implicit operator ReadOnlySpan<T>(T[] array) => new ReadOnlySpan<T>(array);

        // Как у .NET: срез знаков печатается строкой (AlternateLookup словаря
        // делает из него ключ), любой другой — именем типа и длиной.
        public override string ToString()
        {
            if (typeof(T) == typeof(char))
            {
                return new string((char[])(object)array, start, length);
            }
            return "System.ReadOnlySpan<" + typeof(T).Name + ">[" + length + "]";
        }
    }

    public static class MemoryExtensions
    {
        public static Span<T> AsSpan<T>(this T[] array, int start, int length) => new Span<T>(array, start, length);

        // `ReadOnlySpan<char> s = "literal"` компилятор пишет этим вызовом, а не
        // приведением строки (фаза N10d). Копия знаков, как и у приведения.
        public static ReadOnlySpan<char> AsSpan(this string text) => text;

        // Фаза N10d: HashHelpers из CoreLib ищет простое число в таблице.
        public static int BinarySearch<T>(this ReadOnlySpan<T> span, T value) where T : IComparable<T>
        {
            int low = 0;
            int high = span.Length - 1;
            while (low <= high)
            {
                int middle = low + ((high - low) >> 1);
                int order = span.ItemAt(middle).CompareTo(value);
                if (order == 0)
                {
                    return middle;
                }
                if (order < 0)
                {
                    low = middle + 1;
                }
                else
                {
                    high = middle - 1;
                }
            }
            return ~low;
        }

        // Фаза N10d: сравнители строк из CoreLib сверяют срез знаков со строкой.
        // У .NET здесь `T : IEquatable<T>`; наши char и string этого интерфейса
        // не объявляют, а сравнение по умолчанию идёт через него и так.
        public static bool SequenceEqual<T>(this ReadOnlySpan<T> span, ReadOnlySpan<T> other)
        {
            if (span.Length != other.Length)
            {
                return false;
            }
            EqualityComparer<T> comparer = EqualityComparer<T>.Default;
            for (int i = 0; i < span.Length; i++)
            {
                if (!comparer.Equals(span.ItemAt(i), other.ItemAt(i)))
                {
                    return false;
                }
            }
            return true;
        }

        internal static bool EqualsOrdinalIgnoreCase(this ReadOnlySpan<char> span, ReadOnlySpan<char> other)
        {
            if (span.Length != other.Length)
            {
                return false;
            }
            for (int i = 0; i < span.Length; i++)
            {
                if (char.ToUpperInvariant(span.ItemAt(i)) != char.ToUpperInvariant(other.ItemAt(i)))
                {
                    return false;
                }
            }
            return true;
        }

        public static bool Contains<T>(this ReadOnlySpan<T> span, T value) where T : IEquatable<T> => IndexOf(span, value) >= 0;

        public static int IndexOf<T>(this ReadOnlySpan<T> span, T value) where T : IEquatable<T>
        {
            EqualityComparer<T> comparer = EqualityComparer<T>.Default;
            for (int i = 0; i < span.Length; i++)
            {
                if (comparer.Equals(span.ItemAt(i), value))
                {
                    return i;
                }
            }
            return -1;
        }
    }
}

namespace System.Runtime.CompilerServices
{
    // Метка методов-расширений (`this` у первого параметра).
    [AttributeUsage(AttributeTargets.Class | AttributeTargets.Method, Inherited = false)]
    public sealed class ExtensionAttribute : Attribute
    {
    }
}
