// Образец для фазы N4a. Каждая строка — горсть методов строк, форматирования
// или разбора, которыми пользуются обычные программы.
//
// Эталон снимается с dotnet в режиме инвариантной глобализации
// (`DOTNET_SYSTEM_GLOBALIZATION_INVARIANT=1`, так запускает `clr-check`): у
// FreeOS культур нет, и сравнение строк там порядковое, как здесь.
//
// Чего здесь нет намеренно: дробных чисел (N4c), `string.Join` над
// коллекцией (N4b), формата `N` с разделителем разрядов (N4c).

using System.Text;

namespace FreeOs.Samples.Text;

public static class Program
{
    public static int Main()
    {
        Console.OutputEncoding = Encoding.UTF8;
        Console.WriteLine("text: start");

        string padded = "  Hello, FreeOS world  ";
        string text = padded.Trim();
        Console.WriteLine("[" + text + "] " + text.Length + " " + padded.TrimStart().Length + " " + padded.TrimEnd().Length);
        Console.WriteLine(text.ToUpper() + " " + text.ToLower() + " " + "Привет, Мир".ToUpper());
        Console.WriteLine(text.Substring(7) + "|" + text.Substring(0, 5) + "|" + text.IndexOf("FreeOS") + "|"
            + text.IndexOf('o') + "|" + text.LastIndexOf('o') + "|" + text.IndexOf("nope"));
        Console.WriteLine(text.Contains("world") + " " + text.StartsWith("Hello") + " " + text.EndsWith("d") + " "
            + text.Contains('!'));

        string[] parts = "a,b,,c".Split(',');
        Console.WriteLine(parts.Length + " [" + string.Join("|", parts) + "] " + string.Concat(parts));
        Console.WriteLine("abcabc".Replace("b", "XY") + " " + "hello".Replace('l', 'L') + " " + "x".PadLeft(4, '.')
            + " " + "y".PadRight(3) + "|");
        Console.WriteLine(new string('-', 6) + new string(new[] { 'o', 'k' }) + " " + "abc".ToCharArray().Length
            + " " + "line".Insert(2, "__") + " " + "remove".Remove(1, 3));
        Console.WriteLine(string.IsNullOrWhiteSpace("  \t") + " " + string.Equals("abc", "ABC", StringComparison.OrdinalIgnoreCase)
            + " " + string.Compare("apple", "banana") + " " + "b".CompareTo("a"));

        // Форматирование целых.
        Console.WriteLine(string.Format("{0} + {1} = {2}", 2, 3, 5) + " " + string.Format("[{0,4}|{1,-4}]", 7, 8));
        Console.WriteLine($"{42,5}|{-7,-4}|{255:X}|{255:x4}|{3:D3}|{-12:D4}|{int.MinValue}|{ulong.MaxValue}");
        Console.WriteLine(255.ToString("X8") + " " + 1000.ToString() + " " + (-5L).ToString("D3") + " " + ((byte)200).ToString("x"));

        // StringBuilder.
        var builder = new StringBuilder();
        builder.Append("x=").Append(10).Append(',').Append(true).Append(' ').Append(-3L);
        builder.Insert(0, ">>");
        builder.Replace("x", "y");
        builder[2] = 'Y';
        Console.WriteLine(builder.ToString() + " " + builder.Length + " " + builder[3]);
        builder.Clear().Append("ok");
        Console.WriteLine(builder + " " + builder.Length);

        // Разбор.
        int parsed = int.Parse("-123");
        bool good = int.TryParse("12a", out int bad);
        long big = long.Parse("9000000000");
        Console.WriteLine(parsed + " " + good + " " + bad + " " + big + " " + int.TryParse(" 77 ", out int spaced) + " " + spaced);
        try
        {
            int.Parse("x");
        }
        catch (FormatException error)
        {
            Console.WriteLine("format: " + error.Message);
        }

        // Math и char.
        Console.WriteLine(Math.Max(3, 7) + " " + Math.Min(-2L, 5L) + " " + Math.Abs(-9) + " " + Math.Clamp(15, 0, 10)
            + " " + Math.Sign(-4));
        char letter = 'ж';
        Console.WriteLine(char.IsLetter(letter) + " " + char.IsDigit('7') + " " + char.ToUpper(letter) + " " + (int)'A'
            + " " + char.IsWhiteSpace(' ') + " " + char.IsUpper('Q'));

        Console.WriteLine("text: done");
        return parsed + 200;
    }
}
