// Опоры для исходников, взятых у dotnet/runtime как есть (фаза N10).
//
// Каталог `FromDotnet/` — файлы dotnet/runtime под MIT, байт в байт (только
// концы строк LF), с их копирайтом и `FromDotnet/LICENSE.TXT`. Откуда каждый:
//
//   PriorityQueue.cs, PriorityQueueDebugView.cs —
//     src/libraries/System.Collections/src/System/Collections/Generic/
//     коммит bb31474ef5f70cb926e3e7e10b99378cbb2811ea (2026-09-16)
//
//   Фаза N10b, тот же коммит:
//   LinkedList.cs, SortedList.cs, SortedDictionary.cs, SortedSet.cs,
//   SortedSet.TreeSubSet.cs, SortedSetEqualityComparer.cs, StackDebugView.cs,
//   Stack.cs — src/libraries/System.Collections/src/System/Collections/Generic/
//   Queue.cs, QueueDebugView.cs, ICollectionDebugView.cs, IDictionaryDebugView.cs,
//   DebugViewDictionaryItem.cs — src/libraries/System.Private.CoreLib/src/System/Collections/Generic/
//   BitHelper.cs — src/libraries/Common/src/System/Collections/Generic/
//   Obsoletions.cs — src/libraries/Common/src/System/
//
//   Фаза N10c, тот же коммит: List.cs, ArraySortHelper.cs —
//   src/libraries/System.Private.CoreLib/src/System/Collections/Generic/;
//   ReadOnlyCollection.cs, ReadOnlySet.cs —
//   src/libraries/System.Private.CoreLib/src/System/Collections/ObjectModel/
//
//   Фаза N10d: HashSetEqualityComparer.cs, InsertionBehavior.cs,
//   NonRandomizedStringEqualityComparer.cs, IInternalStringEqualityComparer.cs,
//   IAlternateEqualityComparer.cs (тот же коммит) и Dictionary.cs, HashSet.cs
//   (ТЕГ v10.0.5 — версия установленного .NET, с которым сверяется clr-check:
//   в main TrimExcess считает размер иначе) —
//   src/libraries/System.Private.CoreLib/src/System/Collections/Generic/;
//   HashHelpers.SerializationInfoTable.cs (тот же коммит) и HashHelpers.cs
//   (тег v10.0.5; в main он переехал в Common/) —
//   src/libraries/System.Private.CoreLib/src/System/Collections/
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
    public readonly ref struct Span<T>
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

        public Span(T[] array)
        {
            this.array = array;
            start = 0;
            length = array == null ? 0 : array.Length;
        }

        // `stackalloc T[n]` компилятор превращает в `localloc` и этот
        // конструктор. Адресов у интерпретатора нет: `localloc` кладёт на стек
        // ноль (vm.rs), а память среза — обычный массив в куче, обнулённый, как
        // стек .NET без SkipLocalsInit. Живёт он дольше кадра, но срез наружу не
        // уходит — компилятор это запрещает (ref struct у .NET).
        public unsafe Span(void* pointer, int length)
        {
            if (length < 0)
            {
                throw new ArgumentOutOfRangeException("length");
            }
            array = new T[length];
            start = 0;
            this.length = length;
        }

        public int Length => length;

        public bool IsEmpty => length == 0;

        public void Clear()
        {
            for (int i = 0; i < length; i++)
            {
                array[start + i] = default;
            }
        }

        public Span<T> Slice(int start) => Slice(start, length - start);

        public Span<T> Slice(int start, int length)
        {
            if ((uint)start > (uint)this.length || (uint)length > (uint)(this.length - start))
            {
                throw new ArgumentOutOfRangeException();
            }
            return new Span<T>(array, this.start + start, length);
        }

        public static implicit operator Span<T>(T[] array) => new Span<T>(array);

        // Ссылка на первый элемент — для MemoryMarshal.GetReference.
        internal ref T Reference => ref array[start];

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
        internal const string Arg_ArrayPlusOffTooSmall = "Destination array is not long enough to copy all the items in the collection. Check array index and length.";
        internal const string Arg_InsufficientSpace = "Insufficient space in the target location to copy the information.";
        internal const string Arg_KeyNotFoundWithKey = "The given key '{0}' was not present in the dictionary.";
        internal const string Arg_WrongType = "The value '{0}' is not of type '{1}' and cannot be used in this generic collection.";
        internal const string ArgumentOutOfRange_BiggerThanCollection = "Must be less than or equal to the size of the collection.";
        internal const string ArgumentOutOfRange_IndexMustBeLess = "Index was out of range. Must be non-negative and less than the size of the collection.";
        internal const string ArgumentOutOfRange_SmallCapacity = "capacity was less than the current size.";
        internal const string Argument_AddingDuplicate = "An item with the same key has already been added. Key: {0}";
        internal const string ExternalLinkedListNode = "The LinkedList node does not belong to current LinkedList.";
        internal const string InvalidOperation_EmptyStack = "Stack empty.";
        internal const string InvalidOperation_EnumOpCantHappen = "Enumeration has either not started or has already finished.";
        internal const string LinkedListEmpty = "The LinkedList is empty.";
        internal const string LinkedListNodeIsAttached = "The LinkedList node already belongs to a LinkedList.";
        internal const string NotSupported_KeyCollectionSet = "Mutating a key collection derived from a dictionary is not allowed.";
        internal const string NotSupported_SortedListNestedWrite = "This operation is not supported on SortedList nested types because they require modifying the original SortedList.";
        internal const string NotSupported_ValueCollectionSet = "Mutating a value collection derived from a dictionary is not allowed.";
        internal const string Serialization_InvalidOnDeser = "OnDeserialization method was called while the object was not being deserialized.";
        internal const string Serialization_MismatchedCount = "The serialized Count information doesn't match the number of items.";
        internal const string Serialization_MissingValues = "The values for this dictionary are missing.";
        internal const string SortedSet_LowerValueGreaterThanUpperValue = "Must be less than or equal to upperValue.";

        internal const string Arg_BogusIComparer = "Unable to sort because the IComparer.Compare() method returns inconsistent results. Either a value does not compare equal to itself, or one value repeatedly compared to another value yields different results. IComparer: '{0}'.";
        internal const string ArgumentOutOfRange_Count = "Count must be positive and count must refer to a location within the string/array/collection.";
        internal const string ArgumentOutOfRange_ListInsert = "Index must be within the bounds of the List.";
        internal const string ArgumentOutOfRange_NeedNonNegNum = "Non-negative number required.";
        internal const string InvalidOperation_IComparerFailed = "Failed to compare two elements in the array.";
        internal const string NotSupported_ReadOnlyCollection = "Collection is read-only.";

        // Фаза N10d: HashHelpers.cs (Common) зовёт SR напрямую.
        internal const string Arg_HTCapacityOverflow = "Hashtable's capacity overflowed and went negative. Check load factor, capacity and the current size of the table.";

        // У .NET это string.Format с текущей культурой; культур у среды нет,
        // и форматирование здесь всегда инвариантное.
        internal static string Format(string resourceFormat, object p1) => string.Format(resourceFormat, p1);

        internal static string Format(string resourceFormat, object p1, object p2) => string.Format(resourceFormat, p1, p2);
    }

    // Имена аргументов и ресурсов, которыми говорит ThrowHelper из CoreLib
    // (Queue.cs). Только те, что нужны перенесённым файлам.
    internal enum ExceptionArgument
    {
        action,
        array,
        arrayIndex,
        capacity,
        collection,
        comparison,
        converter,
        count,
        dictionary,
        index,
        info,
        item,
        key,
        list,
        match,
        other,
        startIndex,
        value,
    }

    internal enum ExceptionResource
    {
        Arg_ArrayPlusOffTooSmall,
        Arg_NonZeroLowerBound,
        Arg_RankMultiDimNotSupported,
        ArgumentOutOfRange_BiggerThanCollection,
        ArgumentOutOfRange_Count,
        ArgumentOutOfRange_IndexMustBeLess,
        ArgumentOutOfRange_IndexMustBeLessOrEqual,
        ArgumentOutOfRange_ListInsert,
        ArgumentOutOfRange_NeedNonNegNum,
        ArgumentOutOfRange_SmallCapacity,
        Argument_InvalidOffLen,
        InvalidOperation_IComparerFailed,
        InvalidOperation_IncompatibleComparer,
        NotSupported_KeyCollectionSet,
        NotSupported_ReadOnlyCollection,
        NotSupported_ValueCollectionSet,
        Serialization_MissingKeys,
        Serialization_NullKey,
    }

    public class PlatformNotSupportedException : NotSupportedException
    {
        public PlatformNotSupportedException()
            : base("Operation is not supported on this platform.")
        {
        }

        public PlatformNotSupportedException(string message)
            : base(message)
        {
        }
    }
}

