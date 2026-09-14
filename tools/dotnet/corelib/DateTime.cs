// DateTime (фаза N5b): тики по 100 нс от 1 января 0001 года и вид (UTC,
// местное, не указано). Календарная арифметика — та же, что у .NET; часы и
// часовой пояс спрашиваются у среды (`Clock`).

using System.Globalization;

namespace System
{
    public enum DateTimeKind
    {
        Unspecified = 0,
        Utc = 1,
        Local = 2,
    }

    public enum DayOfWeek
    {
        Sunday = 0,
        Monday = 1,
        Tuesday = 2,
        Wednesday = 3,
        Thursday = 4,
        Friday = 5,
        Saturday = 6,
    }

    public struct DateTime : IComparable, IComparable<DateTime>, IEquatable<DateTime>, IFormattable
    {
        private const long TicksPerMillisecond = 10000;
        private const long TicksPerSecond = 10000000;
        private const long TicksPerMinute = 600000000;
        private const long TicksPerHour = 36000000000;
        private const long TicksPerDay = 864000000000;

        private const int DaysPerYear = 365;
        private const int DaysPer4Years = 1461;
        private const int DaysPer100Years = 36524;
        private const int DaysPer400Years = 146097;
        private const int DaysTo1970 = 719162;
        private const int DaysTo10000 = 3652059;

        internal const long MaxTicks = DaysTo10000 * TicksPerDay - 1;
        internal const long UnixEpochTicks = DaysTo1970 * TicksPerDay;

        private static readonly int[] DaysToMonth365 = { 0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334, 365 };
        private static readonly int[] DaysToMonth366 = { 0, 31, 60, 91, 121, 152, 182, 213, 244, 274, 305, 335, 366 };

        public static readonly DateTime MinValue = new DateTime(0, DateTimeKind.Unspecified);
        public static readonly DateTime MaxValue = new DateTime(MaxTicks, DateTimeKind.Unspecified);
        public static readonly DateTime UnixEpoch = new DateTime(UnixEpochTicks, DateTimeKind.Utc);

        private readonly long ticks;
        private readonly DateTimeKind kind;

        public DateTime(long ticks)
            : this(ticks, DateTimeKind.Unspecified)
        {
        }

        public DateTime(long ticks, DateTimeKind kind)
        {
            if (ticks < 0 || ticks > MaxTicks)
            {
                throw new ArgumentOutOfRangeException("ticks", "Ticks must be between DateTime.MinValue.Ticks and DateTime.MaxValue.Ticks.");
            }
            CheckKind(kind);
            this.ticks = ticks;
            this.kind = kind;
        }

        public DateTime(int year, int month, int day)
        {
            ticks = DateToTicks(year, month, day);
            kind = DateTimeKind.Unspecified;
        }

        public DateTime(int year, int month, int day, int hour, int minute, int second)
            : this(year, month, day, hour, minute, second, 0, DateTimeKind.Unspecified)
        {
        }

        public DateTime(int year, int month, int day, int hour, int minute, int second, DateTimeKind kind)
            : this(year, month, day, hour, minute, second, 0, kind)
        {
        }

        public DateTime(int year, int month, int day, int hour, int minute, int second, int millisecond)
            : this(year, month, day, hour, minute, second, millisecond, DateTimeKind.Unspecified)
        {
        }

        public DateTime(int year, int month, int day, int hour, int minute, int second, int millisecond, DateTimeKind kind)
        {
            if (millisecond < 0 || millisecond >= 1000)
            {
                throw new ArgumentOutOfRangeException("millisecond", "Valid values are between 0 and 999, inclusive.");
            }
            CheckKind(kind);
            ticks = DateToTicks(year, month, day) + TimeToTicks(hour, minute, second) + millisecond * TicksPerMillisecond;
            this.kind = kind;
        }

        private static void CheckKind(DateTimeKind kind)
        {
            if (kind < DateTimeKind.Unspecified || kind > DateTimeKind.Local)
            {
                throw new ArgumentException("Invalid DateTimeKind value.", "kind");
            }
        }

        internal static bool TryDateToTicks(int year, int month, int day, out long result)
        {
            result = 0;
            if (year < 1 || year > 9999 || month < 1 || month > 12 || day < 1)
            {
                return false;
            }
            int[] days = IsLeap(year) ? DaysToMonth366 : DaysToMonth365;
            if (day > days[month] - days[month - 1])
            {
                return false;
            }
            int y = year - 1;
            int n = y * 365 + y / 4 - y / 100 + y / 400 + days[month - 1] + day - 1;
            result = n * TicksPerDay;
            return true;
        }

