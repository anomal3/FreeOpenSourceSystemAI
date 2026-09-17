// Linq (фаза N4d): `System.Linq.Enumerable` в объёме обычной программы.
//
// Всё на C#, итераторами `yield return`: выполнение отложено до перебора, как
// у .NET, и условие `Where` зовётся ровно столько раз, сколько у .NET, — это
// видно программе через замыкание со счётчиком. `Any` и `First` останавливаются
// на первом подходящем.
//
// Сортировка устойчивая: у .NET `OrderBy` устойчив по контракту (в отличие от
// `List.Sort`), и равные ключи сохраняют порядок источника. Здесь сравнение
// ключей при равенстве решает номером элемента — порядок полный, и какая
// сортировка его ни упорядочит, результат один.
//
// Тексты исключений — как у .NET: программы их печатают.

using System.Collections;
using System.Collections.Generic;

namespace System.Linq
{
    public interface IGrouping<out TKey, out TElement> : IEnumerable<TElement>
    {
        TKey Key { get; }
    }

    public interface IOrderedEnumerable<out TElement> : IEnumerable<TElement>
    {
        IOrderedEnumerable<TElement> CreateOrderedEnumerable<TKey>(Func<TElement, TKey> keySelector, IComparer<TKey> comparer, bool descending);
    }

    public static class Enumerable
    {
        private static Exception NoElements() => new InvalidOperationException("Sequence contains no elements");

        private static Exception NoMatch() => new InvalidOperationException("Sequence contains no matching element");

        private static Exception MoreThanOne() => new InvalidOperationException("Sequence contains more than one element");

        private static Exception MoreThanOneMatch() => new InvalidOperationException("Sequence contains more than one matching element");

        private static void Check(object argument, string name)
        {
            if (argument == null)
            {
                throw new ArgumentNullException(name);
            }
        }

        // ----------------------------------------------------------------
        // Создание
        // ----------------------------------------------------------------

        public static IEnumerable<TResult> Empty<TResult>() => Array.Empty<TResult>();

        // Range и Repeat у .NET — не просто перечислители, а списки только для
        // чтения (`IList<T>`): коллекция, построенная из них, узнаёт число
        // элементов заранее и берёт таблицу нужного размера (фаза N10d —
        // `new HashSet<int>(Enumerable.Range(0, 20))` у .NET получает ёмкость
        // 23, а не растёт 3 → 7 → 17 → 37; программа видит это через
        // `EnsureCapacity(0)`). Пустой диапазон — пустой массив, как у .NET.
        public static IEnumerable<int> Range(int start, int count)
        {
            if (count < 0 || (long)start + count - 1 > int.MaxValue)
            {
                throw new ArgumentOutOfRangeException("count");
            }
            return count == 0 ? Array.Empty<int>() : new RangeIterator(start, count);
        }

        public static IEnumerable<TResult> Repeat<TResult>(TResult element, int count)
        {
            if (count < 0)
            {
                throw new ArgumentOutOfRangeException("count");
            }
            return count == 0 ? Array.Empty<TResult>() : new RepeatIterator<TResult>(element, count);
        }

        private abstract class ReadOnlyListIterator<T> : IList<T>, IReadOnlyList<T>
        {
            public abstract int Count { get; }

            public abstract T this[int index] { get; }

            T IList<T>.this[int index]
            {
                get => this[index];
                set => throw new NotSupportedException("Collection is read-only.");
            }

            public bool IsReadOnly => true;

            public abstract IEnumerator<T> GetEnumerator();

            IEnumerator IEnumerable.GetEnumerator() => GetEnumerator();

            public abstract bool Contains(T item);

            public abstract int IndexOf(T item);

            public void CopyTo(T[] array, int arrayIndex)
            {
                for (int i = 0; i < Count; i++)
                {
                    array[arrayIndex + i] = this[i];
                }
            }

            public void Add(T item) => throw new NotSupportedException("Collection is read-only.");

            public void Clear() => throw new NotSupportedException("Collection is read-only.");

            public void Insert(int index, T item) => throw new NotSupportedException("Collection is read-only.");

            public bool Remove(T item) => throw new NotSupportedException("Collection is read-only.");

            public void RemoveAt(int index) => throw new NotSupportedException("Collection is read-only.");
        }

        private sealed class RangeIterator : ReadOnlyListIterator<int>
        {
            private readonly int start;
            private readonly int count;

            public RangeIterator(int start, int count)
            {
                this.start = start;
                this.count = count;
            }

            public override int Count => count;

            public override int this[int index]
            {
                get
                {
                    if ((uint)index >= (uint)count)
                    {
                        throw new ArgumentOutOfRangeException("index");
                    }
                    return start + index;
                }
            }

            public override IEnumerator<int> GetEnumerator()
            {
                for (int i = 0; i < count; i++)
                {
                    yield return start + i;
                }
            }

            public override bool Contains(int item) => (uint)(item - start) < (uint)count;

            public override int IndexOf(int item) => Contains(item) ? item - start : -1;
        }

        private sealed class RepeatIterator<T> : ReadOnlyListIterator<T>
        {
            private readonly T element;
            private readonly int count;

            public RepeatIterator(T element, int count)
            {
                this.element = element;
                this.count = count;
            }

            public override int Count => count;

            public override T this[int index]
            {
                get
                {
                    if ((uint)index >= (uint)count)
                    {
                        throw new ArgumentOutOfRangeException("index");
                    }
                    return element;
                }
            }

