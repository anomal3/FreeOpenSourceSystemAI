// Образец для фазы N11: async/await на однопоточной очереди задач.
// Под настоящим dotnet задачи идут на пуле потоков, поэтому всё, что печатается,
// упорядочено задержками не меньше 50 мс или последовательными await, а
// общих изменяемых данных у одновременно идущих задач нет.

using System.Diagnostics;

namespace FreeOs.Samples.Asyncs;

public static class Program
{
    private static readonly object gate = new object();
    private static int locked;

    private static async Task<string> Fails(Func<Task> action)
    {
        try
        {
            await action();
            return "no exception";
        }
        catch (Exception e)
        {
            return e.GetType().Name + ": " + e.Message;
        }
    }

    private static string FailsNow(Action action)
    {
        try
        {
            action();
            return "no exception";
        }
        catch (Exception e)
        {
            return e.GetType().Name + ": " + e.Message;
        }
    }

    // Печатает до первого await и после него: видно, что начало идёт
    // синхронно, а конец — уже продолжением.
    private static async Task<int> Compute(string name, int delay)
    {
        Console.WriteLine(name + ": begin");
        await Task.Delay(delay);
        Console.WriteLine(name + ": end");
        return delay;
    }

    private static async Task<int> Worker(int n, int delay)
    {
        await Task.Delay(delay).ConfigureAwait(false);
        Console.WriteLine("worker " + n + " done");
        return n * n;
    }

    // Задержки у падающих задач разные: под dotnet две задачи с одной задержкой
    // падают в случайном порядке, и WhenAll собирает ошибки в этом порядке.
    private static async Task Boom(string text, int delay = 10)
    {
        await Task.Delay(delay);
        throw new InvalidOperationException(text);
    }

    private static async Task ThrowsEarly()
    {
        throw new ArgumentException("early");
#pragma warning disable CS0162
        await Task.Delay(10);
#pragma warning restore CS0162
    }

    private static async ValueTask<int> Twice(int x)
    {
        await Task.Yield();
        return x * 2;
    }

    private static ValueTask<int> Ready(int x) => new ValueTask<int>(x);

    private static async Task Cancellable(CancellationToken token)
    {
        for (int i = 0; i < 100; i++)
        {
            token.ThrowIfCancellationRequested();
            await Task.Delay(10, token);
        }
    }

    public static async Task<int> Main()
    {
        Console.WriteLine("asyncs: start");

        // Синхронная часть и продолжение.
        Task<int> first = Compute("first", 20);
        Console.WriteLine("started: " + first.IsCompleted);
        int a = await first;
        Console.WriteLine("first: " + a + " " + first.Status + " " + first.IsCompletedSuccessfully + " " + first.IsCompleted);

        // Задержка измеряется.
        var watch = Stopwatch.StartNew();
        await Task.Delay(50);
        Console.WriteLine("delay: " + (watch.ElapsedMilliseconds >= 40) + " " + Task.CompletedTask.IsCompleted + " " + Task.Delay(0).IsCompleted);

        // Несколько задач разом: порядок завершения задают задержки.
        int[] squares = await Task.WhenAll(Worker(1, 150), Worker(2, 50), Worker(3, 100));
        Console.WriteLine("all: " + string.Join(",", squares));
        Task<int> slow = Task.Delay(300).ContinueWith(_ => 0);
        Task<int> quick = Compute("quick", 30);
        Task<int> winner = await Task.WhenAny(slow, quick);
        Console.WriteLine("any: " + (winner == quick) + " " + winner.Result + " " + slow.IsCompleted);
        await slow;

        // Task.Run: та же очередь, результат приходит через await.
        int sum = await Task.Run(() =>
        {
            int s = 0;
            for (int i = 1; i <= 100; i++)
            {
                s += i;
            }
            return s;
        });
        await Task.Run(async () => await Task.Delay(10));
        Console.WriteLine("run: " + sum);

        // Исключения: await отдаёт исключение как есть, Result и Wait — в обёртке.
        Console.WriteLine("boom: " + await Fails(() => Boom("boom")));
        Task faulted = Boom("wrapped");
        await Fails(() => faulted);
        Console.WriteLine("wrapped: " + FailsNow(() => faulted.Wait()) + " | " + faulted.Status + " " + faulted.IsFaulted + " " + faulted.Exception!.InnerExceptions.Count + " " + faulted.Exception.InnerException!.Message);
        Task early = ThrowsEarly();
        Console.WriteLine("early: " + early.IsFaulted + " " + await Fails(() => early));
        Task both = Task.WhenAll(Boom("one", 10), Boom("two", 60));
        Console.WriteLine("both: " + await Fails(() => both) + " | " + both.Exception!.InnerExceptions.Count + " " + FailsNow(() => both.Wait()));
        Console.WriteLine("from: " + await Fails(() => Task.FromException(new NotSupportedException("made"))) + " | " + await Task.FromResult(7));

        // Отмена.
        var source = new CancellationTokenSource();
        Task delayed = Task.Delay(1000, source.Token);
        source.Cancel();
        Console.WriteLine("cancel: " + await Fails(() => delayed) + " | " + delayed.Status + " " + delayed.IsCanceled + " " + source.IsCancellationRequested);
        var timed = new CancellationTokenSource();
        timed.CancelAfter(40);
        Console.WriteLine("timed: " + await Fails(() => Cancellable(timed.Token)) + " | " + FailsNow(() => timed.Token.ThrowIfCancellationRequested()) + " " + CancellationToken.None.CanBeCanceled);

        // ValueTask и Task.Yield.
        Console.WriteLine("value: " + await Twice(21) + " " + await Ready(5) + " " + Ready(6).IsCompletedSuccessfully);
        await Task.Yield();

        // TaskCompletionSource, ContinueWith, async-лямбда.
        var completion = new TaskCompletionSource<string>();
        _ = Task.Delay(20).ContinueWith(_ => completion.SetResult("signalled"));
        Console.WriteLine("completion: " + await completion.Task + " " + completion.Task.Status);
        int plusOne = await Task.FromResult(3).ContinueWith(t => t.Result + 1);
        Func<int, Task<int>> add = async x =>
        {
            await Task.Delay(10);
            return x + 1;
        };
        Console.WriteLine("continue: " + plusOne + " " + await add(41));

        // lock — Monitor.
        for (int i = 0; i < 3; i++)
        {
            lock (gate)
            {
                locked++;
            }
        }
        Console.WriteLine("lock: " + locked);

        Console.WriteLine("asyncs: done");
        return 30;
    }
}
