// Образец для фазы N3a. Каждая группа строк проверяет свой кусок среды:
// раскладку полей, таблицу виртуальных методов, поиск реализации интерфейса,
// порядок статических конструкторов, копирование структур и упаковку.
//
// Чего здесь нет намеренно: интерполяции строк (`$"..."` собирается через
// обобщённый DefaultInterpolatedStringHandler — фаза N3c), печати дробных чисел и
// `Enum.ToString()` (форматирование — фаза N4), исключений (N3b).

using System.Text;

namespace FreeOs.Samples.Objects;

public abstract class Animal
{
    private static int created;

    protected readonly string name;

    protected Animal(string name)
    {
        this.name = name;
        created++;
    }

    public static int Created => created;

    public int Age { get; set; }

    public abstract string Sound();

    public virtual string Describe() => name + " says " + Sound();

    public override string ToString() => "Animal " + name;
}

public class Dog : Animal
{
    public Dog(string name) : base(name)
    {
    }

    public override string Sound() => "woof";
}

public sealed class Puppy : Dog
{
    public Puppy(string name) : base(name)
    {
    }

    public override string Sound() => "yip";

    public override string Describe() => "little " + base.Describe();
}

public sealed class Cat : Animal
{
    public Cat() : this("Cat")
    {
    }

    private Cat(string name) : base(name)
    {
    }

    public override string Sound() => "meow";

    // Скрывает, а не переопределяет: вызов через Animal его не видит.
    public new string ToString() => "hidden cat";
}

public interface ICounter
{
    int Current { get; }

    int Next();

    // Член по умолчанию: реализации в классе нет, тело берётся из интерфейса.
    string Label => "counter at " + Current;
}

public interface IResettable
{
    void Reset();
}

public class Counter : ICounter, IResettable
{
    private int value;

    public int Current => value;

    public int Next() => ++value;

    // Явная реализация: в классе метод закрытый, найти его можно только
    // через таблицу MethodImpl.
    void IResettable.Reset() => value = 0;
}

public sealed class StepCounter : Counter, ICounter
{
    // Повторная реализация интерфейса в наследнике: ICounter.Next теперь здесь.
    public new int Next() => base.Next() + base.Next();
}

public static class Registry
{
    public static int Count;

    static Registry()
    {
        Console.WriteLine("Registry: static constructor");
        Count = 100;
    }

    public static void Add() => Count++;
}

public class Lazy
{
    public static readonly string Greeting;

    static Lazy()
    {
        Console.WriteLine("Lazy: static constructor");
        Greeting = "hello from Lazy";
    }

    public Lazy()
    {
        Console.WriteLine("Lazy: instance constructor");
    }
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

    public readonly int Sum => X + Y;

    public void Move(int delta)
    {
        X += delta;
        Y += delta;
    }

    public override string ToString() => "(" + X + ", " + Y + ")";
}

public struct Plain
{
    public int Value;
}

public struct Segment
{
    public Point From;
    public Point To;

    public override string ToString() => From.ToString() + "-" + To.ToString();
}

public class Holder
{
    public Point Where;
    public Segment Line;
    public byte Small;
    public short Signed;
}

public enum Level : byte
{
    Low = 1,
    High = 200,
}

