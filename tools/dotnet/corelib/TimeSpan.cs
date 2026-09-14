// TimeSpan (фаза N5b): длительность в тиках по 100 нс, как в .NET. Печать и
// разбор — те же правила, что у TimeSpanFormat/TimeSpanParse инвариантной
// культуры. Строки обходятся циклом по индексу, не foreach (см. IO.cs).

using System.Globalization;
using System.Text;

namespace System
{
    public struct TimeSpan : IComparable, IComparable<TimeSpan>, IEquatable<TimeSpan>, IFormattable
    {
        public const long TicksPerMicrosecond = 10;
        public const long TicksPerMillisecond = 10000;
        public const long TicksPerSecond = 10000000;
        public const long TicksPerMinute = 600000000;
        public const long TicksPerHour = 36000000000;
        public const long TicksPerDay = 864000000000;

        public static readonly TimeSpan Zero = new TimeSpan(0);
        public static readonly TimeSpan MaxValue = new TimeSpan(long.MaxValue);
        public static readonly TimeSpan MinValue = new TimeSpan(long.MinValue);

        private readonly long ticks;

        public TimeSpan(long ticks)
        {
            this.ticks = ticks;
        }

        public TimeSpan(int hours, int minutes, int seconds)
        {
            ticks = FromParts(0, hours, minutes, seconds, 0, 0);
        }

        public TimeSpan(int days, int hours, int minutes, int seconds)
        {
            ticks = FromParts(days, hours, minutes, seconds, 0, 0);
        }

        public TimeSpan(int days, int hours, int minutes, int seconds, int milliseconds)
        {
            ticks = FromParts(days, hours, minutes, seconds, milliseconds, 0);
        }

        public TimeSpan(int days, int hours, int minutes, int seconds, int milliseconds, int microseconds)
        {
            ticks = FromParts(days, hours, minutes, seconds, milliseconds, microseconds);
        }

        // Сумма в микросекундах: у .NET переполнение считается по ней.
        private static long FromParts(long days, long hours, long minutes, long seconds, long milliseconds, long microseconds)
        {
            decimalCheck(days, hours, minutes, seconds, milliseconds, microseconds);
            long total = days * 86400000000L + hours * 3600000000L + minutes * 60000000L + seconds * 1000000L + milliseconds * 1000L + microseconds;
            return total * TicksPerMicrosecond;
        }

        // Без decimal и checked: границы проверяются в долях дня заранее.
        private static void decimalCheck(long days, long hours, long minutes, long seconds, long milliseconds, long microseconds)
        {
            double total = days * 86400000000.0 + hours * 3600000000.0 + minutes * 60000000.0 + seconds * 1000000.0 + milliseconds * 1000.0 + microseconds;
            if (total > 922337203685477580.0 || total < -922337203685477580.0)
            {
                throw new ArgumentOutOfRangeException(null, "TimeSpan overflowed because the duration is too long.");
            }
        }

        public long Ticks => ticks;

        public int Days => (int)(ticks / TicksPerDay);

        public int Hours => (int)(ticks / TicksPerHour % 24);

        public int Minutes => (int)(ticks / TicksPerMinute % 60);

        public int Seconds => (int)(ticks / TicksPerSecond % 60);

        public int Milliseconds => (int)(ticks / TicksPerMillisecond % 1000);

        public int Microseconds => (int)(ticks / TicksPerMicrosecond % 1000);

        public int Nanoseconds => (int)(ticks % TicksPerMicrosecond * 100);

        public double TotalDays => (double)ticks / TicksPerDay;

        public double TotalHours => (double)ticks / TicksPerHour;

        public double TotalMinutes => (double)ticks / TicksPerMinute;

        public double TotalSeconds => (double)ticks / TicksPerSecond;

        public double TotalMilliseconds
        {
            get
            {
                double value = (double)ticks / TicksPerMillisecond;
                if (value > 922337203685477.0)
                {
                    return 922337203685477.0;
                }
                if (value < -922337203685477.0)
                {
                    return -922337203685477.0;
                }
                return value;
            }
        }

        public double TotalMicroseconds => (double)ticks / TicksPerMicrosecond;

        public TimeSpan Add(TimeSpan ts) => this + ts;

        public TimeSpan Subtract(TimeSpan ts) => this - ts;

        public TimeSpan Multiply(double factor) => this * factor;

