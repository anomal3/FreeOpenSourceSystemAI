// Задачи и async/await (фаза N11).
//
// Компилятор превращает `async` в конечный автомат: структуру с MoveNext,
// «строителем» (AsyncTaskMethodBuilder) и полями для ожидающих. Среде нужны
// только имена типов и их договор — сама машина состояний уже в программе.
// Поток у среды один (веха v0.7c), поэтому «выполнить позже» значит «положить
// в очередь», а не «на другой поток»: очередь готовых продолжений и список
// таймеров живут в AsyncPump, и крутит её тот, кто ждёт, — Wait/Result/
// GetResult у не завершённой задачи или цикл сообщений WinForms между
// событиями. Порядок продолжений — очередь, а не «синхронно при завершении»:
// у .NET на одном потоке с пулом это неразличимо в том, что печатает программа.
//
// Чего здесь нет: потоков (`Task.Run` — та же очередь, а не пул), контекстов
// синхронизации (`ConfigureAwait` ничего не меняет), отмены с обратным вызовом
// из другого потока. Ожидание задачи, которую некому завершить (ни очереди, ни
// таймеров), — не зависание, а исключение: на одном потоке это единственный
// честный ответ.

using System.Collections.Generic;
using System.Runtime.CompilerServices;
using System.Threading;
using System.Threading.Tasks;

namespace System.Threading.Tasks
{
    public enum TaskStatus
    {
        Created = 0,
        WaitingForActivation = 1,
        WaitingToRun = 2,
        Running = 3,
        WaitingForChildrenToComplete = 4,
        RanToCompletion = 5,
        Canceled = 6,
        Faulted = 7,
    }

    // Очередь готовых продолжений и таймеры. Всё, что «выполнится позже».
    internal static class AsyncPump
    {
        private static readonly Queue<Action> ready = new Queue<Action>();
        private static readonly List<Delayed> timers = new List<Delayed>();

        private sealed class Delayed
        {
            internal long Due;
            internal Action Action;
        }

        internal static long NowMs() => Diagnostics.Stopwatch.GetTimestamp() / 10000;

        internal static void Post(Action action) => ready.Enqueue(action);

        internal static void PostAt(long dueMs, Action action) => timers.Add(new Delayed { Due = dueMs, Action = action });

        // Один оборот: все готовые продолжения и подошедшие таймеры. `false` —
        // делать было нечего.
        internal static bool RunOnce()
        {
            bool busy = false;
            while (ready.Count > 0)
            {
                ready.Dequeue()();
                busy = true;
            }
            if (timers.Count > 0)
            {
                long now = NowMs();
                for (int i = 0; i < timers.Count;)
                {
                    Delayed timer = timers[i];
                    if (timer.Due <= now)
                    {
                        timers.RemoveAt(i);
                        timer.Action();
                        busy = true;
                    }
                    else
                    {
                        i++;
                    }
                }
            }
            return busy;
        }

        // Ждать завершения задачи, крутя очередь. Если очередь пуста и таймеров
        // нет, завершить задачу некому — исключение, а не вечный сон.
        internal static void RunUntil(Task task)
        {
            while (!task.IsCompleted)
            {
                if (RunOnce())
                {
                    continue;
                }
                if (task.IsCompleted)
                {
                    break;
                }
                if (timers.Count == 0)
                {
                    throw new InvalidOperationException("The task cannot complete: nothing is scheduled to complete it, and the runtime has one thread.");
                }
                long wait = long.MaxValue;
                for (int i = 0; i < timers.Count; i++)
                {
                    wait = Math.Min(wait, timers[i].Due - NowMs());
                }
                if (wait > 0)
                {
                    Thread.Sleep((int)Math.Min(wait, int.MaxValue));
                }
            }
        }
    }

    public class Task
    {
        private static Task completed;

        private TaskStatus status = TaskStatus.WaitingForActivation;
        private List<Exception> exceptions;
        private List<Action> continuations;
        // Упакованная машина состояний async-метода: строитель кладёт её сюда
        // при первом ожидании, и все продолжения зовут MoveNext у неё.
        internal IAsyncStateMachine Box;

        internal Task()
        {
        }

        public static Task CompletedTask
        {
            get
            {
                if (completed == null)
                {
                    completed = new Task();
                    completed.status = TaskStatus.RanToCompletion;
                }
                return completed;
            }
        }

        public TaskStatus Status => status;

        public bool IsCompleted => status >= TaskStatus.RanToCompletion;

        public bool IsCompletedSuccessfully => status == TaskStatus.RanToCompletion;

        public bool IsFaulted => status == TaskStatus.Faulted;

        public bool IsCanceled => status == TaskStatus.Canceled;

        public AggregateException Exception => exceptions == null ? null : new AggregateException(exceptions);

        internal Exception FirstException => exceptions == null ? null : exceptions[0];

        internal bool TrySetCompleted()
        {
            if (IsCompleted)
            {
                return false;
            }
            status = TaskStatus.RanToCompletion;
            Finish();
            return true;
        }

        internal bool TrySetFaulted(Exception exception)
        {
            if (IsCompleted)
            {
                return false;
            }
            if (exception is OperationCanceledException && !(exception is AggregateException))
            {
                status = TaskStatus.Canceled;
            }
            else
            {
                status = TaskStatus.Faulted;
            }
            exceptions = new List<Exception>();
            if (exception is AggregateException aggregate)
            {
                exceptions.AddRange(aggregate.InnerExceptions);
            }
            else
            {
                exceptions.Add(exception);
            }
            Finish();
            return true;
        }

        internal bool TrySetCanceled()
        {
            if (IsCompleted)
            {
                return false;
            }
            status = TaskStatus.Canceled;
            exceptions = new List<Exception> { new TaskCanceledException(this) };
            Finish();
            return true;
        }

        private void Finish()
        {
            List<Action> pending = continuations;
            continuations = null;
            if (pending != null)
            {
                for (int i = 0; i < pending.Count; i++)
                {
                    AsyncPump.Post(pending[i]);
                }
            }
        }