            public override IEnumerator<T> GetEnumerator()
            {
                for (int i = 0; i < count; i++)
                {
                    yield return element;
                }
            }

            public override bool Contains(T item) => EqualityComparer<T>.Default.Equals(element, item);

            public override int IndexOf(T item) => Contains(item) ? 0 : -1;
        }

        // ----------------------------------------------------------------
        // Фильтр и проекция
        // ----------------------------------------------------------------

        public static IEnumerable<TSource> Where<TSource>(this IEnumerable<TSource> source, Func<TSource, bool> predicate)
        {
            Check(source, "source");
            Check(predicate, "predicate");
            return WhereIterator(source, predicate);
        }

        private static IEnumerable<TSource> WhereIterator<TSource>(IEnumerable<TSource> source, Func<TSource, bool> predicate)
        {
            foreach (TSource item in source)
            {
                if (predicate(item))
                {
                    yield return item;
                }
            }
        }

        public static IEnumerable<TSource> Where<TSource>(this IEnumerable<TSource> source, Func<TSource, int, bool> predicate)
        {
            Check(source, "source");
            Check(predicate, "predicate");
            return WhereIndexIterator(source, predicate);
        }

        private static IEnumerable<TSource> WhereIndexIterator<TSource>(IEnumerable<TSource> source, Func<TSource, int, bool> predicate)
        {
            int index = 0;
            foreach (TSource item in source)
            {
                if (predicate(item, index++))
                {
                    yield return item;
                }
            }
        }

        public static IEnumerable<TResult> Select<TSource, TResult>(this IEnumerable<TSource> source, Func<TSource, TResult> selector)
        {
            Check(source, "source");
            Check(selector, "selector");
            return SelectIterator(source, selector);
        }

        private static IEnumerable<TResult> SelectIterator<TSource, TResult>(IEnumerable<TSource> source, Func<TSource, TResult> selector)
        {
            foreach (TSource item in source)
            {
                yield return selector(item);
            }
        }

        public static IEnumerable<TResult> Select<TSource, TResult>(this IEnumerable<TSource> source, Func<TSource, int, TResult> selector)
        {
            Check(source, "source");
            Check(selector, "selector");
            return SelectIndexIterator(source, selector);
        }

        private static IEnumerable<TResult> SelectIndexIterator<TSource, TResult>(IEnumerable<TSource> source, Func<TSource, int, TResult> selector)
        {
            int index = 0;
            foreach (TSource item in source)
            {
                yield return selector(item, index++);
            }
        }

        public static IEnumerable<TResult> SelectMany<TSource, TResult>(this IEnumerable<TSource> source, Func<TSource, IEnumerable<TResult>> selector)
        {
            Check(source, "source");
            Check(selector, "selector");
            return SelectManyIterator(source, selector);
        }

        private static IEnumerable<TResult> SelectManyIterator<TSource, TResult>(IEnumerable<TSource> source, Func<TSource, IEnumerable<TResult>> selector)
        {
            foreach (TSource item in source)
            {
                foreach (TResult inner in selector(item))
                {
                    yield return inner;
                }
            }
        }

        public static IEnumerable<TResult> OfType<TResult>(this IEnumerable source)
        {
            Check(source, "source");
            return OfTypeIterator<TResult>(source);
        }

        private static IEnumerable<TResult> OfTypeIterator<TResult>(IEnumerable source)
        {
            foreach (object item in source)
            {
                if (item is TResult typed)
                {
                    yield return typed;
                }
            }
        }

        public static IEnumerable<TResult> Cast<TResult>(this IEnumerable source)
        {
            Check(source, "source");
            if (source is IEnumerable<TResult> typed)
            {
                return typed;
            }
            return CastIterator<TResult>(source);
        }

        private static IEnumerable<TResult> CastIterator<TResult>(IEnumerable source)
        {
            foreach (object item in source)
            {
                yield return (TResult)item;
            }
        }

        // ----------------------------------------------------------------
        // Части последовательности
        // ----------------------------------------------------------------

        public static IEnumerable<TSource> Take<TSource>(this IEnumerable<TSource> source, int count)
        {
            Check(source, "source");
            return TakeIterator(source, count);
        }

        private static IEnumerable<TSource> TakeIterator<TSource>(IEnumerable<TSource> source, int count)
        {
            if (count <= 0)
            {
                yield break;
            }
            foreach (TSource item in source)
            {
                yield return item;
                if (--count == 0)
                {
                    yield break;
                }
            }
        }

        public static IEnumerable<TSource> Skip<TSource>(this IEnumerable<TSource> source, int count)
        {
            Check(source, "source");
            return SkipIterator(source, count);
        }

        private static IEnumerable<TSource> SkipIterator<TSource>(IEnumerable<TSource> source, int count)
        {
            foreach (TSource item in source)
            {
                if (count > 0)
                {
                    count--;
                    continue;
                }
                yield return item;
            }
        }

        public static IEnumerable<TSource> TakeWhile<TSource>(this IEnumerable<TSource> source, Func<TSource, bool> predicate)
        {
            Check(source, "source");
            Check(predicate, "predicate");
            return TakeWhileIterator(source, predicate);
        }

        private static IEnumerable<TSource> TakeWhileIterator<TSource>(IEnumerable<TSource> source, Func<TSource, bool> predicate)
        {
            foreach (TSource item in source)
            {
                if (!predicate(item))
                {
                    yield break;
                }
                yield return item;
            }
        }