        private static long DateToTicks(int year, int month, int day)
        {
            if (!TryDateToTicks(year, month, day, out long result))
            {
                throw new ArgumentOutOfRangeException(null, "Year, Month, and Day parameters describe an un-representable DateTime.");
            }
            return result;
        }

        private static long TimeToTicks(int hour, int minute, int second)
        {
            if ((uint)hour >= 24 || (uint)minute >= 60 || (uint)second >= 60)
            {
                throw new ArgumentOutOfRangeException(null, "Hour, Minute, and Second parameters describe an un-representable DateTime.");
            }
            return hour * TicksPerHour + minute * TicksPerMinute + second * TicksPerSecond;
        }

        private static bool IsLeap(int year) => (year & 3) == 0 && ((year & 15) == 0 || year % 25 != 0);

        // Год, месяц и день из тиков — как GetDate в .NET.
        internal static void GetDate(long ticks, out int year, out int month, out int day, out int dayOfYear)
        {
            int n = (int)(ticks / TicksPerDay);
            int y400 = n / DaysPer400Years;
            n -= y400 * DaysPer400Years;
            int y100 = n / DaysPer100Years;
            if (y100 == 4)
            {
                y100 = 3;
            }
            n -= y100 * DaysPer100Years;
            int y4 = n / DaysPer4Years;
            n -= y4 * DaysPer4Years;
            int y1 = n / DaysPerYear;
            if (y1 == 4)
            {
                y1 = 3;
            }
            year = y400 * 400 + y100 * 100 + y4 * 4 + y1 + 1;
            n -= y1 * DaysPerYear;
            dayOfYear = n + 1;
            bool leap = y1 == 3 && (y4 != 24 || y100 == 3);
            int[] days = leap ? DaysToMonth366 : DaysToMonth365;
            int m = (n >> 5) + 1;
            while (n >= days[m])
            {
                m++;
            }
            month = m;
            day = n - days[m - 1] + 1;
        }

        public long Ticks => ticks;

        public DateTimeKind Kind => kind;

        public int Year
        {
            get
            {
                GetDate(ticks, out int year, out _, out _, out _);
                return year;
            }
        }

        public int Month
        {
            get
            {
                GetDate(ticks, out _, out int month, out _, out _);
                return month;
            }
        }

        public int Day
        {
            get
            {
                GetDate(ticks, out _, out _, out int day, out _);
                return day;
            }
        }

        public int DayOfYear
        {
            get
            {
                GetDate(ticks, out _, out _, out _, out int dayOfYear);
                return dayOfYear;
            }
        }

        public DayOfWeek DayOfWeek => (DayOfWeek)((int)(ticks / TicksPerDay + 1) % 7);

        public int Hour => (int)(ticks / TicksPerHour % 24);

        public int Minute => (int)(ticks / TicksPerMinute % 60);

        public int Second => (int)(ticks / TicksPerSecond % 60);

        public int Millisecond => (int)(ticks / TicksPerMillisecond % 1000);

        public int Microsecond => (int)(ticks / 10 % 1000);

        public int Nanosecond => (int)(ticks % 10 * 100);

        public DateTime Date => new DateTime(ticks - ticks % TicksPerDay, kind);

        public TimeSpan TimeOfDay => new TimeSpan(ticks % TicksPerDay);

        public static DateTime UtcNow => new DateTime(UnixEpochTicks + Clock.UtcTicks(), DateTimeKind.Utc);

        public static DateTime Now => UtcNow.ToLocalTime();

        public static DateTime Today => Now.Date;

        // Смещение пояса у FreeOS постоянное (строка `timezone=` настроек):
        // летнего времени нет, и оно не зависит от даты.
        internal static long LocalOffsetTicks => Clock.LocalOffsetMinutes() * TicksPerMinute;

        public DateTime ToLocalTime()
        {
            if (kind == DateTimeKind.Local)
            {
                return this;
            }
            return new DateTime(Clamp(ticks + LocalOffsetTicks), DateTimeKind.Local);
        }

        public DateTime ToUniversalTime()
        {
            if (kind == DateTimeKind.Utc)
            {
                return this;
            }
            return new DateTime(Clamp(ticks - LocalOffsetTicks), DateTimeKind.Utc);
        }