namespace System.Collections
{
    internal static class ThrowHelper
    {
        internal static void ThrowVersionCheckFailed() =>
            throw new InvalidOperationException(SR.InvalidOperation_EnumFailedVersion);

        // У CoreLib и System.Collections бывают разные тексты под одним именем
        // ресурса, а класс SR здесь один. В SR лежат тексты System.Collections:
        // их файлы зовут SR напрямую. Файлы CoreLib идут через ThrowHelper — и
        // тексты CoreLib, расходящиеся с ними, стоят тут.
        private const string CoreLibEnumFailedVersion = "Collection was modified; enumeration operation may not execute.";
        private const string CoreLibWrongType = "The value \"{0}\" is not of type \"{1}\" and cannot be used in this generic collection.";
        private const string CoreLibBiggerThanCollection = "Larger than collection size.";

        internal static void ThrowInvalidOperationException_InvalidOperation_EnumFailedVersion() =>
            throw new InvalidOperationException(CoreLibEnumFailedVersion);

        internal static void ThrowArgumentException_Argument_IncompatibleArrayType() =>
            throw new ArgumentException(SR.Argument_IncompatibleArrayType);

        internal static void ThrowArgumentOutOfRange_IndexMustBeLessOrEqualException() =>
            throw new ArgumentOutOfRangeException("index", SR.ArgumentOutOfRange_IndexMustBeLessOrEqual);

        internal static void ThrowArgumentOutOfRangeException(ExceptionArgument argument, ExceptionResource resource) =>
            throw new ArgumentOutOfRangeException(argument.ToString(), Resource(resource));

        internal static void ThrowArgumentException(ExceptionResource resource) =>
            throw new ArgumentException(Resource(resource));

        internal static void ThrowArgumentException(ExceptionResource resource, ExceptionArgument argument) =>
            throw new ArgumentException(Resource(resource), argument.ToString());

        private static string Resource(ExceptionResource resource) => resource switch
        {
            ExceptionResource.Arg_ArrayPlusOffTooSmall => SR.Arg_ArrayPlusOffTooSmall,
            ExceptionResource.Arg_NonZeroLowerBound => SR.Arg_NonZeroLowerBound,
            ExceptionResource.Arg_RankMultiDimNotSupported => SR.Arg_RankMultiDimNotSupported,
            ExceptionResource.ArgumentOutOfRange_BiggerThanCollection => CoreLibBiggerThanCollection,
            ExceptionResource.ArgumentOutOfRange_Count => SR.ArgumentOutOfRange_Count,
            ExceptionResource.ArgumentOutOfRange_IndexMustBeLess => SR.ArgumentOutOfRange_IndexMustBeLess,
            ExceptionResource.ArgumentOutOfRange_IndexMustBeLessOrEqual => SR.ArgumentOutOfRange_IndexMustBeLessOrEqual,
            ExceptionResource.ArgumentOutOfRange_ListInsert => SR.ArgumentOutOfRange_ListInsert,
            ExceptionResource.ArgumentOutOfRange_NeedNonNegNum => SR.ArgumentOutOfRange_NeedNonNegNum,
            ExceptionResource.ArgumentOutOfRange_SmallCapacity => SR.ArgumentOutOfRange_SmallCapacity,
            ExceptionResource.Argument_InvalidOffLen => SR.Argument_InvalidOffLen,
            ExceptionResource.InvalidOperation_IComparerFailed => SR.InvalidOperation_IComparerFailed,
            ExceptionResource.NotSupported_ReadOnlyCollection => SR.NotSupported_ReadOnlyCollection,
            ExceptionResource.InvalidOperation_IncompatibleComparer => CoreLibIncompatibleComparer,
            ExceptionResource.NotSupported_KeyCollectionSet => SR.NotSupported_KeyCollectionSet,
            ExceptionResource.NotSupported_ValueCollectionSet => SR.NotSupported_ValueCollectionSet,
            ExceptionResource.Serialization_MissingKeys => CoreLibSerializationMissingKeys,
            ExceptionResource.Serialization_NullKey => CoreLibSerializationNullKey,
            _ => resource.ToString(),
        };