        public TimeSpan Divide(double divisor) => this / divisor;

        public double Divide(TimeSpan ts) => this / ts;

        public TimeSpan Negate() => -this;

        public TimeSpan Duration()
        {
            if (ticks == long.MinValue)
            {
                throw TooLong();
            }
            return new TimeSpan(ticks >= 0 ? ticks : -ticks);
        }

        public static TimeSpan FromTicks(long value) => new TimeSpan(value);

        public static TimeSpan FromDays(double value) => Interval(value, TicksPerDay);

        public static TimeSpan FromHours(double value) => Interval(value, TicksPerHour);

        public static TimeSpan FromMinutes(double value) => Interval(value, TicksPerMinute);

        public static TimeSpan FromSeconds(double value) => Interval(value, TicksPerSecond);

        public static TimeSpan FromMilliseconds(double value) => Interval(value, TicksPerMillisecond);

        public static TimeSpan FromMicroseconds(double value) => Interval(value, TicksPerMicrosecond);

        // Целые перегрузки .NET 9 и 10: компилятор выбирает их для целых чисел.
        public static TimeSpan FromDays(int days) => new TimeSpan(FromParts(days, 0, 0, 0, 0, 0));

        public static TimeSpan FromDays(int days, int hours = 0, long minutes = 0, long seconds = 0, long milliseconds = 0, long microseconds = 0) =>
            new TimeSpan(FromParts(days, hours, minutes, seconds, milliseconds, microseconds));

        public static TimeSpan FromHours(int hours) => new TimeSpan(FromParts(0, hours, 0, 0, 0, 0));

        public static TimeSpan FromHours(int hours, long minutes = 0, long seconds = 0, long milliseconds = 0, long microseconds = 0) =>
            new TimeSpan(FromParts(0, hours, minutes, seconds, milliseconds, microseconds));

        public static TimeSpan FromMinutes(long minutes) => new TimeSpan(FromParts(0, 0, minutes, 0, 0, 0));

        public static TimeSpan FromMinutes(long minutes, long seconds = 0, long milliseconds = 0, long microseconds = 0) =>
            new TimeSpan(FromParts(0, 0, minutes, seconds, milliseconds, microseconds));

        public static TimeSpan FromSeconds(long seconds) => new TimeSpan(FromParts(0, 0, 0, seconds, 0, 0));

        public static TimeSpan FromSeconds(long seconds, long milliseconds = 0, long microseconds = 0) =>
            new TimeSpan(FromParts(0, 0, 0, seconds, milliseconds, microseconds));

        public static TimeSpan FromMilliseconds(long milliseconds) => new TimeSpan(FromParts(0, 0, 0, 0, milliseconds, 0));

        public static TimeSpan FromMilliseconds(long milliseconds, long microseconds = 0) =>
            new TimeSpan(FromParts(0, 0, 0, 0, milliseconds, microseconds));

        public static TimeSpan FromMicroseconds(long microseconds) => new TimeSpan(FromParts(0, 0, 0, 0, 0, microseconds));

        private static TimeSpan Interval(double value, double scale)
        {
            if (double.IsNaN(value))
            {
                throw new ArgumentException("TimeSpan does not accept floating point Not-a-Number values.");
            }
            return FromDoubleTicks(value * scale);
        }

        private static TimeSpan FromDoubleTicks(double value)
        {
            if (value > long.MaxValue || value < long.MinValue || double.IsNaN(value))
            {
                throw TooLong();
            }
            if (value == long.MaxValue)
            {
                return MaxValue;
            }
            return new TimeSpan((long)value);
        }

        internal static OverflowException TooLong() => new OverflowException("TimeSpan overflowed because the duration is too long.");

        public static TimeSpan operator +(TimeSpan t1, TimeSpan t2)
        {
            long result = t1.ticks + t2.ticks;
            // Переполнение: знаки слагаемых равны, а у суммы другой.
            if ((t1.ticks >> 63 == t2.ticks >> 63) && (t1.ticks >> 63 != result >> 63))
            {
                throw TooLong();
            }
            return new TimeSpan(result);
        }

        public static TimeSpan operator -(TimeSpan t1, TimeSpan t2)
        {
            long result = t1.ticks - t2.ticks;
            if ((t1.ticks >> 63 != t2.ticks >> 63) && (t1.ticks >> 63 != result >> 63))
            {
                throw TooLong();
            }
            return new TimeSpan(result);
        }

