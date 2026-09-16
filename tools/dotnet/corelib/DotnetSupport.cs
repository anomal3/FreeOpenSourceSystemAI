// Опоры для исходников, взятых у dotnet/runtime как есть (фаза N10).
//
// Каталог `FromDotnet/` — файлы dotnet/runtime под MIT, байт в байт (только
// концы строк LF), с их копирайтом и `FromDotnet/LICENSE.TXT`. Откуда каждый:
//
//   PriorityQueue.cs, PriorityQueueDebugView.cs —
//     src/libraries/System.Collections/src/System/Collections/Generic/
//     коммит bb31474ef5f70cb926e3e7e10b99378cbb2811ea (2026-09-16)
//
// Правило пробы: чужой файл НЕ правится. Всё, чего ему не хватает, живёт здесь
// или дописано к нашим типам. Тогда новая версия из dotnet/runtime ложится
// поверх копированием, а цена переноса видна одним этим файлом. Правка
// чужого текста разошлась бы с оригиналом молча, и при следующем обновлении
// её пришлось бы искать сравнением.
//
// Всё здесь — не перенос, а своя запись: внутренние помощники System.Collections
// (SR, ThrowHelper, EnumerableHelpers) у них живут в других файлах и тянут за
// собой ресурсы и обобщённую арифметику. Тексты сообщений взяты из
// System.Collections/src/Resources/Strings.resx того же коммита: программа,
// печатающая `e.Message`, обязана напечатать то же, что под dotnet.

using System.Collections.Generic;

namespace System
{
    // Кортеж `(a, b)`. Компилятор пишет `ValueTuple<T1, T2>` в сигнатуры и
    // читает поля `Item1`/`Item2` напрямую — поэтому это поля, а не свойства,
    // как и в .NET. Без этого типа не собирается ни одна строка PriorityQueue
    // (CS8179 — 16 мест).
    public struct ValueTuple<T1, T2> : IEquatable<ValueTuple<T1, T2>>
    {
        public T1 Item1;
        public T2 Item2;

        public ValueTuple(T1 item1, T2 item2)
        {
            Item1 = item1;
            Item2 = item2;
        }

        public bool Equals(ValueTuple<T1, T2> other) =>
            EqualityComparer<T1>.Default.Equals(Item1, other.Item1) && EqualityComparer<T2>.Default.Equals(Item2, other.Item2);

        public override bool Equals(object obj) => obj is ValueTuple<T1, T2> other && Equals(other);

        // У .NET хэш кортежа перемешан случайным зерном процесса, так что
        // программа на конкретное число опираться не может; достаточно, чтобы
        // равные кортежи давали равный хэш.
        public override int GetHashCode()
        {
            int first = Item1 == null ? 0 : Item1.GetHashCode();
            int second = Item2 == null ? 0 : Item2.GetHashCode();
            return first * 31 + second;
        }

        public override string ToString() =>
            "(" + (Item1 == null ? "" : Item1.ToString()) + ", " + (Item2 == null ? "" : Item2.ToString()) + ")";
    }

    // Срез с записью. Устроен как наш `ReadOnlySpan<T>` (Span.cs): обычная
    // структура поверх массива, а не `ref struct` поверх указателя, — у
    // интерпретатора нет адресной арифметики, есть только места (`value.rs`).
    public readonly struct Span<T>
    {
        private readonly T[] array;
        private readonly int start;
        private readonly int length;

        public Span(T[] array, int start, int length)
        {
            if (array == null ? start != 0 || length != 0 : (uint)start > (uint)array.Length || (uint)length > (uint)(array.Length - start))
            {
                throw new ArgumentOutOfRangeException();
            }
            this.array = array;
            this.start = start;
            this.length = length;
        }

        public int Length => length;

        public bool IsEmpty => length == 0;

        public ref T this[int index]
        {
            get
            {
                if ((uint)index >= (uint)length)
                {
                    throw new IndexOutOfRangeException();
                }
                return ref array[start + index];
            }
        }

        public static implicit operator ReadOnlySpan<T>(Span<T> span) => new ReadOnlySpan<T>(span.array, span.start, span.length);
    }

    // Внутренние строки System.Collections — только те, что нужны перенесённым
    // файлам. У .NET это свойства над ресурсами; здесь ресурсов нет.
    internal static class SR
    {
        internal const string InvalidOperation_EmptyQueue = "Queue empty.";
        internal const string InvalidOperation_EnumFailedVersion = "Collection was modified after the enumerator was instantiated.";
        internal const string Arg_RankMultiDimNotSupported = "Only single dimensional arrays are supported for the requested action.";
        internal const string Arg_NonZeroLowerBound = "The lower bound of target array must be zero.";
        internal const string ArgumentOutOfRange_IndexMustBeLessOrEqual = "Index was out of range. Must be non-negative and less than or equal to the size of the collection.";
        internal const string Argument_InvalidOffLen = "Offset and length were out of bounds for the array or count is greater than the number of elements from index to the end of the source collection.";
        internal const string Argument_IncompatibleArrayType = "Target array type is not compatible with the type of items in the collection.";
    }
}

namespace System.Collections
{
    internal static class ThrowHelper
    {
        internal static void ThrowVersionCheckFailed() =>
            throw new InvalidOperationException(SR.InvalidOperation_EnumFailedVersion);
    }
}

