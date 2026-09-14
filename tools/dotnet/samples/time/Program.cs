// Образец для фазы N5b: время и окружение — DateTime и его форматы, разбор,
// TimeSpan, Stopwatch, Thread.Sleep и Environment.
//
// Всё, что зависит от мига запуска, часового пояса и железа машины, печатается
// проверкой (`True`), а не значением: сравнивать значение было бы не с чем.
// Остальное — от фиксированных дат, и совпасть с dotnet обязано до символа.

using System.Diagnostics;
using System.Globalization;
using System.Text;

namespace FreeOs.Samples.Time;

public static class Program
{
    public static int Main(string[] args)
    {
        Console.OutputEncoding = Encoding.UTF8;
        Console.WriteLine("time: start");

        // Дата из частей и её поля.
        var moment = new DateTime(2026, 9, 14, 21, 5, 7, 123);
        Console.WriteLine(moment.Ticks + " " + moment.Year + "-" + moment.Month + "-" + moment.Day + " " + moment.DayOfWeek + " "
            + moment.DayOfYear + " " + moment.Kind);
        Console.WriteLine(moment.Hour + ":" + moment.Minute + ":" + moment.Second + "." + moment.Millisecond + " " + moment.TimeOfDay
            + " " + moment.Date.ToString("s"));
        Console.WriteLine(moment);

        // Стандартные форматы инвариантной культуры.
        foreach (string format in new[] { "d", "D", "f", "F", "g", "G", "m", "o", "r", "s", "t", "T", "u", "y" })
        {
            Console.WriteLine(format + ": " + moment.ToString(format));
        }

        // Пользовательские форматы.
        Console.WriteLine(moment.ToString("dddd, d MMMM yyyy 'at' h:mm tt") + " | " + moment.ToString("yy/M/d HH:mm:ss.fff"));
        Console.WriteLine(moment.AddMilliseconds(-3).ToString("ddd MMM %d ffff FFFF gg \\h\\e\\y \"q\" %H") + " | "
            + moment.AddHours(-12).ToString("hh:mm:ss t yyyyy"));
        Console.WriteLine($"{moment:HH:mm} {moment,22:yyyy-MM-dd} {new DateTime(5, 1, 2):yyy yy y M dd}");
        Console.WriteLine(DateTime.SpecifyKind(moment, DateTimeKind.Utc).ToString("o") + " " + new DateTime(2026, 1, 1, 0, 0, 0, DateTimeKind.Utc).ToString("K|u|r"));

        // Арифметика и сравнение.
        Console.WriteLine(new DateTime(2024, 1, 31).AddMonths(1).ToString("yyyy-MM-dd") + " " + new DateTime(2024, 2, 29).AddYears(1).ToString("yyyy-MM-dd")
            + " " + moment.AddDays(100.5).ToString("o") + " " + moment.AddHours(-30).AddMinutes(90).AddSeconds(3600.25).AddTicks(5).ToString("o"));
        Console.WriteLine(DateTime.IsLeapYear(1900) + " " + DateTime.IsLeapYear(2000) + " " + DateTime.IsLeapYear(2024) + " "
            + DateTime.DaysInMonth(2026, 2) + " " + DateTime.DaysInMonth(2024, 2) + " " + new DateTime(2000, 1, 1).DayOfWeek);
        TimeSpan untilNewYear = new DateTime(2027, 1, 1) - moment;
        Console.WriteLine(untilNewYear + " " + moment.Subtract(TimeSpan.FromMinutes(5)).ToString("T") + " " + (moment < moment.AddTicks(1)) + " "
            + (moment == new DateTime(moment.Ticks)) + " " + moment.CompareTo(new DateTime(2026, 1, 1)) + " " + moment.Equals(moment.Date));
        Console.WriteLine(DateTime.MinValue.ToString("o") + " " + DateTime.MaxValue.Ticks + " " + DateTime.MaxValue.ToString("o"));

        // Разбор.
        Console.WriteLine(DateTime.Parse("2026-09-14T21:05:07.1234567").ToString("o") + " " + DateTime.Parse("09/14/2026 21:05").ToString("o") + " "
            + DateTime.Parse("2026-09-14").ToString("o"));
        Console.WriteLine(DateTime.ParseExact("14.09.2026 07:05", "dd.MM.yyyy HH:mm", CultureInfo.InvariantCulture).ToString("o") + " "
            + DateTime.TryParse("2026-02-30", out DateTime bad) + " " + bad.Ticks + " "
            + DateTime.TryParseExact("20260914", "yyyyMMdd", CultureInfo.InvariantCulture, DateTimeStyles.None, out DateTime compact) + " "
            + compact.ToString("D"));
        try
        {
            _ = new DateTime(2026, 2, 30);
        }
        catch (ArgumentOutOfRangeException error)
        {
            Console.WriteLine("ctor: " + error.GetType().Name);
        }
        try
        {
            _ = DateTime.Parse("not a date");
        }
        catch (FormatException error)
        {
            Console.WriteLine("parse: " + error.GetType().Name);
        }

        // TimeSpan.
        var span = new TimeSpan(1, 2, 3, 4, 567);
        Console.WriteLine(span + " " + span.Ticks + " " + span.Days + " " + span.Hours + " " + span.Minutes + " " + span.Seconds + " "
            + span.Milliseconds + " " + span.TotalHours + " " + span.TotalMinutes + " " + span.TotalMilliseconds);
        Console.WriteLine(span.ToString("g") + " " + span.ToString("G") + " " + (-span).ToString("c") + " " + span.ToString(@"d\.hh\:mm\:ss\.fff")
            + " " + span.ToString("%h") + " " + (-span).ToString("g"));
        Console.WriteLine(TimeSpan.FromSeconds(90.5) + " " + TimeSpan.FromMilliseconds(1500) + " " + TimeSpan.FromMinutes(-2.25) + " " + TimeSpan.Zero
            + " " + TimeSpan.MaxValue + " " + TimeSpan.MinValue + " " + TimeSpan.FromHours(36) + " " + TimeSpan.FromDays(2));
        Console.WriteLine(TimeSpan.Parse("1.02:03:04.5") + " " + TimeSpan.Parse("-00:00:30") + " " + TimeSpan.Parse("12:34") + " "
            + TimeSpan.TryParse("25:00", out TimeSpan wrong) + " " + TimeSpan.Parse("5") + " "
            + TimeSpan.ParseExact("3:04", @"h\:mm", CultureInfo.InvariantCulture));
        Console.WriteLine((span + TimeSpan.FromHours(1)) + " " + (span - TimeSpan.FromDays(3)) + " " + (span * 2) + " " + (span / 4) + " "
            + (span / TimeSpan.FromHours(1)) + " " + (-span).Duration() + " " + (span > TimeSpan.FromDays(1)) + " "
            + TimeSpan.Compare(span, span) + " " + span.Equals(new TimeSpan(span.Ticks)));
        try
        {
            _ = TimeSpan.FromDays(1e9);
        }
        catch (OverflowException error)
        {
            Console.WriteLine("span: " + error.GetType().Name + " " + wrong.Ticks);
        }

        // Часы: миг запуска и часовой пояс заранее не известны.
        DateTime utc = DateTime.UtcNow;
        DateTime local = DateTime.Now;
        Console.WriteLine("now: " + (utc.Year >= 2026) + " " + utc.Kind + " " + local.Kind + " " + (Math.Abs((local - utc).TotalHours) <= 14) + " "
            + (DateTime.Today == local.Date) + " " + DateTime.Today.Kind + " " + (Math.Abs((utc.ToLocalTime() - local).TotalSeconds) < 5) + " "
            + (Math.Abs((local.ToUniversalTime() - utc).TotalSeconds) < 5) + " " + local.ToUniversalTime().Kind);

        // Секундомер и сон.
        long tick0 = Environment.TickCount64;
        int tick32 = Environment.TickCount;
        var watch = Stopwatch.StartNew();
        long stamp = Stopwatch.GetTimestamp();
        Thread.Sleep(40);
        Thread.Sleep(TimeSpan.FromMilliseconds(30));
        watch.Stop();
        TimeSpan frozen = watch.Elapsed;
        Thread.Sleep(15);
        Console.WriteLine("slept: " + (frozen.TotalMilliseconds >= 60) + " " + (watch.ElapsedMilliseconds >= 60) + " "
            + (Environment.TickCount64 - tick0 >= 60) + " " + (Environment.TickCount - tick32 >= 60) + " "
            + (Stopwatch.GetElapsedTime(stamp).TotalMilliseconds >= 60) + " " + watch.IsRunning + " " + (frozen == watch.Elapsed) + " "
            + (watch.ElapsedTicks * TimeSpan.TicksPerSecond / Stopwatch.Frequency == frozen.Ticks) + " " + Stopwatch.IsHighResolution);
        watch.Reset();
        Console.WriteLine("reset: " + watch.Elapsed + " " + watch.IsRunning);
        watch.Restart();
        Console.WriteLine("restart: " + watch.IsRunning);
        try
        {
            Thread.Sleep(-5);
        }
        catch (ArgumentOutOfRangeException error)
        {
            Console.WriteLine("sleep: " + error.GetType().Name);
        }

        // Окружение.
        Console.WriteLine("args: " + args.Length + " [" + string.Join("|", args) + "]");
        string[] line = Environment.GetCommandLineArgs();
        Console.WriteLine("command line: " + line.Length + " " + Path.GetFileName(line[0]) + " " + string.Join("|", line, 1, line.Length - 1));
        Console.WriteLine("machine: " + (Environment.ProcessorCount >= 1) + " " + Environment.Is64BitProcess);

        // Выход мимо `finally` и мимо `return`.
        Console.WriteLine("time: done");
        try
        {
            Environment.Exit(args.Length + 19);
        }
        finally
        {
            Console.WriteLine("finally after Environment.Exit must not run");
        }
        return 99;
    }
}