        public static TimeSpan operator -(TimeSpan t)
        {
            if (t.ticks == long.MinValue)
            {
                throw new OverflowException("Negating the minimum value of a twos complement number is invalid.");
            }
            return new TimeSpan(-t.ticks);
        }

        public static TimeSpan operator +(TimeSpan t) => t;

        public static TimeSpan operator *(TimeSpan timeSpan, double factor)
        {
            if (double.IsNaN(factor))
            {
                throw new ArgumentException("TimeSpan does not accept floating point Not-a-Number values.");
            }
            return FromDoubleTicks(Math.Round(timeSpan.ticks * factor));
        }

        public static TimeSpan operator *(double factor, TimeSpan timeSpan) => timeSpan * factor;

        public static TimeSpan operator /(TimeSpan timeSpan, double divisor)
        {
            if (double.IsNaN(divisor))
            {
                throw new ArgumentException("TimeSpan does not accept floating point Not-a-Number values.");
            }
            return FromDoubleTicks(Math.Round(timeSpan.ticks / divisor));
        }

        public static double operator /(TimeSpan t1, TimeSpan t2) => t1.ticks / (double)t2.ticks;

        public static bool operator ==(TimeSpan t1, TimeSpan t2) => t1.ticks == t2.ticks;

        public static bool operator !=(TimeSpan t1, TimeSpan t2) => t1.ticks != t2.ticks;

        public static bool operator <(TimeSpan t1, TimeSpan t2) => t1.ticks < t2.ticks;

        public static bool operator <=(TimeSpan t1, TimeSpan t2) => t1.ticks <= t2.ticks;

        public static bool operator >(TimeSpan t1, TimeSpan t2) => t1.ticks > t2.ticks;

        public static bool operator >=(TimeSpan t1, TimeSpan t2) => t1.ticks >= t2.ticks;

        public static int Compare(TimeSpan t1, TimeSpan t2) => t1.ticks < t2.ticks ? -1 : t1.ticks > t2.ticks ? 1 : 0;

        public int CompareTo(TimeSpan value) => Compare(this, value);

        public int CompareTo(object value)
        {
            if (value == null)
            {
                return 1;
            }
            if (value is TimeSpan other)
            {
                return Compare(this, other);
            }
            throw new ArgumentException("Object must be of type TimeSpan.");
        }

        public bool Equals(TimeSpan obj) => ticks == obj.ticks;

        public override bool Equals(object value) => value is TimeSpan other && ticks == other.ticks;

        public static bool Equals(TimeSpan t1, TimeSpan t2) => t1.ticks == t2.ticks;

        public override int GetHashCode() => (int)ticks ^ (int)(ticks >> 32);

        public override string ToString() => TimeSpanFormat.Format(this, null);

        public string ToString(string format) => TimeSpanFormat.Format(this, format);

        public string ToString(string format, IFormatProvider formatProvider) => TimeSpanFormat.Format(this, format);

        public static TimeSpan Parse(string s)
        {
            if (s == null)
            {
                throw new ArgumentNullException("input");
            }
            int status = TimeSpanFormat.TryParse(s, out TimeSpan result);
            if (status != 0)
            {
                throw TimeSpanFormat.Failure(status);
            }
            return result;
        }

        public static TimeSpan Parse(string input, IFormatProvider formatProvider) => Parse(input);

        public static bool TryParse(string s, out TimeSpan result)
        {
            if (s == null)
            {
                result = Zero;
                return false;
            }
            return TimeSpanFormat.TryParse(s, out result) == 0;
        }

        public static bool TryParse(string input, IFormatProvider formatProvider, out TimeSpan result) => TryParse(input, out result);

        public static TimeSpan ParseExact(string input, string format, IFormatProvider formatProvider)
        {
            if (input == null)
            {
                throw new ArgumentNullException("input");
            }
            if (format == null)
            {
                throw new ArgumentNullException("format");
            }
            int status = TimeSpanFormat.TryParseExact(input, format, out TimeSpan result);
            if (status != 0)
            {
                throw TimeSpanFormat.Failure(status);
            }
            return result;
        }

        public static bool TryParseExact(string input, string format, IFormatProvider formatProvider, out TimeSpan result)
        {
            if (input == null || format == null)
            {
                result = Zero;
                return false;
            }
            return TimeSpanFormat.TryParseExact(input, format, out result) == 0;
        }
    }