        // Члены ThrowHelper из CoreLib, которыми пользуются List и
        // ReadOnlyCollection (фаза N10c). Тела — как у .NET (ThrowHelper.cs того
        // же коммита): имя аргумента — имя элемента перечисления.
        internal static void IfNullAndNullsAreIllegalThenThrow<T>(object value, ExceptionArgument argName)
        {
            if (!(default(T) == null) && value == null)
            {
                ThrowArgumentNullException(argName);
            }
        }

        internal static void ThrowArgumentException_BadComparer(object comparer) =>
            throw new ArgumentException(SR.Format(SR.Arg_BogusIComparer, comparer));

        internal static void ThrowArgumentNullException(ExceptionArgument argument) =>
            throw new ArgumentNullException(argument.ToString());

        internal static void ThrowArgumentOutOfRange_IndexMustBeLessException() =>
            throw new ArgumentOutOfRangeException(nameof(ExceptionArgument.index), SR.ArgumentOutOfRange_IndexMustBeLess);

        internal static void ThrowCountArgumentOutOfRange_ArgumentOutOfRange_Count() =>
            throw new ArgumentOutOfRangeException(nameof(ExceptionArgument.count), SR.ArgumentOutOfRange_Count);

        internal static void ThrowIndexArgumentOutOfRange_NeedNonNegNumException() =>
            throw new ArgumentOutOfRangeException(nameof(ExceptionArgument.index), SR.ArgumentOutOfRange_NeedNonNegNum);

        internal static void ThrowInvalidOperationException() => throw new InvalidOperationException();

        internal static void ThrowInvalidOperationException(ExceptionResource resource) =>
            throw new InvalidOperationException(Resource(resource));

        internal static void ThrowInvalidOperationException(ExceptionResource resource, Exception e) =>
            throw new InvalidOperationException(Resource(resource), e);

        internal static void ThrowInvalidOperationException_InvalidOperation_EnumOpCantHappen() =>
            throw new InvalidOperationException(SR.InvalidOperation_EnumOpCantHappen);

        internal static void ThrowNotSupportedException() => throw new NotSupportedException();

        internal static void ThrowNotSupportedException(ExceptionResource resource) =>
            throw new NotSupportedException(Resource(resource));

        internal static void ThrowStartIndexArgumentOutOfRange_ArgumentOutOfRange_IndexMustBeLess() =>
            throw new ArgumentOutOfRangeException(nameof(ExceptionArgument.startIndex), SR.ArgumentOutOfRange_IndexMustBeLess);

        internal static void ThrowStartIndexArgumentOutOfRange_ArgumentOutOfRange_IndexMustBeLessOrEqual() =>
            throw new ArgumentOutOfRangeException(nameof(ExceptionArgument.startIndex), SR.ArgumentOutOfRange_IndexMustBeLessOrEqual);

        internal static void ThrowWrongValueTypeArgumentException<T>(T value, Type targetType) =>
            throw new ArgumentException(SR.Format(CoreLibWrongType, (object)value, targetType), nameof(value));

        // Члены ThrowHelper из CoreLib, которыми пользуются Dictionary и HashSet
        // (фаза N10d). Тексты — Strings.resx CoreLib того же коммита; имена без
        // близнеца в System.Collections лежат здесь, а не в SR.
        private const string CoreLibConcurrentOperations = "Operations that change non-concurrent collections must have exclusive access. A concurrent update was performed on this collection and corrupted its state. The collection's state is no longer correct.";
        private const string CoreLibIncompatibleComparer = "The collection's comparer does not support the requested operation.";
        private const string CoreLibSerializationMissingKeys = "The Keys for this Hashtable are missing.";
        private const string CoreLibSerializationNullKey = "One of the serialized keys is null.";

        internal static void ThrowInvalidOperationException_ConcurrentOperationsNotSupported() =>
            throw new InvalidOperationException(CoreLibConcurrentOperations);

        internal static void ThrowArgumentOutOfRangeException_NeedNonNegNum(string paramName) =>
            throw new ArgumentOutOfRangeException(paramName, SR.ArgumentOutOfRange_NeedNonNegNum);

        internal static void ThrowArgumentOutOfRangeException(ExceptionArgument argument) =>
            throw new ArgumentOutOfRangeException(argument.ToString());

        internal static void ThrowKeyNotFoundException<T>(T key) =>
            throw new Generic.KeyNotFoundException(SR.Format(SR.Arg_KeyNotFoundWithKey, (object)key));

        internal static void ThrowWrongKeyTypeArgumentException<T>(T key, Type targetType) =>
            throw new ArgumentException(SR.Format(CoreLibWrongType, (object)key, targetType), nameof(key));

        internal static void ThrowAddingDuplicateWithKeyArgumentException<T>(T key) =>
            throw new ArgumentException(SR.Format(SR.Argument_AddingDuplicate, (object)key));

        internal static void ThrowSerializationException(ExceptionResource resource) =>
            throw new Runtime.Serialization.SerializationException(Resource(resource));
    }
}