        // Продолжение: сразу в очередь, если задача уже завершена.
        internal void OnCompleted(Action continuation)
        {
            if (continuation == null)
            {
                throw new ArgumentNullException("continuation");
            }
            if (IsCompleted)
            {
                AsyncPump.Post(continuation);
                return;
            }
            if (continuations == null)
            {
                continuations = new List<Action>();
            }
            continuations.Add(continuation);
        }

        // Ожидание до конца, как у GetAwaiter().GetResult(): исключение задачи
        // как есть, отмена — TaskCanceledException.
        internal void WaitCore()
        {
            if (!IsCompleted)
            {
                AsyncPump.RunUntil(this);
            }
            if (status == TaskStatus.Faulted)
            {
                throw exceptions[0];
            }
            if (status == TaskStatus.Canceled)
            {
                throw exceptions != null ? exceptions[0] : new TaskCanceledException(this);
            }
        }

        // `Wait()` и `Result` заворачивают исключение в AggregateException.
        public void Wait()
        {
            if (!IsCompleted)
            {
                AsyncPump.RunUntil(this);
            }
            if (status == TaskStatus.Faulted || status == TaskStatus.Canceled)
            {
                throw new AggregateException(exceptions);
            }
        }

        public TaskAwaiter GetAwaiter() => new TaskAwaiter(this);

        public ConfiguredTaskAwaitable ConfigureAwait(bool continueOnCapturedContext) => new ConfiguredTaskAwaitable(this);

        public Task ContinueWith(Action<Task> continuationAction)
        {
            if (continuationAction == null)
            {
                throw new ArgumentNullException("continuationAction");
            }
            var next = new Task();
            OnCompleted(() =>
            {
                try
                {
                    continuationAction(this);
                    next.TrySetCompleted();
                }
                catch (Exception e)
                {
                    next.TrySetFaulted(e);
                }
            });
            return next;
        }

        public Task<TResult> ContinueWith<TResult>(Func<Task, TResult> continuationFunction)
        {
            if (continuationFunction == null)
            {
                throw new ArgumentNullException("continuationFunction");
            }
            var next = new Task<TResult>();
            OnCompleted(() =>
            {
                try
                {
                    next.TrySetResult(continuationFunction(this));
                }
                catch (Exception e)
                {
                    next.TrySetFaulted(e);
                }
            });
            return next;
        }

        public static Task<TResult> FromResult<TResult>(TResult result)
        {
            var task = new Task<TResult>();
            task.TrySetResult(result);
            return task;
        }

        public static Task FromException(Exception exception)
        {
            if (exception == null)
            {
                throw new ArgumentNullException("exception");
            }
            var task = new Task();
            task.TrySetFaulted(exception);
            return task;
        }

        public static Task<TResult> FromException<TResult>(Exception exception)
        {
            if (exception == null)
            {
                throw new ArgumentNullException("exception");
            }
            var task = new Task<TResult>();
            task.TrySetFaulted(exception);
            return task;
        }

        public static Task FromCanceled(CancellationToken cancellationToken)
        {
            var task = new Task();
            task.TrySetCanceled();
            return task;
        }

        public static Task<TResult> FromCanceled<TResult>(CancellationToken cancellationToken)
        {
            var task = new Task<TResult>();
            task.TrySetCanceled();
            return task;
        }

        public static Task Delay(int millisecondsDelay) => Delay(millisecondsDelay, CancellationToken.None);

        public static Task Delay(TimeSpan delay) => Delay(delay, CancellationToken.None);

        public static Task Delay(TimeSpan delay, CancellationToken cancellationToken)
        {
            long total = (long)delay.TotalMilliseconds;
            if (total < -1 || total > int.MaxValue)
            {
                throw new ArgumentOutOfRangeException("delay", "The value needs to translate in milliseconds to -1 (signifying an infinite timeout), 0, or a positive integer less than or equal to the maximum allowed timer duration.");
            }
            return Delay((int)total, cancellationToken);
        }

        public static Task Delay(int millisecondsDelay, CancellationToken cancellationToken)
        {
            if (millisecondsDelay < -1)
            {
                throw new ArgumentOutOfRangeException("millisecondsDelay", "The value needs to be either -1 (signifying an infinite timeout), 0 or a positive integer.");
            }
            if (cancellationToken.IsCancellationRequested)
            {
                return FromCanceled(cancellationToken);
            }
            if (millisecondsDelay == 0)
            {
                return CompletedTask;
            }
            var task = new Task();
            if (millisecondsDelay > 0)
            {
                AsyncPump.PostAt(AsyncPump.NowMs() + millisecondsDelay, () => task.TrySetCompleted());
            }
            if (cancellationToken.CanBeCanceled)
            {
                cancellationToken.Register(() => task.TrySetCanceled());
            }
            return task;
        }

        // «На другом потоке» — в очереди: работа начнётся, когда её дождутся
        // или когда цикл событий доберётся до очереди.
        public static Task Run(Action action)
        {
            if (action == null)
            {
                throw new ArgumentNullException("action");
            }
            var task = new Task();
            AsyncPump.Post(() =>
            {
                try
                {
                    action();
                    task.TrySetCompleted();
                }
                catch (Exception e)
                {
                    task.TrySetFaulted(e);
                }
            });
            return task;
        }

        public static Task Run(Action action, CancellationToken cancellationToken) => Run(action);

        public static Task<TResult> Run<TResult>(Func<TResult> function)
        {
            if (function == null)
            {
                throw new ArgumentNullException("function");
            }
            var task = new Task<TResult>();
            AsyncPump.Post(() =>
            {
                try
                {
                    task.TrySetResult(function());
                }
                catch (Exception e)
                {
                    task.TrySetFaulted(e);
                }
            });
            return task;
        }

        public static Task<TResult> Run<TResult>(Func<TResult> function, CancellationToken cancellationToken) => Run(function);

