// Образец для фазы N10c: List<T> и сортировка из dotnet/runtime. Самое важное
// здесь — порядок равных ключей: сортировка .NET неустойчива, и какой из
// равных элементов окажется первым, решает её алгоритм (вставки до 16
// элементов, дальше быстрая сортировка с переходом на кучу). Совпасть с dotnet
// можно только тем же алгоритмом.

using System.Collections.ObjectModel;

namespace FreeOs.Samples.ListSort;

public readonly record struct Item(int Key, string Name)
{
    public override string ToString() => Key + Name;
}

public sealed class ByKey : IComparer<Item>
{
    public int Compare(Item x, Item y) => x.Key.CompareTo(y.Key);
}

public static class Program
{
    private static string Join<T>(IEnumerable<T> items) => string.Join(",", items);

    private static string Fails(Action action)
    {
        try
        {
            action();
            return "no exception";
        }
        catch (Exception e)
        {
            return e.GetType().Name + ": " + e.Message.Replace("\r\n", " | ").Replace("\n", " | ");
        }
    }

    // Пятьдесят элементов с ключами 0..4 — равных много, путь сортировки
    // проходит и разбиение, и вставки.
    private static List<Item> Items(int count)
    {
        var items = new List<Item>();
        int seed = 17;
        for (int i = 0; i < count; i++)
        {
            seed = (seed * 1103515245 + 12345) & 0x7FFFFFFF;
            items.Add(new Item(seed % 5, ((char)('a' + i % 26)).ToString() + i));
        }
        return items;
    }

    public static int Main()
    {
        Console.WriteLine("listsort: start");

        // Устойчивость: у .NET её нет, и порядок равных виден.
        List<Item> small = Items(12);
        small.Sort(new ByKey());
        Console.WriteLine("small: " + Join(small));
        List<Item> large = Items(50);
        large.Sort(new ByKey());
        Console.WriteLine("large: " + Join(large));
        List<Item> byComparison = Items(40);
        byComparison.Sort((x, y) => y.Key.CompareTo(x.Key));
        Console.WriteLine("comparison: " + Join(byComparison));
        List<Item> part = Items(30);
        part.Sort(5, 20, new ByKey());
        Console.WriteLine("part: " + Join(part));

        // Array.Sort с ключами и значениями, числа с NaN, строки.
        int[] keys = { 5, 3, 5, 1, 3, 5, 2, 1, 4, 3, 5, 2, 1, 4, 3, 5, 2, 1, 4, 3 };
        string[] values = new string[keys.Length];
        for (int i = 0; i < values.Length; i++)
        {
            values[i] = "v" + i;
        }
        Array.Sort(keys, values);
        Console.WriteLine("keys: " + Join(keys) + " | " + Join(values));
        double[] doubles = { 3.5, double.NaN, -1, 0, double.NegativeInfinity, 2, double.NaN, -0.0, 1e10 };
        Array.Sort(doubles);
        Console.WriteLine("doubles: " + Join(doubles));
        string[] words = { "pear", "Apple", "fig", "apple", "Fig", "banana", "kiwi", "Kiwi", "date" };
        Array.Sort(words, StringComparer.Ordinal);
        Console.WriteLine("ordinal: " + Join(words));
        Console.WriteLine("search: " + Array.BinarySearch(keys, 4) + " " + Array.BinarySearch(keys, 6) + " " + large.BinarySearch(new Item(2, "?"), new ByKey()));

        // List<T>: вставки, диапазоны, поиск.
        var list = new List<int>(3) { 1, 2, 3 };
        Console.WriteLine("capacity: " + list.Capacity + " " + Join(list));
        list.Add(4);
        Console.WriteLine("grown: " + list.Capacity);
        list.InsertRange(2, new[] { 20, 21, 22 });
        list.Insert(0, 0);
        list.AddRange(Enumerable.Range(5, 4));
        Console.WriteLine("inserted: " + Join(list) + " " + list.Count + " " + list.Capacity);
        Console.WriteLine("range: " + Join(list.GetRange(2, 4)) + " " + Join(list.Slice(1, 3)) + " " + list.IndexOf(21) + " " + list.LastIndexOf(4) + " " + list.IndexOf(99));
        Console.WriteLine("find: " + list.Find(x => x > 20) + " " + list.FindIndex(x => x > 20) + " " + list.FindLast(x => x < 5) + " " + list.FindLastIndex(x => x < 5) + " "
            + Join(list.FindAll(x => x % 2 == 0)) + " " + list.Exists(x => x == 7) + " " + list.TrueForAll(x => x >= 0));
        Console.WriteLine("removed: " + list.RemoveAll(x => x >= 20) + " " + Join(list));
        list.RemoveRange(1, 2);
        list.Reverse(0, 4);
        Console.WriteLine("reversed: " + Join(list) + " " + Join(list.ConvertAll(x => "#" + x)));
        list.TrimExcess();
        Console.WriteLine("trimmed: " + list.Capacity + " " + list.EnsureCapacity(20) + " " + list.Capacity);
        int total = 0;
        list.ForEach(x => total += x);
        Console.WriteLine("sum: " + total);

        // ReadOnlyCollection и необобщённый IList.
        ReadOnlyCollection<int> readOnly = list.AsReadOnly();
        Console.WriteLine("read only: " + readOnly.Count + " " + readOnly[0] + " " + readOnly.Contains(7) + " "
            + Fails(() => ((IList<int>)readOnly).Add(1)));
        System.Collections.IList untyped = list;
        Console.WriteLine("untyped: " + untyped.Add(9) + " " + untyped.Contains(9) + " " + untyped.IsFixedSize + " " + Fails(() => untyped.Add("text")));
        object[] boxes = new object[list.Count];
        untyped.CopyTo(boxes, 0);
        Console.WriteLine("boxes: " + Join(boxes));

        // Исключения и проверка версии.
        Console.WriteLine("index: " + Fails(() => _ = list[99]));
        Console.WriteLine("insert: " + Fails(() => list.Insert(99, 1)));
        Console.WriteLine("range fails: " + Fails(() => list.GetRange(1, 99)));
        Console.WriteLine("modified: " + Fails(() =>
        {
            foreach (int x in list)
            {
                list.Add(x);
            }
        }));

        // HashSet как ISet.
        var set = new HashSet<string> { "a", "b", "c" };
        ISet<string> iset = set;
        iset.SymmetricExceptWith(new[] { "c", "d", "d" });
        Console.WriteLine("set: " + Join(set) + " " + set.IsSupersetOf(new[] { "a" }) + " " + set.SetEquals(new[] { "d", "b", "a" }) + " " + set.Overlaps(new[] { "z" }));

        Console.WriteLine("listsort: done");
        return 16;
    }
}
