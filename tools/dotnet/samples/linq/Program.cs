// Образец для фазы N4d: Linq в объёме, которым пользуются обычные программы, —
// фильтр, проекция, сортировка (устойчивая), группировка, агрегаты, запросы
// в синтаксисе `from … select`, отложенное выполнение — и большие массивы
// примитивов, которые обязаны занимать байт на `byte`, а не значение среды.

namespace FreeOs.Samples.Linq;

public sealed class Person
{
    public Person(string name, int age, string city)
    {
        Name = name;
        Age = age;
        City = city;
    }

    public string Name { get; }

    public int Age { get; }

    public string City { get; }
}

public static class Program
{
    public static int Main()
    {
        Console.WriteLine("linq: start");

        int[] numbers = { 5, 3, 8, 1, 9, 2, 7, 4, 6, 0 };
        List<int> squares = numbers.Where(n => n % 2 == 0).Select(n => n * n).ToList();
        Console.WriteLine(string.Join(",", squares) + " " + numbers.Sum() + " " + numbers.Max() + " " + numbers.Min() + " "
            + numbers.Average() + " " + numbers.Count(n => n > 4));
        Console.WriteLine(string.Join(",", numbers.OrderBy(n => n).Take(3)) + " " + string.Join(",", numbers.OrderByDescending(n => n).Skip(7))
            + " " + numbers.First() + " " + numbers.Last(n => n < 5) + " " + numbers.FirstOrDefault(n => n > 100) + " "
            + numbers.Any(n => n == 7) + " " + numbers.All(n => n >= 0) + " " + numbers.Contains(11));

        var people = new List<Person>
        {
            new("Ann", 31, "Oslo"), new("Bob", 25, "Rome"), new("Cid", 31, "Oslo"), new("Dan", 19, "Rome"), new("Eve", 25, "Kyiv"),
        };
        Console.WriteLine(string.Join(" ", people.OrderBy(p => p.Age).Select(p => p.Name)) + " | "
            + string.Join(" ", people.OrderByDescending(p => p.Age).ThenByDescending(p => p.Name).Select(p => p.Name)));
        foreach (IGrouping<string, Person> group in people.GroupBy(p => p.City))
        {
            Console.WriteLine(group.Key + ": " + group.Count() + " " + string.Join("+", group.Select(p => p.Name)) + " avg "
                + group.Average(p => p.Age));
        }
        Dictionary<string, int> ages = people.ToDictionary(p => p.Name, p => p.Age);
        Console.WriteLine(ages["Cid"] + " " + people.Sum(p => p.Age) + " " + people.Max(p => p.Age) + " " + people.Average(p => p.Age)
            + " " + people.Single(p => p.Age < 20).Name + " " + people.ElementAt(1).Name + " " + people.MinBy(p => p.Age)!.Name);

        IEnumerable<string> query = from p in people where p.Age > 20 orderby p.Name descending select p.Name.ToUpper();
        Console.WriteLine(string.Join(",", query) + " " + string.Join(",", numbers.Select(n => n % 3).Distinct()) + " "
            + Enumerable.Range(1, 5).Aggregate(1, (product, x) => product * x) + " " + string.Concat(Enumerable.Repeat("ab", 3)));
        Console.WriteLine(string.Join(",", numbers.Zip(people, (n, p) => p.Name + n)) + " "
            + people.SelectMany(p => p.Name.ToCharArray()).Count() + " " + string.Join(",", numbers.Reverse().Take(2)) + " "
            + numbers.Take(3).SequenceEqual(new[] { 5, 3, 8 }) + " " + string.Join(",", numbers.Take(2).Concat(numbers.Skip(8)).ToArray()));

        object[] mixed = { 1, "two", 3.5, "four", 5 };
        Console.WriteLine(string.Join(",", mixed.OfType<string>()) + " " + mixed.OfType<int>().Sum() + " "
            + string.Join(",", new List<object> { "x", "y" }.Cast<string>()) + " " + string.Join(",", people.Select((p, i) => i + p.Name)));

        // Отложенное выполнение: условие считается при переборе, а не при записи.
        int calls = 0;
        IEnumerable<int> lazy = numbers.Where(n =>
        {
            calls++;
            return n > 5;
        });
        int before = calls;
        int found = lazy.Count();
        Console.WriteLine(before + " " + found + " " + calls + " " + lazy.Any() + " " + calls);

        try
        {
            Array.Empty<int>().First();
        }
        catch (InvalidOperationException e)
        {
            Console.WriteLine(e.Message);
        }
        try
        {
            people.Single(p => p.Age == 31);
        }
        catch (InvalidOperationException e)
        {
            Console.WriteLine(e.Message);
        }

        // Большие массивы примитивов: четыре миллиона байт и миллион int.
        byte[] bytes = new byte[4_000_000];
        int[] ints = new int[1_000_000];
        for (int i = 0; i < 4000; i++)
        {
            bytes[i * 1000] = (byte)(i % 251);
            ints[i * 250] = i;
        }
        long sum = 0;
        for (int i = 0; i < 4000; i++)
        {
            sum += bytes[i * 1000] + ints[i * 250];
        }
        bool[] composite = new bool[10_000];
        int primes = 0;
        for (int i = 2; i < composite.Length; i++)
        {
            if (!composite[i])
            {
                primes++;
                for (int j = i * i; j < composite.Length; j += i)
                {
                    composite[j] = true;
                }
            }
        }
        Console.WriteLine(bytes.Length + " " + ints.Length + " " + sum + " " + primes + " " + bytes[3999000] + " " + ints[999750]);

        Console.WriteLine("linq: done");
        return primes % 100;
    }
}