        public static Task Run(Func<Task> function)
        {
            if (function == null)
            {
                throw new ArgumentNullException("function");
            }
            var task = new Task();
            AsyncPump.Post(() =>
            {
                Task inner;
                try
                {
                    inner = function();
                }
                catch (Exception e)
                {
                    task.TrySetFaulted(e);
                    return;
                }
                if (inner == null)
                {
                    task.TrySetCanceled();
                    return;
                }
                inner.OnCompleted(() => task.CompleteFrom(inner));
            });
            return task;
        }

        public static Task<TResult> Run<TResult>(Func<Task<TResult>> function)
        {
            if (function == null)
            {
                throw new ArgumentNullException("function");
            }
            var task = new Task<TResult>();
            AsyncPump.Post(() =>
            {
                Task<TResult> inner;
                try
                {
                    inner = function();
                }
                catch (Exception e)
                {
                    task.TrySetFaulted(e);
                    return;
                }
                if (inner == null)
                {
                    task.TrySetCanceled();
                    return;
                }
                inner.OnCompleted(() =>
                {
                    if (inner.IsCompletedSuccessfully)
                    {
                        task.TrySetResult(inner.Result);
                    }
                    else
                    {
                        task.CompleteFrom(inner);
                    }
                });
            });
            return task;
        }

        // Перенять исход другой задачи (уже завершённой): успех, ошибку, отмену.
        internal void CompleteFrom(Task other)
        {
            if (other.status == TaskStatus.Faulted)
            {
                TrySetFaulted(new AggregateException(other.exceptions));
            }
            else if (other.status == TaskStatus.Canceled)
            {
                TrySetCanceled();
            }
            else
            {
                TrySetCompleted();
            }
        }

        public static Task WhenAll(params Task[] tasks)
        {
            if (tasks == null)
            {
                throw new ArgumentNullException("tasks");
            }
            return WhenAll((IEnumerable<Task>)tasks);
        }

        // `params ReadOnlySpan<T>` (C# 13): компилятор выбирает эти перегрузки
        // для вызова с перечисленными аргументами и собирает их во встроенный
        // массив (InlineArrays.cs).
        public static Task WhenAll(params ReadOnlySpan<Task> tasks) => WhenAll((IEnumerable<Task>)tasks.ToArray());

        public static Task<TResult[]> WhenAll<TResult>(params ReadOnlySpan<Task<TResult>> tasks) => WhenAll((IEnumerable<Task<TResult>>)tasks.ToArray());

        public static Task<Task> WhenAny(params ReadOnlySpan<Task> tasks) => WhenAny((IEnumerable<Task>)tasks.ToArray());

        public static Task<Task<TResult>> WhenAny<TResult>(params ReadOnlySpan<Task<TResult>> tasks) => WhenAny(tasks.ToArray());

        // Перегрузки на две задачи — у .NET они есть, и компилятор берёт их
        // раньше массива и среза.
        public static Task WhenAll(Task task1, Task task2) => WhenAll(new[] { task1, task2 });

        public static Task<TResult[]> WhenAll<TResult>(Task<TResult> task1, Task<TResult> task2) => WhenAll(new[] { task1, task2 });

        public static Task<Task> WhenAny(Task task1, Task task2) => WhenAny(new[] { task1, task2 });

        public static Task<Task<TResult>> WhenAny<TResult>(Task<TResult> task1, Task<TResult> task2) => WhenAny(new[] { task1, task2 });

        public static Task WhenAll(IEnumerable<Task> tasks)
        {
            if (tasks == null)
            {
                throw new ArgumentNullException("tasks");
            }
            var list = new List<Task>(tasks);
            var all = new Task();
            if (list.Count == 0)
            {
                all.TrySetCompleted();
                return all;
            }
            int remaining = list.Count;
            for (int i = 0; i < list.Count; i++)
            {
                if (list[i] == null)
                {
                    throw new ArgumentException("The tasks argument included a null value.", "tasks");
                }
                list[i].OnCompleted(() =>
                {
                    if (--remaining == 0)
                    {
                        all.CompleteFromAll(list);
                    }
                });
            }
            return all;
        }

        public static Task<TResult[]> WhenAll<TResult>(params Task<TResult>[] tasks)
        {
            if (tasks == null)
            {
                throw new ArgumentNullException("tasks");
            }
            return WhenAll((IEnumerable<Task<TResult>>)tasks);
        }

        public static Task<TResult[]> WhenAll<TResult>(IEnumerable<Task<TResult>> tasks)
        {
            if (tasks == null)
            {
                throw new ArgumentNullException("tasks");
            }
            var list = new List<Task<TResult>>(tasks);
            var all = new Task<TResult[]>();
            if (list.Count == 0)
            {
                all.TrySetResult(new TResult[0]);
                return all;
            }
            int remaining = list.Count;
            for (int i = 0; i < list.Count; i++)
            {
                if (list[i] == null)
                {
                    throw new ArgumentException("The tasks argument included a null value.", "tasks");
                }
                list[i].OnCompleted(() =>
                {
                    if (--remaining == 0)
                    {
                        var plain = new List<Task>(list.Count);
                        for (int j = 0; j < list.Count; j++)
                        {
                            plain.Add(list[j]);
                        }
                        if (!all.CompleteFromAll(plain))
                        {
                            var results = new TResult[list.Count];
                            for (int j = 0; j < list.Count; j++)
                            {
                                results[j] = list[j].Result;
                            }
                            all.TrySetResult(results);
                        }
                    }
                });
            }
            return all;
        }

