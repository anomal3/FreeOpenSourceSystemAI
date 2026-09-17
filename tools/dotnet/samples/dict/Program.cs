// Образец для фазы N10d: Dictionary<TKey, TValue> и HashSet<T> из dotnet/runtime.
// Что здесь решает алгоритм, а не договор: порядок обхода после удалений и
// повторных вставок (освободившиеся записи занимаются с конца списка
// свободных), простые размеры таблиц (EnsureCapacity и TrimExcess), порядок
// множества после операций над ним и поиск по срезу знаков без строки.
// Совпасть с dotnet можно только тем же кодом.

using System.Runtime.CompilerServices;
using System.Runtime.InteropServices;

namespace FreeOs.Samples.Dict;

public sealed class ByLength : IEqualityComparer<string>
{
    public bool Equals(string? x, string? y) => (x?.Length ?? -1) == (y?.Length ?? -1);

    public int GetHashCode(string obj) => obj.Length;
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

    public static int Main()
    {
        Console.WriteLine("dict: start");

        // Порядок обхода: вставки, удаления, новые ключи в освободившихся записях.
        var d = new Dictionary<string, int>();
        for (int i = 0; i < 10; i++)
        {
            d["k" + i] = i;
        }
        d.Remove("k3");
        d.Remove("k7");
        d.Remove("k1");
        d["n1"] = 11;
        d["n2"] = 12;
        d["n3"] = 13;
        d["n4"] = 14;
        Console.WriteLine("order: " + Join(d.Select(p => p.Key + "=" + p.Value)));
        Console.WriteLine("keys: " + Join(d.Keys) + " | " + Join(d.Values) + " | " + d.Count);

        // Ёмкость — простые числа из таблицы HashHelpers.
        var sized = new Dictionary<int, int>(10);
        Console.WriteLine("capacity: " + sized.EnsureCapacity(0) + " " + sized.EnsureCapacity(20) + " " + sized.EnsureCapacity(100) + " " + new Dictionary<int, int>().EnsureCapacity(1));
        for (int i = 0; i < 50; i++)
        {
            sized[i * 7] = i;
        }
        sized.TrimExcess(60);
        Console.WriteLine("trimmed: " + sized.EnsureCapacity(0) + " " + sized.Count);
        // Удаление во время обхода у .NET Core разрешено.
        foreach (KeyValuePair<int, int> pair in sized)
        {
            if (pair.Value % 2 == 1)
            {
                sized.Remove(pair.Key);
            }
        }
        sized[1000] = 1;
        sized[1001] = 2;
        Console.WriteLine("odd removed: " + sized.Count + " " + Join(sized.Keys.Take(6)) + " " + Join(sized.Keys.Skip(sized.Count - 3)));

        // Сравнители: без учёта регистра, свой, и что возвращает Comparer.
        var ci = new Dictionary<string, int>(StringComparer.OrdinalIgnoreCase) { ["Alpha"] = 1, ["beta"] = 2 };
        Console.WriteLine("ignore case: " + ci["ALPHA"] + " " + ci.ContainsKey("Beta") + " " + (ci.Comparer == StringComparer.OrdinalIgnoreCase) + " "
            + (new Dictionary<string, int>().Comparer == EqualityComparer<string>.Default) + " " + (new Dictionary<string, int>(StringComparer.Ordinal).Comparer == StringComparer.Ordinal)
            + " " + (new Dictionary<int, int>().Comparer == EqualityComparer<int>.Default));
        var byLength = new Dictionary<string, string>(new ByLength()) { ["aa"] = "two", ["bbb"] = "three" };
        Console.WriteLine("custom: " + byLength["zz"] + " " + byLength.TryGetValue("wxyz", out _) + " " + byLength.Comparer.GetType().Name + " " + byLength.Count);

        // TryAdd, Remove со значением, исключения.
        Console.WriteLine("try: " + d.TryAdd("k0", 99) + " " + d.TryAdd("z", 26) + " " + d.Remove("z", out int removed) + " " + removed + " " + d.Remove("z", out removed) + " " + removed);
        Console.WriteLine("missing: " + Fails(() => _ = d["nope"]) + " | " + Fails(() => d.Add("k0", 1)) + " | " + Fails(() => d.Add(null!, 1)) + " | " + Fails(() => new Dictionary<int, int>(-1)));

        // Необобщённый IDictionary.
        System.Collections.IDictionary untyped = d;
        Console.WriteLine("untyped: " + untyped["k0"] + " " + (untyped[5] == null) + " " + untyped.Contains("k2") + " " + Fails(() => untyped.Add(5, 1)) + " | " + Fails(() => untyped.Add("s", "text")) + " " + untyped.IsFixedSize + " " + untyped.IsReadOnly);
        var entries = new List<string>();
        foreach (System.Collections.DictionaryEntry entry in untyped)
        {
            entries.Add(entry.Key + ":" + entry.Value);
            if (entries.Count == 3)
            {
                break;
            }
        }
        Console.WriteLine("entries: " + Join(entries));

        // Коллекции ключей и значений.
        string[] keys = new string[d.Count + 1];
        d.Keys.CopyTo(keys, 1);
        Console.WriteLine("copied: " + Join(keys.Select(k => k ?? "-")) + " " + Fails(() => d.Keys.CopyTo(new string[2], 0)) + " | " + Fails(() => ((ICollection<string>)d.Keys).Add("x")) + " " + d.Values.Contains(14) + " " + d.Keys.Contains("k3"));

        // Поиск по срезу знаков без строки.
        Dictionary<string, int>.AlternateLookup<ReadOnlySpan<char>> lookup = d.GetAlternateLookup<ReadOnlySpan<char>>();
        ReadOnlySpan<char> text = "xxk2yy";
        Console.WriteLine("span: " + lookup[text.Slice(2, 2)] + " " + lookup.ContainsKey("k3") + " " + lookup.TryGetValue("n4", out string? actual, out int found) + " " + actual + " " + found + " "
            + lookup.Remove("n4") + " " + d.Count + " " + d.TryGetAlternateLookup<ReadOnlySpan<char>>(out _) + " " + byLength.TryGetAlternateLookup<ReadOnlySpan<char>>(out _) + " " + ci.GetAlternateLookup<ReadOnlySpan<char>>().ContainsKey("BETA"));
        lookup[text.Slice(0, 2)] = 7;
        Console.WriteLine("span added: " + d["xx"] + " " + Fails(() => _ = lookup["zz"]) + " | " + Fails(() => byLength.GetAlternateLookup<ReadOnlySpan<char>>()));

        // CollectionsMarshal: ссылка на значение внутри словаря.
        ref int counter = ref CollectionsMarshal.GetValueRefOrAddDefault(d, "count", out bool exists);
        counter += 5;
        ref int again = ref CollectionsMarshal.GetValueRefOrAddDefault(d, "count", out bool existsAgain);
        again *= 2;
        ref int none = ref CollectionsMarshal.GetValueRefOrNullRef(d, "nope");
        Console.WriteLine("marshal: " + exists + " " + existsAgain + " " + d["count"] + " " + Unsafe.IsNullRef(ref none) + " " + CollectionsMarshal.AsSpan(new List<int> { 1, 2, 3 }).Length);

        // Словарь из словаря и из пар, очистка.
        var copy = new Dictionary<string, int>(d);
        var fromPairs = new Dictionary<string, int>(d.Where(p => p.Value > 10));
        copy.Clear();
        Console.WriteLine("copies: " + copy.Count + " " + copy.EnsureCapacity(0) + " " + Join(fromPairs.Keys) + " " + d.Count);

        // HashSet: порядок обхода после операций над множествами.
        var set = new HashSet<int>();
        for (int i = 0; i < 12; i++)
        {
            set.Add(i * 3 % 12 + i / 4);
        }
        Console.WriteLine("set: " + Join(set) + " " + set.Count + " " + set.Add(5) + " " + set.Remove(9) + " " + set.Add(9) + " " + Join(set));
        var a = new HashSet<int>(Enumerable.Range(0, 20));
        a.ExceptWith(new[] { 3, 4, 5, 15 });
        a.SymmetricExceptWith(new[] { 4, 21, 0, 22, 4 });
        Console.WriteLine("sym: " + Join(a));
        var b = new HashSet<int> { 30, 1, 2, 31 };
        a.SymmetricExceptWith(b);
        Console.WriteLine("sym set: " + Join(a) + " " + a.IsSupersetOf(b) + " " + a.Overlaps(b) + " " + a.IsProperSubsetOf(Enumerable.Range(0, 40)) + " " + a.SetEquals(a.ToArray()) + " " + a.IsProperSupersetOf(new[] { 6, 7 }));
        a.IntersectWith(Enumerable.Range(10, 30));
        Console.WriteLine("intersect: " + Join(a) + " " + a.Count);
        a.UnionWith(new[] { 5, 6, 7 });
        int gone = a.RemoveWhere(x => x % 2 == 0);
        Console.WriteLine("union: " + Join(a) + " " + gone + " " + a.EnsureCapacity(0) + " " + a.EnsureCapacity(50));
        a.TrimExcess();
        Console.WriteLine("trim: " + a.EnsureCapacity(0) + " " + a.Count + " " + a.TryGetValue(7, out int seven) + " " + seven + " " + a.TryGetValue(8, out int eight) + " " + eight);

        // Множество строк без учёта регистра и его поиск по срезу.
        var words = new HashSet<string>(StringComparer.OrdinalIgnoreCase) { "Apple", "banana", "APPLE", "Cherry" };
        HashSet<string>.AlternateLookup<ReadOnlySpan<char>> wordLookup = words.GetAlternateLookup<ReadOnlySpan<char>>();
        Console.WriteLine("words: " + Join(words) + " " + wordLookup.Contains("BANANA") + " " + wordLookup.Add("date") + " " + wordLookup.Add("DATE") + " " + wordLookup.Remove("cherry") + " "
            + Join(words) + " " + (words.Comparer == StringComparer.OrdinalIgnoreCase) + " " + wordLookup.TryGetValue("apple", out string? stored) + " " + stored);

        // Сравнитель множеств, копирование, изменение при обходе.
        IEqualityComparer<HashSet<int>> setComparer = HashSet<int>.CreateSetComparer();
        Console.WriteLine("set comparer: " + setComparer.Equals(new HashSet<int> { 1, 2 }, new HashSet<int> { 2, 1 }) + " " + setComparer.Equals(new HashSet<int> { 1 }, new HashSet<int> { 1, 2 }) + " "
            + (setComparer.GetHashCode(new HashSet<int> { 1, 2 }) == setComparer.GetHashCode(new HashSet<int> { 2, 1 })) + " " + new HashSet<HashSet<int>>(setComparer) { new HashSet<int> { 1 }, new HashSet<int> { 1 } }.Count);
        int[] target = new int[a.Count + 2];
        a.CopyTo(target, 1, 3);
        Console.WriteLine("copy: " + Join(target) + " " + Fails(() => a.CopyTo(new int[1])) + " | " + Fails(() => a.CopyTo(target, 1, 99)));
        Console.WriteLine("modified: " + Fails(() =>
        {
            foreach (int x in a)
            {
                a.Add(x + 100);
            }
        }) + " | " + Fails(() =>
        {
            foreach (KeyValuePair<string, int> pair in d)
            {
                d[pair.Key + "!"] = 0;
            }
        }));

        Console.WriteLine("dict: done");
        return 21;
    }
}
