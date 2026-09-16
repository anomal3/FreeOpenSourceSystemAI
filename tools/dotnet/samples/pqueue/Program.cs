// Образец для фазы N10: `PriorityQueue<TElement, TPriority>` — первый файл,
// взятый у dotnet/runtime, а не написанный заново. Проверяется то, что
// программа видит у настоящего .NET: порядок извлечения при равных
// приоритетах, раскладка четверичной кучи в `UnorderedItems`, рост ёмкости,
// свой сравнитель, удаление из середины, тексты исключений и проверка версии
// у перечислителя.

namespace FreeOs.Samples.PQueue;

// Наибольший первым: путь очереди со своим сравнителем, а не с `Comparer.Default`.
public sealed class Descending : IComparer<int>
{
    public int Compare(int x, int y) => y.CompareTo(x);
}

public static class Program
{
    private static IEnumerable<(string, int)> Stream()
    {
        yield return ("five", 5);
        yield return ("one", 1);
        yield return ("four", 4);
        yield return ("two", 2);
        yield return ("three", 3);
    }

    private static string Drain<TElement, TPriority>(PriorityQueue<TElement, TPriority> queue)
    {
        var parts = new List<string>();
        while (queue.TryDequeue(out TElement? element, out TPriority? priority))
        {
            parts.Add(element + ":" + priority);
        }
        return string.Join(" ", parts);
    }

    private static string Layout<TElement, TPriority>(PriorityQueue<TElement, TPriority> queue)
    {
        var parts = new List<string>();
        foreach ((TElement element, TPriority priority) in queue.UnorderedItems)
        {
            parts.Add(element + ":" + priority);
        }
        return string.Join(" ", parts);
    }

    // Кратчайшие пути от A — то, ради чего очередь с приоритетами обычно и
    // берут. Узел может лечь в очередь несколько раз; устаревшие записи
    // пропускаются.
    private static int Dijkstra()
    {
        var edges = new Dictionary<string, List<(string To, int Cost)>>
        {
            ["A"] = new() { ("B", 7), ("C", 9), ("F", 14) },
            ["B"] = new() { ("A", 7), ("C", 10), ("D", 15) },
            ["C"] = new() { ("A", 9), ("B", 10), ("D", 11), ("F", 2) },
            ["D"] = new() { ("B", 15), ("C", 11), ("E", 6) },
            ["E"] = new() { ("D", 6), ("F", 9) },
            ["F"] = new() { ("A", 14), ("C", 2), ("E", 9) },
        };
        var best = new Dictionary<string, int> { ["A"] = 0 };
        var order = new List<string>();
        var frontier = new PriorityQueue<string, int>();
        frontier.Enqueue("A", 0);
        while (frontier.TryDequeue(out string? node, out int distance))
        {
            if (distance > best[node])
            {
                continue;
            }
            order.Add(node + "=" + distance);
            foreach ((string to, int cost) in edges[node])
            {
                int candidate = distance + cost;
                if (!best.TryGetValue(to, out int known) || candidate < known)
                {
                    best[to] = candidate;
                    frontier.Enqueue(to, candidate);
                }
            }
        }
        Console.WriteLine("dijkstra " + string.Join(" ", order));
        return best["E"];
    }

