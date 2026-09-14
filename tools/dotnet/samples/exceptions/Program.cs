// Образец для фазы N3b. Порядок строк здесь и есть проверка: фильтр `when`
// печатает раньше, чем `finally` внутренних кадров (двухпроходный поиск
// обработчика, как в CLR), `finally` выполняется до возврата значения, а
// исключение из `finally` заменяет летящее.
//
// Чего здесь нет намеренно: `ToString()` брошенного исключения (в нём стек
// вызовов с путями и номерами строк, у своей среды его нет), интерполяции
// строк (N3c), печати дробных (N4).

#pragma warning disable CS8600, CS8602

using System.Text;

namespace FreeOs.Samples.Exceptions;

public class AppError : Exception
{
    public AppError(string message, int code)
        : base(message)
    {
        Code = code;
    }

    public int Code { get; }
}

public sealed class DeepError : AppError
{
    public DeepError(string message)
        : base(message, 7)
    {
    }

    public override string Message => "deep: " + base.Message;
}

public sealed class Resource : IDisposable
{
    private readonly string name;

    public Resource(string name)
    {
        this.name = name;
        Console.WriteLine("open " + name);
    }

    public void Dispose() => Console.WriteLine("close " + name);
}

public static class Program
{
    private static void Thrower(int level)
    {
        if (level == 0)
        {
            throw new DeepError("bottom");
        }
        try
        {
            Thrower(level - 1);
        }
        finally
        {
            Console.WriteLine("unwind " + level);
        }
    }

    private static bool Log(string what)
    {
        Console.WriteLine("filter " + what);
        return what.Length > 3;
    }

    private static int ReturnThroughFinally()
    {
        try
        {
            return 1;
        }
        finally
        {
            Console.WriteLine("finally before return");
        }
    }

    private static int Nested()
    {
        int result = 0;
        try
        {
            try
            {
                throw new InvalidOperationException("inner");
            }
            catch (ArgumentException)
            {
                result = -1;
            }
            finally
            {
                Console.WriteLine("inner finally");
                result += 10;
            }
        }
        catch (InvalidOperationException e)
        {
            Console.WriteLine("outer caught " + e.Message);
            result += 100;
        }
        return result;
    }

    private static int Divide(int a, int b) => a / b;

    public static int Main()
    {
        Console.OutputEncoding = Encoding.UTF8;
        Console.WriteLine("exceptions: start");

        try
        {
            throw new AppError("simple", 3);
        }
        catch (AppError e)
        {
            Console.WriteLine("caught " + e.Message + " code " + e.Code);
        }

        // Фильтр печатает раньше, чем «unwind 1..3».
        try
        {
            Thrower(3);
        }
        catch (DeepError e) when (Log("deep"))
        {
            Console.WriteLine(e.Message + " code " + e.Code);
        }

        try
        {
            try
            {
                throw new AppError("x", 1);
            }
            catch (AppError) when (Log("no"))
            {
                Console.WriteLine("wrong handler");
            }
        }
        catch (Exception e)
        {
            Console.WriteLine("fell through to " + e.GetType().Name);
        }

        Console.WriteLine("finally returns " + ReturnThroughFinally());
        Console.WriteLine("nested " + Nested());

        // Исключения самой среды.
        string text = null;
        try
        {
            Console.WriteLine(text.Length);
        }
        catch (NullReferenceException e)
        {
            Console.WriteLine("null: " + e.Message);
        }
        int[] numbers = new int[2];
        try
        {
            numbers[5] = 1;
        }
        catch (IndexOutOfRangeException e)
        {
            Console.WriteLine("index: " + e.Message);
        }
        try
        {
            Console.WriteLine(Divide(10, numbers[0]));
        }
        catch (DivideByZeroException e)
        {
            Console.WriteLine("divide: " + e.Message);
        }
        object boxed = "text";
        try
        {
            int number = (int)boxed;
            Console.WriteLine(number);
        }
        catch (InvalidCastException)
        {
            Console.WriteLine("cast failed");
        }
        try
        {
            int big = int.MaxValue;
            big = checked(big + numbers[1] + 1);
            Console.WriteLine(big);
        }
        catch (OverflowException e)
        {
            Console.WriteLine("overflow: " + e.Message);
        }

        // throw; сохраняет исключение.
        try
        {
            try
            {
                throw new NotSupportedException("again");
            }
            catch (Exception e)
            {
                Console.WriteLine("log " + e.Message);
                throw;
            }
        }
        catch (NotSupportedException e)
        {
            Console.WriteLine("rethrown " + e.Message);
        }

        // using — это try/finally с Dispose.
        try
        {
            using (new Resource("file"))
            {
                throw new ArgumentNullException("path");
            }
        }
        catch (ArgumentException e)
        {
            Console.WriteLine("argument: " + e.Message);
        }

        // Исключение из finally заменяет летящее.
        try
        {
            try
            {
                throw new AppError("first", 1);
            }
            finally
            {
                Console.WriteLine("finally throws");
                throw new AppError("second", 2);
            }
        }
        catch (AppError e)
        {
            Console.WriteLine("got " + e.Message);
        }

        // Не брошенное исключение печатается без стека.
        Console.WriteLine(new AppError("not thrown", 0).ToString());
        Console.WriteLine(new Exception().Message);

        // break из try с finally.
        int count = 0;
        for (int i = 0; i < 5; i++)
        {
            try
            {
                if (i == 3)
                {
                    break;
                }
                count++;
            }
            finally
            {
                count += 10;
            }
        }
        Console.WriteLine("loop " + count);

        Console.WriteLine("exceptions: done");
        return count;
    }
}