        public static IEnumerable<TSource> SkipWhile<TSource>(this IEnumerable<TSource> source, Func<TSource, bool> predicate)
        {
            Check(source, "source");
            Check(predicate, "predicate");
            return SkipWhileIterator(source, predicate);
        }

        private static IEnumerable<TSource> SkipWhileIterator<TSource>(IEnumerable<TSource> source, Func<TSource, bool> predicate)
        {
            bool yielding = false;
            foreach (TSource item in source)
            {
                if (!yielding && !predicate(item))
                {
                    yielding = true;
                }
                if (yielding)
                {
                    yield return item;
                }
            }
        }

        public static IEnumerable<TSource> Concat<TSource>(this IEnumerable<TSource> first, IEnumerable<TSource> second)
        {
            Check(first, "first");
            Check(second, "second");
            return ConcatIterator(first, second);
        }

        private static IEnumerable<TSource> ConcatIterator<TSource>(IEnumerable<TSource> first, IEnumerable<TSource> second)
        {
            foreach (TSource item in first)
            {
                yield return item;
            }
            foreach (TSource item in second)
            {
                yield return item;
            }
        }

        public static IEnumerable<TSource> Append<TSource>(this IEnumerable<TSource> source, TSource element) =>
            Concat(source, new[] { element });

        public static IEnumerable<TSource> Prepend<TSource>(this IEnumerable<TSource> source, TSource element) =>
            Concat(new[] { element }, source);

        public static IEnumerable<TSource> Reverse<TSource>(this IEnumerable<TSource> source)
        {
            Check(source, "source");
            return ReverseIterator(source);
        }

        // С .NET 10 у массива свой `Reverse` — чтобы `array.Reverse()` не ушёл
        // в разворот среза на месте.
        public static IEnumerable<TSource> Reverse<TSource>(this TSource[] array)
        {
            Check(array, "array");
            return ReverseIterator(array);
        }

        private static IEnumerable<TSource> ReverseIterator<TSource>(IEnumerable<TSource> source)
        {
            TSource[] buffer = ToArray(source);
            for (int i = buffer.Length - 1; i >= 0; i--)
            {
                yield return buffer[i];
            }
        }

        public static IEnumerable<TResult> Zip<TFirst, TSecond, TResult>(
            this IEnumerable<TFirst> first, IEnumerable<TSecond> second, Func<TFirst, TSecond, TResult> resultSelector)
        {
            Check(first, "first");
            Check(second, "second");
            Check(resultSelector, "resultSelector");
            return ZipIterator(first, second, resultSelector);
        }

        private static IEnumerable<TResult> ZipIterator<TFirst, TSecond, TResult>(
            IEnumerable<TFirst> first, IEnumerable<TSecond> second, Func<TFirst, TSecond, TResult> resultSelector)
        {
            using (IEnumerator<TFirst> a = first.GetEnumerator())
            using (IEnumerator<TSecond> b = second.GetEnumerator())
            {
                while (a.MoveNext() && b.MoveNext())
                {
                    yield return resultSelector(a.Current, b.Current);
                }
            }
        }

        public static IEnumerable<TSource> DefaultIfEmpty<TSource>(this IEnumerable<TSource> source) => DefaultIfEmpty(source, default);

        public static IEnumerable<TSource> DefaultIfEmpty<TSource>(this IEnumerable<TSource> source, TSource defaultValue)
        {
            Check(source, "source");
            return DefaultIfEmptyIterator(source, defaultValue);
        }

        private static IEnumerable<TSource> DefaultIfEmptyIterator<TSource>(IEnumerable<TSource> source, TSource defaultValue)
        {
            bool any = false;
            foreach (TSource item in source)
            {
                any = true;
                yield return item;
            }
            if (!any)
            {
                yield return defaultValue;
            }
        }

        // ----------------------------------------------------------------
        // Множества
        // ----------------------------------------------------------------

        public static IEnumerable<TSource> Distinct<TSource>(this IEnumerable<TSource> source)
        {
            Check(source, "source");
            return DistinctIterator(source);
        }

        private static IEnumerable<TSource> DistinctIterator<TSource>(IEnumerable<TSource> source)
        {
            var seen = new HashSet<TSource>();
            foreach (TSource item in source)
            {
                if (seen.Add(item))
                {
                    yield return item;
                }
            }
        }

        public static IEnumerable<TSource> Union<TSource>(this IEnumerable<TSource> first, IEnumerable<TSource> second) =>
            Distinct(Concat(first, second));

        public static IEnumerable<TSource> Intersect<TSource>(this IEnumerable<TSource> first, IEnumerable<TSource> second)
        {
            Check(first, "first");
            Check(second, "second");
            return IntersectIterator(first, second);
        }

        private static IEnumerable<TSource> IntersectIterator<TSource>(IEnumerable<TSource> first, IEnumerable<TSource> second)
        {
            var set = new HashSet<TSource>(second);
            foreach (TSource item in first)
            {
                if (set.Remove(item))
                {
                    yield return item;
                }
            }
        }

        public static IEnumerable<TSource> Except<TSource>(this IEnumerable<TSource> first, IEnumerable<TSource> second)
        {
            Check(first, "first");
            Check(second, "second");
            return ExceptIterator(first, second);
        }

        private static IEnumerable<TSource> ExceptIterator<TSource>(IEnumerable<TSource> first, IEnumerable<TSource> second)
        {
            var set = new HashSet<TSource>(second);
            foreach (TSource item in first)
            {
                if (set.Add(item))
                {
                    yield return item;
                }
            }
        }