namespace System.Collections.Generic
{
    // Своя запись Common/src/System/Collections/Generic/EnumerableHelpers.cs.
    // Поведение повторено там, где программа его видит: у пустой коллекции
    // массив нулевой длины (`Capacity` 0), у перечисления без `ICollection<T>`
    // рост 4, 8, 16… — `Capacity` очереди после конструктора из такого
    // перечисления печатается образцом.
    // У CoreLib это перечислитель массива с общим пустым экземпляром; Queue
    // отдаёт его, когда очередь пуста. Нужен только пустой.
    internal sealed class SZGenericArrayEnumerator<T> : IEnumerator<T>
    {
        internal static readonly SZGenericArrayEnumerator<T> Empty = new SZGenericArrayEnumerator<T>();

        private SZGenericArrayEnumerator()
        {
        }

        public T Current => throw new InvalidOperationException(SR.InvalidOperation_EnumOpCantHappen);

        object System.Collections.IEnumerator.Current => Current;

        public bool MoveNext() => false;

        public void Reset()
        {
        }

        public void Dispose()
        {
        }
    }

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
        public static void Fail(string message) => throw new InvalidOperationException(message);

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

        public string Name { get; set; }
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
    public sealed class NotNullWhenAttribute : Attribute
    {
        public NotNullWhenAttribute(bool returnValue)
        {
            ReturnValue = returnValue;
        }

        public bool ReturnValue { get; }
    }

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

// ---- Фаза N10b: коллекции System.Collections и очередь CoreLib ----
//
// LinkedList, Stack, Queue, SortedList, SortedDictionary и SortedSet взяты тем
// же путём, что PriorityQueue. Им не хватило словарных и множественных
// интерфейсов (у нас их не было вовсе: Dictionary написан без IDictionary),
// атрибутов и интерфейсов двоичной сериализации — сами форматтеры в .NET давно
// устарели, но типы по-прежнему помечены `[Serializable]` и реализуют
// `ISerializable`, — и нескольких помощников.

namespace System
{
    [AttributeUsage(AttributeTargets.Class | AttributeTargets.Struct | AttributeTargets.Enum | AttributeTargets.Delegate, Inherited = false)]
    public sealed class SerializableAttribute : Attribute
    {
    }

    [AttributeUsage(AttributeTargets.Field, Inherited = false)]
    public sealed class NonSerializedAttribute : Attribute
    {
    }

    [AttributeUsage(AttributeTargets.Class | AttributeTargets.Struct | AttributeTargets.Enum | AttributeTargets.Constructor | AttributeTargets.Method | AttributeTargets.Property | AttributeTargets.Field | AttributeTargets.Event | AttributeTargets.Interface | AttributeTargets.Delegate, Inherited = false)]
    public sealed class ObsoleteAttribute : Attribute
    {
        public ObsoleteAttribute()
        {
        }

        public ObsoleteAttribute(string message)
        {
            Message = message;
        }

        public ObsoleteAttribute(string message, bool error)
        {
            Message = message;
            IsError = error;
        }

        public string Message { get; }

        public bool IsError { get; }

        public string DiagnosticId { get; set; }

        public string UrlFormat { get; set; }
    }
}

namespace System.ComponentModel
{
    public enum EditorBrowsableState
    {
        Always = 0,
        Never = 1,
        Advanced = 2,
    }

    [AttributeUsage(AttributeTargets.Class | AttributeTargets.Struct | AttributeTargets.Enum | AttributeTargets.Constructor | AttributeTargets.Method | AttributeTargets.Property | AttributeTargets.Field | AttributeTargets.Event | AttributeTargets.Interface | AttributeTargets.Delegate)]
    public sealed class EditorBrowsableAttribute : Attribute
    {
        public EditorBrowsableAttribute(EditorBrowsableState state)
        {
            State = state;
        }

        public EditorBrowsableState State { get; }
    }
}

namespace System.Runtime.CompilerServices
{
    // Выражения коллекций `[a, b]` для ReadOnlyCollection (фаза N10c): компилятор
    // читает атрибут, среде он не нужен.
    [AttributeUsage(AttributeTargets.Class | AttributeTargets.Struct | AttributeTargets.Interface, Inherited = false)]
    public sealed class CollectionBuilderAttribute : Attribute
    {
        public CollectionBuilderAttribute(Type builderType, string methodName)
        {
            BuilderType = builderType;
            MethodName = methodName;
        }

        public Type BuilderType { get; }

        public string MethodName { get; }
    }

    [AttributeUsage(AttributeTargets.Class | AttributeTargets.Struct | AttributeTargets.Enum | AttributeTargets.Interface | AttributeTargets.Delegate, Inherited = false)]
    public sealed class TypeForwardedFromAttribute : Attribute
    {
        public TypeForwardedFromAttribute(string assemblyFullName)
        {
            AssemblyFullName = assemblyFullName;
        }

        public string AssemblyFullName { get; }
    }
}

namespace System.Runtime.Serialization
{
    // Двоичной сериализации у среды нет. Типы нужны перенесённым коллекциям: они
    // реализуют ISerializable, и без этих имён текст не собирается.
    // SerializationInfo хранит значения по имени — этого хватает, чтобы
    // GetObjectData и OnDeserialization коллекций работали, если их позовут
    // напрямую.
    public interface ISerializable
    {
        void GetObjectData(SerializationInfo info, StreamingContext context);
    }

    public interface IDeserializationCallback
    {
        void OnDeserialization(object sender);
    }

    public readonly struct StreamingContext
    {
    }

    public sealed class SerializationInfo
    {
        private readonly System.Collections.Generic.Dictionary<string, object> values = new System.Collections.Generic.Dictionary<string, object>();

        public int MemberCount => values.Count;

        public void AddValue(string name, object value) => values.Add(name, value);

        public void AddValue(string name, object value, Type type) => values.Add(name, value);

        public void AddValue(string name, int value) => values.Add(name, value);