public static class Program
{
    public static int Main()
    {
        Console.OutputEncoding = Encoding.UTF8;
        Console.WriteLine("objects: start");

        // Виртуальные вызовы по цепочке наследования.
        Animal[] zoo = { new Dog("Rex"), new Puppy("Bim"), new Cat() };
        for (int i = 0; i < zoo.Length; i++)
        {
            Console.WriteLine(zoo[i].Describe());
        }
        Console.WriteLine(zoo[0].ToString());
        Cat cat = (Cat)zoo[2];
        Console.WriteLine(cat.ToString());
        Console.WriteLine(((object)cat).ToString());
        Console.WriteLine("created: " + Animal.Created);
        zoo[1].Age = 2;
        Console.WriteLine("age: " + zoo[1].Age);

        // Проверки типов.
        object thing = zoo[1];
        Console.WriteLine("is Dog: " + (thing is Dog));
        Console.WriteLine("is Cat: " + (thing is Cat));
        Console.WriteLine("as Animal: " + ((thing as Animal) != null));
        if (thing is Puppy puppy)
        {
            Console.WriteLine("puppy: " + puppy.Sound());
        }
        Console.WriteLine("type: " + thing.GetType().Name + " / " + thing.GetType().FullName);

        // Интерфейсы.
        Counter counter = new Counter();
        ICounter view = counter;
        view.Next();
        view.Next();
        Console.WriteLine(view.Label);
        ((IResettable)counter).Reset();
        Console.WriteLine("after reset: " + view.Current);
        ICounter stepper = new StepCounter();
        stepper.Next();
        Console.WriteLine("step: " + stepper.Current);
        Counter asBase = (Counter)stepper;
        asBase.Next();
        Console.WriteLine("base next: " + stepper.Current);
        Console.WriteLine("is IResettable: " + (stepper is IResettable));

        // Статические конструкторы: строка «before» печатается раньше.
        Console.WriteLine("before Registry");
        Registry.Add();
        Registry.Add();
        Console.WriteLine("registry: " + Registry.Count);
        Console.WriteLine("before Lazy");
        Lazy lazy = new Lazy();
        Console.WriteLine(Lazy.Greeting);
        Console.WriteLine("lazy is " + (lazy != null));

        // Структуры копируются при присваивании.
        Point a = new Point(1, 2);
        Point b = a;
        b.Move(10);
        Console.WriteLine("a = " + a + ", b = " + b + ", sum " + b.Sum);
        Point[] points = new Point[3];
        points[1].Move(5);
        ref Point third = ref points[2];
        third.X = 7;
        Console.WriteLine("points: " + points[0] + " " + points[1] + " " + points[2]);
        Point copy = points[1];
        copy.Move(100);
        Console.WriteLine("array keeps " + points[1] + ", copy " + copy);

        Holder holder = new Holder();
        holder.Where.Move(3);
        holder.Line.To.Move(4);
        holder.Line.From = holder.Where;
        holder.Where.X = 50;
        Console.WriteLine("holder: " + holder.Where + " line " + holder.Line);
        holder.Small = 255;
        holder.Small++;
        holder.Signed = -32768;
        holder.Signed--;
        Console.WriteLine("narrow: " + holder.Small + " " + holder.Signed);
        Console.WriteLine("plain: " + new Plain().ToString());

        // Упаковка копирует значение.
        object boxed = a;
        a.Move(1);
        Point unboxed = (Point)boxed;
        Console.WriteLine("boxed " + boxed + ", moved " + a + ", unboxed " + unboxed);
        object number = 42;
        int back = (int)number;
        Console.WriteLine("number " + number + " back " + (back + 1) + " is int: " + (number is int));
        Console.WriteLine("boxed equals: " + number.Equals(42) + " " + boxed.Equals(unboxed) + " " + boxed.Equals(a));

        // Перечисление — это его базовый тип.
        Level level = Level.High;
        Holder[] holders = new Holder[2];
        Console.WriteLine("level: " + (int)level + " " + (level == Level.High) + " null slot " + (holders[1] == null));

        // Массив с инициализатором из данных сборки (FieldRVA).
        int[] primes = { 2, 3, 5, 7, 11, 13, 17, 19, 23, 29 };
        long total = 0;
        foreach (int prime in primes)
        {
            total += prime;
        }
        Console.WriteLine("primes: " + total);
        string[] words = { "раз", "два", "три" };
        Console.WriteLine(words[0] + words[1] + words[2] + " " + words.Length);

        Console.WriteLine("same: " + ReferenceEquals(zoo[0], zoo[0]) + " " + ReferenceEquals(zoo[0], zoo[1]));
        Console.WriteLine("objects: done");
        return Animal.Created;
    }
}
