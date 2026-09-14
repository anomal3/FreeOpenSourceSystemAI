// Примитивы. На стеке своей среды это не структуры с полем, а числа
// (`int32`, `int64`, `native int`, `F`), поэтому полей у них здесь нет, а
// печать написана в Rust: `ToString` получает `this` указателем на число или
// упакованным объектом, и среда достаёт значение сама.

using System.Runtime.CompilerServices;

namespace System
{
    public struct Boolean
    {
        [MethodImpl(MethodImplOptions.InternalCall)]
        public override extern string ToString();
    }

    public struct Char
    {
        [MethodImpl(MethodImplOptions.InternalCall)]
        public override extern string ToString();
    }

    public struct SByte
    {
        [MethodImpl(MethodImplOptions.InternalCall)]
        public override extern string ToString();
    }

    public struct Byte
    {
        [MethodImpl(MethodImplOptions.InternalCall)]
        public override extern string ToString();
    }

    public struct Int16
    {
        [MethodImpl(MethodImplOptions.InternalCall)]
        public override extern string ToString();
    }

    public struct UInt16
    {
        [MethodImpl(MethodImplOptions.InternalCall)]
        public override extern string ToString();
    }

    public struct Int32
    {
        [MethodImpl(MethodImplOptions.InternalCall)]
        public override extern string ToString();
    }

    public struct UInt32
    {
        [MethodImpl(MethodImplOptions.InternalCall)]
        public override extern string ToString();
    }

    public struct Int64
    {
        [MethodImpl(MethodImplOptions.InternalCall)]
        public override extern string ToString();
    }

    public struct UInt64
    {
        [MethodImpl(MethodImplOptions.InternalCall)]
        public override extern string ToString();
    }

    public struct IntPtr
    {
    }

    public struct UIntPtr
    {
    }

    // Печать дробных чисел — кратчайшее представление, читаемое обратно в то же
    // число, — фаза N4. До неё `ToString` у них наследуется от ValueType и
    // печатает имя типа; образцы дробные числа не печатают.
    public struct Single
    {
    }

    public struct Double
    {
    }
}