        public object GetValue(string name, Type type)
        {
            if (!values.TryGetValue(name, out object value))
            {
                throw new SerializationException("Member '" + name + "' was not found.");
            }
            return value;
        }

        public int GetInt32(string name) => (int)GetValue(name, typeof(int));

        // Тип, под которым объект был бы записан (фаза N10d,
        // NonRandomizedStringEqualityComparer пишет себя как GenericEqualityComparer).
        public Type ObjectType { get; private set; }

        public void SetType(Type type)
        {
            if (type == null)
            {
                throw new ArgumentNullException("type");
            }
            ObjectType = type;
        }
    }

    public class SerializationException : SystemException
    {
        public SerializationException(string message)
            : base(message)
        {
        }
    }
}

namespace System.Numerics
{
    public static class BitOperations
    {
        public static int Log2(uint value)
        {
            int log = 0;
            while ((value >>= 1) != 0)
            {
                log++;
            }
            return log;
        }

        // Хеш строк (фаза N10d, String.cs) вращает слово на пять бит.
        public static uint RotateLeft(uint value, int offset) => (value << offset) | (value >> (32 - offset));
    }
}

namespace System.Collections
{
    public interface IDictionary : ICollection
    {
        object this[object key] { get; set; }

        ICollection Keys { get; }

        ICollection Values { get; }

        bool IsReadOnly { get; }

        bool IsFixedSize { get; }

        bool Contains(object key);

        void Add(object key, object value);

        void Clear();

        new IDictionaryEnumerator GetEnumerator();

        void Remove(object key);
    }

    public interface IDictionaryEnumerator : IEnumerator
    {
        object Key { get; }

        object Value { get; }

        DictionaryEntry Entry { get; }
    }

    public struct DictionaryEntry
    {
        private object _key;
        private object _value;

        public DictionaryEntry(object key, object value)
        {
            _key = key;
            _value = value;
        }

        public object Key
        {
            get => _key;
            set => _key = value;
        }

        public object Value
        {
            get => _value;
            set => _value = value;
        }

        public void Deconstruct(out object key, out object value)
        {
            key = _key;
            value = _value;
        }

        public override string ToString() => "[" + _key + ", " + _value + "]";
    }
}

namespace System.Collections.Generic
{
    public interface IDictionary<TKey, TValue> : ICollection<KeyValuePair<TKey, TValue>>
    {
        TValue this[TKey key] { get; set; }

        ICollection<TKey> Keys { get; }

        ICollection<TValue> Values { get; }

        bool ContainsKey(TKey key);

        void Add(TKey key, TValue value);

        bool Remove(TKey key);

        bool TryGetValue(TKey key, [System.Diagnostics.CodeAnalysis.MaybeNullWhen(false)] out TValue value);
    }

    public interface IReadOnlyDictionary<TKey, TValue> : IReadOnlyCollection<KeyValuePair<TKey, TValue>>
    {
        TValue this[TKey key] { get; }

        IEnumerable<TKey> Keys { get; }

        IEnumerable<TValue> Values { get; }

        bool ContainsKey(TKey key);

        bool TryGetValue(TKey key, [System.Diagnostics.CodeAnalysis.MaybeNullWhen(false)] out TValue value);
    }

    public interface ISet<T> : ICollection<T>
    {
        new bool Add(T item);

        void UnionWith(IEnumerable<T> other);

        void IntersectWith(IEnumerable<T> other);

        void ExceptWith(IEnumerable<T> other);

        void SymmetricExceptWith(IEnumerable<T> other);

        bool IsSubsetOf(IEnumerable<T> other);

        bool IsSupersetOf(IEnumerable<T> other);

        bool IsProperSupersetOf(IEnumerable<T> other);

        bool IsProperSubsetOf(IEnumerable<T> other);

        bool Overlaps(IEnumerable<T> other);

        bool SetEquals(IEnumerable<T> other);
    }

    public interface IReadOnlySet<T> : IReadOnlyCollection<T>
    {
        bool Contains(T item);

        bool IsProperSubsetOf(IEnumerable<T> other);

        bool IsProperSupersetOf(IEnumerable<T> other);

        bool IsSubsetOf(IEnumerable<T> other);

        bool IsSupersetOf(IEnumerable<T> other);

        bool Overlaps(IEnumerable<T> other);

        bool SetEquals(IEnumerable<T> other);
    }
}

// ---- Фаза N10c: List<T>, ReadOnlyCollection<T>, ReadOnlySet<T> и сортировка ----
//
// Взяты из CoreLib тем же путём: List.cs, ArraySortHelper.cs,
// ReadOnlyCollection.cs, ReadOnlySet.cs. Рукописные List<T> и сортировка
// удалены. ArraySortHelper.CoreCLR.cs не взят: помощника для типов с
// IComparable<T> он создаёт рефлексией среды
// (CreateInstanceForAnotherGenericParameter). Здесь помощник один — через
// сравнитель; алгоритм у обоих один и тот же, и порядок выходит тот же.

namespace System.Collections.Generic
{
    internal interface IArraySortHelper<TKey>
    {
        void Sort(Span<TKey> keys, IComparer<TKey> comparer);

        int BinarySearch(TKey[] keys, int index, int length, TKey value, IComparer<TKey> comparer);
    }

    internal sealed partial class ArraySortHelper<T> : IArraySortHelper<T>
    {
        private static readonly IArraySortHelper<T> s_defaultArraySortHelper = new ArraySortHelper<T>();

        public static IArraySortHelper<T> Default => s_defaultArraySortHelper;
    }

    internal sealed partial class GenericArraySortHelper<T> : IArraySortHelper<T>
    {
    }

    internal interface IArraySortHelper<TKey, TValue>
    {
        void Sort(Span<TKey> keys, Span<TValue> values, IComparer<TKey> comparer);
    }

    internal sealed partial class ArraySortHelper<TKey, TValue> : IArraySortHelper<TKey, TValue>
    {
        private static readonly IArraySortHelper<TKey, TValue> s_defaultArraySortHelper = new ArraySortHelper<TKey, TValue>();

