// Встроенные массивы аргументов (фаза N11). Компилятор C# 13 собирает
// аргументы `params ReadOnlySpan<T>` (Task.WhenAll, string.Join и Concat с
// пятью и более строками, Path.Combine…) в одну из этих структур .NET 10:
// `[InlineArray(N)]` — одно объявленное поле и N ячеек подряд. Среда
// раскладывает такую структуру как N одинаковых полей (dispatch.rs), а
// ссылку на ячейку даёт Unsafe.As/Unsafe.Add (natives.rs).
//
// Файл порождён: пятнадцать одинаковых типов, InlineArray2…InlineArray16 —
// ровно те, что объявляет System.Private.CoreLib 10.0.5.

namespace System.Runtime.CompilerServices
{
    [InlineArray(2)]
    public struct InlineArray2<T>
    {
        private T _element0;
    }

    [InlineArray(3)]
    public struct InlineArray3<T>
    {
        private T _element0;
    }

    [InlineArray(4)]
    public struct InlineArray4<T>
    {
        private T _element0;
    }

    [InlineArray(5)]
    public struct InlineArray5<T>
    {
        private T _element0;
    }

    [InlineArray(6)]
    public struct InlineArray6<T>
    {
        private T _element0;
    }

    [InlineArray(7)]
    public struct InlineArray7<T>
    {
        private T _element0;
    }

    [InlineArray(8)]
    public struct InlineArray8<T>
    {
        private T _element0;
    }

    [InlineArray(9)]
    public struct InlineArray9<T>
    {
        private T _element0;
    }

    [InlineArray(10)]
    public struct InlineArray10<T>
    {
        private T _element0;
    }

    [InlineArray(11)]
    public struct InlineArray11<T>
    {
        private T _element0;
    }

    [InlineArray(12)]
    public struct InlineArray12<T>
    {
        private T _element0;
    }

    [InlineArray(13)]
    public struct InlineArray13<T>
    {
        private T _element0;
    }

    [InlineArray(14)]
    public struct InlineArray14<T>
    {
        private T _element0;
    }

    [InlineArray(15)]
    public struct InlineArray15<T>
    {
        private T _element0;
    }

    [InlineArray(16)]
    public struct InlineArray16<T>
    {
        private T _element0;
    }
}
