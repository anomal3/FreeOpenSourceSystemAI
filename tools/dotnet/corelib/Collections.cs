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

    internal static class ArraySortHelper<T>
    {
        public static void Sort(T[] items, int index, int length, IComparer<T> comparer)
        {
            comparer = comparer ?? Comparer<T>.Default;
            QuickSort(items, index, index + length - 1, comparer);
        }

        private static void QuickSort(T[] items, int left, int right, IComparer<T> comparer)
        {
            while (right - left >= 16)
            {
                int middle = left + (right - left) / 2;
                if (comparer.Compare(items[middle], items[left]) < 0)
                {
                    Swap(items, left, middle);
                }
                if (comparer.Compare(items[right], items[left]) < 0)
                {
                    Swap(items, left, right);
                }
                if (comparer.Compare(items[right], items[middle]) < 0)
                {
                    Swap(items, middle, right);
                }
                T pivot = items[middle];
                int i = left;
                int j = right;
                while (i <= j)
                {
                    while (comparer.Compare(items[i], pivot) < 0)
                    {
                        i++;
                    }
                    while (comparer.Compare(pivot, items[j]) < 0)
                    {
                        j--;
                    }
                    if (i <= j)
                    {
                        Swap(items, i, j);
                        i++;
                        j--;
                    }
                }
                // Меньшую половину — рекурсией, большую — циклом: глубина не
                // больше логарифма длины.
                if (j - left < right - i)
                {
                    QuickSort(items, left, j, comparer);
                    left = i;
                }
                else
                {
                    QuickSort(items, i, right, comparer);
                    right = j;
                }
            }
            for (int i = left + 1; i <= right; i++)
            {
                T item = items[i];
                int j = i - 1;
                while (j >= left && comparer.Compare(items[j], item) > 0)
                {
                    items[j + 1] = items[j];
                    j--;
                }
                items[j + 1] = item;
            }
        }

        private static void Swap(T[] items, int a, int b)
        {
            T item = items[a];
            items[a] = items[b];
            items[b] = item;
        }
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

    public class List<T> : IList<T>, IReadOnlyList<T>
    {
        private T[] items;
        private int size;
        private int version;

        public List()
        {
            items = new T[0];
        }

        public List(int capacity)
        {
            if (capacity < 0)
            {
                throw new ArgumentOutOfRangeException("capacity");
            }
            items = new T[capacity];
        }

        public List(IEnumerable<T> collection)
            : this()
        {
            AddRange(collection);
        }

        public int Count => size;

        public int Capacity => items.Length;

        bool ICollection<T>.IsReadOnly => false;

        public T this[int index]
        {
            get
            {
                if ((uint)index >= (uint)size)
                {
                    throw IndexOutOfRange();
                }
                return items[index];
            }
            set
            {
                if ((uint)index >= (uint)size)
                {
                    throw IndexOutOfRange();
                }
                items[index] = value;
                version++;
            }
        }

        private static ArgumentOutOfRangeException IndexOutOfRange() =>
            new ArgumentOutOfRangeException("index", "Index was out of range. Must be non-negative and less than the size of the collection.");

        private void Grow(int needed)
        {
            int capacity = items.Length == 0 ? 4 : items.Length * 2;
            if (capacity < needed)
            {
                capacity = needed;
            }
            T[] bigger = new T[capacity];
            for (int i = 0; i < size; i++)
            {
                bigger[i] = items[i];
            }
            items = bigger;
        }

        public void Add(T item)
        {
            if (size == items.Length)
            {
                Grow(size + 1);
            }
            items[size++] = item;
            version++;
        }

        public void AddRange(IEnumerable<T> collection)
        {
            if (collection == null)
            {
                throw new ArgumentNullException("collection");
            }
            foreach (T item in collection)
            {
                Add(item);
            }
        }

        public void Insert(int index, T item)
        {
            if ((uint)index > (uint)size)
            {
                throw new ArgumentOutOfRangeException("index", "Index must be within the bounds of the List.");
            }
            if (size == items.Length)
            {
                Grow(size + 1);
            }
            for (int i = size; i > index; i--)
            {
                items[i] = items[i - 1];
            }
            items[index] = item;
            size++;
            version++;
        }

        public bool Remove(T item)
        {
            int index = IndexOf(item);
            if (index < 0)
            {
                return false;
            }
            RemoveAt(index);
            return true;
        }

        public void RemoveAt(int index)
        {
            if ((uint)index >= (uint)size)
            {
                throw IndexOutOfRange();
            }
            size--;
            for (int i = index; i < size; i++)
            {
                items[i] = items[i + 1];
            }
            items[size] = default;
            version++;
        }

        public int RemoveAll(Predicate<T> match)
        {
            int kept = 0;
            for (int i = 0; i < size; i++)
            {
                if (!match(items[i]))
                {
                    items[kept++] = items[i];
                }
            }
            int removed = size - kept;
            for (int i = kept; i < size; i++)
            {
                items[i] = default;
            }
            size = kept;
            version++;
            return removed;
        }

        public void Clear()
        {
            for (int i = 0; i < size; i++)
            {
                items[i] = default;
            }
            size = 0;
            version++;
        }

        public bool Contains(T item) => IndexOf(item) >= 0;

        public int IndexOf(T item)
        {
            EqualityComparer<T> comparer = EqualityComparer<T>.Default;
            for (int i = 0; i < size; i++)
            {
                if (comparer.Equals(items[i], item))
                {
                    return i;
                }
            }
            return -1;
        }

        public T Find(Predicate<T> match)
        {
            for (int i = 0; i < size; i++)
            {
                if (match(items[i]))
                {
                    return items[i];
                }
            }
            return default;
        }

        public int FindIndex(Predicate<T> match)
        {
            for (int i = 0; i < size; i++)
            {
                if (match(items[i]))
                {
                    return i;
                }
            }
            return -1;
        }

        public List<T> FindAll(Predicate<T> match)
        {
            var found = new List<T>();
            for (int i = 0; i < size; i++)
            {
                if (match(items[i]))
                {
                    found.Add(items[i]);
                }
            }
            return found;
        }

        public bool Exists(Predicate<T> match) => FindIndex(match) >= 0;

        public void ForEach(Action<T> action)
        {
            int start = version;
            for (int i = 0; i < size; i++)
            {
                action(items[i]);
                if (version != start)
                {
                    throw new InvalidOperationException("Collection was modified; enumeration operation may not execute.");
                }
            }
        }

        public void Sort() => Sort((IComparer<T>)null);

        public void Sort(IComparer<T> comparer)
        {
            if (size > 1)
            {
                ArraySortHelper<T>.Sort(items, 0, size, comparer);
            }
            version++;
        }

        public void Sort(Comparison<T> comparison) => Sort(new ComparisonComparer<T>(comparison));

        public void Reverse()
        {
            for (int i = 0, j = size - 1; i < j; i++, j--)
            {
                T item = items[i];
                items[i] = items[j];
                items[j] = item;
            }
            version++;
        }

        public T[] ToArray()
        {
            T[] array = new T[size];
            for (int i = 0; i < size; i++)
            {
                array[i] = items[i];
            }
            return array;
        }

        public void CopyTo(T[] array, int arrayIndex)
        {
            for (int i = 0; i < size; i++)
            {
                array[arrayIndex + i] = items[i];
            }
        }

        public Enumerator GetEnumerator() => new Enumerator(this);

        IEnumerator<T> IEnumerable<T>.GetEnumerator() => new Enumerator(this);

        IEnumerator IEnumerable.GetEnumerator() => new Enumerator(this);

        public struct Enumerator : IEnumerator<T>
        {
            private readonly List<T> list;
            private readonly int version;
            private int index;
            private T current;

            internal Enumerator(List<T> list)
            {
                this.list = list;
                version = list.version;
                index = 0;
                current = default;
            }

            public T Current => current;

            object IEnumerator.Current => current;

            public bool MoveNext()
            {
                if (version != list.version)
                {
                    throw new InvalidOperationException("Collection was modified; enumeration operation may not execute.");
                }
                if (index < list.size)
                {
                    current = list.items[index];
                    index++;
                    return true;
                }
                current = default;
                return false;
            }

            public void Reset()
            {
                index = 0;
                current = default;
            }

            public void Dispose()
            {
            }
        }
    }

    public class Dictionary<TKey, TValue> : IEnumerable<KeyValuePair<TKey, TValue>>
    {
        // Номер записи, с которой начинается список свободных: `next` у
        // свободной записи хранит `StartOfFreeList - следующая`, и значения
        // меньше -1 отличают свободную запись от занятой (`next >= -1`).
        private const int StartOfFreeList = -3;

        private struct Entry
        {
            public int hashCode;
            public int next;
            public TKey key;
            public TValue value;
        }

        private int[] buckets;
        private Entry[] entries;
        private int count;
        private int freeList;
        private int freeCount;
        private int version;
        private readonly IEqualityComparer<TKey> comparer;

        public Dictionary()
            : this(null)
        {
        }

        public Dictionary(IEqualityComparer<TKey> comparer)
        {
            this.comparer = comparer ?? EqualityComparer<TKey>.Default;
        }

        public int Count => count - freeCount;

        public KeyCollection Keys => new KeyCollection(this);

        public ValueCollection Values => new ValueCollection(this);

        public TValue this[TKey key]
        {
            get
            {
                int index = FindEntry(key);
                if (index >= 0)
                {
                    return entries[index].value;
                }
                throw new KeyNotFoundException("The given key '" + key.ToString() + "' was not present in the dictionary.");
            }
            set => Insert(key, value, true);
        }

        private void Initialize(int capacity)
        {
            int size = capacity < 3 ? 3 : capacity;
            buckets = new int[size];
            entries = new Entry[size];
            freeList = -1;
        }

        private int Hash(TKey key) => comparer.GetHashCode(key) & 0x7FFFFFFF;

        private int FindEntry(TKey key)
        {
            if (key == null)
            {
                throw new ArgumentNullException("key");
            }
            if (buckets == null)
            {
                return -1;
            }
            int hash = Hash(key);
            int i = buckets[hash % buckets.Length] - 1;
            while (i >= 0)
            {
                if (entries[i].hashCode == hash && comparer.Equals(entries[i].key, key))
                {
                    return i;
                }
                i = entries[i].next;
            }
            return -1;
        }

        private void Insert(TKey key, TValue value, bool overwrite)
        {
            if (key == null)
            {
                throw new ArgumentNullException("key");
            }
            if (buckets == null)
            {
                Initialize(0);
            }
            int hash = Hash(key);
            int bucket = hash % buckets.Length;
            for (int i = buckets[bucket] - 1; i >= 0; i = entries[i].next)
            {
                if (entries[i].hashCode == hash && comparer.Equals(entries[i].key, key))
                {
                    if (!overwrite)
                    {
                        throw new ArgumentException("An item with the same key has already been added. Key: " + key.ToString());
                    }
                    entries[i].value = value;
                    return;
                }
            }
            int index;
            if (freeCount > 0)
            {
                index = freeList;
                freeList = StartOfFreeList - entries[freeList].next;
                freeCount--;
            }
            else
            {
                if (count == entries.Length)
                {
                    Resize();
                    bucket = hash % buckets.Length;
                }
                index = count;
                count++;
            }
            entries[index].hashCode = hash;
            entries[index].next = buckets[bucket] - 1;
            entries[index].key = key;
            entries[index].value = value;
            buckets[bucket] = index + 1;
            version++;
        }

        private void Resize()
        {
            int size = entries.Length * 2 + 1;
            Entry[] bigger = new Entry[size];
            for (int i = 0; i < count; i++)
            {
                bigger[i] = entries[i];
            }
            buckets = new int[size];
            for (int i = 0; i < count; i++)
            {
                if (bigger[i].next >= -1)
                {
                    int bucket = bigger[i].hashCode % size;
                    bigger[i].next = buckets[bucket] - 1;
                    buckets[bucket] = i + 1;
                }
            }
            entries = bigger;
        }

        public void Add(TKey key, TValue value) => Insert(key, value, false);

        public bool TryAdd(TKey key, TValue value)
        {
            if (FindEntry(key) >= 0)
            {
                return false;
            }
            Insert(key, value, false);
            return true;
        }

        public bool ContainsKey(TKey key) => FindEntry(key) >= 0;

        public bool TryGetValue(TKey key, out TValue value)
        {
            int index = FindEntry(key);
            if (index >= 0)
            {
                value = entries[index].value;
                return true;
            }
            value = default;
            return false;
        }

        public bool Remove(TKey key)
        {
            if (key == null)
            {
                throw new ArgumentNullException("key");
            }
            if (buckets == null)
            {
                return false;
            }
            int hash = Hash(key);
            int bucket = hash % buckets.Length;
            int last = -1;
            for (int i = buckets[bucket] - 1; i >= 0; last = i, i = entries[i].next)
            {
                if (entries[i].hashCode != hash || !comparer.Equals(entries[i].key, key))
                {
                    continue;
                }
                if (last < 0)
                {
                    buckets[bucket] = entries[i].next + 1;
                }
                else
                {
                    entries[last].next = entries[i].next;
                }
                entries[i].next = StartOfFreeList - freeList;
                entries[i].key = default;
                entries[i].value = default;
                freeList = i;
                freeCount++;
                return true;
            }
            return false;
        }

        public void Clear()
        {
            if (count > 0)
            {
                buckets = null;
                entries = null;
                count = 0;
                freeCount = 0;
                version++;
            }
        }

        public Enumerator GetEnumerator() => new Enumerator(this);

        IEnumerator<KeyValuePair<TKey, TValue>> IEnumerable<KeyValuePair<TKey, TValue>>.GetEnumerator() => new Enumerator(this);

        IEnumerator IEnumerable.GetEnumerator() => new Enumerator(this);

        public struct Enumerator : IEnumerator<KeyValuePair<TKey, TValue>>
        {
            private readonly Dictionary<TKey, TValue> dictionary;
            private readonly int version;
            private int index;
            private KeyValuePair<TKey, TValue> current;

            internal Enumerator(Dictionary<TKey, TValue> dictionary)
            {
                this.dictionary = dictionary;
                version = dictionary.version;
                index = 0;
                current = default;
            }

            public KeyValuePair<TKey, TValue> Current => current;

            object IEnumerator.Current => current;

            public bool MoveNext()
            {
                if (version != dictionary.version)
                {
                    throw new InvalidOperationException("Collection was modified; enumeration operation may not execute.");
                }
                while (index < dictionary.count)
                {
                    int at = index++;
                    if (dictionary.entries[at].next >= -1)
                    {
                        current = new KeyValuePair<TKey, TValue>(dictionary.entries[at].key, dictionary.entries[at].value);
                        return true;
                    }
                }
                current = default;
                return false;
            }

            public void Reset()
            {
                index = 0;
                current = default;
            }

            public void Dispose()
            {
            }
        }

        public sealed class KeyCollection : IEnumerable<TKey>
        {
            private readonly Dictionary<TKey, TValue> dictionary;

            internal KeyCollection(Dictionary<TKey, TValue> dictionary) => this.dictionary = dictionary;

            public int Count => dictionary.Count;

            public IEnumerator<TKey> GetEnumerator()
            {
                foreach (KeyValuePair<TKey, TValue> pair in dictionary)
                {
                    yield return pair.Key;
                }
            }

            IEnumerator IEnumerable.GetEnumerator() => GetEnumerator();
        }

        public sealed class ValueCollection : IEnumerable<TValue>
        {
            private readonly Dictionary<TKey, TValue> dictionary;

            internal ValueCollection(Dictionary<TKey, TValue> dictionary) => this.dictionary = dictionary;

            public int Count => dictionary.Count;

            public IEnumerator<TValue> GetEnumerator()
            {
                foreach (KeyValuePair<TKey, TValue> pair in dictionary)
                {
                    yield return pair.Value;
                }
            }

            IEnumerator IEnumerable.GetEnumerator() => GetEnumerator();
        }
    }

    public class HashSet<T> : IEnumerable<T>
    {
        private const int StartOfFreeList = -3;

        private struct Slot
        {
            public int hashCode;
            public int next;
            public T value;
        }

        private int[] buckets;
        private Slot[] slots;
        private int count;
        private int freeList;
        private int freeCount;
        private int version;
        private readonly IEqualityComparer<T> comparer;

        public HashSet()
            : this((IEqualityComparer<T>)null)
        {
        }

        public HashSet(IEqualityComparer<T> comparer)
        {
            this.comparer = comparer ?? EqualityComparer<T>.Default;
        }

        public HashSet(IEnumerable<T> collection)
            : this((IEqualityComparer<T>)null)
        {
            foreach (T item in collection)
            {
                Add(item);
            }
        }

        public int Count => count - freeCount;

        private int Hash(T item) => item == null ? 0 : comparer.GetHashCode(item) & 0x7FFFFFFF;

        private int Find(T item)
        {
            if (buckets == null)
            {
                return -1;
            }
            int hash = Hash(item);
            for (int i = buckets[hash % buckets.Length] - 1; i >= 0; i = slots[i].next)
            {
                if (slots[i].hashCode == hash && comparer.Equals(slots[i].value, item))
                {
                    return i;
                }
            }
            return -1;
        }

        public bool Contains(T item) => Find(item) >= 0;

        public bool Add(T item)
        {
            if (buckets == null)
            {
                buckets = new int[3];
                slots = new Slot[3];
                freeList = -1;
            }
            if (Find(item) >= 0)
            {
                return false;
            }
            int hash = Hash(item);
            int index;
            if (freeCount > 0)
            {
                index = freeList;
                freeList = StartOfFreeList - slots[freeList].next;
                freeCount--;
            }
            else
            {
                if (count == slots.Length)
                {
                    Resize();
                }
                index = count;
                count++;
            }
            int bucket = hash % buckets.Length;
            slots[index].hashCode = hash;
            slots[index].next = buckets[bucket] - 1;
            slots[index].value = item;
            buckets[bucket] = index + 1;
            version++;
            return true;
        }

        private void Resize()
        {
            int size = slots.Length * 2 + 1;
            Slot[] bigger = new Slot[size];
            for (int i = 0; i < count; i++)
            {
                bigger[i] = slots[i];
            }
            buckets = new int[size];
            for (int i = 0; i < count; i++)
            {
                if (bigger[i].next >= -1)
                {
                    int bucket = bigger[i].hashCode % size;
                    bigger[i].next = buckets[bucket] - 1;
                    buckets[bucket] = i + 1;
                }
            }
            slots = bigger;
        }

        public bool Remove(T item)
        {
            if (buckets == null)
            {
                return false;
            }
            int hash = Hash(item);
            int bucket = hash % buckets.Length;
            int last = -1;
            for (int i = buckets[bucket] - 1; i >= 0; last = i, i = slots[i].next)
            {
                if (slots[i].hashCode != hash || !comparer.Equals(slots[i].value, item))
                {
                    continue;
                }
                if (last < 0)
                {
                    buckets[bucket] = slots[i].next + 1;
                }
                else
                {
                    slots[last].next = slots[i].next;
                }
                slots[i].next = StartOfFreeList - freeList;
                slots[i].value = default;
                freeList = i;
                freeCount++;
                return true;
            }
            return false;
        }

        public IEnumerator<T> GetEnumerator()
        {
            int start = version;
            for (int i = 0; i < count; i++)
            {
                if (version != start)
                {
                    throw new InvalidOperationException("Collection was modified; enumeration operation may not execute.");
                }
                if (slots[i].next >= -1)
                {
                    yield return slots[i].value;
                }
            }
        }

        IEnumerator IEnumerable.GetEnumerator() => GetEnumerator();
    }

    public class Queue<T> : IEnumerable<T>
    {
        private T[] items = new T[4];
        private int head;
        private int size;

        public int Count => size;

        public void Enqueue(T item)
        {
            if (size == items.Length)
            {
                T[] bigger = new T[items.Length * 2];
                for (int i = 0; i < size; i++)
                {
                    bigger[i] = items[(head + i) % items.Length];
                }
                items = bigger;
                head = 0;
            }
            items[(head + size) % items.Length] = item;
            size++;
        }

        public T Dequeue()
        {
            if (size == 0)
            {
                throw new InvalidOperationException("Queue empty.");
            }
            T item = items[head];
            items[head] = default;
            head = (head + 1) % items.Length;
            size--;
            return item;
        }

        public T Peek()
        {
            if (size == 0)
            {
                throw new InvalidOperationException("Queue empty.");
            }
            return items[head];
        }

        public IEnumerator<T> GetEnumerator()
        {
            for (int i = 0; i < size; i++)
            {
                yield return items[(head + i) % items.Length];
            }
        }

        IEnumerator IEnumerable.GetEnumerator() => GetEnumerator();
    }

    public class Stack<T> : IEnumerable<T>
    {
        private T[] items = new T[4];
        private int size;

        public int Count => size;

        public void Push(T item)
        {
            if (size == items.Length)
            {
                T[] bigger = new T[items.Length * 2];
                for (int i = 0; i < size; i++)
                {
                    bigger[i] = items[i];
                }
                items = bigger;
            }
            items[size++] = item;
        }

        public T Pop()
        {
            if (size == 0)
            {
                throw new InvalidOperationException("Stack empty.");
            }
            T item = items[--size];
            items[size] = default;
            return item;
        }

        public T Peek()
        {
            if (size == 0)
            {
                throw new InvalidOperationException("Stack empty.");
            }
            return items[size - 1];
        }

        // Стек перебирается от вершины, как у .NET.
        public IEnumerator<T> GetEnumerator()
        {
            for (int i = size - 1; i >= 0; i--)
            {
                yield return items[i];
            }
        }

        IEnumerator IEnumerable.GetEnumerator() => GetEnumerator();
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