        // Исход WhenAll: все ошибки вместе, иначе отмена, если была, иначе
        // успех. `true` — задача завершена здесь (ошибкой или отменой).
        internal bool CompleteFromAll(List<Task> tasks)
        {
            List<Exception> errors = null;
            bool canceled = false;
            for (int i = 0; i < tasks.Count; i++)
            {
                if (tasks[i].status == TaskStatus.Faulted)
                {
                    if (errors == null)
                    {
                        errors = new List<Exception>();
                    }
                    errors.AddRange(tasks[i].exceptions);
                }
                else if (tasks[i].status == TaskStatus.Canceled)
                {
                    canceled = true;
                }
            }
            if (errors != null)
            {
                TrySetFaulted(new AggregateException(errors));
                return true;
            }
            if (canceled)
            {
                TrySetCanceled();
                return true;
            }
            if (!(this is IHasResult))
            {
                TrySetCompleted();
            }
            return false;
        }

        public static Task<Task> WhenAny(params Task[] tasks)
        {
            if (tasks == null)
            {
                throw new ArgumentNullException("tasks");
            }
            return WhenAny((IEnumerable<Task>)tasks);
        }

        public static Task<Task> WhenAny(IEnumerable<Task> tasks)
        {
            if (tasks == null)
            {
                throw new ArgumentNullException("tasks");
            }
            var list = new List<Task>(tasks);
            if (list.Count == 0)
            {
                throw new ArgumentException("The tasks argument contains no tasks.", "tasks");
            }
            var any = new Task<Task>();
            for (int i = 0; i < list.Count; i++)
            {
                Task task = list[i];
                if (task == null)
                {
                    throw new ArgumentException("The tasks argument included a null value.", "tasks");
                }
                task.OnCompleted(() => any.TrySetResult(task));
            }
            return any;
        }

        public static Task<Task<TResult>> WhenAny<TResult>(params Task<TResult>[] tasks)
        {
            if (tasks == null)
            {
                throw new ArgumentNullException("tasks");
            }
            var any = new Task<Task<TResult>>();
            for (int i = 0; i < tasks.Length; i++)
            {
                Task<TResult> task = tasks[i];
                if (task == null)
                {
                    throw new ArgumentException("The tasks argument included a null value.", "tasks");
                }
                task.OnCompleted(() => any.TrySetResult(task));
            }
            return any;
        }

        public static YieldAwaitable Yield() => new YieldAwaitable();
    }

    // Метка задачи с результатом: WhenAll<T> завершает её сам, значениями.
    internal interface IHasResult
    {
    }

    public class Task<TResult> : Task, IHasResult
    {
        private TResult result;

        internal Task()
        {
        }

        public TResult Result
        {
            get
            {
                Wait();
                return result;
            }
        }

        internal bool TrySetResult(TResult value)
        {
            if (IsCompleted)
            {
                return false;
            }
            result = value;
            return TrySetCompleted();
        }

        internal TResult ResultCore()
        {
            WaitCore();
            return result;
        }

        public new TaskAwaiter<TResult> GetAwaiter() => new TaskAwaiter<TResult>(this);

        public new ConfiguredTaskAwaitable<TResult> ConfigureAwait(bool continueOnCapturedContext) => new ConfiguredTaskAwaitable<TResult>(this);

        public Task ContinueWith(Action<Task<TResult>> continuationAction)
        {
            if (continuationAction == null)
            {
                throw new ArgumentNullException("continuationAction");
            }
            var next = new Task();
            OnCompleted(() =>
            {
                try
                {
                    continuationAction(this);
                    next.TrySetCompleted();
                }
                catch (Exception e)
                {
                    next.TrySetFaulted(e);
                }
            });
            return next;
        }

        public Task<TNewResult> ContinueWith<TNewResult>(Func<Task<TResult>, TNewResult> continuationFunction)
        {
            if (continuationFunction == null)
            {
                throw new ArgumentNullException("continuationFunction");
            }
            var next = new Task<TNewResult>();
            OnCompleted(() =>
            {
                try
                {
                    next.TrySetResult(continuationFunction(this));
                }
                catch (Exception e)
                {
                    next.TrySetFaulted(e);
                }
            });
            return next;
        }
    }

    public class TaskCompletionSource
    {
        private readonly Task task = new Task();

        public Task Task => task;

        public void SetResult()
        {
            if (!task.TrySetCompleted())
            {
                throw new InvalidOperationException("An attempt was made to transition a task to a final state when it had already completed.");
            }
        }

        public bool TrySetResult() => task.TrySetCompleted();

        public void SetException(Exception exception)
        {
            if (exception == null)
            {
                throw new ArgumentNullException("exception");
            }
            if (!task.TrySetFaulted(exception))
            {
                throw new InvalidOperationException("An attempt was made to transition a task to a final state when it had already completed.");
            }
        }

        public bool TrySetException(Exception exception) => task.TrySetFaulted(exception);

        public void SetCanceled()
        {
            if (!task.TrySetCanceled())
            {
                throw new InvalidOperationException("An attempt was made to transition a task to a final state when it had already completed.");
            }
        }

        public bool TrySetCanceled() => task.TrySetCanceled();
    }

    public class TaskCompletionSource<TResult>
    {
        private readonly Task<TResult> task = new Task<TResult>();

        public Task<TResult> Task => task;

        public void SetResult(TResult result)
        {
            if (!task.TrySetResult(result))
            {
                throw new InvalidOperationException("An attempt was made to transition a task to a final state when it had already completed.");
            }
        }

        public bool TrySetResult(TResult result) => task.TrySetResult(result);

        public void SetException(Exception exception)
        {
            if (exception == null)
            {
                throw new ArgumentNullException("exception");
            }
            if (!task.TrySetFaulted(exception))
            {
                throw new InvalidOperationException("An attempt was made to transition a task to a final state when it had already completed.");
            }
        }

        public bool TrySetException(Exception exception) => task.TrySetFaulted(exception);

        public void SetCanceled()
        {
            if (!task.TrySetCanceled())
            {
                throw new InvalidOperationException("An attempt was made to transition a task to a final state when it had already completed.");
            }
        }

        public bool TrySetCanceled() => task.TrySetCanceled();
    }

    public class TaskCanceledException : OperationCanceledException
    {
        public TaskCanceledException()
            : base("A task was canceled.")
        {
        }

