// Часы, сон и окружение программы (фаза N5b): Environment, Stopwatch,
// Thread.Sleep. Всё, что знает только машина, — члены `Clock`, их выполняет
// среда, спрашивая хост.

using System.Runtime.CompilerServices;

namespace System
{
    internal static class Clock
    {
        // Время суток: тики от начала эпохи Unix, UTC.
        [MethodImpl(MethodImplOptions.InternalCall)]
        internal static extern long UtcTicks();

        // Смещение местного времени от UTC, минуты.
        [MethodImpl(MethodImplOptions.InternalCall)]
        internal static extern int LocalOffsetMinutes();

        // Монотонные часы в тиках от произвольной точки.
        [MethodImpl(MethodImplOptions.InternalCall)]
        internal static extern long MonotonicTicks();

        // -1 — спать без конца.
        [MethodImpl(MethodImplOptions.InternalCall)]
        internal static extern void Sleep(int milliseconds);

        [MethodImpl(MethodImplOptions.InternalCall)]
        internal static extern int ProcessorCount();

        // Не возвращается: среда заканчивает программу, минуя finally.
        [MethodImpl(MethodImplOptions.InternalCall)]
        internal static extern void Exit(int exitCode);

        [MethodImpl(MethodImplOptions.InternalCall)]
        internal static extern string[] CommandLine();
    }

    public static class Environment
    {
        // Поток один (веха v0.7c).
        public static int CurrentManagedThreadId => 1;

        // Перевод строки FreeOS — LF.
        public static string NewLine => "\n";

        public static long TickCount64 => Clock.MonotonicTicks() / TimeSpan.TicksPerMillisecond;

        public static int TickCount => (int)TickCount64;

        public static int ProcessorCount => Clock.ProcessorCount();

        public static bool Is64BitProcess => true;

        public static bool Is64BitOperatingSystem => true;

        public static string CurrentDirectory => IO.Directory.GetCurrentDirectory();

        // Первый элемент — путь к сборке, как у `dotnet app.dll`.
        public static string[] GetCommandLineArgs() => Clock.CommandLine();

        public static string CommandLine => string.Join(" ", GetCommandLineArgs());

        public static void Exit(int exitCode) => Clock.Exit(exitCode);
    }
}

namespace System.Diagnostics
{
    // Отметка времени — тик по 100 нс, как у Windows: тогда `Elapsed` получается
    // из отметок умножением на ровно единицу, без дробной ошибки.
    public class Stopwatch
    {
        public static readonly long Frequency = TimeSpan.TicksPerSecond;
        public static readonly bool IsHighResolution = true;

        private long elapsed;
        private long started;
        private bool running;

        public static Stopwatch StartNew()
        {
            var watch = new Stopwatch();
            watch.Start();
            return watch;
        }

        public bool IsRunning => running;

        public TimeSpan Elapsed => new TimeSpan(RawElapsed());

        public long ElapsedMilliseconds => RawElapsed() / TimeSpan.TicksPerMillisecond;

        public long ElapsedTicks => RawElapsed();

        public void Start()
        {
            if (!running)
            {
                started = GetTimestamp();
                running = true;
            }
        }

        public void Stop()
        {
            if (running)
            {
                elapsed += GetTimestamp() - started;
                running = false;
                if (elapsed < 0)
                {
                    elapsed = 0;
                }
            }
        }

        public void Reset()
        {
            elapsed = 0;
            started = 0;
            running = false;
        }

        public void Restart()
        {
            elapsed = 0;
            started = GetTimestamp();
            running = true;
        }

        private long RawElapsed()
        {
            long total = elapsed;
            if (running)
            {
                total += GetTimestamp() - started;
            }
            return total;
        }

        public static long GetTimestamp() => Clock.MonotonicTicks();

        public static TimeSpan GetElapsedTime(long startingTimestamp) => GetElapsedTime(startingTimestamp, GetTimestamp());

        public static TimeSpan GetElapsedTime(long startingTimestamp, long endingTimestamp) => new TimeSpan(endingTimestamp - startingTimestamp);

        public override string ToString() => Elapsed.ToString();
    }
}

namespace System.Threading
{
    public static class Timeout
    {
        public const int Infinite = -1;
        public static readonly TimeSpan InfiniteTimeSpan = new TimeSpan(-10000);
    }

    public sealed class Thread
    {
        private Thread()
        {
        }

        public static void Sleep(int millisecondsTimeout)
        {
            if (millisecondsTimeout < -1)
            {
                throw new ArgumentOutOfRangeException("millisecondsTimeout", "Number must be either non-negative and less than or equal to Int32.MaxValue or -1.");
            }
            Clock.Sleep(millisecondsTimeout);
        }

        public static void Sleep(TimeSpan timeout)
        {
            long milliseconds = (long)timeout.TotalMilliseconds;
            if (milliseconds < -1 || milliseconds > int.MaxValue)
            {
                throw new ArgumentOutOfRangeException("timeout", "Number must be either non-negative and less than or equal to Int32.MaxValue or -1.");
            }
            Sleep((int)milliseconds);
        }
    }
}
