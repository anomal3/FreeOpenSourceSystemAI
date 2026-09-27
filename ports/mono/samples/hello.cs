// Первая сборка IL под Mono во FreeOS (фаза 60).
//
// Собирается чужим компилятором — csc из .NET Framework 4 на машине сборки —
// и чужим же рантаймом проверяется: тот же hello.exe, запущенный под .NET
// Framework на Windows, обязан напечатать ровно то же, что Mono во FreeOS.
// Поэтому в выводе нет ничего, что зависит от машины: ни времени, ни версии
// системы, ни текста сообщений об ошибках (у Mono и Microsoft он разный), а
// числа печатаются в инвариантной культуре.
//
// Каждая строка проверяет свою часть рантайма: обобщения и интерфейсы
// (mscorlib), LINQ (System.Core), очередь (System.dll), исключения, итераторы,
// замыкания, поток.
//
// Язык — C# 5: другого у csc из .NET Framework нет.

using System;
using System.Collections.Generic;
using System.Globalization;
using System.Linq;
using System.Text;
using System.Threading;

interface IShape
{
    string Name { get; }
    double Area();
}

struct Circle : IShape
{
    readonly double radius;
    public Circle(double radius) { this.radius = radius; }
    public string Name { get { return "circle"; } }
    public double Area() { return Math.PI * radius * radius; }
}

class Square : IShape
{
    readonly double side;
    public Square(double side) { this.side = side; }
    public string Name { get { return "square"; } }
    public virtual double Area() { return side * side; }
}

static class Program
{
    static IEnumerable<int> Fibonacci(int count)
    {
        int a = 0, b = 1;
        for (int i = 0; i < count; i++)
        {
            yield return a;
            int next = a + b;
            a = b;
            b = next;
        }
    }

    static T Largest<T>(IEnumerable<T> items) where T : IComparable<T>
    {
        T best = default(T);
        bool first = true;
        foreach (T item in items)
        {
            if (first || item.CompareTo(best) > 0)
            {
                best = item;
                first = false;
            }
        }
        return best;
    }

    static int Main(string[] args)
    {
        Thread.CurrentThread.CurrentCulture = CultureInfo.InvariantCulture;
        Console.WriteLine("hello: Hello from C# on Mono");

        // Обобщённый список, сортировка, склейка строк.
        var words = new List<string> { "pear", "apple", "fig", "banana" };
        words.Sort(StringComparer.Ordinal);
        Console.WriteLine("hello: sorted " + string.Join(",", words));

        // LINQ — это System.Core.dll.
        int squares = Enumerable.Range(1, 10).Where(n => n % 2 == 0).Select(n => n * n).Sum();
        Console.WriteLine("hello: even squares sum to " + squares);

        // Итератор и обобщённый метод с ограничением.
        Console.WriteLine("hello: fibonacci " + string.Join(" ", Fibonacci(12)));
        Console.WriteLine("hello: largest word " + Largest(words));

        // Интерфейс через структуру и класс, форматирование чисел.
        var shapes = new IShape[] { new Circle(1.5), new Square(2) };
        foreach (var shape in shapes)
            Console.WriteLine(string.Format(CultureInfo.InvariantCulture, "hello: {0} area {1:F4}", shape.Name, shape.Area()));

        // Словарь, StringBuilder.
        var counts = new Dictionary<char, int>();
        foreach (char c in "mississippi")
        {
            int seen;
            counts.TryGetValue(c, out seen);
            counts[c] = seen + 1;
        }
        var line = new StringBuilder();
        foreach (var pair in counts.OrderBy(p => p.Key))
            line.Append(pair.Key).Append('=').Append(pair.Value).Append(' ');
        Console.WriteLine("hello: letters " + line.ToString().TrimEnd());

        // Очередь — это System.dll.
        var queue = new Queue<string>();
        queue.Enqueue("first");
        queue.Enqueue("second");
        Console.WriteLine("hello: queue gives " + queue.Dequeue() + " then " + queue.Dequeue());

        // Исключение: брошено, поймано по типу, finally исполнен.
        bool cleaned = false;
        try
        {
            try
            {
                int.Parse("not a number");
            }
            finally
            {
                cleaned = true;
            }
        }
        catch (FormatException e)
        {
            Console.WriteLine("hello: caught " + e.GetType().Name + ", finally ran " + cleaned);
        }

        // Замыкание над переменной цикла.
        var actions = new List<Func<int>>();
        for (int i = 0; i < 3; i++)
        {
            int copy = i;
            actions.Add(() => copy * 10);
        }
        Console.WriteLine("hello: closures " + string.Join(",", actions.Select(a => a())));

        // Поток: свой стек, своё хранилище, join.
        long total = 0;
        var worker = new Thread(() =>
        {
            for (int i = 1; i <= 100000; i++)
                total += i;
        });
        worker.Start();
        worker.Join();
        Console.WriteLine("hello: thread summed " + total);

        Console.WriteLine("hello: args " + args.Length);
        return 0;
    }
}
