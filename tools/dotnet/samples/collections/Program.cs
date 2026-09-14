// Образец для фазы N4b. Порядок перебора здесь — часть проверки: у .NET
// `Dictionary` и `HashSet` перебирают в порядке добавления, а освобождённое
// удалением место занимает следующий добавленный элемент. Хэши строк у .NET
// случайны от запуска к запуску, так что на порядок они не влияют — и своя
// реализация обязана повторить именно устройство записей, а не хэш.

using System.Text;

namespace FreeOs.Samples.Collections;

public sealed class Person : IEquatable<Person>
{
    public Person(string name, int age)
    {
        Name = name;
        Age = age;
    }

    public string Name { get; }

    public int Age { get; }

    public bool Equals(Person? other) => other != null && other.Name == Name && other.Age == Age;

    public override bool Equals(object? obj) => Equals(obj as Person);

    public override int GetHashCode() => Name.Length * 31 + Age;
}

public readonly struct Cell : IEquatable<Cell>
{
    public Cell(int row, int column)
    {
        Row = row;
        Column = column;
    }

    public int Row { get; }

    public int Column { get; }

    public bool Equals(Cell other) => Row == other.Row && Column == other.Column;

    public override bool Equals(object? obj) => obj is Cell other && Equals(other);

    public override int GetHashCode() => Row * 100 + Column;

    public override string ToString() => "R" + Row + "C" + Column;
}

public static class Program
{
    private static IEnumerable<int> Evens(int limit)
    {
        for (int i = 0; i <= limit; i++)
        {
            if (i % 2 == 0)
            {
                yield return i;
            }
        }
    }

    private static IEnumerable<string> Words()
    {
        yield return "alpha";
        try
        {
            yield return "beta";
            yield return "gamma";
        }
        finally
        {
            Console.WriteLine("words: finally");
        }
    }

    private static int Sum(IEnumerable<int> values)
    {
        int total = 0;
        foreach (int value in values)
        {
            total += value;
        }
        return total;
    }

    public static int Main()
    {
        Console.OutputEncoding = Encoding.UTF8;
        Console.WriteLine("collections: start");

        var list = new List<int> { 5, 3, 8 };
        list.Add(1);
        list.Insert(0, 9);
        list.Remove(3);
        list.RemoveAt(0);
        Console.WriteLine("list " + string.Join(",", list) + " count " + list.Count + " has 8 " + list.Contains(8)
            + " at " + list.IndexOf(8));
        list.Sort();
        list[1] = 7;
        list.Reverse();
        Console.WriteLine("sorted " + string.Join(",", list) + " sum " + Sum(list) + " array " + list.ToArray().Length);

        var names = new List<string> { "pear", "apple", "fig" };
        names.Sort();
        names.AddRange(new[] { "kiwi", "lime" });
        Console.WriteLine("names " + string.Join(" ", names) + " " + names.Find(n => n.StartsWith("k")) + " "
            + names.FindIndex(n => n.Length == 3) + " " + names.Exists(n => n == "lime"));

        var cells = new List<Cell> { new Cell(1, 2), new Cell(3, 4) };
        Cell first = cells[0];
        Console.WriteLine("cells " + first + " " + cells.Contains(new Cell(3, 4)) + " " + cells.IndexOf(new Cell(9, 9)));

        // Словарь: порядок добавления, место удалённого занимает новый.
        var ages = new Dictionary<string, int>();
        ages["ann"] = 30;
        ages.Add("bob", 25);
        ages["cid"] = 41;
        ages["ann"] = 31;
        ages.Remove("bob");
        ages["dan"] = 19;
        var order = new StringBuilder();
        foreach (KeyValuePair<string, int> pair in ages)
        {
            order.Append(pair.Key).Append('=').Append(pair.Value).Append(' ');
        }
        Console.WriteLine("dict " + order + "count " + ages.Count + " " + ages.ContainsKey("bob") + " "
            + ages.TryGetValue("cid", out int cid) + " " + cid);
        try
        {
            Console.WriteLine(ages["zed"]);
        }
        catch (KeyNotFoundException error)
        {
            Console.WriteLine("missing: " + error.Message);
        }
        Console.WriteLine("keys " + string.Join(",", ages.Keys) + " values " + string.Join(",", ages.Values));

        var grid = new Dictionary<Cell, string> { [new Cell(0, 0)] = "origin", [new Cell(2, 5)] = "far" };
        var people = new Dictionary<Person, int> { [new Person("Ann", 30)] = 1 };
        Console.WriteLine("struct key " + grid[new Cell(2, 5)] + " class key " + people[new Person("Ann", 30)] + " "
            + people.ContainsKey(new Person("Ann", 31)));

        var set = new HashSet<int> { 4, 1, 4, 9 };
        bool added = set.Add(1);
        set.Remove(4);
        set.Add(16);
        Console.WriteLine("set " + string.Join(",", set) + " count " + set.Count + " added " + added + " has 9 " + set.Contains(9));

        var queue = new Queue<string>();
        queue.Enqueue("a");
        queue.Enqueue("b");
        queue.Enqueue("c");
        var stack = new Stack<int>();
        stack.Push(1);
        stack.Push(2);
        Console.WriteLine("queue " + queue.Dequeue() + queue.Peek() + queue.Count + " stack " + stack.Pop() + stack.Peek() + stack.Count);

        // Массивы — тоже последовательности.
        int[] numbers = { 3, 1, 2 };
        Array.Sort(numbers);
        IList<int> view = numbers;
        Console.WriteLine("array " + string.Join(",", numbers) + " sum " + Sum(numbers) + " index " + Array.IndexOf(numbers, 2)
            + " ilist " + view.Count + view[2]);

        // Итераторы.
        Console.WriteLine("evens " + string.Join(",", Evens(9)) + " sum " + Sum(Evens(100)));
        foreach (string word in Words())
        {
            Console.WriteLine("word " + word);
            if (word == "beta")
            {
                break;
            }
        }

        Console.WriteLine("collections: done");
        return ages.Count + set.Count;
    }
}
