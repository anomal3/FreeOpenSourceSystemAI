// Образец для фазы N4d: перечисления — печать по имени и по флагам, форматы,
// разбор, списки имён и значений, HasFlag, перечисление ключом словаря.

namespace FreeOs.Samples.Enums;

public enum Color
{
    Red,
    Green = 5,
    Blue,
}

[Flags]
public enum Access
{
    None = 0,
    Read = 1,
    Write = 2,
    Execute = 4,
    ReadWrite = Read | Write,
    All = 7,
}

[Flags]
public enum Style
{
    Bold = 1,
    Italic = 2,
    Underline = 4,
}

public enum Level : byte
{
    Low = 1,
    High = 200,
}

public enum Big : long
{
    Min = long.MinValue,
    Max = long.MaxValue,
}

public enum Signed : sbyte
{
    Negative = -1,
    Zero = 0,
}

public static class Program
{
    public static int Main()
    {
        Console.WriteLine("enums: start");

        Color color = Pick(Color.Blue);
        Access access = Access.Read | Pick(Access.Execute);
        Console.WriteLine(color + " " + Color.Red + " " + (Color)5 + " " + (Color)42 + " " + (int)color);
        Console.WriteLine(access + " | " + Access.ReadWrite + " | " + Access.All + " | " + (Access)0 + " | " + (Access)8 + " | "
            + (Access)9);
        Console.WriteLine((Style)0 + " " + (Style)3 + " " + (Style)8 + " " + Level.High + " " + (Level)7 + " " + Big.Min + " "
            + Signed.Negative + " " + (Signed)(-5));
        Console.WriteLine($"{color} {color:D} {color:G} {color:X} {access:F} {access:D} {Level.High:X} {Signed.Negative:X} {(Style)3:G}");

        Console.WriteLine(Enum.Parse<Color>("Green") + " " + Enum.Parse<Access>("Read, Write") + " "
            + Enum.TryParse<Color>("purple", out Color missing) + " " + missing + " " + Enum.TryParse("blue", true, out Color blue) + " "
            + blue + " " + Enum.Parse<Color>("6") + " " + Enum.Parse<Level>(" High "));
        Console.WriteLine(string.Join(",", Enum.GetNames<Color>()) + " " + string.Join(",", Enum.GetValues<Access>()) + " "
            + Enum.IsDefined(Color.Green) + " " + Enum.IsDefined((Color)3) + " " + Enum.GetName(Color.Green));
        Console.WriteLine(access.HasFlag(Access.Read) + " " + access.HasFlag(Access.Write) + " " + color.Equals(Color.Blue) + " "
            + color.CompareTo(Color.Red) + " " + (color == Color.Blue) + " " + typeof(Color).Name + " " + color.GetType().FullName);
        try
        {
            Enum.Parse<Color>("nope");
        }
        catch (ArgumentException e)
        {
            Console.WriteLine(e.Message);
        }

        var byColor = new Dictionary<Color, int> { [Color.Red] = 1, [Color.Blue] = 3 };
        object boxed = Color.Green;
        Console.WriteLine(byColor[Color.Blue] + " " + byColor.ContainsKey(Color.Green) + " " + boxed + " " + (boxed is Color) + " "
            + Describe(color) + " " + Describe(Color.Red));

        Console.WriteLine("enums: done");
        return (int)Color.Blue;
    }

    // Через вызов — чтобы компилятор не свернул выражение в константу.
    private static T Pick<T>(T value) => value;

    private static string Describe(Color color) => color switch
    {
        Color.Red => "warm",
        Color.Blue => "cold",
        _ => "other",
    };
}