        public TaskCanceledException(string message)
            : base(message)
        {
        }

        public TaskCanceledException(Task task)
            : base("A task was canceled.")
        {
            Task = task;
        }

        public Task Task { get; }
    }

    // ValueTask: у .NET — либо результат на месте, либо задача. Здесь всегда
    // задача под капотом, когда результат не готов сразу; программа видит тот
    // же договор.
    public readonly struct ValueTask
    {
        private readonly Task task;

        public ValueTask(Task task)
        {
            if (task == null)
            {
                throw new ArgumentNullException("task");
            }
            this.task = task;
        }

        public static ValueTask CompletedTask => default;

        public bool IsCompleted => task == null || task.IsCompleted;

        public bool IsCompletedSuccessfully => task == null || task.IsCompletedSuccessfully;

        public bool IsFaulted => task != null && task.IsFaulted;

        public bool IsCanceled => task != null && task.IsCanceled;

        public Task AsTask() => task ?? Task.CompletedTask;

        public ValueTaskAwaiter GetAwaiter() => new ValueTaskAwaiter(AsTask());

        public ConfiguredValueTaskAwaitable ConfigureAwait(bool continueOnCapturedContext) => new ConfiguredValueTaskAwaitable(AsTask());
    }

    public readonly struct ValueTask<TResult>
    {
        private readonly Task<TResult> task;
        private readonly TResult result;

        public ValueTask(TResult result)
        {
            task = null;
            this.result = result;
        }

        public ValueTask(Task<TResult> task)
        {
            if (task == null)
            {
                throw new ArgumentNullException("task");
            }
            this.task = task;
            result = default;
        }

        public bool IsCompleted => task == null || task.IsCompleted;

        public bool IsCompletedSuccessfully => task == null || task.IsCompletedSuccessfully;

        public bool IsFaulted => task != null && task.IsFaulted;

        public bool IsCanceled => task != null && task.IsCanceled;

        public TResult Result => task == null ? result : task.ResultCore();

        public Task<TResult> AsTask() => task ?? Task.FromResult(result);

        public ValueTaskAwaiter<TResult> GetAwaiter() => new ValueTaskAwaiter<TResult>(AsTask());

        public ConfiguredValueTaskAwaitable<TResult> ConfigureAwait(bool continueOnCapturedContext) => new ConfiguredValueTaskAwaitable<TResult>(AsTask());
    }
}

namespace System
{
    public class AggregateException : Exception
    {
        private readonly Collections.ObjectModel.ReadOnlyCollection<Exception> inner;

        public AggregateException()
            : this(new List<Exception>())
        {
        }

        public AggregateException(string message)
            : base(message)
        {
            inner = new Collections.ObjectModel.ReadOnlyCollection<Exception>(new List<Exception>());
        }

        public AggregateException(IEnumerable<Exception> innerExceptions)
            : this(Message0(new List<Exception>(innerExceptions)), new List<Exception>(innerExceptions))
        {
        }

        public AggregateException(params Exception[] innerExceptions)
            : this((IEnumerable<Exception>)innerExceptions)
        {
        }

        public AggregateException(string message, Exception innerException)
            : base(message, innerException)
        {
            inner = new Collections.ObjectModel.ReadOnlyCollection<Exception>(new List<Exception> { innerException });
        }

        public AggregateException(string message, IEnumerable<Exception> innerExceptions)
            : this(message, new List<Exception>(innerExceptions))
        {
        }

        private AggregateException(string message, List<Exception> exceptions)
            : base(message, exceptions.Count > 0 ? exceptions[0] : null)
        {
            inner = new Collections.ObjectModel.ReadOnlyCollection<Exception>(exceptions);
        }

        // Текст .NET: «One or more errors occurred. (текст) (текст)…».
        private static string Message0(List<Exception> exceptions)
        {
            string text = "One or more errors occurred.";
            for (int i = 0; i < exceptions.Count; i++)
            {
                text += " (" + exceptions[i].Message + ")";
            }
            return text;
        }

        public Collections.ObjectModel.ReadOnlyCollection<Exception> InnerExceptions => inner;

        public AggregateException Flatten()
        {
            var flat = new List<Exception>();
            var queue = new Queue<AggregateException>();
            queue.Enqueue(this);
            while (queue.Count > 0)
            {
                AggregateException current = queue.Dequeue();
                for (int i = 0; i < current.inner.Count; i++)
                {
                    if (current.inner[i] is AggregateException nested)
                    {
                        queue.Enqueue(nested);
                    }
                    else
                    {
                        flat.Add(current.inner[i]);
                    }
                }
            }
            return new AggregateException(flat);
        }

        public override string ToString()
        {
            string text = base.ToString();
            for (int i = 0; i < inner.Count; i++)
            {
                text += "\n---> (Inner Exception #" + i + ") " + inner[i].ToString() + "<---\n";
            }
            return text;
        }
    }

    public class OperationCanceledException : SystemException
    {
        public OperationCanceledException()
            : base("The operation was canceled.")
        {
        }

        public OperationCanceledException(string message)
            : base(message)
        {
        }

        public OperationCanceledException(string message, Exception innerException)
            : base(message, innerException)
        {
        }

        public OperationCanceledException(CancellationToken token)
            : base("The operation was canceled.")
        {
            CancellationToken = token;
        }

        public CancellationToken CancellationToken { get; }
    }
}

namespace System.Threading
{
    // Отмена: флаг и список обратных вызовов. Поток один, поэтому Cancel
    // зовёт их сразу, здесь же.
    public sealed class CancellationTokenSource : IDisposable
    {
        private bool canceled;
        private List<Action> callbacks;

        public CancellationTokenSource()
        {
        }

        public CancellationTokenSource(int millisecondsDelay)
        {
            CancelAfter(millisecondsDelay);
        }

        public CancellationTokenSource(TimeSpan delay)
        {
            CancelAfter((int)delay.TotalMilliseconds);
        }

        public bool IsCancellationRequested => canceled;