        // ----------------------------------------------------------------
        // Сортировка и группировка
        // ----------------------------------------------------------------

        public static IOrderedEnumerable<TSource> OrderBy<TSource, TKey>(this IEnumerable<TSource> source, Func<TSource, TKey> keySelector) =>
            new OrderedEnumerable<TSource, TKey>(source, keySelector, null, false, null);

        public static IOrderedEnumerable<TSource> OrderBy<TSource, TKey>(
            this IEnumerable<TSource> source, Func<TSource, TKey> keySelector, IComparer<TKey> comparer) =>
            new OrderedEnumerable<TSource, TKey>(source, keySelector, comparer, false, null);

        public static IOrderedEnumerable<TSource> OrderByDescending<TSource, TKey>(this IEnumerable<TSource> source, Func<TSource, TKey> keySelector) =>
            new OrderedEnumerable<TSource, TKey>(source, keySelector, null, true, null);

        public static IOrderedEnumerable<TSource> OrderByDescending<TSource, TKey>(
            this IEnumerable<TSource> source, Func<TSource, TKey> keySelector, IComparer<TKey> comparer) =>
            new OrderedEnumerable<TSource, TKey>(source, keySelector, comparer, true, null);

        public static IOrderedEnumerable<TSource> ThenBy<TSource, TKey>(this IOrderedEnumerable<TSource> source, Func<TSource, TKey> keySelector)
        {
            Check(source, "source");
            return source.CreateOrderedEnumerable(keySelector, null, false);
        }

        public static IOrderedEnumerable<TSource> ThenByDescending<TSource, TKey>(this IOrderedEnumerable<TSource> source, Func<TSource, TKey> keySelector)
        {
            Check(source, "source");
            return source.CreateOrderedEnumerable(keySelector, null, true);
        }

        public static IEnumerable<IGrouping<TKey, TSource>> GroupBy<TSource, TKey>(this IEnumerable<TSource> source, Func<TSource, TKey> keySelector) =>
            GroupBy(source, keySelector, item => item);

        public static IEnumerable<IGrouping<TKey, TElement>> GroupBy<TSource, TKey, TElement>(
            this IEnumerable<TSource> source, Func<TSource, TKey> keySelector, Func<TSource, TElement> elementSelector)
        {
            Check(source, "source");
            Check(keySelector, "keySelector");
            Check(elementSelector, "elementSelector");
            return GroupByIterator(source, keySelector, elementSelector);
        }

        // Группы — в порядке первого появления ключа, как у `Lookup` в .NET.
        private static IEnumerable<IGrouping<TKey, TElement>> GroupByIterator<TSource, TKey, TElement>(
            IEnumerable<TSource> source, Func<TSource, TKey> keySelector, Func<TSource, TElement> elementSelector)
        {
            var groups = new List<Grouping<TKey, TElement>>();
            var byKey = new Dictionary<TKey, Grouping<TKey, TElement>>();
            Grouping<TKey, TElement> nullGroup = null;
            foreach (TSource item in source)
            {
                TKey key = keySelector(item);
                Grouping<TKey, TElement> group;
                if (key == null)
                {
                    if (nullGroup == null)
                    {
                        nullGroup = new Grouping<TKey, TElement>(key);
                        groups.Add(nullGroup);
                    }
                    group = nullGroup;
                }
                else if (!byKey.TryGetValue(key, out group))
                {
                    group = new Grouping<TKey, TElement>(key);
                    byKey.Add(key, group);
                    groups.Add(group);
                }
                group.Add(elementSelector(item));
            }
            foreach (Grouping<TKey, TElement> group in groups)
            {
                yield return group;
            }
        }

        // ----------------------------------------------------------------
        // Сборка в коллекцию
        // ----------------------------------------------------------------

        public static List<TSource> ToList<TSource>(this IEnumerable<TSource> source)
        {
            Check(source, "source");
            return new List<TSource>(source);
        }

        public static TSource[] ToArray<TSource>(this IEnumerable<TSource> source)
        {
            Check(source, "source");
            return new List<TSource>(source).ToArray();
        }

        public static HashSet<TSource> ToHashSet<TSource>(this IEnumerable<TSource> source)
        {
            Check(source, "source");
            return new HashSet<TSource>(source);
        }

        public static Dictionary<TKey, TSource> ToDictionary<TSource, TKey>(this IEnumerable<TSource> source, Func<TSource, TKey> keySelector) =>
            ToDictionary(source, keySelector, item => item);

        public static Dictionary<TKey, TElement> ToDictionary<TSource, TKey, TElement>(
            this IEnumerable<TSource> source, Func<TSource, TKey> keySelector, Func<TSource, TElement> elementSelector)
        {
            Check(source, "source");
            Check(keySelector, "keySelector");
            Check(elementSelector, "elementSelector");
            var dictionary = new Dictionary<TKey, TElement>();
            foreach (TSource item in source)
            {
                dictionary.Add(keySelector(item), elementSelector(item));
            }
            return dictionary;
        }

        // ----------------------------------------------------------------
        // Элементы
        // ----------------------------------------------------------------

        public static TSource First<TSource>(this IEnumerable<TSource> source)
        {
            Check(source, "source");
            foreach (TSource item in source)
            {
                return item;
            }
            throw NoElements();
        }

