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

    public abstract class Enum : ValueType
    {
        // Имена значений берутся из метаданных, а флаги печатаются через запятую —
        // это фаза N4. До неё член отказывает с названием, а не печатает число.
        [MethodImpl(MethodImplOptions.InternalCall)]
        public override extern string ToString();
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
    }

    public abstract class Delegate
    {
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
