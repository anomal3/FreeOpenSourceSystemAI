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
    }

    public static class MemoryExtensions
    {
        public static Span<T> AsSpan<T>(this T[] array, int start, int length) => new Span<T>(array, start, length);

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