        public CancellationToken Token => new CancellationToken(this);

        public void Cancel()
        {
            if (canceled)
            {
                return;
            }
            canceled = true;
            List<Action> pending = callbacks;
            callbacks = null;
            if (pending != null)
            {
                for (int i = 0; i < pending.Count; i++)
                {
                    pending[i]();
                }
            }
        }

        public void Cancel(bool throwOnFirstException) => Cancel();

        public void CancelAfter(int millisecondsDelay)
        {
            if (millisecondsDelay < -1)
            {
                throw new ArgumentOutOfRangeException("millisecondsDelay");
            }
            if (millisecondsDelay == -1 || canceled)
            {
                return;
            }
            Tasks.AsyncPump.PostAt(Tasks.AsyncPump.NowMs() + millisecondsDelay, Cancel);
        }

        public void CancelAfter(TimeSpan delay) => CancelAfter((int)delay.TotalMilliseconds);

        internal void Register(Action callback)
        {
            if (canceled)
            {
                callback();
                return;
            }
            if (callbacks == null)
            {
                callbacks = new List<Action>();
            }
            callbacks.Add(callback);
        }

        public void Dispose()
        {
        }
    }

    public readonly struct CancellationToken
    {
        private readonly CancellationTokenSource source;

        internal CancellationToken(CancellationTokenSource source)
        {
            this.source = source;
        }

        public CancellationToken(bool canceled)
        {
            source = null;
            if (canceled)
            {
                source = new CancellationTokenSource();
                source.Cancel();
            }
        }

        public static CancellationToken None => default;

        public bool IsCancellationRequested => source != null && source.IsCancellationRequested;

        public bool CanBeCanceled => source != null;

        public void ThrowIfCancellationRequested()
        {
            if (IsCancellationRequested)
            {
                throw new OperationCanceledException(this);
            }
        }

        public CancellationTokenRegistration Register(Action callback)
        {
            if (callback == null)
            {
                throw new ArgumentNullException("callback");
            }
            if (source != null)
            {
                source.Register(callback);
            }
            return default;
        }
    }

    public readonly struct CancellationTokenRegistration : IDisposable
    {
        public void Dispose()
        {
        }
    }

    // `lock (x)` компилятор пишет через Monitor. Поток один: взять замок
    // всегда можно, отпускать нечего; `Wait` без второго потока не проснётся.
    public static class Monitor
    {
        public static void Enter(object obj)
        {
            if (obj == null)
            {
                throw new ArgumentNullException("obj");
            }
        }

        public static void Enter(object obj, ref bool lockTaken)
        {
            Enter(obj);
            lockTaken = true;
        }

        public static void Exit(object obj)
        {
            if (obj == null)
            {
                throw new ArgumentNullException("obj");
            }
        }

        public static bool TryEnter(object obj)
        {
            Enter(obj);
            return true;
        }

        public static void Pulse(object obj)
        {
        }

        public static void PulseAll(object obj)
        {
        }
    }
}

namespace System.Runtime.CompilerServices
{
    public interface IAsyncStateMachine
    {
        void MoveNext();

        void SetStateMachine(IAsyncStateMachine stateMachine);
    }

    public interface INotifyCompletion
    {
        void OnCompleted(Action continuation);
    }

    public interface ICriticalNotifyCompletion : INotifyCompletion
    {
        void UnsafeOnCompleted(Action continuation);
    }

    // Строитель async-метода, возвращающего Task. Компилятор зовёт Start на
    // машине состояний, лежащей на стеке; при первом ожидании она упаковывается
    // (копией) в объект, и дальше живёт только эта копия — как у .NET, где
    // коробка и есть задача.
    public struct AsyncTaskMethodBuilder
    {
        private Task task;

        public static AsyncTaskMethodBuilder Create() => default;

        public Task Task
        {
            get
            {
                if (task == null)
                {
                    task = new Task();
                }
                return task;
            }
        }

        public void Start<TStateMachine>(ref TStateMachine stateMachine) where TStateMachine : IAsyncStateMachine
        {
            stateMachine.MoveNext();
        }

        public void SetStateMachine(IAsyncStateMachine stateMachine)
        {
        }

        public void SetResult() => Task.TrySetCompleted();

        public void SetException(Exception exception) => Task.TrySetFaulted(exception);

        public void AwaitOnCompleted<TAwaiter, TStateMachine>(ref TAwaiter awaiter, ref TStateMachine stateMachine)
            where TAwaiter : INotifyCompletion
            where TStateMachine : IAsyncStateMachine
        {
            IAsyncStateMachine box = AsyncBox.Of(Task, ref stateMachine);
            awaiter.OnCompleted(() => box.MoveNext());
        }

        public void AwaitUnsafeOnCompleted<TAwaiter, TStateMachine>(ref TAwaiter awaiter, ref TStateMachine stateMachine)
            where TAwaiter : ICriticalNotifyCompletion
            where TStateMachine : IAsyncStateMachine
        {
            IAsyncStateMachine box = AsyncBox.Of(Task, ref stateMachine);
            awaiter.UnsafeOnCompleted(() => box.MoveNext());
        }
    }

    internal static class AsyncBox
    {
        // Упаковать машину состояний один раз. Копия несёт и строитель с уже
        // созданной задачей, поэтому следующие ожидания из коробки найдут ту же
        // задачу и ту же коробку.
        internal static IAsyncStateMachine Of<TStateMachine>(Task task, ref TStateMachine stateMachine) where TStateMachine : IAsyncStateMachine
        {
            if (task.Box == null)
            {
                task.Box = stateMachine;
            }
            return task.Box;
        }
    }

    public struct AsyncTaskMethodBuilder<TResult>
    {
        private Task<TResult> task;

        public static AsyncTaskMethodBuilder<TResult> Create() => default;

        public Task<TResult> Task
        {
            get
            {
                if (task == null)
                {
                    task = new Task<TResult>();
                }
                return task;
            }
        }

