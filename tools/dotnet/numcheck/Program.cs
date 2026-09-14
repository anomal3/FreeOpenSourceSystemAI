// Эталон печати и разбора чисел (фаза N4c), см. numcheck.csproj.
//
// Строка вывода — поля через табуляцию:
//
//   D <биты double, 16 hex> <формат> <результат>
//   S <биты float, 8 hex>   <формат> <результат>
//   I <ширина> <значение>   <формат> <результат>
//   P <строка> <биты double или !Fail>
//   Q <строка> <биты float или !Fail>
//
// Результат `!Format` — FormatException. Символы вне 0x20..0x7E, обратная
// косая черта и табуляция записаны как \uXXXX.
//
// Случайность своя и с постоянным зерном: вывод один и тот же от запуска к
// запуску, и расхождение воспроизводится.

using System.Globalization;
using System.Text;

namespace FreeOs.Tools.NumCheck;

public static class Program
{
    private static readonly CultureInfo Invariant = CultureInfo.InvariantCulture;
    private static ulong state = 0x9E3779B97F4A7C15;

    private static ulong Next()
    {
        // xorshift64*
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        return state * 0x2545F4914F6CDD1D;
    }

    private static int Below(int n) => (int)(Next() % (ulong)n);

    private static readonly string[] CustomFormats =
    {
        "0", "00", "0.0", "0.00", "#", "#.#", "#.##", "#,##0", "#,##0.00", "#,#", "0,0.0", "#,##0,,", "#,##0,,.0",
        "0.###E+0", "0.0E0", "0.00e-00", "##0.0E+000", "00.00%", "0.0‰", "#%", "'#'0.0'x'", "\"q\"0\\#",
        "#.#;(#.#);zero", "0;-0", ";;", "0.0;", "#;#;", "E+0", "abc", "0.0E", "0.0E+", "#,,", ",0", "0,",
        "00,000.00,", "#0.##0", "0.0.0", "0¤", "[0]", "0.#####################", "000000000000000000000.0",
        "#,##0.00;(#,##0.00)", "0.00;;'nil'", "0e0", "#E+00", "0.0\\E+0", "'E'0", "%#.#", "0 %", "0.0,", "0,.0",
    };

    private static readonly string[] DoubleFormats =
    {
        "", "R", "r", "G", "g", "G0", "G1", "G2", "G3", "G5", "G7", "G10", "G15", "G16", "G17", "G20", "g4", "G99",
        "F", "F0", "F1", "F2", "F3", "F5", "F10", "F20", "f4", "F99",
        "E", "E0", "E1", "E2", "E5", "E10", "E16", "E20", "e3",
        "N", "N0", "N1", "N3", "n2", "P", "P0", "P1", "P4", "p", "C", "C0", "C3", "c",
        "D", "X", "B", "Z", "Q2", "F1x", "G1000000000", "E00", "F012",
    };

    private static readonly string[] SingleFormats =
    {
        "", "R", "G", "G1", "G3", "G6", "G7", "G8", "G9", "G12", "F", "F0", "F2", "F5", "F12", "E", "E0", "E3", "E9",
        "N", "N1", "P", "P0", "C", "0", "0.00", "#,##0.###", "0.###E+0", "#.#;(#.#);zero", "00.0000000000", "D",
    };

    private static readonly string[] IntegerFormats =
    {
        "", "G", "G0", "G1", "G3", "G10", "G25", "g5", "D", "D0", "D1", "D5", "D20", "d3", "X", "X0", "X4", "X16",
        "x", "x8", "B", "B8", "b", "b40", "N", "N0", "N2", "n4", "F", "F0", "F3", "E", "E0", "E2", "e10", "E25",
        "P", "P0", "p1", "C", "C0", "c3", "R", "R3", "Z", "Q", "0", "00000", "#,##0", "#,##0.00", "0.###E+0",
        "#;(#);zero", "0,,", "#%", "0.0‰", "'x'#", "#.##", "0e-0", "00.00",
    };

    public static void Main()
    {
        var output = new StreamWriter(Console.OpenStandardOutput(), new UTF8Encoding(false), 1 << 16) { NewLine = "\n" };

        foreach (double value in DoubleValues())
        {
            string bits = BitConverter.DoubleToInt64Bits(value).ToString("X16");
            foreach (string format in DoubleFormats.Concat(CustomFormats))
            {
                output.WriteLine("D\t" + bits + "\t" + Escape(format) + "\t" + Format(() => value.ToString(format, Invariant)));
            }
        }

        foreach (float value in SingleValues())
        {
            string bits = BitConverter.SingleToInt32Bits(value).ToString("X8");
            foreach (string format in SingleFormats)
            {
                output.WriteLine("S\t" + bits + "\t" + Escape(format) + "\t" + Format(() => value.ToString(format, Invariant)));
            }
        }

        foreach ((int width, Func<string, string> print, string text) in IntegerValues())
        {
            foreach (string format in IntegerFormats)
            {
                output.WriteLine("I\t" + width + "\t" + text + "\t" + Escape(format) + "\t" + Format(() => print(format)));
            }
        }

        foreach (string input in ParseInputs())
        {
            string d = double.TryParse(input, NumberStyles.Float | NumberStyles.AllowThousands, Invariant, out double parsed)
                ? BitConverter.DoubleToInt64Bits(parsed).ToString("X16")
                : "!Fail";
            string f = float.TryParse(input, NumberStyles.Float | NumberStyles.AllowThousands, Invariant, out float single)
                ? BitConverter.SingleToInt32Bits(single).ToString("X8")
                : "!Fail";
            output.WriteLine("P\t" + Escape(input) + "\t" + d);
            output.WriteLine("Q\t" + Escape(input) + "\t" + f);
        }

        output.Flush();
    }