        private static long Clamp(long value) => value < 0 ? 0 : value > MaxTicks ? MaxTicks : value;

        public static DateTime SpecifyKind(DateTime value, DateTimeKind kind) => new DateTime(value.ticks, kind);

        private static ArgumentOutOfRangeException AddOutOfRange() =>
            new ArgumentOutOfRangeException("value", "The added or subtracted value results in an un-representable DateTime.");

        public DateTime AddTicks(long value)
        {
            if (value > MaxTicks - ticks || value < -ticks)
            {
                throw AddOutOfRange();
            }
            return new DateTime(ticks + value, kind);
        }

        public DateTime Add(TimeSpan value) => AddTicks(value.Ticks);

        // Целая часть и дробь переводятся в тики порознь, как в .NET 7+.
        private DateTime AddUnits(double value, long ticksPerUnit)
        {
            if (Math.Abs(value) > MaxTicks / ticksPerUnit)
            {
                throw AddOutOfRange();
            }
            double integral = Math.Truncate(value);
            double fraction = value - integral;
            long result = (long)integral * ticksPerUnit;
            result += (long)(fraction * ticksPerUnit);
            return AddTicks(result);
        }

        public DateTime AddDays(double value) => AddUnits(value, TicksPerDay);

        public DateTime AddHours(double value) => AddUnits(value, TicksPerHour);

        public DateTime AddMinutes(double value) => AddUnits(value, TicksPerMinute);

        public DateTime AddSeconds(double value) => AddUnits(value, TicksPerSecond);

        public DateTime AddMilliseconds(double value) => AddUnits(value, TicksPerMillisecond);

        public DateTime AddMicroseconds(double value) => AddUnits(value, 10);

        public DateTime AddMonths(int months)
        {
            if (months < -120000 || months > 120000)
            {
                throw new ArgumentOutOfRangeException("months", "Months value must be between +/-120000.");
            }
            GetDate(ticks, out int year, out int month, out int day, out _);
            int i = month - 1 + months;
            if (i >= 0)
            {
                month = i % 12 + 1;
                year += i / 12;
            }
            else
            {
                month = 12 + (i + 1) % 12;
                year += (i - 11) / 12;
            }
            if (year < 1 || year > 9999)
            {
                throw new ArgumentOutOfRangeException("months", "The added or subtracted value results in an un-representable DateTime.");
            }
            int days = DaysInMonth(year, month);
            if (day > days)
            {
                day = days;
            }
            return new DateTime(DateToTicks(year, month, day) + ticks % TicksPerDay, kind);
        }

        public DateTime AddYears(int value)
        {
            if (value < -10000 || value > 10000)
            {
                throw new ArgumentOutOfRangeException("years", "Years value must be between +/-10000.");
            }
            return AddMonths(value * 12);
        }

        public TimeSpan Subtract(DateTime value) => new TimeSpan(ticks - value.ticks);

        public DateTime Subtract(TimeSpan value) => AddTicks(-value.Ticks);

        public static bool IsLeapYear(int year)
        {
            if (year < 1 || year > 9999)
            {
                throw new ArgumentOutOfRangeException("year", "Year must be between 1 and 9999.");
            }
            return IsLeap(year);
        }

        public static int DaysInMonth(int year, int month)
        {
            if (month < 1 || month > 12)
            {
                throw new ArgumentOutOfRangeException("month", "Month must be between one and twelve.");
            }
            int[] days = IsLeapYear(year) ? DaysToMonth366 : DaysToMonth365;
            return days[month] - days[month - 1];
        }

        public static DateTime operator +(DateTime d, TimeSpan t) => d.AddTicks(t.Ticks);

        public static DateTime operator -(DateTime d, TimeSpan t) => d.AddTicks(-t.Ticks);

        public static TimeSpan operator -(DateTime d1, DateTime d2) => new TimeSpan(d1.ticks - d2.ticks);

        public static bool operator ==(DateTime d1, DateTime d2) => d1.ticks == d2.ticks;

        public static bool operator !=(DateTime d1, DateTime d2) => d1.ticks != d2.ticks;

        public static bool operator <(DateTime t1, DateTime t2) => t1.ticks < t2.ticks;

        public static bool operator <=(DateTime t1, DateTime t2) => t1.ticks <= t2.ticks;

        public static bool operator >(DateTime t1, DateTime t2) => t1.ticks > t2.ticks;

