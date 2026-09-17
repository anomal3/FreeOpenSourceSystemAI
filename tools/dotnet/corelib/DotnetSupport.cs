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

        // У .NET это string.Format с текущей культурой; культур у среды нет,
        // и форматирование здесь всегда инвариантное.
        internal static string Format(string resourceFormat, object p1) => string.Format(resourceFormat, p1);

        internal static string Format(string resourceFormat, object p1, object p2) => string.Format(resourceFormat, p1, p2);
    }

    // Имена аргументов и ресурсов, которыми говорит ThrowHelper из CoreLib
    // (Queue.cs). Только те, что нужны перенесённым файлам.
    internal enum ExceptionArgument
    {
        array,
        arrayIndex,
    }

    internal enum ExceptionResource
    {
        ArgumentOutOfRange_IndexMustBeLessOrEqual,
        Argument_InvalidOffLen,
        Arg_RankMultiDimNotSupported,
        Arg_NonZeroLowerBound,
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

        internal static void ThrowInvalidOperationException_InvalidOperation_EnumFailedVersion() =>
            throw new InvalidOperationException(SR.InvalidOperation_EnumFailedVersion);

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
            ExceptionResource.ArgumentOutOfRange_IndexMustBeLessOrEqual => SR.ArgumentOutOfRange_IndexMustBeLessOrEqual,
            ExceptionResource.Argument_InvalidOffLen => SR.Argument_InvalidOffLen,
            ExceptionResource.Arg_RankMultiDimNotSupported => SR.Arg_RankMultiDimNotSupported,
            _ => SR.Arg_NonZeroLowerBound,
        };
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
