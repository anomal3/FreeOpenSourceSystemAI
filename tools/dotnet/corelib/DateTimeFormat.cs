// Печать и разбор DateTime (фаза N5b) по правилам инвариантной культуры:
// стандартные форматы раскрываются в шаблоны, шаблон печатается и разбирается
// по буквам, как в DateTimeFormat/DateTimeParse у .NET. Строки обходятся циклом
// по индексу, не foreach (см. IO.cs).

using System.Globalization;
using System.Text;

namespace System
{
    internal static class DateTimeFormat
    {
        private static readonly string[] DayNames = { "Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday" };
        private static readonly string[] DayAbbreviations = { "Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat" };
        private static readonly string[] MonthNames =
            { "January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December" };
        private static readonly string[] MonthAbbreviations = { "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec" };

        private const string RoundTrip = "yyyy'-'MM'-'dd'T'HH':'mm':'ss'.'fffffffK";

        private static FormatException BadFormat() => new FormatException("Input string was not in a correct format.");

        internal static FormatException Failure(string s) => new FormatException("String '" + s + "' was not recognized as a valid DateTime.");

        // Стандартный формат — одна буква; `null` — если это не он.
        private static string Expand(char c)
        {
            switch (c)
            {
                case 'd':
                    return "MM/dd/yyyy";
                case 'D':
                    return "dddd, dd MMMM yyyy";
                case 'f':
                    return "dddd, dd MMMM yyyy HH:mm";
                case 'F':
                case 'U':
                    return "dddd, dd MMMM yyyy HH:mm:ss";
                case 'g':
                    return "MM/dd/yyyy HH:mm";
                case 'G':
                    return "MM/dd/yyyy HH:mm:ss";
                case 'm':
                case 'M':
                    return "MMMM dd";
                case 'o':
                case 'O':
                    return RoundTrip;
                case 'r':
                case 'R':
                    return "ddd, dd MMM yyyy HH':'mm':'ss 'GMT'";
                case 's':
                    return "yyyy'-'MM'-'dd'T'HH':'mm':'ss";
                case 't':
                    return "HH:mm";
                case 'T':
                    return "HH:mm:ss";
                case 'u':
                    return "yyyy'-'MM'-'dd HH':'mm':'ss'Z'";
                case 'y':
                case 'Y':
                    return "yyyy MMMM";
                default:
                    return null;
            }
        }

