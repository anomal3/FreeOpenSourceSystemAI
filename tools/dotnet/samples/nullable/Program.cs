// Образец для фазы N10b: `Nullable<T>`. До этой фазы структура в corelib была
// пустой, и `x?.M() ?? 0` у метода со значимым результатом не собирался вовсе.
// Проверяется то, что у .NET делает среда, а не библиотека: пустое значение
// упаковывается в null, полное — в сам T, распаковка принимает и то и другое,
// `is int?` узнаёт упакованный int.

namespace FreeOs.Samples.Nullable;

public sealed class Node
{
    public Node? Next;
    public int Depth() => 1 + (Next?.Depth() ?? 0);
}

public struct Point
{
    public int X;
    public int Y;
    public override string ToString() => "(" + X + ", " + Y + ")";
}

public static class Program
{
    private static string Show(object? value) => value == null ? "null" : value.GetType().Name + " " + value;

    private static int? Parse(string text) => int.TryParse(text, out int value) ? value : null;

    public static int Main()
    {
        Console.WriteLine("nullable: start");

        int? empty = null;
        int? five = 5;
        Console.WriteLine("values: " + empty.HasValue + " " + five.HasValue + " " + five.Value + " " + empty.GetValueOrDefault() + " "
            + empty.GetValueOrDefault(7) + " " + five.GetValueOrDefault(7) + " '" + empty + "' '" + five + "'");

        object? boxedEmpty = empty;
        object? boxedFive = five;
        Console.WriteLine("boxed: " + (boxedEmpty == null) + " " + Show(boxedFive) + " " + (boxedFive is int) + " " + (boxedFive is int?) + " " + ("x" is int?));

        int? back = (int?)boxedFive;
        int? backEmpty = (int?)boxedEmpty;
        object plain = 12;
        int? fromPlain = (int?)plain;
        Console.WriteLine("unboxed: " + back + " " + backEmpty.HasValue + " " + fromPlain + " " + (plain as int?));

        try
        {
            _ = empty.Value;
        }
        catch (InvalidOperationException e)
        {
            Console.WriteLine("no value: " + e.Message);
        }
        try
        {
            object text = "x";
            _ = (int?)text;
        }
        catch (InvalidCastException)
        {
            Console.WriteLine("wrong type: InvalidCastException");
        }

        Console.WriteLine("operators: " + (five + 1) + " " + (empty + 1 == null) + " " + (five > 3) + " " + (empty > 3) + " " + (empty == null) + " " + (five == 5) + " " + (empty ?? -1));
        Console.WriteLine("equals: " + five.Equals(5) + " " + empty.Equals(null) + " " + five.GetHashCode() + " " + empty.GetHashCode());

        var chain = new Node { Next = new Node { Next = new Node() } };
        Console.WriteLine("depth: " + chain.Depth() + " " + chain.Next.Next!.Next?.Depth());

        Point? point = new Point { X = 3, Y = 4 };
        object? boxedPoint = point;
        Console.WriteLine("struct: " + Show(boxedPoint) + " " + ((Point?)boxedPoint)!.Value.Y + " " + (default(Point?) == null));

        var parsed = new List<int?> { Parse("10"), Parse("x"), Parse("-3") };
        int sum = 0;
        foreach (int? item in parsed)
        {
            sum += item ?? 100;
        }
        Console.WriteLine("list: " + parsed.Count(x => x.HasValue) + " " + sum + " " + string.Join("|", parsed));

        Console.WriteLine("nullable: done");
        return 11;
    }
}
