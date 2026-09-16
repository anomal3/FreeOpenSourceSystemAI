// Исключения (фаза N3b).
//
// Тексты сообщений по умолчанию — те же, что у .NET на английской системе:
// программа, печатающая `e.Message`, обязана напечатать то же, что под
// настоящим dotnet. Исключения, которые бросает сама среда (`null.Length`,
// выход за массив, деление на ноль), создаются конструктором без параметров —
// поэтому текст живёт здесь, в одном месте, а не в Rust.

namespace System
{
    public class Exception
    {
        private readonly string message;

        public Exception()
        {
        }

        public Exception(string message)
        {
            this.message = message;
        }

        public Exception(string message, Exception innerException)
        {
            this.message = message;
            InnerException = innerException;
        }

        public Exception InnerException { get; }

        public virtual string Message => message ?? "Exception of type '" + GetType().FullName + "' was thrown.";

        // Без стека вызовов: у своей среды его нет, а у не брошенного
        // исключения его нет и в .NET.
        public override string ToString()
        {
            string text = GetType().FullName;
            string description = Message;
            if (!string.IsNullOrEmpty(description))
            {
                text = text + ": " + description;
            }
            if (InnerException != null)
            {
                text = text + " ---> " + InnerException.ToString();
            }
            return text;
        }
    }

    public class SystemException : Exception
    {
        public SystemException()
            : base("System error.")
        {
        }

        public SystemException(string message)
            : base(message)
        {
        }

        public SystemException(string message, Exception innerException)
            : base(message, innerException)
        {
        }
    }

    public class ArgumentException : SystemException
    {
        public ArgumentException()
            : base("Value does not fall within the expected range.")
        {
        }

        public ArgumentException(string message)
            : base(message)
        {
        }

        public ArgumentException(string message, string paramName)
            : base(message)
        {
            ParamName = paramName;
        }

        public virtual string ParamName { get; }

        public override string Message
        {
            get
            {
                string text = base.Message;
                if (!string.IsNullOrEmpty(ParamName))
                {
                    text = text + " (Parameter '" + ParamName + "')";
                }
                return text;
            }
        }
    }

    public class ArgumentNullException : ArgumentException
    {
        public ArgumentNullException()
            : base("Value cannot be null.")
        {
        }

        public ArgumentNullException(string paramName)
            : base("Value cannot be null.", paramName)
        {
        }

        public ArgumentNullException(string paramName, string message)
            : base(message, paramName)
        {
        }

        public static void ThrowIfNull(object argument, [Runtime.CompilerServices.CallerArgumentExpression("argument")] string paramName = null)
        {
            if (argument == null)
            {
                throw new ArgumentNullException(paramName);
            }
        }
    }

    public class ArgumentOutOfRangeException : ArgumentException
    {
        public ArgumentOutOfRangeException()
            : base("Specified argument was out of the range of valid values.")
        {
        }

        public ArgumentOutOfRangeException(string paramName)
            : base("Specified argument was out of the range of valid values.", paramName)
        {
        }

        public ArgumentOutOfRangeException(string paramName, string message)
            : base(message, paramName)
        {
        }

        public ArgumentOutOfRangeException(string paramName, object actualValue, string message)
            : base(message, paramName)
        {
            ActualValue = actualValue;
        }

        public virtual object ActualValue { get; }

        // Как у .NET: значение идёт второй строкой после « (Parameter '…')».
        public override string Message
        {
            get
            {
                string text = base.Message;
                if (ActualValue == null)
                {
                    return text;
                }
                return text + Environment.NewLine + "Actual value was " + ActualValue.ToString() + ".";
            }
        }

        // У .NET это обобщённый `ThrowIfNegative<T>` над INumberBase<T>; обобщённой
        // арифметики у своей corelib нет, а очереди хватает `int`.
        public static void ThrowIfNegative(int value, [Runtime.CompilerServices.CallerArgumentExpression("value")] string paramName = null)
        {
            if (value < 0)
            {
                throw new ArgumentOutOfRangeException(paramName, value, paramName + " ('" + value.ToString() + "') must be a non-negative value.");
            }
        }
    }

    public class InvalidOperationException : SystemException
    {
        public InvalidOperationException()
            : base("Operation is not valid due to the current state of the object.")
        {
        }

        public InvalidOperationException(string message)
            : base(message)
        {
        }

        public InvalidOperationException(string message, Exception innerException)
            : base(message, innerException)
        {
        }
    }

    public class NotSupportedException : SystemException
    {
        public NotSupportedException()
            : base("Specified method is not supported.")
        {
        }

        public NotSupportedException(string message)
            : base(message)
        {
        }
    }

    public class NotImplementedException : SystemException
    {
        public NotImplementedException()
            : base("The method or operation is not implemented.")
        {
        }

        public NotImplementedException(string message)
            : base(message)
        {
        }
    }

    public class NullReferenceException : SystemException
    {
        public NullReferenceException()
            : base("Object reference not set to an instance of an object.")
        {
        }

        public NullReferenceException(string message)
            : base(message)
        {
        }
    }

    public sealed class IndexOutOfRangeException : SystemException
    {
        public IndexOutOfRangeException()
            : base("Index was outside the bounds of the array.")
        {
        }

        public IndexOutOfRangeException(string message)
            : base(message)
        {
        }
    }

    public class ArithmeticException : SystemException
    {
        public ArithmeticException()
            : base("Overflow or underflow in the arithmetic operation.")
        {
        }

        public ArithmeticException(string message)
            : base(message)
        {
        }
    }

    public class DivideByZeroException : ArithmeticException
    {
        public DivideByZeroException()
            : base("Attempted to divide by zero.")
        {
        }

        public DivideByZeroException(string message)
            : base(message)
        {
        }
    }

    public class OverflowException : ArithmeticException
    {
        public OverflowException()
            : base("Arithmetic operation resulted in an overflow.")
        {
        }

        public OverflowException(string message)
            : base(message)
        {
        }
    }

    public class InvalidCastException : SystemException
    {
        public InvalidCastException()
            : base("Specified cast is not valid.")
        {
        }

        public InvalidCastException(string message)
            : base(message)
        {
        }
    }

    public class FormatException : SystemException
    {
        public FormatException()
            : base("One of the identified items was in an invalid format.")
        {
        }

        public FormatException(string message)
            : base(message)
        {
        }
    }

    public sealed class OutOfMemoryException : SystemException
    {
        public OutOfMemoryException()
            : base("Insufficient memory to continue the execution of the program.")
        {
        }
    }

    public class ArrayTypeMismatchException : SystemException
    {
        public ArrayTypeMismatchException()
            : base("Attempted to access an element as a type incompatible with the array.")
        {
        }
    }

    public sealed class TypeInitializationException : SystemException
    {
        public TypeInitializationException(string fullTypeName, Exception innerException)
            : base("The type initializer for '" + fullTypeName + "' threw an exception.", innerException)
        {
        }
    }
}