    internal static class TimeSpanFormat
    {
        internal static string Format(TimeSpan value, string format)
        {
            if (format == null || format.Length == 0 || format == "c" || format == "t" || format == "T")
            {
                return Standard(value, 'c');
            }
            if (format.Length == 1)
            {
                char c = format[0];
                if (c == 'g' || c == 'G')
                {
                    return Standard(value, c);
                }
                throw BadFormat();
            }
            return Custom(value, format);
        }

        private static FormatException BadFormat() => new FormatException("Input string was not in a correct format.");

        // c: [-][d.]hh:mm:ss[.fffffff]; g: [-][d:]h:mm:ss[.FFFFFFF];
        // G: [-]d:hh:mm:ss.fffffff.
        private static string Standard(TimeSpan value, char kind)
        {
            long ticks = value.Ticks;
            var result = new StringBuilder();
            ulong magnitude;
            if (ticks < 0)
            {
                result.Append('-');
                magnitude = (ulong)(-(ticks + 1)) + 1;
            }
            else
            {
                magnitude = (ulong)ticks;
            }
            ulong days = magnitude / (ulong)TimeSpan.TicksPerDay;
            ulong rest = magnitude % (ulong)TimeSpan.TicksPerDay;
            int hours = (int)(rest / (ulong)TimeSpan.TicksPerHour);
            int minutes = (int)(rest / (ulong)TimeSpan.TicksPerMinute % 60);
            int seconds = (int)(rest / (ulong)TimeSpan.TicksPerSecond % 60);
            int fraction = (int)(rest % (ulong)TimeSpan.TicksPerSecond);
            if (kind == 'c')
            {
                if (days != 0)
                {
                    result.Append(days).Append('.');
                }
                Digits(result, hours, 2);
            }
            else if (kind == 'g')
            {
                if (days != 0)
                {
                    result.Append(days).Append(':');
                }
                result.Append(hours);
            }
            else
            {
                result.Append(days).Append(':');
                Digits(result, hours, 2);
            }
            result.Append(':');
            Digits(result, minutes, 2);
            result.Append(':');
            Digits(result, seconds, 2);
            if (kind == 'G' || (kind == 'c' && fraction != 0))
            {
                result.Append('.');
                Digits(result, fraction, 7);
            }
            else if (kind == 'g' && fraction != 0)
            {
                result.Append('.');
                int digits = 7;
                while (fraction % 10 == 0)
                {
                    fraction /= 10;
                    digits--;
                }
                Digits(result, fraction, digits);
            }
            return result.ToString();
        }

        internal static void Digits(StringBuilder result, long value, int width)
        {
            string text = ((ulong)value).ToString();
            for (int i = text.Length; i < width; i++)
            {
                result.Append('0');
            }
            result.Append(text);
        }

        private static int Repeat(string format, int position)
        {
            char c = format[position];
            int end = position + 1;
            while (end < format.Length && format[end] == c)
            {
                end++;
            }
            return end - position;
        }

        // Пользовательский формат: знака не печатает, как и .NET.
        private static string Custom(TimeSpan value, string format)
        {
            long ticks = value.Ticks;
            long day = ticks / TimeSpan.TicksPerDay;
            long time = ticks % TimeSpan.TicksPerDay;
            if (ticks < 0)
            {
                day = -day;
                time = -time;
            }
            int hours = (int)(time / TimeSpan.TicksPerHour % 24);
            int minutes = (int)(time / TimeSpan.TicksPerMinute % 60);
            int seconds = (int)(time / TimeSpan.TicksPerSecond % 60);
            int fraction = (int)(time % TimeSpan.TicksPerSecond);
            var result = new StringBuilder();
            int i = 0;
            while (i < format.Length)
            {
                char c = format[i];
                int length;
                switch (c)
                {
                    case 'h':
                    case 'm':
                    case 's':
                        length = Repeat(format, i);
                        if (length > 2)
                        {
                            throw BadFormat();
                        }
                        Digits(result, c == 'h' ? hours : c == 'm' ? minutes : seconds, length);
                        break;
                    case 'f':
                    case 'F':
                        length = Repeat(format, i);
                        if (length > 7)
                        {
                            throw BadFormat();
                        }
                        long part = fraction;
                        for (int k = length; k < 7; k++)
                        {
                            part /= 10;
                        }
                        if (c == 'f')
                        {
                            Digits(result, part, length);
                        }
                        else
                        {
                            int digits = length;
                            while (digits > 0 && part % 10 == 0)
                            {
                                part /= 10;
                                digits--;
                            }
                            if (digits > 0)
                            {
                                Digits(result, part, digits);
                            }
                        }
                        break;
                    case 'd':
                        length = Repeat(format, i);
                        if (length > 8)
                        {
                            throw BadFormat();
                        }
                        Digits(result, day, length);
                        break;
                    case '\'':
                    case '"':
                        length = Quoted(format, i, result);
                        break;
                    case '%':
                        if (i + 1 >= format.Length || format[i + 1] == '%')
                        {
                            throw BadFormat();
                        }
                        result.Append(Custom(value, format.Substring(i + 1, 1)));
                        length = 2;
                        break;
                    case '\\':
                        if (i + 1 >= format.Length)
                        {
                            throw BadFormat();
                        }
                        result.Append(format[i + 1]);
                        length = 2;
                        break;
                    default:
                        throw BadFormat();
                }
                i += length;
            }
            return result.ToString();
        }