    private static string Format(Func<string> print)
    {
        try
        {
            return Escape(print());
        }
        catch (FormatException)
        {
            return "!Format";
        }
    }

    private static string Escape(string text)
    {
        var builder = new StringBuilder(text.Length);
        foreach (char c in text)
        {
            if (c < 0x20 || c > 0x7E || c == '\\')
            {
                builder.Append("\\u").Append(((int)c).ToString("X4"));
            }
            else
            {
                builder.Append(c);
            }
        }
        return builder.ToString();
    }

    private static IEnumerable<double> DoubleValues()
    {
        double[] fixedValues =
        {
            0.0, -0.0, double.NaN, double.PositiveInfinity, double.NegativeInfinity, double.MaxValue, double.MinValue,
            double.Epsilon, -double.Epsilon, 1, -1, 0.1, 0.2, 0.3, 0.5, 1.5, 2.5, -2.5, 0.125, 0.375, 2.675, 1e15, 1e16,
            1e17, 1e-4, 1e-5, 123456789012345678.0, 9007199254740992.0, 1.0 / 3, 2.0 / 3, float.MaxValue, 1e308,
            1e-308, 2.2250738585072014E-308, 9.5, 99.95, 0.995, 999.9999, 0.0005, 0.00049999999999999999, 0.0015,
            -0.4, 12345.6789, 1234567.891, -60, 100, 1e21, 1e22, 1e23, 5e-324, 4503599627370496.5, 0.1 + 0.2,
            7.0E-10, 123.456, 1e-7, 9.999999999999999e22, 0.6822871999174, 299792458, 6.02214076e23, 1.602176634e-19,
        };
        foreach (double value in fixedValues)
        {
            yield return value;
        }
        for (int i = 0; i < 400; i++)
        {
            yield return BitConverter.Int64BitsToDouble((long)Next());
        }
        for (int i = 0; i < 400; i++)
        {
            // Десятичное с немногими цифрами — как в обычной программе.
            double value = (long)(Next() % 10_000_000_000UL) / Math.Pow(10, Below(20)) * Math.Pow(10, Below(8));
            yield return Below(2) == 0 ? value : -value;
        }
        for (int i = 0; i < 300; i++)
        {
            // Ровно посередине между двумя десятичными.
            double value = ((long)(Next() % 100_000) + 0.5) / Math.Pow(2, Below(12));
            yield return Below(2) == 0 ? value : -value;
        }
        for (int i = 0; i < 200; i++)
        {
            // Девятки перед переносом разряда.
            double value = (Math.Pow(10, Below(12) + 1) - 1) / Math.Pow(10, Below(15));
            yield return Below(3) == 0 ? Math.BitIncrement(value) : Below(2) == 0 ? Math.BitDecrement(value) : value;
        }
        for (int i = 0; i < 300; i++)
        {
            // Нечётная мантисса с двумя-тремя двоичными знаками дроби: точная
            // запись на цифру длиннее кратчайшей и кончается пятёркой, и две
            // кратчайшие отстоят от числа одинаково.
            long mantissa = (1L << 52) | (long)(Next() & ((1UL << 52) - 1)) | 1;
            double value = mantissa / Math.Pow(2, Below(3) + 2);
            yield return Below(2) == 0 ? value : -value;
        }
        for (int e = -30; e <= 30; e++)
        {
            double power = Math.Pow(10, e);
            yield return power;
            yield return Math.BitIncrement(power);
            yield return Math.BitDecrement(power);
        }
    }