        public static IArraySortHelper<TKey, TValue> Default => s_defaultArraySortHelper;
    }

    internal sealed partial class GenericArraySortHelper<TKey, TValue> : IArraySortHelper<TKey, TValue>
    {
    }
}

namespace System.Collections
{
    // Common/src/System/Collections/Generic/CollectionHelpers.cs у .NET —
    // проверки ICollection.CopyTo; тексты из тех же ресурсов.
    internal static class CollectionHelpers
    {
        internal static void CopyTo<T>(ICollection<T> collection, Array array, int index)
        {
            if (array == null)
            {
                throw new ArgumentNullException("array");
            }
            if (array.Rank != 1)
            {
                throw new ArgumentException(SR.Arg_RankMultiDimNotSupported, "array");
            }
            if (array.GetLowerBound(0) != 0)
            {
                throw new ArgumentException(SR.Arg_NonZeroLowerBound, "array");
            }
            if (index < 0 || index > array.Length)
            {
                throw new ArgumentOutOfRangeException("index", SR.ArgumentOutOfRange_NeedNonNegNum);
            }
            if (array.Length - index < collection.Count)
            {
                throw new ArgumentException(SR.Arg_ArrayPlusOffTooSmall);
            }
            if (array is T[] items)
            {
                collection.CopyTo(items, index);
                return;
            }
            if (array is object[] objects)
            {
                try
                {
                    foreach (T item in collection)
                    {
                        objects[index++] = item;
                    }
                }
                catch (ArrayTypeMismatchException)
                {
                    throw new ArgumentException(SR.Argument_IncompatibleArrayType, "array");
                }
                return;
            }
            throw new ArgumentException(SR.Argument_IncompatibleArrayType, "array");
        }
    }
}

namespace System
{
    // `typeof(T) == typeof(Half)` у сортировки — ради правил NaN. Чисел
    // половинной точности у среды нет, и значений этого типа не бывает; тип
    // нужен, чтобы сравнение собралось, и оператор — чтобы собралась ветка.
    public readonly struct Half
    {
        public static bool operator <(Half left, Half right) => false;

        public static bool operator >(Half left, Half right) => false;

        public static bool IsNaN(Half value) => false;
    }

    // `^1` и `a..b`: компилятору нужны типы. Своя запись, а не Index.cs из
    // dotnet/runtime — тот форматирует себя в срез через TryFormat.
    public readonly struct Index : IEquatable<Index>
    {
        private readonly int _value;

        public Index(int value, bool fromEnd = false)
        {
            if (value < 0)
            {
                throw new ArgumentOutOfRangeException("value", "Non-negative number required.");
            }
            _value = fromEnd ? ~value : value;
        }

        private Index(int value, int raw)
        {
            _value = raw;
        }

        public static Index Start => new Index(0);

        public static Index End => new Index(0, true);

        public static Index FromStart(int value) => new Index(value);

        public static Index FromEnd(int value) => new Index(value, true);

        public int Value => _value < 0 ? ~_value : _value;

        public bool IsFromEnd => _value < 0;

        public int GetOffset(int length) => _value < 0 ? length + _value + 1 : _value;

        public override bool Equals(object value) => value is Index index && _value == index._value;

        public bool Equals(Index other) => _value == other._value;

        public override int GetHashCode() => _value;

        public static implicit operator Index(int value) => FromStart(value);

        public override string ToString() => IsFromEnd ? "^" + Value.ToString() : Value.ToString();
    }

    public readonly struct Range : IEquatable<Range>
    {
        public Index Start { get; }

        public Index End { get; }

        public Range(Index start, Index end)
        {
            Start = start;
            End = end;
        }

        public static Range StartAt(Index start) => new Range(start, Index.End);

        public static Range EndAt(Index end) => new Range(Index.Start, end);

        public static Range All => new Range(Index.Start, Index.End);

        public (int Offset, int Length) GetOffsetAndLength(int length)
        {
            int start = Start.GetOffset(length);
            int end = End.GetOffset(length);
            if ((uint)end > (uint)length || (uint)start > (uint)end)
            {
                throw new ArgumentOutOfRangeException("length");
            }
            return (start, end - start);
        }

        public override bool Equals(object value) => value is Range range && range.Start.Equals(Start) && range.End.Equals(End);

        public bool Equals(Range other) => other.Start.Equals(Start) && other.End.Equals(End);

        public override int GetHashCode() => Start.GetHashCode() * 31 + End.GetHashCode();

        public override string ToString() => Start.ToString() + ".." + End.ToString();
    }
}

namespace System.Runtime.CompilerServices
{
    // Ссылки на элементы массива (фаза N10c). У интерпретатора ссылка — место
    // (value.rs), а не адрес: сдвиг и сравнение понятны только у элементов
    // одного массива, и среда проверяет это сама (natives.rs). Для остальных
    // мест — отказ, а не выдуманный адрес.
    public static class Unsafe
    {
        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern ref T Add<T>(ref T source, int elementOffset);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern bool AreSame<T>(ref T left, ref T right);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern bool IsAddressLessThan<T>(ref T left, ref T right);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern bool IsAddressGreaterThan<T>(ref T left, ref T right);

        public static bool IsAddressGreaterThanOrEqualTo<T>(ref T left, ref T right) => !IsAddressLessThan(ref left, ref right);

        public static bool IsAddressLessThanOrEqualTo<T>(ref T left, ref T right) => !IsAddressGreaterThan(ref left, ref right);

        // Расстояние в байтах: число элементов, умноженное на тот же размер, что
        // даёт `sizeof(T)` у среды, — частное выходит номером элемента.
        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern IntPtr ByteOffset<T>(ref T origin, ref T target);