        public static TSource First<TSource>(this IEnumerable<TSource> source, Func<TSource, bool> predicate)
        {
            Check(source, "source");
            Check(predicate, "predicate");
            foreach (TSource item in source)
            {
                if (predicate(item))
                {
                    return item;
                }
            }
            throw NoMatch();
        }

        public static TSource FirstOrDefault<TSource>(this IEnumerable<TSource> source)
        {
            Check(source, "source");
            foreach (TSource item in source)
            {
                return item;
            }
            return default;
        }

        public static TSource FirstOrDefault<TSource>(this IEnumerable<TSource> source, Func<TSource, bool> predicate)
        {
            Check(source, "source");
            Check(predicate, "predicate");
            foreach (TSource item in source)
            {
                if (predicate(item))
                {
                    return item;
                }
            }
            return default;
        }

        public static TSource Last<TSource>(this IEnumerable<TSource> source)
        {
            Check(source, "source");
            bool found = false;
            TSource last = default;
            foreach (TSource item in source)
            {
                found = true;
                last = item;
            }
            if (!found)
            {
                throw NoElements();
            }
            return last;
        }

        public static TSource Last<TSource>(this IEnumerable<TSource> source, Func<TSource, bool> predicate)
        {
            Check(source, "source");
            Check(predicate, "predicate");
            bool found = false;
            TSource last = default;
            foreach (TSource item in source)
            {
                if (predicate(item))
                {
                    found = true;
                    last = item;
                }
            }
            if (!found)
            {
                throw NoMatch();
            }
            return last;
        }

        public static TSource LastOrDefault<TSource>(this IEnumerable<TSource> source)
        {
            Check(source, "source");
            TSource last = default;
            foreach (TSource item in source)
            {
                last = item;
            }
            return last;
        }

        public static TSource LastOrDefault<TSource>(this IEnumerable<TSource> source, Func<TSource, bool> predicate)
        {
            Check(source, "source");
            Check(predicate, "predicate");
            TSource last = default;
            foreach (TSource item in source)
            {
                if (predicate(item))
                {
                    last = item;
                }
            }
            return last;
        }

        public static TSource Single<TSource>(this IEnumerable<TSource> source)
        {
            Check(source, "source");
            using (IEnumerator<TSource> e = source.GetEnumerator())
            {
                if (!e.MoveNext())
                {
                    throw NoElements();
                }
                TSource result = e.Current;
                if (e.MoveNext())
                {
                    throw MoreThanOne();
                }
                return result;
            }
        }

        public static TSource Single<TSource>(this IEnumerable<TSource> source, Func<TSource, bool> predicate)
        {
            Check(source, "source");
            Check(predicate, "predicate");
            bool found = false;
            TSource result = default;
            foreach (TSource item in source)
            {
                if (predicate(item))
                {
                    if (found)
                    {
                        throw MoreThanOneMatch();
                    }
                    found = true;
                    result = item;
                }
            }
            if (!found)
            {
                throw NoMatch();
            }
            return result;
        }

        public static TSource SingleOrDefault<TSource>(this IEnumerable<TSource> source)
        {
            Check(source, "source");
            using (IEnumerator<TSource> e = source.GetEnumerator())
            {
                if (!e.MoveNext())
                {
                    return default;
                }
                TSource result = e.Current;
                if (e.MoveNext())
                {
                    throw MoreThanOne();
                }
                return result;
            }
        }

        public static TSource SingleOrDefault<TSource>(this IEnumerable<TSource> source, Func<TSource, bool> predicate)
        {
            Check(source, "source");
            Check(predicate, "predicate");
            bool found = false;
            TSource result = default;
            foreach (TSource item in source)
            {
                if (predicate(item))
                {
                    if (found)
                    {
                        throw MoreThanOneMatch();
                    }
                    found = true;
                    result = item;
                }
            }
            return result;
        }

        public static TSource ElementAt<TSource>(this IEnumerable<TSource> source, int index)
        {
            Check(source, "source");
            if (source is IList<TSource> list)
            {
                return list[index];
            }
            if (index >= 0)
            {
                foreach (TSource item in source)
                {
                    if (index-- == 0)
                    {
                        return item;
                    }
                }
            }
            throw new ArgumentOutOfRangeException("index");
        }

        public static TSource ElementAtOrDefault<TSource>(this IEnumerable<TSource> source, int index)
        {
            Check(source, "source");
            if (index >= 0)
            {
                foreach (TSource item in source)
                {
                    if (index-- == 0)
                    {
                        return item;
                    }
                }
            }
            return default;
        }

        // ----------------------------------------------------------------
        // Проверки
        // ----------------------------------------------------------------

        public static bool Any<TSource>(this IEnumerable<TSource> source)
        {
            Check(source, "source");
            using (IEnumerator<TSource> e = source.GetEnumerator())
            {
                return e.MoveNext();
            }
        }

        public static bool Any<TSource>(this IEnumerable<TSource> source, Func<TSource, bool> predicate)
        {
            Check(source, "source");
            Check(predicate, "predicate");
            foreach (TSource item in source)
            {
                if (predicate(item))
                {
                    return true;
                }
            }
            return false;
        }

        public static bool All<TSource>(this IEnumerable<TSource> source, Func<TSource, bool> predicate)
        {
            Check(source, "source");
            Check(predicate, "predicate");
            foreach (TSource item in source)
            {
                if (!predicate(item))
                {
                    return false;
                }
            }
            return true;
        }