        internal static string Format(DateTime value, string format)
        {
            if (format == null || format.Length == 0)
            {
                format = "G";
            }
            if (format.Length == 1)
            {
                string pattern = Expand(format[0]);
                if (pattern == null)
                {
                    throw BadFormat();
                }
                if (format[0] == 'U')
                {
                    value = value.ToUniversalTime();
                }
                format = pattern;
            }
            var result = new StringBuilder();
            Custom(value, format, result);
            return result.ToString();
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

        private static void Digits(StringBuilder result, long value, int width) => TimeSpanFormat.Digits(result, value, width);

        private static void Custom(DateTime value, string format, StringBuilder result)
        {
            long ticks = value.Ticks;
            DateTime.GetDate(ticks, out int year, out int month, out int day, out _);
            int i = 0;
            while (i < format.Length)
            {
                char c = format[i];
                int length;
                switch (c)
                {
                    case 'g':
                        length = Repeat(format, i);
                        result.Append("A.D.");
                        break;
                    case 'h':
                        length = Repeat(format, i);
                        int hour12 = value.Hour % 12;
                        Digits(result, hour12 == 0 ? 12 : hour12, length > 2 ? 2 : length);
                        break;
                    case 'H':
                        length = Repeat(format, i);
                        Digits(result, value.Hour, length > 2 ? 2 : length);
                        break;
                    case 'm':
                        length = Repeat(format, i);
                        Digits(result, value.Minute, length > 2 ? 2 : length);
                        break;
                    case 's':
                        length = Repeat(format, i);
                        Digits(result, value.Second, length > 2 ? 2 : length);
                        break;
                    case 'f':
                    case 'F':
                        length = Repeat(format, i);
                        if (length > 7)
                        {
                            throw BadFormat();
                        }
                        long fraction = ticks % 10000000;
                        for (int k = length; k < 7; k++)
                        {
                            fraction /= 10;
                        }
                        if (c == 'f')
                        {
                            Digits(result, fraction, length);
                        }
                        else
                        {
                            int digits = length;
                            while (digits > 0 && fraction % 10 == 0)
                            {
                                fraction /= 10;
                                digits--;
                            }
                            if (digits > 0)
                            {
                                Digits(result, fraction, digits);
                            }
                            else if (result.Length > 0 && result[result.Length - 1] == '.')
                            {
                                // Пустая дробь уносит и точку перед собой.
                                result.Remove(result.Length - 1, 1);
                            }
                        }
                        break;
                    case 't':
                        length = Repeat(format, i);
                        string designator = value.Hour < 12 ? "AM" : "PM";
                        if (length == 1)
                        {
                            result.Append(designator[0]);
                        }
                        else
                        {
                            result.Append(designator);
                        }
                        break;
                    case 'd':
                        length = Repeat(format, i);
                        if (length <= 2)
                        {
                            Digits(result, day, length);
                        }
                        else
                        {
                            int dayOfWeek = (int)value.DayOfWeek;
                            result.Append(length == 3 ? DayAbbreviations[dayOfWeek] : DayNames[dayOfWeek]);
                        }
                        break;
                    case 'M':
                        length = Repeat(format, i);
                        if (length <= 2)
                        {
                            Digits(result, month, length);
                        }
                        else
                        {
                            result.Append(length == 3 ? MonthAbbreviations[month - 1] : MonthNames[month - 1]);
                        }
                        break;
                    case 'y':
                        length = Repeat(format, i);
                        Digits(result, length <= 2 ? year % 100 : year, length);
                        break;
                    case 'z':
                        length = Repeat(format, i);
                        long offset = value.Kind == DateTimeKind.Utc ? 0 : DateTime.LocalOffsetTicks;
                        Offset(result, offset, length);
                        break;
                    case 'K':
                        length = 1;
                        if (value.Kind == DateTimeKind.Local)
                        {
                            Offset(result, DateTime.LocalOffsetTicks, 3);
                        }
                        else if (value.Kind == DateTimeKind.Utc)
                        {
                            result.Append('Z');
                        }
                        break;
                    case ':':
                    case '/':
                        length = 1;
                        result.Append(c);
                        break;
                    case '\'':
                    case '"':
                        length = TimeSpanFormat.Quoted(format, i, result);
                        break;
                    case '%':
                        if (i + 1 >= format.Length || format[i + 1] == '%')
                        {
                            throw BadFormat();
                        }
                        Custom(value, format.Substring(i + 1, 1), result);
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
                        length = 1;
                        result.Append(c);
                        break;
                }
                i += length;
            }
        }

        // z — «+3», zz — «+03», zzz — «+03:00».
        private static void Offset(StringBuilder result, long offset, int length)
        {
            if (offset < 0)
            {
                result.Append('-');
                offset = -offset;
            }
            else
            {
                result.Append('+');
            }
            int hours = (int)(offset / TimeSpan.TicksPerHour);
            int minutes = (int)(offset / TimeSpan.TicksPerMinute % 60);
            if (length == 1)
            {
                result.Append(hours);
                return;
            }
            Digits(result, hours, 2);
            if (length >= 3)
            {
                result.Append(':');
                Digits(result, minutes, 2);
            }
        }

        // Разобранные части даты; -1 — части не было.
        private struct Parts
        {
            internal int Year;
            internal int Month;
            internal int Day;
            internal int Hour;
            internal int Minute;
            internal int Second;
            internal long Fraction;
            internal int Meridiem;
            internal bool HasOffset;
            internal long Offset;
            internal bool TwoDigitYear;
        }

        private static Parts NewParts()
        {
            var parts = new Parts();
            parts.Year = -1;
            parts.Month = -1;
            parts.Day = -1;
            parts.Meridiem = -1;
            return parts;
        }

        private static bool Build(ref Parts parts, DateTimeStyles styles, out DateTime result)
        {
            result = DateTime.MinValue;
            if (parts.Year == -1 && parts.Month == -1 && parts.Day == -1)
            {
                if ((styles & DateTimeStyles.NoCurrentDateDefault) != 0)
                {
                    parts.Year = 1;
                    parts.Month = 1;
                    parts.Day = 1;
                }
                else
                {
                    DateTime today = DateTime.Now;
                    parts.Year = today.Year;
                    parts.Month = today.Month;
                    parts.Day = today.Day;
                }
            }
            if (parts.Year == -1)
            {
                parts.Year = DateTime.Now.Year;
            }
            else if (parts.TwoDigitYear && parts.Year < 100)
            {
                // Окно двузначного года инвариантного календаря — до 2049.
                parts.Year += parts.Year <= 49 ? 2000 : 1900;
            }
            if (parts.Month == -1)
            {
                parts.Month = 1;
            }
            if (parts.Day == -1)
            {
                parts.Day = 1;
            }
            if (parts.Meridiem != -1)
            {
                if (parts.Hour > 12)
                {
                    return false;
                }
                if (parts.Meridiem == 0 && parts.Hour == 12)
                {
                    parts.Hour = 0;
                }
                else if (parts.Meridiem == 1 && parts.Hour < 12)
                {
                    parts.Hour += 12;
                }
            }
            if (parts.Hour > 23 || parts.Minute > 59 || parts.Second > 59)
            {
                return false;
            }
            if (!DateTime.TryDateToTicks(parts.Year, parts.Month, parts.Day, out long ticks))
            {
                return false;
            }
            ticks += parts.Hour * TimeSpan.TicksPerHour + parts.Minute * TimeSpan.TicksPerMinute + parts.Second * TimeSpan.TicksPerSecond + parts.Fraction;
            if (!parts.HasOffset)
            {
                result = new DateTime(ticks, (styles & DateTimeStyles.AssumeUniversal) != 0 ? DateTimeKind.Utc : DateTimeKind.Unspecified);
                return true;
            }
            ticks -= parts.Offset;
            if (ticks < 0 || ticks > DateTime.MaxTicks)
            {
                return false;
            }
            var utc = new DateTime(ticks, DateTimeKind.Utc);
            result = (styles & DateTimeStyles.AdjustToUniversal) != 0 || (styles & DateTimeStyles.RoundtripKind) != 0 && parts.Offset == 0 ? utc : utc.ToLocalTime();
            return true;
        }

        private static bool Number(string s, ref int position, int least, int most, out int value)
        {
            value = 0;
            int start = position;
            while (position < s.Length && position - start < most && char.IsDigit(s[position]))
            {
                value = value * 10 + (s[position] - '0');
                position++;
            }
            return position - start >= least;
        }

        // Одно из имён без учёта регистра; номер или -1.
        private static int Name(string s, ref int position, string[] names)
        {
            int best = -1;
            int bestLength = 0;
            for (int k = 0; k < names.Length; k++)
            {
                string name = names[k];
                if (name.Length > bestLength && position + name.Length <= s.Length
                    && string.Equals(s.Substring(position, name.Length), name, StringComparison.OrdinalIgnoreCase))
                {
                    best = k;
                    bestLength = name.Length;
                }
            }
            position += bestLength;
            return best;
        }

        private static bool ParseOffset(string s, ref int position, int length, out long offset)
        {
            offset = 0;
            if (position >= s.Length || (s[position] != '+' && s[position] != '-'))
            {
                return false;
            }
            bool negative = s[position] == '-';
            position++;
            if (!Number(s, ref position, length == 1 ? 1 : 2, 2, out int hours))
            {
                return false;
            }
            int minutes = 0;
            if (length >= 3 || (position < s.Length && s[position] == ':'))
            {
                if (position >= s.Length || s[position] != ':')
                {
                    return false;
                }
                position++;
                if (!Number(s, ref position, 2, 2, out minutes))
                {
                    return false;
                }
            }
            if (hours > 14 || minutes > 59)
            {
                return false;
            }
            offset = hours * TimeSpan.TicksPerHour + minutes * TimeSpan.TicksPerMinute;
            if (negative)
            {
                offset = -offset;
            }
            return true;
        }

        internal static bool TryParseExact(string s, string format, DateTimeStyles styles, out DateTime result)
        {
            result = DateTime.MinValue;
            if (format.Length == 0)
            {
                return false;
            }
            if (format.Length == 1)
            {
                format = Expand(format[0]);
                if (format == null)
                {
                    return false;
                }
            }
            Parts parts = NewParts();
            int position = 0;
            if ((styles & DateTimeStyles.AllowLeadingWhite) != 0)
            {
                while (position < s.Length && char.IsWhiteSpace(s[position]))
                {
                    position++;
                }
            }
            int i = 0;
            while (i < format.Length)
            {
                char c = format[i];
                int length = 1;
                int value;
                switch (c)
                {
                    case 'd':
                    case 'M':
                        length = Repeat(format, i);
                        if (length <= 2)
                        {
                            if (!Number(s, ref position, length, 2, out value))
                            {
                                return false;
                            }
                            if (c == 'd')
                            {
                                parts.Day = value;
                            }
                            else
                            {
                                parts.Month = value;
                            }
                        }
                        else if (c == 'd')
                        {
                            if (Name(s, ref position, length == 3 ? DayAbbreviations : DayNames) < 0)
                            {
                                return false;
                            }
                        }
                        else
                        {
                            int index = Name(s, ref position, length == 3 ? MonthAbbreviations : MonthNames);
                            if (index < 0)
                            {
                                return false;
                            }
                            parts.Month = index + 1;
                        }
                        break;
                    case 'y':
                        length = Repeat(format, i);
                        if (!Number(s, ref position, length <= 2 ? length : length, length <= 2 ? 2 : length, out value))
                        {
                            return false;
                        }
                        parts.Year = value;
                        parts.TwoDigitYear = length <= 2;
                        break;
                    case 'h':
                    case 'H':
                    case 'm':
                    case 's':
                        length = Repeat(format, i);
                        if (length > 2 || !Number(s, ref position, length, 2, out value))
                        {
                            return false;
                        }
                        if (c == 'm')
                        {
                            parts.Minute = value;
                        }
                        else if (c == 's')
                        {
                            parts.Second = value;
                        }
                        else
                        {
                            parts.Hour = value;
                        }
                        break;
                    case 'f':
                    case 'F':
                        length = Repeat(format, i);
                        if (length > 7)
                        {
                            return false;
                        }
                        int start = position;
                        long fraction = 0;
                        while (position < s.Length && position - start < length && char.IsDigit(s[position]))
                        {
                            fraction = fraction * 10 + (s[position] - '0');
                            position++;
                        }
                        if (c == 'f' && position - start != length)
                        {
                            return false;
                        }
                        for (int k = position - start; k < 7; k++)
                        {
                            fraction *= 10;
                        }
                        parts.Fraction = fraction;
                        break;
                    case 't':
                        length = Repeat(format, i);
                        if (position >= s.Length)
                        {
                            return false;
                        }
                        char letter = char.ToUpperInvariant(s[position]);
                        if (letter != 'A' && letter != 'P')
                        {
                            return false;
                        }
                        position++;
                        if (length >= 2)
                        {
                            if (position >= s.Length || char.ToUpperInvariant(s[position]) != 'M')
                            {
                                return false;
                            }
                            position++;
                        }
                        parts.Meridiem = letter == 'A' ? 0 : 1;
                        break;
                    case 'z':
                        length = Repeat(format, i);
                        if (!ParseOffset(s, ref position, length, out parts.Offset))
                        {
                            return false;
                        }
                        parts.HasOffset = true;
                        break;
                    case 'K':
                        if (position < s.Length && s[position] == 'Z')
                        {
                            position++;
                            parts.HasOffset = true;
                        }
                        else if (position < s.Length && (s[position] == '+' || s[position] == '-'))
                        {
                            if (!ParseOffset(s, ref position, 3, out parts.Offset))
                            {
                                return false;
                            }
                            parts.HasOffset = true;
                        }
                        break;
                    case 'g':
                        length = Repeat(format, i);
                        if (position + 4 > s.Length || !string.Equals(s.Substring(position, 4), "A.D.", StringComparison.OrdinalIgnoreCase))
                        {
                            return false;
                        }
                        position += 4;
                        break;
                    case '\'':
                    case '"':
                        var literal = new StringBuilder();
                        length = TimeSpanFormat.Quoted(format, i, literal);
                        string text = literal.ToString();
                        if (position + text.Length > s.Length || s.Substring(position, text.Length) != text)
                        {
                            return false;
                        }
                        position += text.Length;
                        break;
                    case '%':
                        break;
                    case '\\':
                        if (i + 1 >= format.Length || position >= s.Length || s[position] != format[i + 1])
                        {
                            return false;
                        }
                        position++;
                        length = 2;
                        break;
                    default:
                        if (char.IsWhiteSpace(c) && (styles & DateTimeStyles.AllowInnerWhite) != 0)
                        {
                            while (position < s.Length && char.IsWhiteSpace(s[position]))
                            {
                                position++;
                            }
                            break;
                        }
                        if (position >= s.Length || s[position] != c)
                        {
                            return false;
                        }
                        position++;
                        break;
                }
                i += length;
            }
            if ((styles & DateTimeStyles.AllowTrailingWhite) != 0)
            {
                while (position < s.Length && char.IsWhiteSpace(s[position]))
                {
                    position++;
                }
            }
            return position == s.Length && Build(ref parts, styles, out result);
        }

        // Разбор без шаблона: числа, имена месяцев и дней, время через «:»,
        // AM/PM, Z/GMT и смещение. Три числа даты — год впереди, если в нём
        // больше двух цифр, иначе месяц/день/год, как у инвариантной культуры.
        internal static bool TryParse(string s, DateTimeStyles styles, out DateTime result)
        {
            result = DateTime.MinValue;
            Parts parts = NewParts();
            var numbers = new int[3];
            var widths = new int[3];
            int count = 0;
            bool time = false;
            bool seenMonth = false;
            int position = 0;
            while (position < s.Length)
            {
                char c = s[position];
                if (char.IsWhiteSpace(c) || c == ',' || c == '/' || c == '-' && !time || c == '.')
                {
                    position++;
                    continue;
                }
                if (char.IsDigit(c))
                {
                    int start = position;
                    Number(s, ref position, 1, 9, out int value);
                    if (!time && position < s.Length && s[position] == ':')
                    {
                        // Время: ч:м[:с[.дробь]].
                        time = true;
                        parts.Hour = value;
                        position++;
                        if (!Number(s, ref position, 1, 2, out parts.Minute))
                        {
                            return false;
                        }
                        if (position < s.Length && s[position] == ':')
                        {
                            position++;
                            if (!Number(s, ref position, 1, 2, out parts.Second))
                            {
                                return false;
                            }
                            if (position + 1 < s.Length && s[position] == '.' && char.IsDigit(s[position + 1]))
                            {
                                position++;
                                int fractionStart = position;
                                long fraction = 0;
                                while (position < s.Length && char.IsDigit(s[position]))
                                {
                                    if (position - fractionStart < 7)
                                    {
                                        fraction = fraction * 10 + (s[position] - '0');
                                    }
                                    position++;
                                }
                                for (int k = position - fractionStart; k < 7; k++)
                                {
                                    fraction *= 10;
                                }
                                parts.Fraction = fraction;
                            }
                        }
                        continue;
                    }
                    if (count == 3)
                    {
                        return false;
                    }
                    numbers[count] = value;
                    widths[count] = position - start;
                    count++;
                    continue;
                }
                if (time && (c == '+' || c == '-'))
                {
                    if (parts.HasOffset || !ParseOffset(s, ref position, 2, out parts.Offset))
                    {
                        return false;
                    }
                    parts.HasOffset = true;
                    continue;
                }
                if (char.IsLetter(c))
                {
                    int start = position;
                    while (position < s.Length && char.IsLetter(s[position]))
                    {
                        position++;
                    }
                    string word = s.Substring(start, position - start);
                    int probe = 0;
                    int month = Name(word, ref probe, MonthNames);
                    if (month < 0 || probe != word.Length)
                    {
                        probe = 0;
                        month = Name(word, ref probe, MonthAbbreviations);
                    }
                    if (month >= 0 && probe == word.Length)
                    {
                        if (seenMonth)
                        {
                            return false;
                        }
                        seenMonth = true;
                        parts.Month = month + 1;
                        continue;
                    }
                    probe = 0;
                    int dayName = Name(word, ref probe, DayNames);
                    if (dayName < 0 || probe != word.Length)
                    {
                        probe = 0;
                        dayName = Name(word, ref probe, DayAbbreviations);
                    }
                    if (dayName >= 0 && probe == word.Length)
                    {
                        continue;
                    }
                    string upper = word.ToUpperInvariant();
                    if (upper == "T" && !time)
                    {
                        continue;
                    }
                    if ((upper == "AM" || upper == "PM" || upper == "A" || upper == "P") && time && parts.Meridiem == -1)
                    {
                        parts.Meridiem = upper[0] == 'A' ? 0 : 1;
                        continue;
                    }
                    if ((upper == "Z" || upper == "GMT" || upper == "UTC") && !parts.HasOffset)
                    {
                        parts.HasOffset = true;
                        parts.Offset = 0;
                        continue;
                    }
                    return false;
                }
                return false;
            }
            if (seenMonth)
            {
                if (count == 2)
                {
                    bool yearFirst = widths[0] > 2 || numbers[0] > 31;
                    parts.Year = yearFirst ? numbers[0] : numbers[1];
                    parts.Day = yearFirst ? numbers[1] : numbers[0];
                    parts.TwoDigitYear = (yearFirst ? widths[0] : widths[1]) <= 2;
                }
                else if (count == 1)
                {
                    if (widths[0] > 2)
                    {
                        parts.Year = numbers[0];
                        parts.Day = 1;
                    }
                    else
                    {
                        parts.Day = numbers[0];
                        parts.Year = DateTime.Now.Year;
                    }
                }
                else
                {
                    return false;
                }
            }
            else if (count == 3)
            {
                if (widths[0] > 2)
                {
                    parts.Year = numbers[0];
                    parts.Month = numbers[1];
                    parts.Day = numbers[2];
                }
                else
                {
                    parts.Month = numbers[0];
                    parts.Day = numbers[1];
                    parts.Year = numbers[2];
                    parts.TwoDigitYear = widths[2] <= 2;
                }
            }
            else if (count == 2)
            {
                if (widths[0] > 2)
                {
                    parts.Year = numbers[0];
                    parts.Month = numbers[1];
                    parts.Day = 1;
                }
                else
                {
                    parts.Month = numbers[0];
                    parts.Day = numbers[1];
                    parts.Year = DateTime.Now.Year;
                }
            }
            else if (count != 0 || !time)
            {
                return false;
            }
            return Build(ref parts, styles, out result);
        }
    }
}