        public void Start<TStateMachine>(ref TStateMachine stateMachine) where TStateMachine : IAsyncStateMachine
        {
            stateMachine.MoveNext();
        }

        public void SetStateMachine(IAsyncStateMachine stateMachine)
        {
        }

        public void SetResult(TResult result) => Task.TrySetResult(result);

        public void SetException(Exception exception) => Task.TrySetFaulted(exception);

        public void AwaitOnCompleted<TAwaiter, TStateMachine>(ref TAwaiter awaiter, ref TStateMachine stateMachine)
            where TAwaiter : INotifyCompletion
            where TStateMachine : IAsyncStateMachine
        {
            IAsyncStateMachine box = AsyncBox.Of(Task, ref stateMachine);
            awaiter.OnCompleted(() => box.MoveNext());
        }

        public void AwaitUnsafeOnCompleted<TAwaiter, TStateMachine>(ref TAwaiter awaiter, ref TStateMachine stateMachine)
            where TAwaiter : ICriticalNotifyCompletion
            where TStateMachine : IAsyncStateMachine
        {
            IAsyncStateMachine box = AsyncBox.Of(Task, ref stateMachine);
            awaiter.UnsafeOnCompleted(() => box.MoveNext());
        }
    }

    // `async void`: задача есть, но её никто не ждёт; исключение из неё у .NET
    // валит процесс — здесь оно всплывает из очереди, где выполнялось
    // продолжение, то есть тоже наружу.
    public struct AsyncVoidMethodBuilder
    {
        private Task task;

        public static AsyncVoidMethodBuilder Create() => default;

        private Task Task
        {
            get
            {
                if (task == null)
                {
                    task = new Task();
                }
                return task;
            }
        }

        public void Start<TStateMachine>(ref TStateMachine stateMachine) where TStateMachine : IAsyncStateMachine
        {
            stateMachine.MoveNext();
        }

        public void SetStateMachine(IAsyncStateMachine stateMachine)
        {
        }

        public void SetResult() => Task.TrySetCompleted();

        public void SetException(Exception exception) => throw exception;

        public void AwaitOnCompleted<TAwaiter, TStateMachine>(ref TAwaiter awaiter, ref TStateMachine stateMachine)
            where TAwaiter : INotifyCompletion
            where TStateMachine : IAsyncStateMachine
        {
            IAsyncStateMachine box = AsyncBox.Of(Task, ref stateMachine);
            awaiter.OnCompleted(() => box.MoveNext());
        }

        public void AwaitUnsafeOnCompleted<TAwaiter, TStateMachine>(ref TAwaiter awaiter, ref TStateMachine stateMachine)
            where TAwaiter : ICriticalNotifyCompletion
            where TStateMachine : IAsyncStateMachine
        {
            IAsyncStateMachine box = AsyncBox.Of(Task, ref stateMachine);
            awaiter.UnsafeOnCompleted(() => box.MoveNext());
        }
    }

    public struct AsyncValueTaskMethodBuilder
    {
        private AsyncTaskMethodBuilder inner;

        public static AsyncValueTaskMethodBuilder Create() => default;

        public ValueTask Task => new ValueTask(inner.Task);

        public void Start<TStateMachine>(ref TStateMachine stateMachine) where TStateMachine : IAsyncStateMachine => inner.Start(ref stateMachine);

        public void SetStateMachine(IAsyncStateMachine stateMachine)
        {
        }

        public void SetResult() => inner.SetResult();

        public void SetException(Exception exception) => inner.SetException(exception);

        public void AwaitOnCompleted<TAwaiter, TStateMachine>(ref TAwaiter awaiter, ref TStateMachine stateMachine)
            where TAwaiter : INotifyCompletion
            where TStateMachine : IAsyncStateMachine
            => inner.AwaitOnCompleted(ref awaiter, ref stateMachine);

        public void AwaitUnsafeOnCompleted<TAwaiter, TStateMachine>(ref TAwaiter awaiter, ref TStateMachine stateMachine)
            where TAwaiter : ICriticalNotifyCompletion
            where TStateMachine : IAsyncStateMachine
            => inner.AwaitUnsafeOnCompleted(ref awaiter, ref stateMachine);
    }

    public struct AsyncValueTaskMethodBuilder<TResult>
    {
        private AsyncTaskMethodBuilder<TResult> inner;

        public static AsyncValueTaskMethodBuilder<TResult> Create() => default;

        public ValueTask<TResult> Task => new ValueTask<TResult>(inner.Task);

        public void Start<TStateMachine>(ref TStateMachine stateMachine) where TStateMachine : IAsyncStateMachine => inner.Start(ref stateMachine);

        public void SetStateMachine(IAsyncStateMachine stateMachine)
        {
        }

        public void SetResult(TResult result) => inner.SetResult(result);

        public void SetException(Exception exception) => inner.SetException(exception);

        public void AwaitOnCompleted<TAwaiter, TStateMachine>(ref TAwaiter awaiter, ref TStateMachine stateMachine)
            where TAwaiter : INotifyCompletion
            where TStateMachine : IAsyncStateMachine
            => inner.AwaitOnCompleted(ref awaiter, ref stateMachine);

        public void AwaitUnsafeOnCompleted<TAwaiter, TStateMachine>(ref TAwaiter awaiter, ref TStateMachine stateMachine)
            where TAwaiter : ICriticalNotifyCompletion
            where TStateMachine : IAsyncStateMachine
            => inner.AwaitUnsafeOnCompleted(ref awaiter, ref stateMachine);
    }

    public readonly struct TaskAwaiter : ICriticalNotifyCompletion
    {
        private readonly Task task;

        internal TaskAwaiter(Task task)
        {
            this.task = task;
        }

        public bool IsCompleted => task.IsCompleted;

        public void OnCompleted(Action continuation) => task.OnCompleted(continuation);

        public void UnsafeOnCompleted(Action continuation) => task.OnCompleted(continuation);

        public void GetResult() => task.WaitCore();
    }