        public static bool Contains<TSource>(this IEnumerable<TSource> source, TSource value)
        {
            Check(source, "source");
            EqualityComparer<TSource> comparer = EqualityComparer<TSource>.Default;
            foreach (TSource item in source)
            {
                if (comparer.Equals(item, value))
                {
                    return true;
                }
            }
            return false;
        }

        public static bool SequenceEqual<TSource>(this IEnumerable<TSource> first, IEnumerable<TSource> second)
        {
            Check(first, "first");
            Check(second, "second");
            EqualityComparer<TSource> comparer = EqualityComparer<TSource>.Default;
            using (IEnumerator<TSource> a = first.GetEnumerator())
            using (IEnumerator<TSource> b = second.GetEnumerator())
            {
                while (a.MoveNext())
                {
                    if (!b.MoveNext() || !comparer.Equals(a.Current, b.Current))
                    {
                        return false;
                    }
                }
                return !b.MoveNext();
            }
        }

        public static int Count<TSource>(this IEnumerable<TSource> source)
        {
            Check(source, "source");
            if (source is ICollection<TSource> collection)
            {
                return collection.Count;
            }
            int count = 0;
            using (IEnumerator<TSource> e = source.GetEnumerator())
            {
                while (e.MoveNext())
                {
                    checked
                    {
                        count++;
                    }
                }
            }
            return count;
        }

        public static int Count<TSource>(this IEnumerable<TSource> source, Func<TSource, bool> predicate)
        {
            Check(source, "source");
            Check(predicate, "predicate");
            int count = 0;
            foreach (TSource item in source)
            {
                if (predicate(item))
                {
                    checked
                    {
                        count++;
                    }
                }
            }
            return count;
        }

        public static long LongCount<TSource>(this IEnumerable<TSource> source)
        {
            Check(source, "source");
            long count = 0;
            foreach (TSource item in source)
            {
                count++;
            }
            return count;
        }

        // ----------------------------------------------------------------
        // Агрегаты
        // ----------------------------------------------------------------

        public static TSource Aggregate<TSource>(this IEnumerable<TSource> source, Func<TSource, TSource, TSource> func)
        {
            Check(source, "source");
            Check(func, "func");
            using (IEnumerator<TSource> e = source.GetEnumerator())
            {
                if (!e.MoveNext())
                {
                    throw NoElements();
                }
                TSource result = e.Current;
                while (e.MoveNext())
                {
                    result = func(result, e.Current);
                }
                return result;
            }
        }

        public static TAccumulate Aggregate<TSource, TAccumulate>(
            this IEnumerable<TSource> source, TAccumulate seed, Func<TAccumulate, TSource, TAccumulate> func)
        {
            Check(source, "source");
            Check(func, "func");
            TAccumulate result = seed;
            foreach (TSource item in source)
            {
                result = func(result, item);
            }
            return result;
        }

        public static TResult Aggregate<TSource, TAccumulate, TResult>(
            this IEnumerable<TSource> source, TAccumulate seed, Func<TAccumulate, TSource, TAccumulate> func, Func<TAccumulate, TResult> resultSelector)
        {
            Check(resultSelector, "resultSelector");
            return resultSelector(Aggregate(source, seed, func));
        }

        public static int Sum(this IEnumerable<int> source)
        {
            Check(source, "source");
            int sum = 0;
            foreach (int item in source)
            {
                checked
                {
                    sum += item;
                }
            }
            return sum;
        }

        public static long Sum(this IEnumerable<long> source)
        {
            Check(source, "source");
            long sum = 0;
            foreach (long item in source)
            {
                checked
                {
                    sum += item;
                }
            }
            return sum;
        }

        public static double Sum(this IEnumerable<double> source)
        {
            Check(source, "source");
            double sum = 0;
            foreach (double item in source)
            {
                sum += item;
            }
            return sum;
        }

        public static int Sum<TSource>(this IEnumerable<TSource> source, Func<TSource, int> selector) => Sum(Select(source, selector));

        public static long Sum<TSource>(this IEnumerable<TSource> source, Func<TSource, long> selector) => Sum(Select(source, selector));

        public static double Sum<TSource>(this IEnumerable<TSource> source, Func<TSource, double> selector) => Sum(Select(source, selector));

        public static double Average(this IEnumerable<int> source)
        {
            Check(source, "source");
            long sum = 0;
            long count = 0;
            foreach (int item in source)
            {
                checked
                {
                    sum += item;
                }
                count++;
            }
            if (count == 0)
            {
                throw NoElements();
            }
            return (double)sum / count;
        }

        public static double Average(this IEnumerable<long> source)
        {
            Check(source, "source");
            long sum = 0;
            long count = 0;
            foreach (long item in source)
            {
                checked
                {
                    sum += item;
                }
                count++;
            }
            if (count == 0)
            {
                throw NoElements();
            }
            return (double)sum / count;
        }

        public static double Average(this IEnumerable<double> source)
        {
            Check(source, "source");
            double sum = 0;
            long count = 0;
            foreach (double item in source)
            {
                sum += item;
                count++;
            }
            if (count == 0)
            {
                throw NoElements();
            }
            return sum / count;
        }

        public static double Average<TSource>(this IEnumerable<TSource> source, Func<TSource, int> selector) => Average(Select(source, selector));

        public static double Average<TSource>(this IEnumerable<TSource> source, Func<TSource, long> selector) => Average(Select(source, selector));

