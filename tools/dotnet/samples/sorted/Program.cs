// Образец для фазы N10b: шесть коллекций, взятых у dotnet/runtime без правки.
// Каждая строка — то, что программа видит у настоящего .NET: порядок обхода,
// что возвращают поиск и удаление, ёмкости после роста и сжатия, границы
// вида SortedSet и тексты исключений.

namespace FreeOs.Samples.Sorted;

public sealed class ByLength : IComparer<string>
{
    public int Compare(string? x, string? y) => (x?.Length ?? 0).CompareTo(y?.Length ?? 0);
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
            return e.GetType().Name + ": " + e.Message;
        }
    }

    public static int Main()
    {
        Console.WriteLine("sorted: start");

        // LinkedList: вставки у узлов, поиск с обеих сторон, удаление.
        var list = new LinkedList<string>(new[] { "b", "d" });
        list.AddFirst("a");
        LinkedListNode<string> d = list.Find("d")!;
        list.AddBefore(d, "c");
        list.AddLast("e");
        list.AddLast("c");
        Console.WriteLine("linked: " + Join(list) + " " + list.Count + " " + list.First!.Value + " " + list.Last!.Value);
        Console.WriteLine("find: " + (list.Find("c")!.Next!.Value) + " " + (list.FindLast("c")!.Previous!.Value) + " " + (list.Find("z") == null));
        list.Remove("c");
        list.RemoveFirst();
        list.RemoveLast();
        Console.WriteLine("removed: " + Join(list) + " " + list.Contains("c") + " " + d.List!.Count);
        var backwards = new List<string>();
        for (LinkedListNode<string>? node = list.Last; node != null; node = node.Previous)
        {
            backwards.Add(node.Value);
        }
        Console.WriteLine("backwards: " + Join(backwards));
        Console.WriteLine("attached: " + Fails(() => list.AddFirst(d)));
        Console.WriteLine("foreign: " + Fails(() => new LinkedList<string>().Remove(d)));
        Console.WriteLine("empty: " + Fails(() => new LinkedList<int>().RemoveFirst()));

        // Stack и Queue теперь тоже из dotnet/runtime.
        var stack = new Stack<int>(new[] { 1, 2, 3 });
        stack.Push(4);
        var copy = new int[6];
        stack.CopyTo(copy, 1);
        object[] boxed = new object[4];
        ((System.Collections.ICollection)stack).CopyTo(boxed, 0);
        Console.WriteLine("stack: " + Join(stack) + " " + stack.Peek() + " " + Join(copy) + " " + Join(stack.ToArray()) + " " + stack.Contains(2));
        Console.WriteLine("stack pop: " + stack.Pop() + " " + stack.TryPop(out int top) + " " + top + " " + stack.Count + " " + Fails(() => new Stack<int>().Pop()));
        var queue = new Queue<string>();
        for (int i = 0; i < 6; i++)
        {
            queue.Enqueue("q" + i);
            if (i % 2 == 1)
            {
                queue.Dequeue();
            }
        }
        Console.WriteLine("queue: " + Join(queue) + " " + queue.Peek() + " " + queue.Contains("q4") + " " + Join(queue.ToArray()) + " " + Fails(() => new Queue<int>().Dequeue()));
        queue.TrimExcess();
        Console.WriteLine("queue capacity: " + queue.EnsureCapacity(0) + " " + Fails(() => queue.TrimExcess(1)));

        // SortedList: двоичный поиск по ключам, индексы, свой сравнитель.
        var sortedList = new SortedList<string, int> { ["pear"] = 3, ["apple"] = 1, ["fig"] = 7 };
        sortedList.Add("kiwi", 2);
        Console.WriteLine("sorted list: " + Join(sortedList) + " " + sortedList.IndexOfKey("kiwi") + " " + sortedList.IndexOfKey("lime") + " " + sortedList.IndexOfValue(7));
        Console.WriteLine("keys: " + Join(sortedList.Keys) + " | " + Join(sortedList.Values) + " " + sortedList.GetKeyAtIndex(1) + " " + sortedList.Capacity);
        sortedList.RemoveAt(0);
        sortedList["fig"] = 70;
        Console.WriteLine("changed: " + Join(sortedList) + " " + sortedList.TryGetValue("pear", out int pear) + " " + pear);
        Console.WriteLine("duplicate: " + Fails(() => sortedList.Add("kiwi", 9)));
        Console.WriteLine("missing: " + Fails(() => _ = sortedList["lime"]));
        var byLength = new SortedList<string, string>(new ByLength()) { ["ccc"] = "3", ["a"] = "1", ["bb"] = "2" };
        Console.WriteLine("by length: " + Join(byLength.Keys) + " " + byLength.ContainsKey("zz"));
        var fromDictionary = new SortedList<int, string>(new Dictionary<int, string> { [5] = "five", [1] = "one", [3] = "three" });
        Console.WriteLine("from dictionary: " + Join(fromDictionary));

        // SortedDictionary: красно-чёрное дерево из SortedSet.
        var tree = new SortedDictionary<int, string>();
        foreach (int key in new[] { 50, 20, 80, 10, 30, 70, 90, 60 })
        {
            tree[key] = "v" + key;
        }
        tree.Remove(20);
        Console.WriteLine("sorted dictionary: " + Join(tree.Keys) + " " + tree.Count + " " + tree[60] + " " + tree.ContainsValue("v90") + " " + tree.ContainsValue("v20"));
        Console.WriteLine("tree missing: " + Fails(() => _ = tree[20]));
        var pairs = new KeyValuePair<int, string>[tree.Count + 1];
        tree.CopyTo(pairs, 1);
        Console.WriteLine("tree copy: " + pairs[1] + " " + pairs[tree.Count]);

        // SortedSet: вид между границами и операции над множествами. SetEquals с
        // List идёт через BitHelper и stackalloc.
        var set = new SortedSet<int> { 9, 3, 7, 1, 5, 11, 13 };
        SortedSet<int> view = set.GetViewBetween(4, 11);
        Console.WriteLine("set: " + Join(set) + " " + set.Min + " " + set.Max + " view " + Join(view) + " " + view.Count + " " + view.Min + " " + view.Max);
        view.Add(6);
        Console.WriteLine("view add: " + Join(set) + " " + Fails(() => view.Add(20)));
        Console.WriteLine("reverse: " + Join(set.Reverse()));
        Console.WriteLine("set ops: " + set.SetEquals(new List<int> { 13, 11, 9, 7, 6, 5, 3, 1, 1 }) + " " + set.IsSupersetOf(new[] { 3, 5 }) + " "
            + set.IsProperSubsetOf(new List<int> { 1, 3, 5, 6, 7, 9, 11, 13, 15 }) + " " + set.Overlaps(new[] { 2, 4, 6 }));
        var other = new SortedSet<int>(new[] { 5, 6, 7, 8, 100 });
        var union = new SortedSet<int>(set);
        union.UnionWith(other);
        var intersection = new SortedSet<int>(set);
        intersection.IntersectWith(other);
        var except = new SortedSet<int>(set);
        except.ExceptWith(other);
        var symmetric = new SortedSet<int>(set);
        symmetric.SymmetricExceptWith(new List<int> { 1, 2, 3, 4 });
        Console.WriteLine("union: " + Join(union) + " | " + Join(intersection) + " | " + Join(except) + " | " + Join(symmetric));
        Console.WriteLine("remove where: " + set.RemoveWhere(x => x % 3 == 0) + " " + Join(set) + " " + set.TryGetValue(7, out int seven) + " " + seven);
        Console.WriteLine("bounds: " + Fails(() => set.GetViewBetween(9, 2)));

        Console.WriteLine("sorted: done");
        return 42;
    }
}
