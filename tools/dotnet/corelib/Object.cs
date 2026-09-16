// Корень иерархии типов и то, без чего компилятор не соберёт ни одной сборки.

using System.Runtime.CompilerServices;

namespace System
{
    public class Object
    {
        public Object()
        {
        }

        public virtual bool Equals(object obj) => this == obj;

        public virtual int GetHashCode() => RuntimeHelpers.GetHashCode(this);

        // Как в .NET: полное имя типа, `FreeOs.Samples.Objects.Plain`.
        public virtual string ToString() => GetType().FullName;

        [MethodImpl(MethodImplOptions.InternalCall)]
        public extern Type GetType();

        public static bool ReferenceEquals(object objA, object objB) => objA == objB;
    }

    public abstract class ValueType
    {
        // Сравнение поле за полем и хэш значения: у значимого типа нет
        // тождества, и равны два экземпляра с одинаковым содержимым.
        [MethodImpl(MethodImplOptions.InternalCall)]
        public override extern bool Equals(object obj);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public override extern int GetHashCode();
    }

    // Перечисления (фаза N4d). Имена и значения достаёт из метаданных среда;
    // печать, флаги и разбор — здесь, по правилам .NET. Значение внутри —
    // `ulong`: биты базового типа, расширенные нулями. В таком виде .NET и
    // сортирует значения — как беззнаковые, поэтому `-1` у `sbyte` идёт последним.
    public abstract class Enum : ValueType, IComparable, IFormattable
    {
        /// Имена в порядке возрастания значений.
        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern string[] InternalGetNames(Type enumType);

        /// Значения в том же порядке.
        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern ulong[] InternalGetValues(Type enumType);

        /// Помечено ли перечисление `[Flags]`.
        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern bool InternalIsFlags(Type enumType);

        /// Ширина базового типа в байтах; у знакового — со знаком минус.
        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern int InternalUnderlying(Type enumType);

        /// Биты упакованного перечисления, расширенные нулями.
        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern ulong InternalToUInt64(object value);

        /// Упакованное перечисление с этими битами.
        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern object InternalBox(Type enumType, ulong value);

        public override string ToString() => Format(GetType(), InternalToUInt64(this), null);

        public string ToString(string format) => Format(GetType(), InternalToUInt64(this), format);

        public string ToString(IFormatProvider provider) => ToString();

        public string ToString(string format, IFormatProvider provider) => ToString(format);

        public bool HasFlag(Enum flag)
        {
            if (flag == null)
            {
                throw new ArgumentNullException("flag");
            }
            if ((object)GetType() != flag.GetType())
            {
                throw new ArgumentException("The argument type, '" + flag.GetType().FullName + "', is not the same as the enum type '"
                    + GetType().FullName + "'.");
            }
            ulong bits = InternalToUInt64(flag);
            return (InternalToUInt64(this) & bits) == bits;
        }

        public int CompareTo(object target)
        {
            if (target == null)
            {
                return 1;
            }
            if ((object)GetType() != target.GetType())
            {
                throw new ArgumentException("Object must be the same type as the enum. The type passed in was '" + target.GetType().FullName
                    + "'; the enum type was '" + GetType().FullName + "'.");
            }
            ulong a = InternalToUInt64(this);
            ulong b = InternalToUInt64(target);
            int underlying = InternalUnderlying(GetType());
            if (underlying < 0)
            {
                long x = SignExtend(a, -underlying);
                long y = SignExtend(b, -underlying);
                return x < y ? -1 : x > y ? 1 : 0;
            }
            return a < b ? -1 : a > b ? 1 : 0;
        }

        public static string[] GetNames<TEnum>() where TEnum : struct => InternalGetNames(typeof(TEnum));

        public static TEnum[] GetValues<TEnum>() where TEnum : struct
        {
            ulong[] raw = InternalGetValues(typeof(TEnum));
            TEnum[] values = new TEnum[raw.Length];
            for (int i = 0; i < raw.Length; i++)
            {
                values[i] = (TEnum)InternalBox(typeof(TEnum), raw[i]);
            }
            return values;
        }

        public static bool IsDefined<TEnum>(TEnum value) where TEnum : struct => IndexOf(InternalGetValues(typeof(TEnum)), InternalToUInt64(value)) >= 0;

        public static string GetName<TEnum>(TEnum value) where TEnum : struct
        {
            int index = IndexOf(InternalGetValues(typeof(TEnum)), InternalToUInt64(value));
            return index >= 0 ? InternalGetNames(typeof(TEnum))[index] : null;
        }

        public static TEnum Parse<TEnum>(string value) where TEnum : struct => (TEnum)Parse(typeof(TEnum), value, false);

        public static TEnum Parse<TEnum>(string value, bool ignoreCase) where TEnum : struct => (TEnum)Parse(typeof(TEnum), value, ignoreCase);

        public static object Parse(Type enumType, string value) => Parse(enumType, value, false);