        public static double Average<TSource>(this IEnumerable<TSource> source, Func<TSource, double> selector) => Average(Select(source, selector));

        public static int Max(this IEnumerable<int> source)
        {
            Check(source, "source");
            using (IEnumerator<int> e = source.GetEnumerator())
            {
                if (!e.MoveNext())
                {
                    throw NoElements();
                }
                int value = e.Current;
                while (e.MoveNext())
                {
                    if (e.Current > value)
                    {
                        value = e.Current;
                    }
                }
                return value;
            }
        }

        public static int Min(this IEnumerable<int> source)
        {
            Check(source, "source");
            using (IEnumerator<int> e = source.GetEnumerator())
            {
                if (!e.MoveNext())
                {
                    throw NoElements();
                }
                int value = e.Current;
                while (e.MoveNext())
                {
                    if (e.Current < value)
                    {
                        value = e.Current;
                    }
                }
                return value;
            }
        }

        public static long Max(this IEnumerable<long> source)
        {
            Check(source, "source");
            using (IEnumerator<long> e = source.GetEnumerator())
            {
                if (!e.MoveNext())
                {
                    throw NoElements();
                }
                long value = e.Current;
                while (e.MoveNext())
                {
                    if (e.Current > value)
                    {
                        value = e.Current;
                    }
                }
                return value;
            }
        }

        public static long Min(this IEnumerable<long> source)
        {
            Check(source, "source");
            using (IEnumerator<long> e = source.GetEnumerator())
            {
                if (!e.MoveNext())
                {
                    throw NoElements();
                }
                long value = e.Current;
                while (e.MoveNext())
                {
                    if (e.Current < value)
                    {
                        value = e.Current;
                    }
                }
                return value;
            }
        }

        // NaN у .NET: `Max` его пропускает, пока есть числа; `Min` им кончает.
        public static double Max(this IEnumerable<double> source)
        {
            Check(source, "source");
            using (IEnumerator<double> e = source.GetEnumerator())
            {
                if (!e.MoveNext())
                {
                    throw NoElements();
                }
                double value = e.Current;
                while (double.IsNaN(value))
                {
                    if (!e.MoveNext())
                    {
                        return value;
                    }
                    value = e.Current;
                }
                while (e.MoveNext())
                {
                    if (e.Current > value)
                    {
                        value = e.Current;
                    }
                }
                return value;
            }
        }

        public static double Min(this IEnumerable<double> source)
        {
            Check(source, "source");
            using (IEnumerator<double> e = source.GetEnumerator())
            {
                if (!e.MoveNext())
                {
                    throw NoElements();
                }
                double value = e.Current;
                if (double.IsNaN(value))
                {
                    return value;
                }
                while (e.MoveNext())
                {
                    double x = e.Current;
                    if (x < value)
                    {
                        value = x;
                    }
                    else if (double.IsNaN(x))
                    {
                        return x;
                    }
                }
                return value;
            }
        }

        public static int Max<TSource>(this IEnumerable<TSource> source, Func<TSource, int> selector) => Max(Select(source, selector));

        public static int Min<TSource>(this IEnumerable<TSource> source, Func<TSource, int> selector) => Min(Select(source, selector));

        public static long Max<TSource>(this IEnumerable<TSource> source, Func<TSource, long> selector) => Max(Select(source, selector));

        public static long Min<TSource>(this IEnumerable<TSource> source, Func<TSource, long> selector) => Min(Select(source, selector));

        public static double Max<TSource>(this IEnumerable<TSource> source, Func<TSource, double> selector) => Max(Select(source, selector));

        public static double Min<TSource>(this IEnumerable<TSource> source, Func<TSource, double> selector) => Min(Select(source, selector));

        // Общий случай через `Comparer<T>.Default`; `null` пропускается, как у .NET.
        public static TSource Max<TSource>(this IEnumerable<TSource> source) => Extreme(source, 1);

        public static TSource Min<TSource>(this IEnumerable<TSource> source) => Extreme(source, -1);

        private static TSource Extreme<TSource>(IEnumerable<TSource> source, int sign)
        {
            Check(source, "source");
            Comparer<TSource> comparer = Comparer<TSource>.Default;
            bool found = false;
            TSource value = default;
            foreach (TSource item in source)
            {
                if (item == null)
                {
                    continue;
                }
                if (!found || comparer.Compare(item, value) * sign > 0)
                {
                    value = item;
                    found = true;
                }
            }
            if (!found && default(TSource) != null)
            {
                throw NoElements();
            }
            return value;
        }

        public static TSource MaxBy<TSource, TKey>(this IEnumerable<TSource> source, Func<TSource, TKey> keySelector) => ExtremeBy(source, keySelector, 1);

        public static TSource MinBy<TSource, TKey>(this IEnumerable<TSource> source, Func<TSource, TKey> keySelector) => ExtremeBy(source, keySelector, -1);

        // Первый из равных, как у .NET.
        private static TSource ExtremeBy<TSource, TKey>(IEnumerable<TSource> source, Func<TSource, TKey> keySelector, int sign)
        {
            Check(source, "source");
            Check(keySelector, "keySelector");
            Comparer<TKey> comparer = Comparer<TKey>.Default;
            using (IEnumerator<TSource> e = source.GetEnumerator())
            {
                if (!e.MoveNext())
                {
                    if (default(TSource) == null)
                    {
                        return default;
                    }
                    throw NoElements();
                }
                TSource value = e.Current;
                TKey key = keySelector(value);
                while (e.MoveNext())
                {
                    TSource next = e.Current;
                    TKey nextKey = keySelector(next);
                    if (comparer.Compare(nextKey, key) * sign > 0)
                    {
                        value = next;
                        key = nextKey;
                    }
                }
                return value;
            }
        }
    }