        // Строка в кавычках вместе с кавычками; `\` внутри экранирует.
        internal static int Quoted(string format, int position, StringBuilder result)
        {
            char quote = format[position];
            int i = position + 1;
            while (i < format.Length)
            {
                char c = format[i];
                if (c == quote)
                {
                    return i + 1 - position;
                }
                if (c == '\\')
                {
                    i++;
                    if (i >= format.Length)
                    {
                        break;
                    }
                    c = format[i];
                }
                result.Append(c);
                i++;
            }
            throw new FormatException("Cannot find a matching quote character for the character '" + quote + "'.");
        }

        internal static Exception Failure(int status) =>
            status == 2
                ? new OverflowException("The TimeSpan string '' could not be parsed because at least one of the numeric components is out of range or contains too many digits.")
                : new FormatException("String '' was not recognized as a valid TimeSpan.");

        // 0 — разобрано, 1 — не тот вид, 2 — число вне границ.
        // Вид: [ws][-]{ d | [d.]hh:mm[:ss[.ff]] | d:hh:mm[:ss[.ff]] }[ws].
        internal static int TryParse(string s, out TimeSpan result)
        {
            result = TimeSpan.Zero;
            int i = 0;
            int end = s.Length;
            while (i < end && char.IsWhiteSpace(s[i]))
            {
                i++;
            }
            while (end > i && char.IsWhiteSpace(s[end - 1]))
            {
                end--;
            }
            bool negative = false;
            if (i < end && s[i] == '-')
            {
                negative = true;
                i++;
            }
            var numbers = new long[5];
            var widths = new int[5];
            var separators = new char[5];
            int count = 0;
            while (true)
            {
                if (i >= end || !char.IsDigit(s[i]) || count == 5)
                {
                    return 1;
                }
                long number = 0;
                int width = 0;
                while (i < end && char.IsDigit(s[i]))
                {
                    if (number > 100000000000L)
                    {
                        return 2;
                    }
                    number = number * 10 + (s[i] - '0');
                    width++;
                    i++;
                }
                numbers[count] = number;
                widths[count] = width;
                count++;
                if (i >= end)
                {
                    break;
                }
                char separator = s[i];
                if (separator != ':' && separator != '.')
                {
                    return 1;
                }
                separators[count - 1] = separator;
                i++;
            }
            long days = 0, hours = 0, minutes = 0, seconds = 0, fraction = 0;
            int fractionWidth = 0;
            string shape = "";
            for (int k = 0; k + 1 < count; k++)
            {
                shape += separators[k];
            }
            switch (shape)
            {
                case "":
                    days = numbers[0];
                    break;
                case ":":
                    hours = numbers[0];
                    minutes = numbers[1];
                    break;
                case "::":
                    hours = numbers[0];
                    minutes = numbers[1];
                    seconds = numbers[2];
                    break;
                case ".:":
                    days = numbers[0];
                    hours = numbers[1];
                    minutes = numbers[2];
                    break;
                case ".::":
                    days = numbers[0];
                    hours = numbers[1];
                    minutes = numbers[2];
                    seconds = numbers[3];
                    break;
                case "::.":
                    hours = numbers[0];
                    minutes = numbers[1];
                    seconds = numbers[2];
                    fraction = numbers[3];
                    fractionWidth = widths[3];
                    break;
                case ".::.":
                    days = numbers[0];
                    hours = numbers[1];
                    minutes = numbers[2];
                    seconds = numbers[3];
                    fraction = numbers[4];
                    fractionWidth = widths[4];
                    break;
                case ":::":
                    days = numbers[0];
                    hours = numbers[1];
                    minutes = numbers[2];
                    seconds = numbers[3];
                    break;
                case ":::.":
                    days = numbers[0];
                    hours = numbers[1];
                    minutes = numbers[2];
                    seconds = numbers[3];
                    fraction = numbers[4];
                    fractionWidth = widths[4];
                    break;
                default:
                    return 1;
            }
            if (days > 10675199 || hours > 23 || minutes > 59 || seconds > 59 || fractionWidth > 7)
            {
                return 2;
            }
            for (int k = fractionWidth; k < 7; k++)
            {
                fraction *= 10;
            }
            long ticks = days * TimeSpan.TicksPerDay + hours * TimeSpan.TicksPerHour + minutes * TimeSpan.TicksPerMinute
                + seconds * TimeSpan.TicksPerSecond + fraction;
            if (ticks < 0)
            {
                return 2;
            }
            result = new TimeSpan(negative ? -ticks : ticks);
            return 0;
        }