        // Ссылка в никуда (фаза N10d): Dictionary и HashSet из CoreLib отдают
        // `ref` на найденное значение, а «не найдено» у них — пустая ссылка. У
        // среды это отдельный вид места (`Pointer::Null`, value.rs): чтение и
        // запись через него — NullReferenceException, сравнение — только здесь.
        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern ref T NullRef<T>();

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern bool IsNullRef<T>(ref readonly T source);

        // У .NET приведение без проверки. Здесь — обычное приведение: значения
        // среды несут свой тип, и подменить его нечем; единственное место вызова
        // (AlternateLookup) проверяет тип строкой раньше через `is`.
        public static T As<T>(object o) where T : class => (T)o;
    }

    // `ref readonly` у параметра компилятор помечает этим атрибутом; тип нужен,
    // чтобы `IsNullRef(in …)` из HashSet собрался.
    [AttributeUsage(AttributeTargets.Parameter, Inherited = false)]
    public sealed class RequiresLocationAttribute : Attribute
    {
    }

    // `allows ref struct` у параметра типа (AlternateLookup) и статический
    // метод в интерфейсе (IInternalStringEqualityComparer) компилятор разрешает
    // только среде, объявившей эти возможности (CS8701, CS9500).
    public static class RuntimeFeature
    {
        public const string ByRefLikeGenerics = nameof(ByRefLikeGenerics);

        public const string DefaultImplementationsOfInterfaces = nameof(DefaultImplementationsOfInterfaces);

        public static bool IsSupported(string feature) => feature == ByRefLikeGenerics || feature == DefaultImplementationsOfInterfaces;
    }

    // Таблица «объект → данные» для конструктора десериализации Dictionary и
    // HashSet (HashHelpers.SerializationInfoTable). У .NET ключ держится слабо и
    // запись уходит вместе с объектом; здесь — список пар по ссылке: форматтера
    // нет, тот конструктор никто не зовёт, и таблица остаётся пустой.
    public sealed class ConditionalWeakTable<TKey, TValue>
        where TKey : class
        where TValue : class
    {
        private readonly List<KeyValuePair<TKey, TValue>> pairs = new List<KeyValuePair<TKey, TValue>>();

        public void Add(TKey key, TValue value)
        {
            if (key == null)
            {
                throw new ArgumentNullException("key");
            }
            if (TryGetValue(key, out _))
            {
                throw new ArgumentException("Key already exists.");
            }
            pairs.Add(new KeyValuePair<TKey, TValue>(key, value));
        }

        public bool TryGetValue(TKey key, out TValue value)
        {
            for (int i = 0; i < pairs.Count; i++)
            {
                if (ReferenceEquals(pairs[i].Key, key))
                {
                    value = pairs[i].Value;
                    return true;
                }
            }
            value = null;
            return false;
        }

        public bool Remove(TKey key)
        {
            for (int i = 0; i < pairs.Count; i++)
            {
                if (ReferenceEquals(pairs[i].Key, key))
                {
                    pairs.RemoveAt(i);
                    return true;
                }
            }
            return false;
        }
    }
}

namespace System.Runtime.InteropServices
{
    public static class MemoryMarshal
    {
        public static ref T GetReference<T>(Span<T> span) => ref span.Reference;

        public static ref readonly T GetReference<T>(ReadOnlySpan<T> span) => ref span[0];
    }
}

// ---- Фаза N10d: Dictionary<TKey, TValue> и HashSet<T> из CoreLib ----
//
// Взяты тем же путём: Dictionary.cs, HashSet.cs, HashSetEqualityComparer.cs,
// InsertionBehavior.cs, NonRandomizedStringEqualityComparer.cs,
// IInternalStringEqualityComparer.cs, IAlternateEqualityComparer.cs,
// HashHelpers.SerializationInfoTable.cs —
// src/libraries/System.Private.CoreLib/src/System/Collections/(Generic/);
// HashHelpers.cs — src/libraries/Common/src/System/Collections/. Рукописные
// Dictionary и HashSet удалены. Что это даёт программе: порядок обхода после
// удалений и повторных вставок (список свободных записей), простые размеры
// таблиц (`EnsureCapacity` возвращает их), поиск по срезу знаков без
// строки (`GetAlternateLookup<ReadOnlySpan<char>>`), `CollectionsMarshal`.
//
// Не взято: BitArray.cs — он весь на Vector128/256/512 и переосмыслении int[]
// как байтов (MemoryMarshal.Cast), это модель памяти, а не файл. Из-за него не
// взят и CollectionsMarshal.cs: его AsBytes(BitArray) читает поле BitArray.
// Остальные члены CollectionsMarshal повторены здесь дословно.

namespace System.Runtime.InteropServices
{
    public static class CollectionsMarshal
    {
        public static Span<T> AsSpan<T>(List<T> list)
        {
            Span<T> span = default;
            if (list != null)
            {
                int size = list._size;
                T[] items = list._items;
                if ((uint)size > (uint)items.Length)
                {
                    System.Collections.ThrowHelper.ThrowInvalidOperationException_ConcurrentOperationsNotSupported();
                }
                span = new Span<T>(items, 0, size);
            }
            return span;
        }

        public static ref TValue GetValueRefOrNullRef<TKey, TValue>(Dictionary<TKey, TValue> dictionary, TKey key)
            => ref dictionary.FindValue(key);

        public static ref TValue GetValueRefOrNullRef<TKey, TValue, TAlternateKey>(Dictionary<TKey, TValue>.AlternateLookup<TAlternateKey> dictionary, TAlternateKey key)
            where TAlternateKey : allows ref struct
            => ref dictionary.FindValue(key, out _);

        public static ref TValue GetValueRefOrAddDefault<TKey, TValue>(Dictionary<TKey, TValue> dictionary, TKey key, out bool exists)
            => ref Dictionary<TKey, TValue>.CollectionsMarshalHelper.GetValueRefOrAddDefault(dictionary, key, out exists);

        public static ref TValue GetValueRefOrAddDefault<TKey, TValue, TAlternateKey>(Dictionary<TKey, TValue>.AlternateLookup<TAlternateKey> dictionary, TAlternateKey key, out bool exists)
            where TAlternateKey : allows ref struct
            => ref dictionary.GetValueRefOrAddDefault(key, out exists);

