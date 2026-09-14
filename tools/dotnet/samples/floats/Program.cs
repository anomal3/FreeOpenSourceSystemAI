// Образец для фазы N4c. Каждая строка — горсть того, что обычная программа
// делает с дробными числами: печать, форматы, разбор, Math, float.
//
// Эталон снимается с dotnet в режиме инвариантной глобализации
// (`DOTNET_SYSTEM_GLOBALIZATION_INVARIANT=1`, так запускает `clr-check`).
//
// Синус, логарифм и дробная степень печатаются с десятью знаками: последний
// бит у них зависит от библиотеки C, и у самого .NET на Windows и Linux он
// бывает разным. Корень, округления и арифметика точны везде и печатаются
// целиком.

using System.Text;

namespace FreeOs.Samples.Floats;

public static class Program
{
    public static int Main()
    {
        Console.OutputEncoding = Encoding.UTF8;
        Console.WriteLine("floats: start");

        // Кратчайшая запись, читаемая обратно.
        double third = 1.0 / Three();
        Console.WriteLine(0.1 + Two() / 10);
        Console.WriteLine(third + " " + 2 * third + " " + 1e21 * One() + " " + 1e-7 * One() + " " + 123.456 * One() + " "
            + (-0.0 * One()) + " " + 100.0 * One());
        Console.WriteLine(double.MaxValue + " " + double.Epsilon + " " + double.NaN + " " + (One() / 0) + " " + (-One() / 0));

        // Стандартные и пользовательские форматы.
        Console.WriteLine($"{Math.PI:F2}|{Math.E:F4}|{12345.6789:N2}|{0.256:P1}|{1234.5:E3}|{-42.5:C}|{2.5:F0}|{3.5:F0}|{0.125:G2}");
        Console.WriteLine(1234567.891.ToString("#,##0.00") + " " + 0.5.ToString("0.###E+0") + " " + (-3.75).ToString("0.0;(0.0);zero")
            + " " + 0.0.ToString("0.0;(0.0);zero") + " " + 2.675.ToString("0.00") + " " + 2.675.ToString("F2"));
        Console.WriteLine(string.Format("[{0,10:F3}] [{1,-8:G3}] [{2}]", Math.Sqrt(2), 1234.5678, 1.5f));
        Console.WriteLine($"{1234567:N0}|{-5:N2}|{42:E2}|{7:P0}|{255:C}|{12345:0,0.00}|{-1:#;neg;zero}|{0:#;neg;zero}|{12:00000}");

        // float: своя кратчайшая запись и арифметика одинарной точности.
        float f = 1f / (float)Three();
        float big = 16777216f;
        double widened = f * 3;
        Console.WriteLine(f + " " + big + " " + (big + 1f) + " " + float.MaxValue + " " + widened + " " + (f * 3 == 1f) + " "
            + (double)f + " " + (float)(0.1 * One()));

        // Разбор.
        double parsed = double.Parse("  -1,234.5e2 ");
        bool ok = double.TryParse("1e400", out double infinite);
        bool bad = double.TryParse("1.2.3", out double zero);
        Console.WriteLine(parsed + " " + ok + " " + infinite + " " + bad + " " + zero + " " + float.Parse("3.14159") + " "
            + double.Parse("NaN") + " " + double.Parse("-Infinity"));
        try
        {
            double.Parse("abc");
        }
        catch (FormatException e)
        {
            Console.WriteLine(e.Message);
        }

        // Точная математика.
        Console.WriteLine(Math.Sqrt(2) + " " + Math.Floor(-2.5) + " " + Math.Ceiling(-2.5) + " " + Math.Truncate(-2.7) + " "
            + Math.Round(2.5) + " " + Math.Round(3.5) + " " + Math.Round(-2.5) + " " + Math.Cbrt(27) + " " + Math.Pow(2, 10));
        Console.WriteLine(Math.Round(2.345, 2) + " " + Math.Round(2.5, MidpointRounding.AwayFromZero) + " " + Math.Round(-1.25, 1)
            + " " + Math.Abs(-0.0 * One()) + " " + Math.Max(1.5, double.NaN) + " " + Math.Min(-0.0 * One(), 0.0) + " "
            + Math.Sign(-3.2) + " " + Math.Clamp(7.5, 0, 5));

        // Трансцендентные функции — с десятью знаками.
        Console.WriteLine($"{Math.Sin(1):F10} {Math.Cos(Math.PI / 3):F10} {Math.Tan(0.5):F10} {Math.Atan2(1, -1):F10} "
            + $"{Math.Exp(1):F10} {Math.Log(10):F10} {Math.Log10(2):F10} {Math.Pow(2, 0.5):F10} {MathF.Sqrt(2f)}");

        // Преобразования в целые насыщают, остаток дробный.
        double[] sources = { 3.99, -3.99, 1e10, double.NaN, -1.0 };
        var converted = new StringBuilder();
        foreach (double source in sources)
        {
            converted.Append((int)source).Append('/').Append((long)source).Append('/').Append((uint)source).Append(' ');
        }
        Console.WriteLine(converted.ToString() + (7 / Two()) + " " + (7 % (Two() + 0.5)) + " " + (-7.5 % Two()));

        // Сравнение, сортировка, ключи словаря.
        var values = new List<double> { 3.5, double.NaN, -1, 0.0, double.NegativeInfinity, 2.25 };
        values.Sort();
        Console.WriteLine(string.Join(" ", values) + " " + double.NaN.Equals(double.NaN) + " " + (double.NaN == Nan()) + " "
            + 0.0.Equals(-0.0) + " " + 1.5.CompareTo(2.5) + " " + 3.0.GetHashCode() + " " + (-0.0).GetHashCode());
        var counts = new Dictionary<double, int>();
        foreach (double x in new[] { 0.5, 1.5, 0.5, -0.0, 0.0 })
        {
            counts[x] = counts.TryGetValue(x, out int count) ? count + 1 : 1;
        }
        Console.WriteLine(counts.Count + " " + counts[0.5] + " " + counts[0.0]);

        // Счёт: сумма ряда обратных квадратов.
        double sum = 0;
        for (int i = 1; i <= 1000; i++)
        {
            sum += 1.0 / ((double)i * i);
        }
        Console.WriteLine($"{sum} {Math.Sqrt(sum * 6)} {BitConverter.DoubleToInt64Bits(sum):X16}");
        var builder = new StringBuilder();
        builder.Append(1.25).Append(' ').Append(2.5f).Append(' ').Append(-1e-10);
        Console.WriteLine(builder.ToString());

        Console.WriteLine("floats: done");
        return (int)(sum * 25);
    }

    // Числа через вызов — чтобы компилятор C# не свернул выражение в
    // константу и его посчитала среда.
    private static double One() => 1;

    private static double Two() => 2;

    private static double Three() => 3;

    private static double Nan() => double.NaN;
}
