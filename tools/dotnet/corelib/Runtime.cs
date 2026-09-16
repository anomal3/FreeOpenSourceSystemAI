// Типы и атрибуты, на которые ссылается код, написанный компилятором:
// тип объекта, инициализатор массива из данных сборки, InternalCall.

using System.Runtime.CompilerServices;

namespace System.Reflection
{
    public abstract class MemberInfo
    {
        public abstract string Name { get; }
    }

    // Компилятор помечает им тип с индексатором (`string[int]`).
    [AttributeUsage(AttributeTargets.Class | AttributeTargets.Struct | AttributeTargets.Interface, Inherited = true)]
    public sealed class DefaultMemberAttribute : Attribute
    {
        public DefaultMemberAttribute(string memberName)
        {
            MemberName = memberName;
        }

        public string MemberName { get; }
    }
}

namespace System
{
    public abstract class Type : Reflection.MemberInfo
    {
        public abstract string FullName { get; }

        public override string ToString() => FullName;

        // Фаза N10: PriorityQueue выбирает путь сравнения по
        // `typeof(TPriority).IsValueType`. Ответ знает только среда (types.rs).
        public extern bool IsValueType
        {
            [MethodImpl(MethodImplOptions.InternalCall)]
            get;
        }

        // `typeof(T)` — это `ldtoken` и этот вызов. Среда кладёт на стек сразу
        // объект типа, так что отдать его — всё, что остаётся.
        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern Type GetTypeFromHandle(RuntimeTypeHandle handle);
    }

    // Тип, который возвращает `GetType()`. Один объект на тип, как в .NET:
    // `a.GetType() == b.GetType()` сравнивает ссылки.
    internal sealed class RuntimeType : Type
    {
        public override extern string Name
        {
            [MethodImpl(MethodImplOptions.InternalCall)]
            get;
        }

        public override extern string FullName
        {
            [MethodImpl(MethodImplOptions.InternalCall)]
            get;
        }
    }
}

namespace System.Runtime.CompilerServices
{
    public static class RuntimeHelpers
    {
        // `int[] a = { 2, 3, 5 }` компилятор записывает байтами в сборку
        // (FieldRVA) и заполняет массив этим вызовом.
        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern void InitializeArray(Array array, RuntimeFieldHandle fldHandle);

        [MethodImpl(MethodImplOptions.InternalCall)]
        public static extern int GetHashCode(object o);

        // У .NET это подсказка JIT: «стоит ли обнулять освободившиеся ячейки,
        // чтобы сборщик не держал ссылки». Ответ `true` всегда верен — лишнее
        // обнуление чисел ничего не ломает, а `false` там, где ссылки есть,
        // оставил бы мусор живым. Точный ответ сэкономил бы копейки.
        public static bool IsReferenceOrContainsReferences<T>() => true;
    }

    public enum MethodImplOptions
    {
        Unmanaged = 4,
        NoInlining = 8,
        ForwardRef = 16,
        Synchronized = 32,
        NoOptimization = 64,
        PreserveSig = 128,
        AggressiveInlining = 256,
        AggressiveOptimization = 512,
        InternalCall = 4096,
    }

    [AttributeUsage(AttributeTargets.Constructor | AttributeTargets.Method, Inherited = false)]
    public sealed class MethodImplAttribute : Attribute
    {
        public MethodImplAttribute(MethodImplOptions methodImplOptions)
        {
            Value = methodImplOptions;
        }

        public MethodImplOptions Value { get; }
    }

    [AttributeUsage(AttributeTargets.Property, Inherited = true)]
    public sealed class IndexerNameAttribute : Attribute
    {
        public IndexerNameAttribute(string indexerName)
        {
        }
    }
}
