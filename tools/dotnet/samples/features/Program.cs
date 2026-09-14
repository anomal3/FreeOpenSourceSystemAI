// Каждая строка здесь ради таблицы или записи в метаданных, а не ради смысла:
// интерфейс с членом по умолчанию, структура, перечисления, делегат, свойство,
// событие, константа (Constant), поле с начальными данными (FieldRVA),
// вложенный обобщённый тип с ограничением (GenericParam,
// GenericParamConstraint), обобщённый метод (MethodSpec), замыкание,
// фильтр исключения (`when`), `finally`, объявление P/Invoke (ImplMap,
// ModuleRef), атрибуты и строки с кириллицей и эмодзи (суррогатная пара в #US).

using System.Runtime.InteropServices;

namespace FreeOs.Samples;

public interface IShape
{
    double Area { get; }

    string Name => "фигура";
}

public struct Point
{
    public int X;
    public int Y;

    public Point(int x, int y)
    {
        X = x;
        Y = y;
    }
}

public enum Color : byte
{
    Red = 1,
    Green = 2,
    Blue = 4,
}

[Flags]
public enum Access
{
    None = 0,
    Read = 1,
    Write = 2,
}

public delegate int Combine(int a, int b);

public sealed class Circle : IShape
{
    public const double Pi = 3.14159;

    private static readonly byte[] Table = { 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16 };

    public Circle(double radius) => Radius = radius;

    public event EventHandler? Changed;

    public double Radius { get; }

    public double Area => Pi * Radius * Radius;

    public void Touch() => Changed?.Invoke(this, EventArgs.Empty);

    public static int Checksum()
    {
        int sum = 0;
        foreach (var value in Table)
        {
            sum += value;
        }
        return sum;
    }

    public class Nested<T>
        where T : IShape
    {
        public T? Value;
    }
}

public static class Program
{
    public static string Started;

    static Program()
    {
        Started = "статический конструктор";
    }

    [DllImport("kernel32")]
    private static extern int GetTickCount();

    public static T Max<T>(T a, T b)
        where T : IComparable<T> => a.CompareTo(b) >= 0 ? a : b;

    public static int Main()
    {
        Console.WriteLine("Привет, FreeOS! 👋");
        var circle = new Circle(2);
        circle.Changed += (_, _) => Console.WriteLine("changed");
        circle.Touch();
        Console.WriteLine($"{circle.Area:F2} {Max(3, 7)} {Max("a", "b")}");
        Combine add = (x, y) => x + y;
        try
        {
            throw new InvalidOperationException("проверка");
        }
        catch (InvalidOperationException error) when (error.Message.Length > 0)
        {
            Console.WriteLine(error.Message);
        }
        catch (Exception)
        {
            Console.WriteLine("other");
        }
        finally
        {
            Console.WriteLine("finally");
        }
        var points = new List<Point> { new(1, 2) };
        foreach (var point in points)
        {
            Console.WriteLine(point.X + point.Y);
        }
        Console.WriteLine(Circle.Checksum() + add(1, 2));
        Console.WriteLine((Color.Red | Color.Blue).ToString());
        Console.WriteLine(Started);
        return 0;
    }
}
