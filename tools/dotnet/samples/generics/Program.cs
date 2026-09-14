// Образец для фазы N3c. Обобщённый код здесь нарочно специализирован и
// ссылочными, и значимыми типами: у `Stack<int>` и `Stack<string>` разная
// раскладка массива, у `Tally<int>` и `Tally<string>` — разные статические
// поля, а `Max<int>` вызывает `CompareTo` через `constrained.` без упаковки.
//
// Чего здесь нет намеренно: `List<T>` и `Dictionary<,>` (фаза N4) и
// форматирования чисел с форматом (`{x:F2}`, N4). `typeof(T).Name` есть: без
// него обобщённый код не назвать по имени.

#pragma warning disable CS8601, CS8603, CS8618

using System.Text;

namespace FreeOs.Samples.Generics;

public sealed class Stack<T>
{
    private T[] items = new T[2];
    private int count;

    public int Count => count;

    public void Push(T item)
    {
        if (count == items.Length)
        {
            T[] bigger = new T[items.Length * 2];
            for (int i = 0; i < count; i++)
            {
                bigger[i] = items[i];
            }
            items = bigger;
        }
        items[count++] = item;
    }

    public T Pop() => items[--count];

    public T Peek() => count > 0 ? items[count - 1] : default;
}

public struct Pair<TFirst, TSecond>
{
    public TFirst First;
    public TSecond Second;

    public Pair(TFirst first, TSecond second)
    {
        First = first;
        Second = second;
    }

    public Pair<TSecond, TFirst> Swap() => new Pair<TSecond, TFirst>(Second, First);

    public override string ToString() => "(" + First + ", " + Second + ")";
}

public static class Tally<T>
{
    public static int Count;

    static Tally()
    {
        Console.WriteLine("Tally<" + typeof(T).Name + "> ready");
    }
}

public interface IShape
{
    int Area();
}

public readonly struct Square : IShape
{
    private readonly int side;

    public Square(int side) => this.side = side;

    public int Area() => side * side;
}

public sealed class Rect : IShape
{
    private readonly int w;
    private readonly int h;

    public Rect(int w, int h)
    {
        this.w = w;
        this.h = h;
    }

    public int Area() => w * h;
}

public sealed class Button
{
    public event EventHandler? Click;

    public string Name { get; }

    public Button(string name) => Name = name;

    public void Press() => Click?.Invoke(this, EventArgs.Empty);
}

public static class Program
{
    private static T Max<T>(T a, T b)
        where T : IComparable<T> => a.CompareTo(b) >= 0 ? a : b;

    private static int TotalArea<T>(T[] shapes)
        where T : IShape
    {
        int total = 0;
        foreach (T shape in shapes)
        {
            total += shape.Area();
        }
        return total;
    }

    private static TResult Apply<T, TResult>(T value, Func<T, TResult> map) => map(value);

    private static void Twice(Action action)
    {
        action();
        action();
    }

    public static int Main()
    {
        Console.OutputEncoding = Encoding.UTF8;
        Console.WriteLine("generics: start");

        var numbers = new Stack<int>();
        for (int i = 1; i <= 5; i++)
        {
            numbers.Push(i * i);
        }
        var words = new Stack<string>();
        words.Push("один");
        words.Push("два");
        Console.WriteLine("stacks: " + numbers.Pop() + " " + numbers.Count + " " + words.Pop() + " " + words.Peek());
        Console.WriteLine("empty peek: " + new Stack<int>().Peek() + " " + (new Stack<string>().Peek() == null));

        var pair = new Pair<int, string>(7, "семь");
        Console.WriteLine("pair " + pair + " swapped " + pair.Swap());

        Tally<int>.Count++;
        Tally<int>.Count++;
        Tally<string>.Count += 10;
        Console.WriteLine("tally " + Tally<int>.Count + " " + Tally<string>.Count);

        Console.WriteLine("max " + Max(3, 9) + " " + Max("apple", "pear") + " " + Max(-5L, -7L));
        Console.WriteLine("area " + TotalArea(new[] { new Square(2), new Square(3) }) + " "
            + TotalArea(new IShape[] { new Rect(2, 5), new Square(4) }));

        // Делегаты и замыкания.
        Func<int, int, int> add = (a, b) => a + b;
        int offset = 100;
        Func<int, int> shift = x => x + offset;
        offset = 1000;
        Console.WriteLine("delegates " + add(2, 3) + " " + shift(5) + " " + Apply(21, x => x * 2) + " "
            + Apply("abc", s => s.Length));
        int calls = 0;
        Action bump = () => calls++;
        bump += () => calls += 10;
        Twice(bump);
        Console.WriteLine("multicast " + calls);

        // События.
        var button = new Button("OK");
        int clicks = 0;
        EventHandler onClick = (sender, _) =>
        {
            clicks++;
            Console.WriteLine("clicked " + ((Button)sender!).Name);
        };
        button.Click += onClick;
        button.Press();
        button.Click -= onClick;
        button.Press();
        Console.WriteLine("clicks " + clicks);

        // Интерполяция.
        string who = "мир";
        int count = 3;
        Console.WriteLine($"interpolated: привет, {who}! {count} x {pair} = {count * pair.First}");

        Console.WriteLine("generics: done");
        return calls;
    }
}