        internal static int TryParseExact(string input, string format, out TimeSpan result)
        {
            result = TimeSpan.Zero;
            if (format.Length == 1 && (format[0] == 'c' || format[0] == 'g' || format[0] == 'G' || format[0] == 't' || format[0] == 'T'))
            {
                return TryParse(input, out result);
            }
            long days = 0, hours = 0, minutes = 0, seconds = 0, fraction = 0;
            int position = 0;
            int i = 0;
            while (i < format.Length)
            {
                char c = format[i];
                int length;
                switch (c)
                {
                    case 'd':
                    case 'h':
                    case 'm':
                    case 's':
                    case 'f':
                    case 'F':
                        length = Repeat(format, i);
                        int limit = c == 'd' ? 8 : c == 'f' || c == 'F' ? 7 : 2;
                        if (length > limit)
                        {
                            return 1;
                        }
                        // Одна буква — одна или две цифры, больше букв — ровно
                        // столько цифр (у d — не меньше).
                        int start = position;
                        int most = c == 'f' || (length == 2 && c != 'd') ? length : c == 'd' ? 8 : c == 'F' ? length : 2;
                        long number = 0;
                        while (position < input.Length && position - start < most && char.IsDigit(input[position]))
                        {
                            number = number * 10 + (input[position] - '0');
                            position++;
                        }
                        int got = position - start;
                        if (got == 0 || (c == 'f' && got != length) || (c == 'd' && got < length) || (length == 2 && c != 'd' && c != 'F' && got != 2))
                        {
                            return 1;
                        }
                        if (c == 'd')
                        {
                            days = number;
                        }
                        else if (c == 'h')
                        {
                            hours = number;
                        }
                        else if (c == 'm')
                        {
                            minutes = number;
                        }
                        else if (c == 's')
                        {
                            seconds = number;
                        }
                        else
                        {
                            for (int k = got; k < 7; k++)
                            {
                                number *= 10;
                            }
                            fraction = number;
                        }
                        break;
                    case '\'':
                    case '"':
                        var literal = new StringBuilder();
                        length = Quoted(format, i, literal);
                        string text = literal.ToString();
                        if (position + text.Length > input.Length || input.Substring(position, text.Length) != text)
                        {
                            return 1;
                        }
                        position += text.Length;
                        break;
                    case '%':
                        length = 1;
                        break;
                    case '\\':
                        if (i + 1 >= format.Length || position >= input.Length || input[position] != format[i + 1])
                        {
                            return 1;
                        }
                        position++;
                        length = 2;
                        break;
                    default:
                        return 1;
                }
                i += length;
            }
            if (position != input.Length)
            {
                return 1;
            }
            if (hours > 23 || minutes > 59 || seconds > 59)
            {
                return 2;
            }
            result = new TimeSpan(days * TimeSpan.TicksPerDay + hours * TimeSpan.TicksPerHour + minutes * TimeSpan.TicksPerMinute
                + seconds * TimeSpan.TicksPerSecond + fraction);
            return 0;
        }
    }
}