    public static int Main()
    {
        Console.WriteLine("pqueue: start");

        // Конструктор из массива: путь ICollection<T> и Heapify.
        var fromArray = new PriorityQueue<string, int>(new[] { ("e", 5), ("b", 2), ("d", 4), ("a", 1), ("c", 3), ("f", 6), ("g", 0) });
        Console.WriteLine("heap " + Layout(fromArray) + " count " + fromArray.Count + " capacity " + fromArray.Capacity);
        Console.WriteLine("peek " + fromArray.Peek() + " default " + (fromArray.Comparer == Comparer<int>.Default));
        Console.WriteLine("drain " + Drain(fromArray));

        // Из перечисления без ICollection<T>: ёмкость растёт 4, 8.
        var fromStream = new PriorityQueue<string, int>(Stream());
        Console.WriteLine("stream capacity " + fromStream.Capacity + " layout " + Layout(fromStream));

        // Равные приоритеты: порядок решает устройство кучи, а не порядок
        // добавления, — и он обязан совпасть с dotnet.
        var ties = new PriorityQueue<string, int>();
        foreach (string word in new[] { "red", "green", "blue", "cyan", "pink", "gold", "gray" })
        {
            ties.Enqueue(word, word.Length);
        }
        Console.WriteLine("ties " + Drain(ties));

        // Свой сравнитель, вставка-и-извлечение в обоих порядках.
        var max = new PriorityQueue<string, int>(new Descending());
        max.EnqueueRange(new[] { ("low", 1), ("high", 9), ("mid", 5) });
        string swapped = max.DequeueEnqueue("top", 10);
        string passed = max.EnqueueDequeue("tiny", 0);
        string kept = max.EnqueueDequeue("huge", 99);
        max.TryPeek(out string? head, out int headPriority);
        Console.WriteLine("max " + swapped + " " + passed + " " + kept + " peek " + head + ":" + headPriority + " rest " + Drain(max));

        // Удаление из середины, пачка с одним приоритетом, ёмкость.
        var jobs = new PriorityQueue<string, int>(2);
        jobs.EnqueueRange(new[] { "x", "y", "z" }, 7);
        jobs.Enqueue("urgent", 1);
        jobs.Enqueue("later", 8);
        bool removed = jobs.Remove("y", out string? gone, out int gonePriority);
        bool missing = jobs.Remove("nope", out _, out _);
        Console.WriteLine("jobs removed " + removed + " " + gone + ":" + gonePriority + " missing " + missing + " layout " + Layout(jobs));
        int ensured = jobs.EnsureCapacity(20);
        jobs.TrimExcess();
        Console.WriteLine("capacity " + ensured + " trimmed " + jobs.Capacity + " count " + jobs.Count);

        // Приоритет ссылочного типа — путь с запомненным сравнителем.
        var names = new PriorityQueue<int, string>();
        names.Enqueue(3, "cherry");
        names.Enqueue(1, "apple");
        names.Enqueue(2, "banana");
        Console.WriteLine("names " + Drain(names));

        // ICollection.CopyTo: в массив пар и в массив не того типа.
        var copy = new PriorityQueue<string, int>(new[] { ("q", 3), ("r", 1), ("s", 2) });
        var pairs = new (string, int)[4];
        ((System.Collections.ICollection)copy.UnorderedItems).CopyTo(pairs, 1);
        Console.WriteLine("copied " + pairs[0] + " " + pairs[1] + " " + pairs[2] + " " + pairs[3]);
        try
        {
            ((System.Collections.ICollection)copy.UnorderedItems).CopyTo(new string[3], 0);
        }
        catch (ArgumentException e)
        {
            Console.WriteLine("copy: " + e.Message);
        }
        copy.Clear();
        Console.WriteLine("cleared " + copy.Count + " capacity " + copy.Capacity);

        try
        {
            copy.Dequeue();
        }
        catch (InvalidOperationException e)
        {
            Console.WriteLine("empty: " + e.Message);
        }
        try
        {
            _ = new PriorityQueue<int, int>(-1);
        }
        catch (ArgumentOutOfRangeException e)
        {
            Console.WriteLine("negative: " + e.ParamName + " | " + e.Message.Replace("\r\n", "\n").Replace("\n", " / "));
        }
        try
        {
            copy.EnqueueRange(null!);
        }
        catch (ArgumentNullException e)
        {
            Console.WriteLine("null: " + e.Message);
        }
        try
        {
            copy.Enqueue("a", 1);
            foreach (var item in copy.UnorderedItems)
            {
                copy.Enqueue("b", 2);
            }
        }
        catch (InvalidOperationException e)
        {
            Console.WriteLine("modified: " + e.Message);
        }

        int total = Dijkstra();
        Console.WriteLine("pqueue: done");
        return total;
    }
}
