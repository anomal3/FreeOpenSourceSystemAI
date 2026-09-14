// Образец для фазы N2. Каждая строка вывода проверяет свой кусок интерпретатора,
// и вывод настоящего dotnet с ним совпадает побайтно (`cargo xtask clr-check`).
//
// Склейки здесь не длиннее четырёх частей намеренно: пять и больше компилятор
// C# 13 собирает через `string.Concat(ReadOnlySpan<string>)` на встроенном
// массиве-структуре, а структуры — фаза N3.

using System.Text;

// Без этой строки настоящий dotnet на русской Windows пишет в канал кодовой
// страницей 866, и сверка кириллицы соврала бы. Для своей среды это пустая
// операция: вывод у неё всегда UTF-8.
Console.OutputEncoding = Encoding.UTF8;

Console.WriteLine("arith: start");

int total = 0;
for (int i = 1; i <= 10; i++)
{
    total += i;
}
Console.WriteLine("sum 1..10 = " + total);
Console.WriteLine("fib(20) = " + Fib(20));
Console.WriteLine("fact(20) = " + Fact(20));
Console.WriteLine("gcd(1071, 462) = " + Gcd(1071, 462));

int dividend = -17;
int divisor = 5;
Console.WriteLine("-17 / 5 = " + (dividend / divisor) + ", -17 % 5 = " + (dividend % divisor));

uint wrapped = unchecked((uint)dividend);
Console.WriteLine("uint: " + wrapped);
Console.WriteLine("uint >> 28: " + (wrapped >> 28));

long big = 1L << 40;
Console.WriteLine("1L << 40 = " + big);
Console.WriteLine("-64 >> 3 = " + (-64 >> Three()));

bool large = total > 50;
char letter = 'Z';
Console.WriteLine("bool: " + large + ", char: " + letter);

Console.WriteLine("switch: " + Name(0) + Name(1));
Console.WriteLine("switch: " + Name(2) + Name(7));

Console.WriteLine(Classify(-5));
Console.WriteLine(Classify(0));
Console.WriteLine(Classify(42));

Console.WriteLine("Привет из IL");
Console.WriteLine("arith: done");
return total == 55 ? 0 : 1;

static int Fib(int n) => n < 2 ? n : Fib(n - 1) + Fib(n - 2);

static long Fact(int n)
{
    long result = 1;
    for (int i = 2; i <= n; i++)
    {
        result *= i;
    }
    return result;
}

static int Gcd(int a, int b)
{
    while (b != 0)
    {
        int rest = a % b;
        a = b;
        b = rest;
    }
    return a;
}

// Не константа, чтобы сдвиг отрицательного числа посчитала среда, а не
// компилятор.
static int Three() => 3;

static string Name(int value)
{
    switch (value)
    {
        case 0:
            return "zero ";
        case 1:
            return "one ";
        case 2:
            return "two ";
        default:
            return "many";
    }
}

static string Classify(int value)
{
    if (value < 0)
    {
        return "negative";
    }
    if (value == 0)
    {
        return "zero";
    }
    return "positive";
}