    public readonly struct TaskAwaiter<TResult> : ICriticalNotifyCompletion
    {
        private readonly Task<TResult> task;

        internal TaskAwaiter(Task<TResult> task)
        {
            this.task = task;
        }

        public bool IsCompleted => task.IsCompleted;

        public void OnCompleted(Action continuation) => task.OnCompleted(continuation);

        public void UnsafeOnCompleted(Action continuation) => task.OnCompleted(continuation);

        public TResult GetResult() => task.ResultCore();
    }

    public readonly struct ConfiguredTaskAwaitable
    {
        private readonly Task task;

        internal ConfiguredTaskAwaitable(Task task)
        {
            this.task = task;
        }

        public ConfiguredTaskAwaiter GetAwaiter() => new ConfiguredTaskAwaiter(task);

        public readonly struct ConfiguredTaskAwaiter : ICriticalNotifyCompletion
        {
            private readonly Task task;

            internal ConfiguredTaskAwaiter(Task task)
            {
                this.task = task;
            }

            public bool IsCompleted => task.IsCompleted;

            public void OnCompleted(Action continuation) => task.OnCompleted(continuation);

            public void UnsafeOnCompleted(Action continuation) => task.OnCompleted(continuation);

            public void GetResult() => task.WaitCore();
        }
    }

    public readonly struct ConfiguredTaskAwaitable<TResult>
    {
        private readonly Task<TResult> task;

        internal ConfiguredTaskAwaitable(Task<TResult> task)
        {
            this.task = task;
        }

        public ConfiguredTaskAwaiter GetAwaiter() => new ConfiguredTaskAwaiter(task);

        public readonly struct ConfiguredTaskAwaiter : ICriticalNotifyCompletion
        {
            private readonly Task<TResult> task;

            internal ConfiguredTaskAwaiter(Task<TResult> task)
            {
                this.task = task;
            }

            public bool IsCompleted => task.IsCompleted;

            public void OnCompleted(Action continuation) => task.OnCompleted(continuation);

            public void UnsafeOnCompleted(Action continuation) => task.OnCompleted(continuation);

            public TResult GetResult() => task.ResultCore();
        }
    }

    public readonly struct ValueTaskAwaiter : ICriticalNotifyCompletion
    {
        private readonly Task task;

        internal ValueTaskAwaiter(Task task)
        {
            this.task = task;
        }

        public bool IsCompleted => task.IsCompleted;

        public void OnCompleted(Action continuation) => task.OnCompleted(continuation);

        public void UnsafeOnCompleted(Action continuation) => task.OnCompleted(continuation);

        public void GetResult() => task.WaitCore();
    }

    public readonly struct ValueTaskAwaiter<TResult> : ICriticalNotifyCompletion
    {
        private readonly Task<TResult> task;

        internal ValueTaskAwaiter(Task<TResult> task)
        {
            this.task = task;
        }

        public bool IsCompleted => task.IsCompleted;

        public void OnCompleted(Action continuation) => task.OnCompleted(continuation);

        public void UnsafeOnCompleted(Action continuation) => task.OnCompleted(continuation);

        public TResult GetResult() => task.ResultCore();
    }

    public readonly struct ConfiguredValueTaskAwaitable
    {
        private readonly Task task;

        internal ConfiguredValueTaskAwaitable(Task task)
        {
            this.task = task;
        }

        public ValueTaskAwaiter GetAwaiter() => new ValueTaskAwaiter(task);
    }

    public readonly struct ConfiguredValueTaskAwaitable<TResult>
    {
        private readonly Task<TResult> task;

        internal ConfiguredValueTaskAwaitable(Task<TResult> task)
        {
            this.task = task;
        }

        public ValueTaskAwaiter<TResult> GetAwaiter() => new ValueTaskAwaiter<TResult>(task);
    }

    // `await Task.Yield()`: продолжение уходит в конец очереди.
    public readonly struct YieldAwaitable
    {
        public YieldAwaiter GetAwaiter() => new YieldAwaiter();

        public readonly struct YieldAwaiter : ICriticalNotifyCompletion
        {
            public bool IsCompleted => false;

            public void OnCompleted(Action continuation) => System.Threading.Tasks.AsyncPump.Post(continuation);

            public void UnsafeOnCompleted(Action continuation) => System.Threading.Tasks.AsyncPump.Post(continuation);

            public void GetResult()
            {
            }
        }
    }

    // Атрибуты, которыми компилятор помечает async-методы и типы; среде они не
    // нужны, компилятору corelib — тоже, но программа ссылается на них по имени.
    [AttributeUsage(AttributeTargets.Method, Inherited = false, AllowMultiple = false)]
    public class StateMachineAttribute : Attribute
    {
        public StateMachineAttribute(Type stateMachineType)
        {
            StateMachineType = stateMachineType;
        }

        public Type StateMachineType { get; }
    }

    [AttributeUsage(AttributeTargets.Method, Inherited = false, AllowMultiple = false)]
    public sealed class AsyncStateMachineAttribute : StateMachineAttribute
    {
        public AsyncStateMachineAttribute(Type stateMachineType)
            : base(stateMachineType)
        {
        }
    }

    [AttributeUsage(AttributeTargets.Method, Inherited = false, AllowMultiple = false)]
    public sealed class IteratorStateMachineAttribute : StateMachineAttribute
    {
        public IteratorStateMachineAttribute(Type stateMachineType)
            : base(stateMachineType)
        {
        }
    }

    [AttributeUsage(AttributeTargets.Class | AttributeTargets.Struct | AttributeTargets.Interface | AttributeTargets.Delegate | AttributeTargets.Enum | AttributeTargets.Method, Inherited = false, AllowMultiple = false)]
    public sealed class AsyncMethodBuilderAttribute : Attribute
    {
        public AsyncMethodBuilderAttribute(Type builderType)
        {
            BuilderType = builderType;
        }

        public Type BuilderType { get; }
    }
}