namespace System.Collections.Generic
{
    // Своя запись Common/src/System/Collections/Generic/EnumerableHelpers.cs.
    // Поведение повторено там, где программа его видит: у пустой коллекции
    // массив нулевой длины (`Capacity` 0), у перечисления без `ICollection<T>`
    // рост 4, 8, 16… — `Capacity` очереди после конструктора из такого
    // перечисления печатается образцом.
    internal static class EnumerableHelpers
    {
        internal static IEnumerator<T> GetEmptyEnumerator<T>() => ((IEnumerable<T>)Array.Empty<T>()).GetEnumerator();

        internal static T[] ToArray<T>(IEnumerable<T> source, out int length)
        {
            if (source is ICollection<T> collection)
            {
                int count = collection.Count;
                if (count != 0)
                {
                    T[] items = new T[count];
                    collection.CopyTo(items, 0);
                    length = count;
                    return items;
                }
            }
            else
            {
                using (IEnumerator<T> e = source.GetEnumerator())
                {
                    if (e.MoveNext())
                    {
                        T[] items = new T[4];
                        items[0] = e.Current;
                        int count = 1;
                        while (e.MoveNext())
                        {
                            if (count == items.Length)
                            {
                                int grown = count << 1;
                                if ((uint)grown > (uint)Array.MaxLength)
                                {
                                    grown = Array.MaxLength <= count ? count + 1 : Array.MaxLength;
                                }
                                Array.Resize(ref items, grown);
                            }
                            items[count++] = e.Current;
                        }
                        length = count;
                        return items;
                    }
                }
            }
            length = 0;
            return Array.Empty<T>();
        }
    }
}

namespace System.Diagnostics
{
    // Вызовы `Debug.Assert` компилятор выбрасывает целиком, если символ DEBUG
    // не задан, — а corelib собирается `-c Release` (clrcheck.rs). Тело здесь
    // только на случай отладочной сборки.
    public static class Debug
    {
        [Conditional("DEBUG")]
        public static void Assert(bool condition)
        {
            if (!condition)
            {
                throw new InvalidOperationException("Debug.Assert failed.");
            }
        }

        [Conditional("DEBUG")]
        public static void Assert(bool condition, string message)
        {
            if (!condition)
            {
                throw new InvalidOperationException(message);
            }
        }
    }

    [AttributeUsage(AttributeTargets.Class | AttributeTargets.Method, AllowMultiple = true)]
    public sealed class ConditionalAttribute : Attribute
    {
        public ConditionalAttribute(string conditionString)
        {
            ConditionString = conditionString;
        }

        public string ConditionString { get; }
    }

    // Атрибуты отладчика среда не читает; они нужны, чтобы чужой текст собрался.
    [AttributeUsage(AttributeTargets.Class | AttributeTargets.Struct | AttributeTargets.Property | AttributeTargets.Field | AttributeTargets.Delegate | AttributeTargets.Enum | AttributeTargets.Assembly, AllowMultiple = true)]
    public sealed class DebuggerDisplayAttribute : Attribute
    {
        public DebuggerDisplayAttribute(string value)
        {
            Value = value;
        }

        public string Value { get; }
    }

    [AttributeUsage(AttributeTargets.Class | AttributeTargets.Struct | AttributeTargets.Assembly, AllowMultiple = true)]
    public sealed class DebuggerTypeProxyAttribute : Attribute
    {
        public DebuggerTypeProxyAttribute(Type type)
        {
        }
    }

    public enum DebuggerBrowsableState
    {
        Never = 0,
        Collapsed = 2,
        RootHidden = 3,
    }

    [AttributeUsage(AttributeTargets.Field | AttributeTargets.Property, AllowMultiple = false)]
    public sealed class DebuggerBrowsableAttribute : Attribute
    {
        public DebuggerBrowsableAttribute(DebuggerBrowsableState state)
        {
            State = state;
        }

        public DebuggerBrowsableState State { get; }
    }
}

namespace System.Diagnostics.CodeAnalysis
{
    [AttributeUsage(AttributeTargets.Parameter, Inherited = false)]
    public sealed class MaybeNullWhenAttribute : Attribute
    {
        public MaybeNullWhenAttribute(bool returnValue)
        {
            ReturnValue = returnValue;
        }

        public bool ReturnValue { get; }
    }
}

namespace System.Runtime.CompilerServices
{
    // Имена элементов кортежа `(TElement Element, TPriority Priority)` компилятор
    // записывает этим атрибутом; без типа он отказывается собирать (CS8137).
    [AttributeUsage(AttributeTargets.Class | AttributeTargets.Struct | AttributeTargets.Property | AttributeTargets.Field | AttributeTargets.Event | AttributeTargets.Parameter | AttributeTargets.ReturnValue)]
    public sealed class TupleElementNamesAttribute : Attribute
    {
        public TupleElementNamesAttribute(string[] transformNames)
        {
        }
    }

    // `ArgumentNullException.ThrowIfNull(items)` узнаёт имя параметра из этого
    // атрибута: без него `ParamName` был бы пуст, и сообщение разошлось бы с
    // dotnet на хвосте « (Parameter 'items')».
    [AttributeUsage(AttributeTargets.Parameter, AllowMultiple = false, Inherited = false)]
    public sealed class CallerArgumentExpressionAttribute : Attribute
    {
        public CallerArgumentExpressionAttribute(string parameterName)
        {
            ParameterName = parameterName;
        }

        public string ParameterName { get; }
    }
}

namespace System.Runtime.InteropServices
{
    // `ref readonly` у индексатора среза компилятор записывает модификатором
    // `modreq(InAttribute)`; разбор сигнатур модификаторы пропускает
    // (clr-meta sig.rs), но сам тип компилятору нужен.
    [AttributeUsage(AttributeTargets.Parameter, Inherited = false)]
    public sealed class InAttribute : Attribute
    {
    }
}
