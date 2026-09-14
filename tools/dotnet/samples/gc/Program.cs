// Образец для фазы N3d. Мусор здесь создаётся всеми путями, какими его
// создают настоящие программы, — массивы, строки, объекты, упаковка,
// замыкания, исключения, — а живое держится всеми путями, какими его держат:
// локальная переменная, поле статического класса, элемент массива, цепочка
// объектов, захват в замыкании, структура внутри массива, строка-литерал.
// Каждое живое значение в конце проверяется: сборщик, собравший живое,
// напечатал бы другое число, а не упал бы.
//
// Сколько выделяется: у своей среды элемент массива занимает 24 байта, а куча
// `/bin/dotnet` — 16 МиБ. Три тысячи массивов по тысяче элементов — около 70
// МиБ, в четыре с лишним раза больше кучи.

#pragma warning disable CS8618

using System.Text;

namespace FreeOs.Samples.Gc;

public sealed class Node
{
    public Node(int value, Node? next)
    {
        Value = value;
        Next = next;
    }

    public int Value { get; }

    public Node? Next { get; }
}

public struct Cell
{
    public string Label;
    public int Weight;
}

public static class Keeper
{
    public static Node Chain;
    public static Cell[] Cells = new Cell[4];
}

public static class Program
{
    private static int Churn(int round)
    {
        int[] buffer = new int[1000];
        for (int i = 0; i < buffer.Length; i += 100)
        {
            buffer[i] = round + i;
        }
        return buffer[round % 10 * 100];
    }

    public static int Main()
    {
        Console.OutputEncoding = Encoding.UTF8;
        Console.WriteLine("gc: start");

        const string literal = "литерал живёт";
        Node local = new Node(-1, null);
        long checksum = 0;
        Func<int>? counter = null;

        for (int round = 0; round < 3000; round++)
        {
            checksum += Churn(round);

            // Строки и упаковка — мусор с первой же итерации.
            string text = "round " + round;
            object boxed = round;
            checksum += text.Length + (int)boxed % 7;

            if (round % 500 == 0)
            {
                Keeper.Chain = new Node(round, Keeper.Chain);
                Keeper.Cells[round / 1000] = new Cell { Label = "cell " + round, Weight = round };
                int captured = round;
                counter = () => captured + 1;
            }

            if (round % 750 == 0)
            {
                try
                {
                    throw new InvalidOperationException("round " + round);
                }
                catch (InvalidOperationException error)
                {
                    checksum += error.Message.Length;
                }
            }
        }

        int chainLength = 0;
        int chainSum = 0;
        for (Node? node = Keeper.Chain; node != null; node = node.Next)
        {
            chainLength++;
            chainSum += node.Value;
        }

        Console.WriteLine("checksum " + checksum);
        Console.WriteLine("chain " + chainLength + " sum " + chainSum);
        Console.WriteLine("cells " + Keeper.Cells[0].Label + " / " + Keeper.Cells[2].Label + " weight " + Keeper.Cells[2].Weight);
        Console.WriteLine("closure " + counter!() + " local " + local.Value);
        Console.WriteLine(literal);
        Console.WriteLine("gc: done");
        return chainLength;
    }
}
