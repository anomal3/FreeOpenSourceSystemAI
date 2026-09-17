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

    // IDictionary и IReadOnlyDictionary добавлены в фазе N10b: SortedList и
    // SortedDictionary из dotnet/runtime принимают словарь этими интерфейсами, и
    // без них `new SortedList<K, V>(dictionary)` не находил у нашего словаря
    // `ICollection<KeyValuePair<K, V>>.Count`.
    public class Dictionary<TKey, TValue> : IDictionary<TKey, TValue>, IReadOnlyDictionary<TKey, TValue>
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

        ICollection<TKey> IDictionary<TKey, TValue>.Keys => Keys;

        ICollection<TValue> IDictionary<TKey, TValue>.Values => Values;

        IEnumerable<TKey> IReadOnlyDictionary<TKey, TValue>.Keys => Keys;

        IEnumerable<TValue> IReadOnlyDictionary<TKey, TValue>.Values => Values;

        bool ICollection<KeyValuePair<TKey, TValue>>.IsReadOnly => false;

        void ICollection<KeyValuePair<TKey, TValue>>.Add(KeyValuePair<TKey, TValue> pair) => Add(pair.Key, pair.Value);

        // Пара есть, если есть ключ и значение при нём равно — как у .NET.
        bool ICollection<KeyValuePair<TKey, TValue>>.Contains(KeyValuePair<TKey, TValue> pair) =>
            TryGetValue(pair.Key, out TValue value) && EqualityComparer<TValue>.Default.Equals(value, pair.Value);

        bool ICollection<KeyValuePair<TKey, TValue>>.Remove(KeyValuePair<TKey, TValue> pair) =>
            ((ICollection<KeyValuePair<TKey, TValue>>)this).Contains(pair) && Remove(pair.Key);

        void ICollection<KeyValuePair<TKey, TValue>>.CopyTo(KeyValuePair<TKey, TValue>[] array, int index)
        {
            CheckCopyTo(array, index, Count);
            foreach (KeyValuePair<TKey, TValue> pair in this)
            {
                array[index++] = pair;
            }
        }

        // Проверки CopyTo с текстами .NET — общие для словаря и его коллекций.
        internal static void CheckCopyTo<T>(T[] array, int index, int count)
        {
            if (array == null)
            {
                throw new ArgumentNullException("array");
            }
            if ((uint)index > (uint)array.Length)
            {
                throw new ArgumentOutOfRangeException("index", index, "Index was out of range. Must be non-negative and less than or equal to the size of the collection.");
            }
            if (array.Length - index < count)
            {
                throw new ArgumentException("Destination array is not long enough to copy all the items in the collection. Check array index and length.");
            }
        }

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

        public sealed class KeyCollection : ICollection<TKey>, IReadOnlyCollection<TKey>
        {
            private readonly Dictionary<TKey, TValue> dictionary;

            internal KeyCollection(Dictionary<TKey, TValue> dictionary) => this.dictionary = dictionary;

            public int Count => dictionary.Count;

            bool ICollection<TKey>.IsReadOnly => true;

            void ICollection<TKey>.Add(TKey item) => throw new NotSupportedException(SR.NotSupported_KeyCollectionSet);

            void ICollection<TKey>.Clear() => throw new NotSupportedException(SR.NotSupported_KeyCollectionSet);

            bool ICollection<TKey>.Remove(TKey item) => throw new NotSupportedException(SR.NotSupported_KeyCollectionSet);

            bool ICollection<TKey>.Contains(TKey item) => dictionary.ContainsKey(item);

            public void CopyTo(TKey[] array, int index)
            {
                CheckCopyTo(array, index, dictionary.Count);
                foreach (KeyValuePair<TKey, TValue> pair in dictionary)
                {
                    array[index++] = pair.Key;
                }
            }

            public IEnumerator<TKey> GetEnumerator()
            {
                foreach (KeyValuePair<TKey, TValue> pair in dictionary)
                {
                    yield return pair.Key;
                }
            }

            IEnumerator IEnumerable.GetEnumerator() => GetEnumerator();
        }

        public sealed class ValueCollection : ICollection<TValue>, IReadOnlyCollection<TValue>
        {
            private readonly Dictionary<TKey, TValue> dictionary;

            internal ValueCollection(Dictionary<TKey, TValue> dictionary) => this.dictionary = dictionary;

            public int Count => dictionary.Count;

            bool ICollection<TValue>.IsReadOnly => true;

            void ICollection<TValue>.Add(TValue item) => throw new NotSupportedException(SR.NotSupported_ValueCollectionSet);

            void ICollection<TValue>.Clear() => throw new NotSupportedException(SR.NotSupported_ValueCollectionSet);

            bool ICollection<TValue>.Remove(TValue item) => throw new NotSupportedException(SR.NotSupported_ValueCollectionSet);

            bool ICollection<TValue>.Contains(TValue item)
            {
                foreach (KeyValuePair<TKey, TValue> pair in dictionary)
                {
                    if (EqualityComparer<TValue>.Default.Equals(pair.Value, item))
                    {
                        return true;
                    }
                }
                return false;
            }

            public void CopyTo(TValue[] array, int index)
            {
                CheckCopyTo(array, index, dictionary.Count);
                foreach (KeyValuePair<TKey, TValue> pair in dictionary)
                {
                    array[index++] = pair.Value;
                }
            }

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

    // ISet и IReadOnlySet — с фазы N10c: ReadOnlySet из dotnet/runtime
    // оборачивает множество этими интерфейсами. Операции над множествами дают
    // то же, что у .NET: те же элементы в том же порядке обхода, потому что
    // добавление идёт по порядку `other`, а удаление не трогает остальных.
    public class HashSet<T> : ISet<T>, IReadOnlySet<T>
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

        bool ICollection<T>.IsReadOnly => false;

        void ICollection<T>.Add(T item) => Add(item);

        public IEqualityComparer<T> Comparer => comparer;

        public void Clear()
        {
            if (count > 0)
            {
                buckets = null;
                slots = null;
                count = 0;
                freeList = -1;
                freeCount = 0;
            }
            version++;
        }

        public void CopyTo(T[] array) => CopyTo(array, 0, Count);

        public void CopyTo(T[] array, int arrayIndex) => CopyTo(array, arrayIndex, Count);

        public void CopyTo(T[] array, int arrayIndex, int count)
        {
            if (array == null)
            {
                throw new ArgumentNullException("array");
            }
            if (arrayIndex < 0)
            {
                throw new ArgumentOutOfRangeException("arrayIndex", arrayIndex, "arrayIndex ('" + arrayIndex.ToString() + "') must be a non-negative value.");
            }
            if (count < 0)
            {
                throw new ArgumentOutOfRangeException("count", count, "count ('" + count.ToString() + "') must be a non-negative value.");
            }
            if (arrayIndex > array.Length || count > array.Length - arrayIndex)
            {
                throw new ArgumentException("Destination array is not long enough to copy all the items in the collection. Check array index and length.");
            }
            foreach (T item in this)
            {
                if (count-- == 0)
                {
                    break;
                }
                array[arrayIndex++] = item;
            }
        }

        // Множество `other` с нашим сравнителем: у .NET так же — сравнение идёт
        // сравнителем этого множества, а не того.
        private HashSet<T> Distinct(IEnumerable<T> other)
        {
            if (other == null)
            {
                throw new ArgumentNullException("other");
            }
            var set = new HashSet<T>(comparer);
            foreach (T item in other)
            {
                set.Add(item);
            }
            return set;
        }

        private List<T> Snapshot()
        {
            var items = new List<T>(Count);
            foreach (T item in this)
            {
                items.Add(item);
            }
            return items;
        }

        public void UnionWith(IEnumerable<T> other)
        {
            if (other == null)
            {
                throw new ArgumentNullException("other");
            }
            foreach (T item in other)
            {
                Add(item);
            }
        }

        public void IntersectWith(IEnumerable<T> other)
        {
            HashSet<T> keep = Distinct(other);
            foreach (T item in Snapshot())
            {
                if (!keep.Contains(item))
                {
                    Remove(item);
                }
            }
        }

        public void ExceptWith(IEnumerable<T> other)
        {
            if (other == null)
            {
                throw new ArgumentNullException("other");
            }
            foreach (T item in other)
            {
                Remove(item);
            }
        }

        public void SymmetricExceptWith(IEnumerable<T> other)
        {
            foreach (T item in Distinct(other).Snapshot())
            {
                if (!Remove(item))
                {
                    Add(item);
                }
            }
        }

        public bool IsSubsetOf(IEnumerable<T> other)
        {
            HashSet<T> set = Distinct(other);
            foreach (T item in this)
            {
                if (!set.Contains(item))
                {
                    return false;
                }
            }
            return true;
        }

        public bool IsProperSubsetOf(IEnumerable<T> other)
        {
            HashSet<T> set = Distinct(other);
            return set.Count > Count && IsSubsetOf(set);
        }

        public bool IsSupersetOf(IEnumerable<T> other)
        {
            foreach (T item in Distinct(other))
            {
                if (!Contains(item))
                {
                    return false;
                }
            }
            return true;
        }

        public bool IsProperSupersetOf(IEnumerable<T> other)
        {
            HashSet<T> set = Distinct(other);
            return Count > set.Count && IsSupersetOf(set);
        }

        public bool Overlaps(IEnumerable<T> other)
        {
            if (other == null)
            {
                throw new ArgumentNullException("other");
            }
            foreach (T item in other)
            {
                if (Contains(item))
                {
                    return true;
                }
            }
            return false;
        }

        public bool SetEquals(IEnumerable<T> other)
        {
            HashSet<T> set = Distinct(other);
            return set.Count == Count && IsSupersetOf(set);
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