    private static IEnumerable<float> SingleValues()
    {
        float[] fixedValues =
        {
            0f, -0f, float.NaN, float.PositiveInfinity, float.NegativeInfinity, float.MaxValue, float.MinValue,
            float.Epsilon, 0.1f, 1f / 3, 16777216f, 1e10f, 1.5f, 2.5f, 0.125f, 3.14159265f, 123456.789f, -0.4f, 1e-7f,
        };
        foreach (float value in fixedValues)
        {
            yield return value;
        }
        for (int i = 0; i < 400; i++)
        {
            yield return BitConverter.Int32BitsToSingle((int)Next());
        }
        for (int i = 0; i < 300; i++)
        {
            float value = (float)((long)(Next() % 10_000_000UL) / Math.Pow(10, Below(10)));
            yield return Below(2) == 0 ? value : -value;
        }
        for (int i = 0; i < 300; i++)
        {
            int mantissa = (1 << 23) | (int)(Next() & ((1UL << 23) - 1)) | 1;
            float value = mantissa / (float)Math.Pow(2, Below(3) + 2);
            yield return Below(2) == 0 ? value : -value;
        }
    }

    private static IEnumerable<(int, Func<string, string>, string)> IntegerValues()
    {
        var values = new List<long>
        {
            0, 1, -1, 5, 45, 95, 995, 1000, 1234567, -1234567, 999999, int.MaxValue, int.MinValue, long.MaxValue,
            long.MinValue, 1_000_000_000_000_000_000, 5_000_000_000,
        };
        for (int i = 0; i < 120; i++)
        {
            long value = (long)(Next() >> Below(64));
            values.Add(Below(2) == 0 ? value : -value);
        }
        for (int k = 0; k < 19; k++)
        {
            long power = (long)Math.Pow(10, k);
            values.Add(power);
            values.Add(power - 1);
            values.Add(power * 5);
        }
        foreach (long value in values)
        {
            yield return (8, f => ((sbyte)value).ToString(f, Invariant), ((sbyte)value).ToString(Invariant));
            yield return (8, f => ((byte)value).ToString(f, Invariant), ((byte)value).ToString(Invariant));
            yield return (16, f => ((short)value).ToString(f, Invariant), ((short)value).ToString(Invariant));
            yield return (16, f => ((ushort)value).ToString(f, Invariant), ((ushort)value).ToString(Invariant));
            yield return (32, f => ((int)value).ToString(f, Invariant), ((int)value).ToString(Invariant));
            yield return (32, f => ((uint)value).ToString(f, Invariant), ((uint)value).ToString(Invariant));
            yield return (64, f => value.ToString(f, Invariant), value.ToString(Invariant));
            yield return (64, f => ((ulong)value).ToString(f, Invariant), ((ulong)value).ToString(Invariant));
        }
    }

    private static IEnumerable<string> ParseInputs()
    {
        string[] fixedInputs =
        {
            "", " ", "1", "-", "+", ".", "-.", "1.", ".1", "1e", "1e+", "1e-5", "1E5", "1,000", ",1", "1,,2", "1.2,3",
            "1 ", " 1", "- 1", "+-1", "--1", "1-", "(1)", "$1", "1e1000", "1e-1000", "-1e-1000", "0e99999999999",
            "1e2147483648", "1e100000000", "1e99999999", "-0", "-0.0", "+0", "0.000", "NaN", "nan", "+NaN", "-NaN",
            "Infinity", "infinity", "+Infinity", "-Infinity", "-infinity", " NaN ", "∞", "Inf", "1\0", "1\0\0",
            "\t1\n", "1 ", " NaN", "0x1", "1_000", "1d", "1f", "١", "9007199254740993",
            "2.2250738585072011e-308", "4.9406564584124654e-324", "2.4703282292062327e-324", "2.4703282292062328e-324",
            "1.7976931348623158e308", "1.7976931348623159e308", "3.4028235677973366e38", "3.4028236e38",
            "1.401298464324817e-45", "7.006492321624085e-46", "7.006492321624086e-46", "1e 5", "1e5 ", "1 e5",
            "00000000000000000000001", "0.00000000000000000000000000000000000000000000000000000000000000000000001",
            "1,2.3e4", "1.2.3", "e5", ".e5", "-.5", "+.5e-0", "1e+05", "NaN1", "Infinityx",
        };
        foreach (string input in fixedInputs)
        {
            yield return input;
        }
        yield return new string('9', 400);
        yield return new string('1', 800) + "e-800";
        yield return "0." + new string('0', 400) + "1e400";
        yield return "1" + new string('0', 330);
        string[] printFormats = { "R", "E16", "F20", "G5", "N3", "0.###E+0", "E3", "G17", "F2" };
        for (int i = 0; i < 1500; i++)
        {
            double value = Below(2) == 0
                ? BitConverter.Int64BitsToDouble((long)Next())
                : (long)(Next() % 10_000_000_000UL) / Math.Pow(10, Below(25));
            string text = value.ToString(printFormats[Below(printFormats.Length)], Invariant);
            switch (Below(8))
            {
                case 0: text = "  " + text + " "; break;
                case 1: text = "+" + text; break;
                case 2: text = text.ToLowerInvariant(); break;
                case 3: text = "000" + text; break;
                case 4: text = text + "0x"[Below(2)]; break;
                case 5: text = text.Insert(Below(text.Length + 1), ","); break;
            }
            yield return text;
        }
    }
}