        public static object Parse(Type enumType, string value, bool ignoreCase)
        {
            switch (TryParseBits(enumType, value, ignoreCase, out ulong bits))
            {
                case 0:
                    return InternalBox(enumType, bits);
                case 1:
                    if (value == null)
                    {
                        throw new ArgumentNullException("value");
                    }
                    throw new ArgumentException("Must specify valid information for parsing in the string.", "value");
                default:
                    throw new ArgumentException("Requested value '" + value + "' was not found.");
            }
        }

        public static bool TryParse<TEnum>(string value, out TEnum result) where TEnum : struct => TryParse(value, false, out result);

        public static bool TryParse<TEnum>(string value, bool ignoreCase, out TEnum result) where TEnum : struct
        {
            if (TryParseBits(typeof(TEnum), value, ignoreCase, out ulong bits) == 0)
            {
                result = (TEnum)InternalBox(typeof(TEnum), bits);
                return true;
            }
            result = default;
            return false;
        }

        /// 0 — разобрано, 1 — пусто, 2 — нет такого имени или числа.
        private static int TryParseBits(Type type, string value, bool ignoreCase, out ulong bits)
        {
            bits = 0;
            if (value == null)
            {
                return 1;
            }
            string text = value.Trim();
            if (text.Length == 0)
            {
                return 1;
            }
            // Число пишется как число базового типа и именем быть не обязано.
            char first = text[0];
            if (char.IsDigit(first) || first == '-' || first == '+')
            {
                if (long.TryParse(text, out long number))
                {
                    int size = InternalUnderlying(type);
                    size = size < 0 ? -size : size;
                    bits = size == 8 ? (ulong)number : (ulong)number & ((1UL << (size * 8)) - 1);
                    return 0;
                }
            }
            string[] names = InternalGetNames(type);
            ulong[] values = InternalGetValues(type);
            foreach (string part in text.Split(','))
            {
                string name = part.Trim();
                int found = -1;
                for (int i = 0; i < names.Length; i++)
                {
                    if (string.Equals(names[i], name, ignoreCase ? StringComparison.OrdinalIgnoreCase : StringComparison.Ordinal))
                    {
                        found = i;
                        break;
                    }
                }
                if (found < 0)
                {
                    return 2;
                }
                bits |= values[found];
            }
            return 0;
        }

        private static string Format(Type type, ulong value, string format)
        {
            if (string.IsNullOrEmpty(format) || format == "G" || format == "g")
            {
                return FormatName(type, value, InternalIsFlags(type));
            }
            if (format.Length == 1)
            {
                switch (format[0])
                {
                    case 'D':
                    case 'd':
                        return FormatNumber(type, value);
                    case 'X':
                    case 'x':
                        int size = InternalUnderlying(type);
                        return value.ToString((format[0] == 'X' ? "X" : "x") + (size < 0 ? -size : size) * 2);
                    case 'F':
                    case 'f':
                        return FormatName(type, value, true);
                }
            }
            throw new FormatException("Format string can be only \"G\", \"g\", \"X\", \"x\", \"F\", \"f\", \"D\" or \"d\".");
        }

        /// Имя значения; у флагов — имена через запятую; если не сложилось — число.
        private static string FormatName(Type type, ulong value, bool flags)
        {
            string[] names = InternalGetNames(type);
            ulong[] values = InternalGetValues(type);
            if (!flags)
            {
                int index = IndexOf(values, value);
                return index >= 0 ? names[index] : FormatNumber(type, value);
            }
            return FlagNames(names, values, value) ?? FormatNumber(type, value);
        }

        // Как `Enum.FormatFlagNames` у .NET: от больших значений к меньшим
        // вычитаются входящие целиком; остаток — и печатается число.
        private static string FlagNames(string[] names, ulong[] values, ulong value)
        {
            if (value == 0)
            {
                return values.Length > 0 && values[0] == 0 ? names[0] : "0";
            }
            int index = values.Length - 1;
            for (; index >= 0; index--)
            {
                if (values[index] == value)
                {
                    return names[index];
                }
                if (values[index] < value)
                {
                    break;
                }
            }
            ulong rest = value;
            string result = null;
            for (; index >= 0; index--)
            {
                ulong current = values[index];
                if (index == 0 && current == 0)
                {
                    break;
                }
                if ((rest & current) == current)
                {
                    rest -= current;
                    result = result == null ? names[index] : names[index] + ", " + result;
                }
            }
            return rest == 0 ? result : null;
        }

        private static string FormatNumber(Type type, ulong value)
        {
            int underlying = InternalUnderlying(type);
            return underlying < 0 ? SignExtend(value, -underlying).ToString() : value.ToString();
        }

        private static long SignExtend(ulong value, int size)
        {
            int shift = 64 - size * 8;
            return (long)(value << shift) >> shift;
        }

        private static int IndexOf(ulong[] values, ulong value)
        {
            for (int i = 0; i < values.Length; i++)
            {
                if (values[i] == value)
                {
                    return i;
                }
            }
            return -1;
        }
    }