    internal sealed class Grouping<TKey, TElement> : IGrouping<TKey, TElement>
    {
        private readonly TKey key;
        private readonly List<TElement> elements = new List<TElement>();

        internal Grouping(TKey key)
        {
            this.key = key;
        }

        public TKey Key => key;

        internal void Add(TElement element) => elements.Add(element);

        public IEnumerator<TElement> GetEnumerator() => elements.GetEnumerator();

        IEnumerator IEnumerable.GetEnumerator() => GetEnumerator();
    }

    internal abstract class OrderedEnumerable<TElement> : IOrderedEnumerable<TElement>
    {
        protected readonly IEnumerable<TElement> source;

        protected OrderedEnumerable(IEnumerable<TElement> source)
        {
            if (source == null)
            {
                throw new ArgumentNullException("source");
            }
            this.source = source;
        }

        /// Сравниватель этой ступени, за которым идут следующие (`ThenBy`).
        internal abstract KeyComparer<TElement> GetComparer(KeyComparer<TElement> next);

        public IOrderedEnumerable<TElement> CreateOrderedEnumerable<TKey>(Func<TElement, TKey> keySelector, IComparer<TKey> comparer, bool descending) =>
            new OrderedEnumerable<TElement, TKey>(source, keySelector, comparer, descending, this);

        public IEnumerator<TElement> GetEnumerator()
        {
            TElement[] buffer = Enumerable.ToArray(source);
            KeyComparer<TElement> comparer = GetComparer(null);
            comparer.ComputeKeys(buffer);
            int[] map = new int[buffer.Length];
            for (int i = 0; i < map.Length; i++)
            {
                map[i] = i;
            }
            MergeSort(map, new int[map.Length], 0, map.Length, comparer);
            for (int i = 0; i < map.Length; i++)
            {
                yield return buffer[map[i]];
            }
        }

        IEnumerator IEnumerable.GetEnumerator() => GetEnumerator();

        private static void MergeSort(int[] map, int[] scratch, int from, int to, KeyComparer<TElement> comparer)
        {
            if (to - from < 2)
            {
                return;
            }
            int middle = from + (to - from) / 2;
            MergeSort(map, scratch, from, middle, comparer);
            MergeSort(map, scratch, middle, to, comparer);
            int left = from;
            int right = middle;
            int at = from;
            while (left < middle && right < to)
            {
                scratch[at++] = comparer.Compare(map[left], map[right]) <= 0 ? map[left++] : map[right++];
            }
            while (left < middle)
            {
                scratch[at++] = map[left++];
            }
            while (right < to)
            {
                scratch[at++] = map[right++];
            }
            for (int i = from; i < to; i++)
            {
                map[i] = scratch[i];
            }
        }
    }

    internal sealed class OrderedEnumerable<TElement, TKey> : OrderedEnumerable<TElement>
    {
        private readonly OrderedEnumerable<TElement> parent;
        private readonly Func<TElement, TKey> keySelector;
        private readonly IComparer<TKey> comparer;
        private readonly bool descending;

        internal OrderedEnumerable(
            IEnumerable<TElement> source, Func<TElement, TKey> keySelector, IComparer<TKey> comparer, bool descending, OrderedEnumerable<TElement> parent)
            : base(source)
        {
            if (keySelector == null)
            {
                throw new ArgumentNullException("keySelector");
            }
            this.parent = parent;
            this.keySelector = keySelector;
            this.comparer = comparer ?? Comparer<TKey>.Default;
            this.descending = descending;
        }

        internal override KeyComparer<TElement> GetComparer(KeyComparer<TElement> next)
        {
            KeyComparer<TElement> own = new KeyComparer<TElement, TKey>(keySelector, comparer, descending, next);
            return parent == null ? own : parent.GetComparer(own);
        }
    }

    internal abstract class KeyComparer<TElement>
    {
        internal abstract void ComputeKeys(TElement[] elements);

        /// Порядок элементов с номерами `a` и `b`; равные ключи решает номер.
        internal abstract int Compare(int a, int b);
    }

    internal sealed class KeyComparer<TElement, TKey> : KeyComparer<TElement>
    {
        private readonly Func<TElement, TKey> keySelector;
        private readonly IComparer<TKey> comparer;
        private readonly bool descending;
        private readonly KeyComparer<TElement> next;
        private TKey[] keys;

        internal KeyComparer(Func<TElement, TKey> keySelector, IComparer<TKey> comparer, bool descending, KeyComparer<TElement> next)
        {
            this.keySelector = keySelector;
            this.comparer = comparer;
            this.descending = descending;
            this.next = next;
        }

        internal override void ComputeKeys(TElement[] elements)
        {
            keys = new TKey[elements.Length];
            for (int i = 0; i < elements.Length; i++)
            {
                keys[i] = keySelector(elements[i]);
            }
            next?.ComputeKeys(elements);
        }

        internal override int Compare(int a, int b)
        {
            int c = comparer.Compare(keys[a], keys[b]);
            if (c == 0)
            {
                return next == null ? a - b : next.Compare(a, b);
            }
            return descending != (c > 0) ? 1 : -1;
        }
    }
}