        public static void SetCount<T>(List<T> list, int count)
        {
            if (count < 0)
            {
                System.Collections.ThrowHelper.ThrowArgumentOutOfRangeException_NeedNonNegNum(nameof(count));
            }
            list._version++;
            if (count > list.Capacity)
            {
                list.Grow(count);
            }
            else if (count < list._size && System.Runtime.CompilerServices.RuntimeHelpers.IsReferenceOrContainsReferences<T>())
            {
                Array.Clear(list._items, count, list._size - count);
            }
            list._size = count;
        }
    }
}

namespace System.Collections.Generic
{
    // Сравнитель строк со случайным хешем: Dictionary и HashSet переходят на
    // него, когда в одной цепочке набирается больше 100 коллизий
    // (HashHelpers.HashCollisionThreshold). У .NET это Marvin32 с ключом из
    // генератора случайных чисел (Marvin.cs — на указателях, не взят); здесь
    // тот же хеш, что у NonRandomizedStringEqualityComparer, перемешанный с
    // числом, взятым при первом обращении. Программа видит только сам переход:
    // хеш строки у .NET и так свой в каждом запуске.
    internal abstract class RandomizedStringEqualityComparer : EqualityComparer<string>, IInternalStringEqualityComparer
    {
        private static readonly uint seed = (uint)Environment.TickCount * 2654435761u + 0x9E3779B9u;
        private readonly IEqualityComparer<string> underlyingComparer;

        private RandomizedStringEqualityComparer(IEqualityComparer<string> underlyingComparer)
        {
            this.underlyingComparer = underlyingComparer;
        }

        internal static RandomizedStringEqualityComparer Create(IEqualityComparer<string> underlyingComparer, bool ignoreCase) =>
            ignoreCase ? new OrdinalIgnoreCaseComparer(underlyingComparer) : new OrdinalComparer(underlyingComparer);

        public IEqualityComparer<string> GetUnderlyingEqualityComparer() => underlyingComparer;

        private static int Mix(int hash)
        {
            uint mixed = ((uint)hash ^ seed) * 0x85EBCA6Bu;
            return (int)(mixed ^ (mixed >> 13));
        }

        private sealed class OrdinalComparer : RandomizedStringEqualityComparer, IAlternateEqualityComparer<ReadOnlySpan<char>, string>
        {
            internal OrdinalComparer(IEqualityComparer<string> wrappedComparer) : base(wrappedComparer)
            {
            }

            public override bool Equals(string x, string y) => string.Equals(x, y);

            public override int GetHashCode(string obj) => obj == null ? 0 : Mix(obj.GetNonRandomizedHashCode());

            int IAlternateEqualityComparer<ReadOnlySpan<char>, string>.GetHashCode(ReadOnlySpan<char> span) =>
                Mix(string.GetNonRandomizedHashCode(span));

            bool IAlternateEqualityComparer<ReadOnlySpan<char>, string>.Equals(ReadOnlySpan<char> span, string target) =>
                !(span.IsEmpty && target == null) && span.SequenceEqual(target);

            string IAlternateEqualityComparer<ReadOnlySpan<char>, string>.Create(ReadOnlySpan<char> span) => span.ToString();
        }

        private sealed class OrdinalIgnoreCaseComparer : RandomizedStringEqualityComparer, IAlternateEqualityComparer<ReadOnlySpan<char>, string>
        {
            internal OrdinalIgnoreCaseComparer(IEqualityComparer<string> wrappedComparer) : base(wrappedComparer)
            {
            }

            public override bool Equals(string x, string y) => string.Equals(x, y, StringComparison.OrdinalIgnoreCase);

            public override int GetHashCode(string obj) => obj == null ? 0 : Mix(obj.GetNonRandomizedHashCodeOrdinalIgnoreCase());

            int IAlternateEqualityComparer<ReadOnlySpan<char>, string>.GetHashCode(ReadOnlySpan<char> span) =>
                Mix(string.GetNonRandomizedHashCodeOrdinalIgnoreCase(span));

            bool IAlternateEqualityComparer<ReadOnlySpan<char>, string>.Equals(ReadOnlySpan<char> span, string target) =>
                !(span.IsEmpty && target == null) && span.EqualsOrdinalIgnoreCase(target);

            string IAlternateEqualityComparer<ReadOnlySpan<char>, string>.Create(ReadOnlySpan<char> span) => span.ToString();
        }
    }

    // У .NET — сравнитель для `T : IEquatable<T>` из EqualityComparer.cs (тот
    // создаёт сравнители отражением среды и не взят). Здесь тип нужен ради
    // одного `typeof`: NonRandomizedStringEqualityComparer записывает себя под
    // этим именем в SerializationInfo. Без ограничения на T: наша строка не
    // объявляет IEquatable<string>, а сравнение по умолчанию и так через него.
    internal sealed class GenericEqualityComparer<T> : EqualityComparer<T>
    {
        public override bool Equals(T x, T y) => Default.Equals(x, y);

        public override int GetHashCode(T obj) => Default.GetHashCode(obj);
    }

    // Пустой перечислитель с общим экземпляром: его отдаёт пустой словарь
    // (Dictionary.cs, IEnumerable<KeyValuePair>.GetEnumerator). У .NET это
    // GenericEmptyEnumerator<T> из Collections/Generic/IEnumerator.cs.
    internal sealed class GenericEmptyEnumerator<T> : IEnumerator<T>
    {
        public static readonly GenericEmptyEnumerator<T> Instance = new GenericEmptyEnumerator<T>();

        private GenericEmptyEnumerator()
        {
        }

        public T Current => throw new InvalidOperationException(SR.InvalidOperation_EnumOpCantHappen);

        object System.Collections.IEnumerator.Current => Current;

        public bool MoveNext() => false;

        public void Reset()
        {
        }

        public void Dispose()
        {
        }
    }
}
