// Коллекции (фаза N4b).
//
// `Dictionary` и `HashSet` устроены так же, как у .NET: массив записей в
// порядке добавления, корзины со ссылками на записи и список свободных мест.
// Порядок перебора — порядок записей, и освобождённое удалением место занимает
// следующий добавленный элемент. Программы на этот порядок полагаются (хоть
// .NET и не обещает его в документации), а хэш на него не влияет вовсе —
// поэтому повторяется устройство, а не хэш-функция.
//
// Сортировка — быстрая с вставками на коротких отрезках. У равных элементов
// порядок после неё может отличаться от .NET (там introsort) — как и между
// версиями самого .NET: сортировка `List<T>.Sort` не устойчивая.

using System.Collections.Generic;

namespace System.Collections
{
    public interface IEnumerable
    {
        IEnumerator GetEnumerator();
    }

    public interface IEnumerator
    {
        object Current { get; }

        bool MoveNext();

        void Reset();
    }
}

namespace System.Collections.Generic
{
    public interface IEnumerable<out T> : IEnumerable
    {
        new IEnumerator<T> GetEnumerator();
    }

    public interface IEnumerator<out T> : IDisposable, IEnumerator
    {
        new T Current { get; }
    }

    public interface IReadOnlyCollection<out T> : IEnumerable<T>
    {
        int Count { get; }
    }

    public interface IReadOnlyList<out T> : IReadOnlyCollection<T>
    {
        T this[int index] { get; }
    }

    public interface ICollection<T> : IEnumerable<T>
    {
        int Count { get; }

        bool IsReadOnly { get; }

        void Add(T item);

        void Clear();

        bool Contains(T item);

        void CopyTo(T[] array, int arrayIndex);

        bool Remove(T item);
    }

    public interface IList<T> : ICollection<T>
    {
        T this[int index] { get; set; }

        int IndexOf(T item);

        void Insert(int index, T item);

        void RemoveAt(int index);
    }

    public interface IEqualityComparer<in T>
    {
        bool Equals(T x, T y);

        int GetHashCode(T obj);
    }

    public interface IComparer<in T>
    {
        int Compare(T x, T y);
    }

    public abstract class EqualityComparer<T> : IEqualityComparer<T>
    {
        public static EqualityComparer<T> Default { get; } = new ObjectEqualityComparer<T>();

        public abstract bool Equals(T x, T y);

        public abstract int GetHashCode(T obj);
    }

    // У .NET для `T : IEquatable<T>` создаётся отдельный сравниватель через
    // отражение. Здесь проверка типа на месте: упаковка значения на каждое
    // сравнение — цена, у интерпретатора незаметная.
    internal sealed class ObjectEqualityComparer<T> : EqualityComparer<T>
    {
        public override bool Equals(T x, T y)
        {
            if (x == null)
            {
                return y == null;
            }
            if (y == null)
            {
                return false;
            }
            if (x is IEquatable<T> equatable)
            {
                return equatable.Equals(y);
            }
            return x.Equals(y);
        }

        public override int GetHashCode(T obj) => obj == null ? 0 : obj.GetHashCode();
    }

    public abstract class Comparer<T> : IComparer<T>
    {
        public static Comparer<T> Default { get; } = new ObjectComparer<T>();

        public abstract int Compare(T x, T y);
    }

    internal sealed class ObjectComparer<T> : Comparer<T>
    {
        public override int Compare(T x, T y)
        {
            if (x == null)
            {
                return y == null ? 0 : -1;
            }
            if (y == null)
            {
                return 1;
            }
            if (x is IComparable<T> typed)
            {
                return typed.CompareTo(y);
            }
            if (x is IComparable untyped)
            {
                return untyped.CompareTo(y);
            }
            throw new InvalidOperationException("Failed to compare two elements in the array.");
        }
    }

    internal sealed class ComparisonComparer<T> : Comparer<T>
    {
        private readonly Comparison<T> comparison;

        public ComparisonComparer(Comparison<T> comparison) => this.comparison = comparison;

        public override int Compare(T x, T y) => comparison(x, y);
    }

    public readonly struct KeyValuePair<TKey, TValue>
    {
        public KeyValuePair(TKey key, TValue value)
        {
            Key = key;
            Value = value;
        }

        public TKey Key { get; }

        public TValue Value { get; }

        public override string ToString() => "[" + Key?.ToString() + ", " + Value?.ToString() + "]";
    }

    public class KeyNotFoundException : SystemException
    {
        public KeyNotFoundException()
            : base("The given key was not present in the dictionary.")
        {
        }

        public KeyNotFoundException(string message)
            : base(message)
        {
        }
    }

}

namespace System
{
    // Реализация интерфейсов коллекций у массива. Массив `T[]` в среде не
    // наследует от этого класса: при вызове `IList<T>` на массиве среда ищет
    // метод здесь и зовёт его с самим массивом в `this` — так же, как
    // `SZArrayHelper` в CoreCLR. Полей у класса нет и быть не может.
    internal sealed class SZArrayHelper<T> : IList<T>, IReadOnlyList<T>
    {
        private SZArrayHelper()
        {
        }

        private T[] Items => (T[])(object)this;

        public int Count => Items.Length;

        public bool IsReadOnly => true;

        public T this[int index]
        {
            get => Items[index];
            set => Items[index] = value;
        }

        public void Add(T item) => throw FixedSize();

        public void Clear() => throw FixedSize();

        public void Insert(int index, T item) => throw FixedSize();

        public bool Remove(T item) => throw FixedSize();

        public void RemoveAt(int index) => throw FixedSize();

        public bool Contains(T item) => Array.IndexOf(Items, item) >= 0;

        public int IndexOf(T item) => Array.IndexOf(Items, item);

        public void CopyTo(T[] array, int arrayIndex)
        {
            T[] items = Items;
            for (int i = 0; i < items.Length; i++)
            {
                array[arrayIndex + i] = items[i];
            }
        }

        public IEnumerator<T> GetEnumerator() => new ArrayEnumerator<T>(Items);

        Collections.IEnumerator Collections.IEnumerable.GetEnumerator() => new ArrayEnumerator<T>(Items);

        private static NotSupportedException FixedSize() => new NotSupportedException("Collection was of a fixed size.");
    }

    internal sealed class ArrayEnumerator<T> : IEnumerator<T>
    {
        private readonly T[] items;
        private int index = -1;

        public ArrayEnumerator(T[] items) => this.items = items;

        public T Current => items[index];

        object Collections.IEnumerator.Current => items[index];

        public bool MoveNext() => ++index < items.Length;

        public void Reset() => index = -1;

        public void Dispose()
        {
        }
    }
}