        public static bool operator >=(DateTime t1, DateTime t2) => t1.ticks >= t2.ticks;

        public static int Compare(DateTime t1, DateTime t2) => t1.ticks < t2.ticks ? -1 : t1.ticks > t2.ticks ? 1 : 0;

        public int CompareTo(DateTime value) => Compare(this, value);

        public int CompareTo(object value)
        {
            if (value == null)
            {
                return 1;
            }
            if (value is DateTime other)
            {
                return Compare(this, other);
            }
            throw new ArgumentException("Object must be of type DateTime.");
        }

        public bool Equals(DateTime value) => ticks == value.ticks;

        public override bool Equals(object value) => value is DateTime other && ticks == other.ticks;

        public static bool Equals(DateTime t1, DateTime t2) => t1.ticks == t2.ticks;

        public override int GetHashCode() => (int)ticks ^ (int)(ticks >> 32);

        public override string ToString() => DateTimeFormat.Format(this, null);

        public string ToString(string format) => DateTimeFormat.Format(this, format);

        public string ToString(IFormatProvider provider) => DateTimeFormat.Format(this, null);

        public string ToString(string format, IFormatProvider provider) => DateTimeFormat.Format(this, format);

        public string ToShortDateString() => DateTimeFormat.Format(this, "d");

        public string ToLongDateString() => DateTimeFormat.Format(this, "D");

        public string ToShortTimeString() => DateTimeFormat.Format(this, "t");

        public string ToLongTimeString() => DateTimeFormat.Format(this, "T");

        public static DateTime Parse(string s) => Parse(s, null, DateTimeStyles.None);

        public static DateTime Parse(string s, IFormatProvider provider) => Parse(s, provider, DateTimeStyles.None);

        public static DateTime Parse(string s, IFormatProvider provider, DateTimeStyles styles)
        {
            if (s == null)
            {
                throw new ArgumentNullException("s");
            }
            if (!DateTimeFormat.TryParse(s, styles, out DateTime result))
            {
                throw DateTimeFormat.Failure(s);
            }
            return result;
        }

        public static bool TryParse(string s, out DateTime result) => TryParse(s, null, DateTimeStyles.None, out result);

        public static bool TryParse(string s, IFormatProvider provider, DateTimeStyles styles, out DateTime result)
        {
            if (s == null)
            {
                result = MinValue;
                return false;
            }
            return DateTimeFormat.TryParse(s, styles, out result);
        }

        public static DateTime ParseExact(string s, string format, IFormatProvider provider) =>
            ParseExact(s, format, provider, DateTimeStyles.None);

        public static DateTime ParseExact(string s, string format, IFormatProvider provider, DateTimeStyles style)
        {
            if (s == null)
            {
                throw new ArgumentNullException("s");
            }
            if (format == null)
            {
                throw new ArgumentNullException("format");
            }
            if (!DateTimeFormat.TryParseExact(s, format, style, out DateTime result))
            {
                throw DateTimeFormat.Failure(s);
            }
            return result;
        }

        public static bool TryParseExact(string s, string format, IFormatProvider provider, DateTimeStyles style, out DateTime result)
        {
            if (s == null || format == null)
            {
                result = MinValue;
                return false;
            }
            return DateTimeFormat.TryParseExact(s, format, style, out result);
        }
    }
}

namespace System.Globalization
{
    // Культура у FreeOS одна — инвариантная (фаза N4a), и провайдер формата
    // ничего не меняет: имя нужно программам, которые его передают.
    public class CultureInfo : IFormatProvider
    {
        private static readonly CultureInfo invariant = new CultureInfo("");

        public CultureInfo(string name)
        {
            Name = name ?? throw new ArgumentNullException("name");
        }

        public static CultureInfo InvariantCulture => invariant;

        public static CultureInfo CurrentCulture => invariant;

        public static CultureInfo CurrentUICulture => invariant;

        public string Name { get; }

        public virtual object GetFormat(Type formatType) => null;

        public override string ToString() => Name;
    }

    public enum DateTimeStyles
    {
        None = 0,
        AllowLeadingWhite = 1,
        AllowTrailingWhite = 2,
        AllowInnerWhite = 4,
        AllowWhiteSpaces = 7,
        NoCurrentDateDefault = 8,
        AdjustToUniversal = 16,
        AssumeLocal = 32,
        AssumeUniversal = 64,
        RoundtripKind = 128,
    }
}