    public struct Void
    {
    }

    public abstract class Array
    {
        public extern int Length
        {
            [MethodImpl(MethodImplOptions.InternalCall)]
            get;
        }

        public static void Sort<T>(T[] array)
        {
            if (array == null)
            {
                throw new ArgumentNullException("array");
            }
            if (array.Length > 1)
            {
                Collections.Generic.ArraySortHelper<T>.Sort(array, 0, array.Length, null);
            }
        }

        public static void Sort<T>(T[] array, Comparison<T> comparison) =>
            Collections.Generic.ArraySortHelper<T>.Sort(array, 0, array.Length, new Collections.Generic.ComparisonComparer<T>(comparison));

        public static int IndexOf<T>(T[] array, T value)
        {
            if (array == null)
            {
                throw new ArgumentNullException("array");
            }
            Collections.Generic.EqualityComparer<T> comparer = Collections.Generic.EqualityComparer<T>.Default;
            for (int i = 0; i < array.Length; i++)
            {
                if (comparer.Equals(array[i], value))
                {
                    return i;
                }
            }
            return -1;
        }

        public static void Reverse<T>(T[] array)
        {
            for (int i = 0, j = array.Length - 1; i < j; i++, j--)
            {
                T item = array[i];
                array[i] = array[j];
                array[j] = item;
            }
        }

        public static T[] Empty<T>() => new T[0];

        // Фаза N10: члены, которыми пользуется PriorityQueue из dotnet/runtime.
        // Число то же, что у .NET: очередь сравнивает с ним свою ёмкость, и
        // другое значение поменяло бы, где рост упирается в предел.
        public static int MaxLength => 0x7FFFFFC7;

        // Многомерных массивов среда не знает вовсе (loader.rs отказывает на
        // загрузке типа), так что любой массив здесь одномерный с нуля.
        public int Rank => 1;

        public int GetLowerBound(int dimension)
        {
            if (dimension != 0)
            {
                throw new IndexOutOfRangeException();
            }
            return 0;
        }

        // Необобщённое копирование и очистка: тип элемента известен только
        // среде, поэтому тело в Rust (natives.rs). Копирование между массивами
        // разных типов элементов — ArrayTypeMismatchException, даже там, где
        // .NET упаковал бы значения в object[]; программам это пока не нужно.
        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern void Copy(Array sourceArray, int sourceIndex, Array destinationArray, int destinationIndex, int length);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern void Clear(Array array, int index, int length);

        // Копирование поэлементно (фаза N9): `Array.Copy(Array, Array, int)`
        // без обобщения потребовал бы члена среды с разбором типов элементов,
        // а базовой библиотеке хватает типизированного.
        internal static void CopyItems<T>(T[] source, T[] destination, int length)
        {
            for (int i = 0; i < length; i++)
            {
                destination[i] = source[i];
            }
        }

        public static void Resize<T>(ref T[] array, int newSize)
        {
            T[] resized = new T[newSize];
            if (array != null)
            {
                int length = array.Length < newSize ? array.Length : newSize;
                for (int i = 0; i < length; i++)
                {
                    resized[i] = array[i];
                }
            }
            array = resized;
        }
    }

    public abstract class Delegate
    {
        // Список вызовов живёт внутри объекта делегата в среде.
        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern Delegate Combine(Delegate a, Delegate b);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern Delegate Remove(Delegate source, Delegate value);
    }

    public abstract class MulticastDelegate : Delegate
    {
    }

    public struct RuntimeTypeHandle
    {
    }

    public struct RuntimeFieldHandle
    {
    }

    public struct RuntimeMethodHandle
    {
    }

    public struct Nullable<T>
        where T : struct
    {
    }

    public interface IDisposable
    {
        void Dispose();
    }

    public interface ICloneable
    {
        object Clone();
    }

    public abstract class Attribute
    {
    }

    [Flags]
    public enum AttributeTargets
    {
        Assembly = 1,
        Module = 2,
        Class = 4,
        Struct = 8,
        Enum = 16,
        Constructor = 32,
        Method = 64,
        Property = 128,
        Field = 256,
        Event = 512,
        Interface = 1024,
        Parameter = 2048,
        Delegate = 4096,
        ReturnValue = 8192,
        GenericParameter = 16384,
        All = 32767,
    }

    [AttributeUsage(AttributeTargets.Class, Inherited = true)]
    public sealed class AttributeUsageAttribute : Attribute
    {
        public AttributeUsageAttribute(AttributeTargets validOn)
        {
            ValidOn = validOn;
        }

        public AttributeTargets ValidOn { get; }

        public bool AllowMultiple { get; set; }

        public bool Inherited { get; set; }
    }

    [AttributeUsage(AttributeTargets.Parameter, Inherited = true)]
    public sealed class ParamArrayAttribute : Attribute
    {
    }

    [AttributeUsage(AttributeTargets.Enum, Inherited = false)]
    public class FlagsAttribute : Attribute
    {
    }
}
