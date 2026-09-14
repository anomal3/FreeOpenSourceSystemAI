// System.Drawing (фаза N6a): точки, размеры, прямоугольники, цвета, кисти,
// перья, шрифты и Graphics поверх окна программы. Рисует среда (`FreeOsWindow`),
// координаты и отсечение считает C#.

using System.Globalization;
using System.Windows.Forms;

namespace System.Drawing
{
    public struct Point : IEquatable<Point>
    {
        public static readonly Point Empty = new Point(0, 0);

        public Point(int x, int y)
        {
            X = x;
            Y = y;
        }

        public int X { get; set; }

        public int Y { get; set; }

        public bool IsEmpty => X == 0 && Y == 0;

        public void Offset(int dx, int dy)
        {
            X += dx;
            Y += dy;
        }

        public static Point operator +(Point pt, Size sz) => new Point(pt.X + sz.Width, pt.Y + sz.Height);

        public static Point operator -(Point pt, Size sz) => new Point(pt.X - sz.Width, pt.Y - sz.Height);

        public static bool operator ==(Point left, Point right) => left.X == right.X && left.Y == right.Y;

        public static bool operator !=(Point left, Point right) => !(left == right);

        public bool Equals(Point other) => this == other;

        public override bool Equals(object obj) => obj is Point other && this == other;

        public override int GetHashCode() => X * 31 + Y;

        public override string ToString() => "{X=" + X + ",Y=" + Y + "}";
    }

    public struct PointF : IEquatable<PointF>
    {
        public static readonly PointF Empty = new PointF(0, 0);

        public PointF(float x, float y)
        {
            X = x;
            Y = y;
        }

        public float X { get; set; }

        public float Y { get; set; }

        public bool IsEmpty => X == 0 && Y == 0;

        public static implicit operator PointF(Point p) => new PointF(p.X, p.Y);

        public static bool operator ==(PointF left, PointF right) => left.X == right.X && left.Y == right.Y;

        public static bool operator !=(PointF left, PointF right) => !(left == right);

        public bool Equals(PointF other) => this == other;

        public override bool Equals(object obj) => obj is PointF other && this == other;

        public override int GetHashCode() => X.GetHashCode() * 31 + Y.GetHashCode();

        public override string ToString() => "{X=" + X + ", Y=" + Y + "}";
    }

    public struct Size : IEquatable<Size>
    {
        public static readonly Size Empty = new Size(0, 0);

        public Size(int width, int height)
        {
            Width = width;
            Height = height;
        }

        public Size(Point pt)
        {
            Width = pt.X;
            Height = pt.Y;
        }

        public int Width { get; set; }

        public int Height { get; set; }

        public bool IsEmpty => Width == 0 && Height == 0;

        public static Size operator +(Size sz1, Size sz2) => new Size(sz1.Width + sz2.Width, sz1.Height + sz2.Height);

        public static Size operator -(Size sz1, Size sz2) => new Size(sz1.Width - sz2.Width, sz1.Height - sz2.Height);

        public static bool operator ==(Size sz1, Size sz2) => sz1.Width == sz2.Width && sz1.Height == sz2.Height;

        public static bool operator !=(Size sz1, Size sz2) => !(sz1 == sz2);

        public static implicit operator SizeF(Size p) => new SizeF(p.Width, p.Height);

        public bool Equals(Size other) => this == other;

        public override bool Equals(object obj) => obj is Size other && this == other;

        public override int GetHashCode() => Width * 31 + Height;

        public override string ToString() => "{Width=" + Width + ", Height=" + Height + "}";
    }

    public struct SizeF : IEquatable<SizeF>
    {
        public static readonly SizeF Empty = new SizeF(0, 0);

        public SizeF(float width, float height)
        {
            Width = width;
            Height = height;
        }

        public float Width { get; set; }

        public float Height { get; set; }

        public bool IsEmpty => Width == 0 && Height == 0;

        public Size ToSize() => new Size((int)Width, (int)Height);

        public static bool operator ==(SizeF sz1, SizeF sz2) => sz1.Width == sz2.Width && sz1.Height == sz2.Height;

        public static bool operator !=(SizeF sz1, SizeF sz2) => !(sz1 == sz2);

        public bool Equals(SizeF other) => this == other;

        public override bool Equals(object obj) => obj is SizeF other && this == other;

        public override int GetHashCode() => Width.GetHashCode() * 31 + Height.GetHashCode();

        public override string ToString() => "{Width=" + Width + ", Height=" + Height + "}";
    }

    public struct Rectangle : IEquatable<Rectangle>
    {
        public static readonly Rectangle Empty = new Rectangle(0, 0, 0, 0);

        public Rectangle(int x, int y, int width, int height)
        {
            X = x;
            Y = y;
            Width = width;
            Height = height;
        }

        public Rectangle(Point location, Size size)
        {
            X = location.X;
            Y = location.Y;
            Width = size.Width;
            Height = size.Height;
        }

        public int X { get; set; }

        public int Y { get; set; }

        public int Width { get; set; }

        public int Height { get; set; }

        public int Left => X;

        public int Top => Y;

        public int Right => X + Width;

        public int Bottom => Y + Height;

        public Point Location
        {
            get => new Point(X, Y);
            set
            {
                X = value.X;
                Y = value.Y;
            }
        }

        public Size Size
        {
            get => new Size(Width, Height);
            set
            {
                Width = value.Width;
                Height = value.Height;
            }
        }

        public bool IsEmpty => X == 0 && Y == 0 && Width == 0 && Height == 0;

        public static Rectangle FromLTRB(int left, int top, int right, int bottom) => new Rectangle(left, top, right - left, bottom - top);

        public bool Contains(int x, int y) => X <= x && x < X + Width && Y <= y && y < Y + Height;

        public bool Contains(Point pt) => Contains(pt.X, pt.Y);

        public bool Contains(Rectangle rect) =>
            X <= rect.X && rect.X + rect.Width <= X + Width && Y <= rect.Y && rect.Y + rect.Height <= Y + Height;

        public bool IntersectsWith(Rectangle rect) =>
            rect.X < X + Width && X < rect.X + rect.Width && rect.Y < Y + Height && Y < rect.Y + rect.Height;

        public static Rectangle Intersect(Rectangle a, Rectangle b)
        {
            int x1 = Math.Max(a.X, b.X);
            int x2 = Math.Min(a.X + a.Width, b.X + b.Width);
            int y1 = Math.Max(a.Y, b.Y);
            int y2 = Math.Min(a.Y + a.Height, b.Y + b.Height);
            if (x2 >= x1 && y2 >= y1)
            {
                return new Rectangle(x1, y1, x2 - x1, y2 - y1);
            }
            return Empty;
        }

        public void Intersect(Rectangle rect)
        {
            Rectangle result = Intersect(rect, this);
            X = result.X;
            Y = result.Y;
            Width = result.Width;
            Height = result.Height;
        }

        public static Rectangle Union(Rectangle a, Rectangle b)
        {
            int x1 = Math.Min(a.X, b.X);
            int x2 = Math.Max(a.X + a.Width, b.X + b.Width);
            int y1 = Math.Min(a.Y, b.Y);
            int y2 = Math.Max(a.Y + a.Height, b.Y + b.Height);
            return new Rectangle(x1, y1, x2 - x1, y2 - y1);
        }

        public static Rectangle Inflate(Rectangle rect, int x, int y)
        {
            Rectangle result = rect;
            result.Inflate(x, y);
            return result;
        }

        public void Inflate(int width, int height)
        {
            X -= width;
            Y -= height;
            Width += 2 * width;
            Height += 2 * height;
        }

        public void Offset(int x, int y)
        {
            X += x;
            Y += y;
        }

        public static bool operator ==(Rectangle left, Rectangle right) =>
            left.X == right.X && left.Y == right.Y && left.Width == right.Width && left.Height == right.Height;

        public static bool operator !=(Rectangle left, Rectangle right) => !(left == right);

        public static implicit operator RectangleF(Rectangle r) => new RectangleF(r.X, r.Y, r.Width, r.Height);

        public bool Equals(Rectangle other) => this == other;

        public override bool Equals(object obj) => obj is Rectangle other && this == other;

        public override int GetHashCode() => ((X * 31 + Y) * 31 + Width) * 31 + Height;

        public override string ToString() => "{X=" + X + ",Y=" + Y + ",Width=" + Width + ",Height=" + Height + "}";
    }

    public struct RectangleF : IEquatable<RectangleF>
    {
        public static readonly RectangleF Empty = new RectangleF(0, 0, 0, 0);

        public RectangleF(float x, float y, float width, float height)
        {
            X = x;
            Y = y;
            Width = width;
            Height = height;
        }

        public RectangleF(PointF location, SizeF size)
        {
            X = location.X;
            Y = location.Y;
            Width = size.Width;
            Height = size.Height;
        }

        public float X { get; set; }

        public float Y { get; set; }

        public float Width { get; set; }

        public float Height { get; set; }

        public float Left => X;

        public float Top => Y;

        public float Right => X + Width;

        public float Bottom => Y + Height;

        public bool IsEmpty => Width <= 0 || Height <= 0;

        public static bool operator ==(RectangleF left, RectangleF right) =>
            left.X == right.X && left.Y == right.Y && left.Width == right.Width && left.Height == right.Height;

        public static bool operator !=(RectangleF left, RectangleF right) => !(left == right);

        public bool Equals(RectangleF other) => this == other;

        public override bool Equals(object obj) => obj is RectangleF other && this == other;

        public override int GetHashCode() => X.GetHashCode() ^ Y.GetHashCode() ^ Width.GetHashCode() ^ Height.GetHashCode();

        public override string ToString() => "{X=" + X + ",Y=" + Y + ",Width=" + Width + ",Height=" + Height + "}";
    }

    // Цвет: ARGB и, у именованного, имя. Как в .NET, именованный цвет не равен
    // безымянному с теми же числами, а два безымянных равны по числам.
    public struct Color : IEquatable<Color>
    {
        public static readonly Color Empty = new Color();

        private readonly int argb;
        private readonly string name;
        private readonly bool known;
        private readonly bool hasValue;

        private Color(int argb, string name, bool known)
        {
            this.argb = argb;
            this.name = name;
            this.known = known;
            hasValue = true;
        }

        public byte A => (byte)(argb >> 24);

        public byte R => (byte)(argb >> 16);

        public byte G => (byte)(argb >> 8);

        public byte B => (byte)argb;

        public bool IsEmpty => !hasValue;

        public bool IsKnownColor => known;

        public bool IsNamedColor => known;

        public bool IsSystemColor => known && SystemColors.IsSystemName(name);

        public string Name
        {
            get
            {
                if (name != null)
                {
                    return name;
                }
                if (!hasValue)
                {
                    return "0";
                }
                return ((uint)argb).ToString("x");
            }
        }

        public int ToArgb() => argb;

        public static Color FromArgb(int argb) => new Color(argb, null, false);

        public static Color FromArgb(int alpha, int red, int green, int blue)
        {
            Check(alpha, "alpha");
            Check(red, "red");
            Check(green, "green");
            Check(blue, "blue");
            return new Color(alpha << 24 | red << 16 | green << 8 | blue, null, false);
        }

        public static Color FromArgb(int red, int green, int blue) => FromArgb(255, red, green, blue);

        public static Color FromArgb(int alpha, Color baseColor) => FromArgb(alpha, baseColor.R, baseColor.G, baseColor.B);

        private static void Check(int value, string name)
        {
            if ((uint)value > 255)
            {
                throw new ArgumentException("Value of '" + value + "' is not valid for '" + name + "'. '" + name
                    + "' should be greater than or equal to 0 and less than or equal to 255.", name);
            }
        }

        internal static Color Known(uint argb, string name) => new Color((int)argb, name, true);

        public static Color FromName(string name)
        {
            Color found = KnownColors.Find(name);
            return found.IsEmpty ? new Color(0, name, false) : found;
        }

        public float GetBrightness()
        {
            int max = Math.Max(R, Math.Max(G, B));
            int min = Math.Min(R, Math.Min(G, B));
            return (max + min) / 510f;
        }

        public static bool operator ==(Color left, Color right) =>
            left.argb == right.argb && left.hasValue == right.hasValue && left.known == right.known && left.name == right.name;

        public static bool operator !=(Color left, Color right) => !(left == right);

        public bool Equals(Color other) => this == other;

        public override bool Equals(object obj) => obj is Color other && this == other;

        public override int GetHashCode() => argb;

        public override string ToString()
        {
            if (name != null)
            {
                return "Color [" + name + "]";
            }
            if (hasValue)
            {
                return "Color [A=" + A + ", R=" + R + ", G=" + G + ", B=" + B + "]";
            }
            return "Color [Empty]";
        }

        public static Color Transparent => Known(0x00FFFFFF, "Transparent");
        public static Color AliceBlue => Known(0xFFF0F8FF, "AliceBlue");
        public static Color Aqua => Known(0xFF00FFFF, "Aqua");
        public static Color Beige => Known(0xFFF5F5DC, "Beige");
        public static Color Black => Known(0xFF000000, "Black");
        public static Color Blue => Known(0xFF0000FF, "Blue");
        public static Color BlueViolet => Known(0xFF8A2BE2, "BlueViolet");
        public static Color Brown => Known(0xFFA52A2A, "Brown");
        public static Color CadetBlue => Known(0xFF5F9EA0, "CadetBlue");
        public static Color Coral => Known(0xFFFF7F50, "Coral");
        public static Color CornflowerBlue => Known(0xFF6495ED, "CornflowerBlue");
        public static Color Crimson => Known(0xFFDC143C, "Crimson");
        public static Color Cyan => Known(0xFF00FFFF, "Cyan");
        public static Color DarkBlue => Known(0xFF00008B, "DarkBlue");
        public static Color DarkCyan => Known(0xFF008B8B, "DarkCyan");
        public static Color DarkGray => Known(0xFFA9A9A9, "DarkGray");
        public static Color DarkGreen => Known(0xFF006400, "DarkGreen");
        public static Color DarkOrange => Known(0xFFFF8C00, "DarkOrange");
        public static Color DarkRed => Known(0xFF8B0000, "DarkRed");
        public static Color DarkSlateGray => Known(0xFF2F4F4F, "DarkSlateGray");
        public static Color DeepSkyBlue => Known(0xFF00BFFF, "DeepSkyBlue");
        public static Color DimGray => Known(0xFF696969, "DimGray");
        public static Color DodgerBlue => Known(0xFF1E90FF, "DodgerBlue");
        public static Color Firebrick => Known(0xFFB22222, "Firebrick");
        public static Color ForestGreen => Known(0xFF228B22, "ForestGreen");
        public static Color Gainsboro => Known(0xFFDCDCDC, "Gainsboro");
        public static Color Gold => Known(0xFFFFD700, "Gold");
        public static Color Gray => Known(0xFF808080, "Gray");
        public static Color Green => Known(0xFF008000, "Green");
        public static Color GreenYellow => Known(0xFFADFF2F, "GreenYellow");
        public static Color HotPink => Known(0xFFFF69B4, "HotPink");
        public static Color Indigo => Known(0xFF4B0082, "Indigo");
        public static Color Ivory => Known(0xFFFFFFF0, "Ivory");
        public static Color Khaki => Known(0xFFF0E68C, "Khaki");
        public static Color Lavender => Known(0xFFE6E6FA, "Lavender");
        public static Color LightBlue => Known(0xFFADD8E6, "LightBlue");
        public static Color LightGray => Known(0xFFD3D3D3, "LightGray");
        public static Color LightGreen => Known(0xFF90EE90, "LightGreen");
        public static Color LightSteelBlue => Known(0xFFB0C4DE, "LightSteelBlue");
        public static Color LightYellow => Known(0xFFFFFFE0, "LightYellow");
        public static Color Lime => Known(0xFF00FF00, "Lime");
        public static Color LimeGreen => Known(0xFF32CD32, "LimeGreen");
        public static Color Magenta => Known(0xFFFF00FF, "Magenta");
        public static Color Maroon => Known(0xFF800000, "Maroon");
        public static Color MidnightBlue => Known(0xFF191970, "MidnightBlue");
        public static Color Navy => Known(0xFF000080, "Navy");
        public static Color Olive => Known(0xFF808000, "Olive");
        public static Color Orange => Known(0xFFFFA500, "Orange");
        public static Color OrangeRed => Known(0xFFFF4500, "OrangeRed");
        public static Color Orchid => Known(0xFFDA70D6, "Orchid");
        public static Color PaleGreen => Known(0xFF98FB98, "PaleGreen");
        public static Color Pink => Known(0xFFFFC0CB, "Pink");
        public static Color Plum => Known(0xFFDDA0DD, "Plum");
        public static Color Purple => Known(0xFF800080, "Purple");
        public static Color Red => Known(0xFFFF0000, "Red");
        public static Color RoyalBlue => Known(0xFF4169E1, "RoyalBlue");
        public static Color Salmon => Known(0xFFFA8072, "Salmon");
        public static Color SeaGreen => Known(0xFF2E8B57, "SeaGreen");
        public static Color Silver => Known(0xFFC0C0C0, "Silver");
        public static Color SkyBlue => Known(0xFF87CEEB, "SkyBlue");
        public static Color SlateGray => Known(0xFF708090, "SlateGray");
        public static Color SteelBlue => Known(0xFF4682B4, "SteelBlue");
        public static Color Tan => Known(0xFFD2B48C, "Tan");
        public static Color Teal => Known(0xFF008080, "Teal");
        public static Color Tomato => Known(0xFFFF6347, "Tomato");
        public static Color Turquoise => Known(0xFF40E0D0, "Turquoise");
        public static Color Violet => Known(0xFFEE82EE, "Violet");
        public static Color Wheat => Known(0xFFF5DEB3, "Wheat");
        public static Color White => Known(0xFFFFFFFF, "White");
        public static Color WhiteSmoke => Known(0xFFF5F5F5, "WhiteSmoke");
        public static Color Yellow => Known(0xFFFFFF00, "Yellow");
        public static Color YellowGreen => Known(0xFF9ACD32, "YellowGreen");
    }

    internal static class KnownColors
    {
        // Для FromName: имена, которые чаще всего пишут строкой.
        internal static Color Find(string name)
        {
            switch (name)
            {
                case "Black": return Color.Black;
                case "White": return Color.White;
                case "Red": return Color.Red;
                case "Green": return Color.Green;
                case "Blue": return Color.Blue;
                case "Gray": return Color.Gray;
                case "Yellow": return Color.Yellow;
                case "Orange": return Color.Orange;
                case "Transparent": return Color.Transparent;
                case "Control": return SystemColors.Control;
                case "ControlText": return SystemColors.ControlText;
                case "Window": return SystemColors.Window;
                case "WindowText": return SystemColors.WindowText;
                default: return Color.Empty;
            }
        }
    }

    // Системные цвета — значения темы Windows по умолчанию: программа,
    // нарисованная под них, выглядит здесь так же, как дома.
    public static class SystemColors
    {
        public static Color Control => Color.Known(0xFFF0F0F0, "Control");
        public static Color ControlText => Color.Known(0xFF000000, "ControlText");
        public static Color ControlDark => Color.Known(0xFFA0A0A0, "ControlDark");
        public static Color ControlLight => Color.Known(0xFFE3E3E3, "ControlLight");
        public static Color ControlDarkDark => Color.Known(0xFF696969, "ControlDarkDark");
        public static Color ControlLightLight => Color.Known(0xFFFFFFFF, "ControlLightLight");
        public static Color Window => Color.Known(0xFFFFFFFF, "Window");
        public static Color WindowText => Color.Known(0xFF000000, "WindowText");
        public static Color Highlight => Color.Known(0xFF0078D7, "Highlight");
        public static Color HighlightText => Color.Known(0xFFFFFFFF, "HighlightText");
        public static Color GrayText => Color.Known(0xFF6D6D6D, "GrayText");
        public static Color ButtonFace => Color.Known(0xFFF0F0F0, "ButtonFace");
        public static Color ButtonShadow => Color.Known(0xFFA0A0A0, "ButtonShadow");

        internal static bool IsSystemName(string name)
        {
            switch (name)
            {
                case "Control":
                case "ControlText":
                case "ControlDark":
                case "ControlLight":
                case "ControlDarkDark":
                case "ControlLightLight":
                case "Window":
                case "WindowText":
                case "Highlight":
                case "HighlightText":
                case "GrayText":
                case "ButtonFace":
                case "ButtonShadow":
                    return true;
                default:
                    return false;
            }
        }
    }

    public abstract class Brush : IDisposable
    {
        public void Dispose()
        {
            Dispose(true);
        }

        protected virtual void Dispose(bool disposing)
        {
        }

        internal abstract Color Paint { get; }
    }

    public sealed class SolidBrush : Brush
    {
        public SolidBrush(Color color)
        {
            Color = color;
        }

        public Color Color { get; set; }

        internal override Color Paint => Color;
    }

    public static class Brushes
    {
        public static Brush Black => new SolidBrush(Color.Black);
        public static Brush White => new SolidBrush(Color.White);
        public static Brush Red => new SolidBrush(Color.Red);
        public static Brush Green => new SolidBrush(Color.Green);
        public static Brush Blue => new SolidBrush(Color.Blue);
        public static Brush Gray => new SolidBrush(Color.Gray);
        public static Brush LightGray => new SolidBrush(Color.LightGray);
        public static Brush DarkGray => new SolidBrush(Color.DarkGray);
        public static Brush Yellow => new SolidBrush(Color.Yellow);
        public static Brush Orange => new SolidBrush(Color.Orange);
        public static Brush DarkOrange => new SolidBrush(Color.DarkOrange);
        public static Brush SteelBlue => new SolidBrush(Color.SteelBlue);
        public static Brush CornflowerBlue => new SolidBrush(Color.CornflowerBlue);
        public static Brush Navy => new SolidBrush(Color.Navy);
        public static Brush SeaGreen => new SolidBrush(Color.SeaGreen);
        public static Brush Transparent => new SolidBrush(Color.Transparent);
    }

    public sealed class Pen : IDisposable
    {
        public Pen(Color color)
            : this(color, 1)
        {
        }

        public Pen(Color color, float width)
        {
            Color = color;
            Width = width;
        }

        public Pen(Brush brush, float width = 1)
            : this(brush.Paint, width)
        {
        }

        public Color Color { get; set; }

        public float Width { get; set; }

        public void Dispose()
        {
        }
    }

    public static class Pens
    {
        public static Pen Black => new Pen(Color.Black);
        public static Pen White => new Pen(Color.White);
        public static Pen Red => new Pen(Color.Red);
        public static Pen Blue => new Pen(Color.Blue);
        public static Pen Gray => new Pen(Color.Gray);
        public static Pen DarkGray => new Pen(Color.DarkGray);
    }

    [Flags]
    public enum FontStyle
    {
        Regular = 0,
        Bold = 1,
        Italic = 2,
        Underline = 4,
        Strikeout = 8,
    }

    public enum GraphicsUnit
    {
        World = 0,
        Display = 1,
        Pixel = 2,
        Point = 3,
        Inch = 4,
        Document = 5,
        Millimeter = 6,
    }

    // Шрифт — только описание: рисует системный шрифт FreeOS, у которого одно
    // начертание на размер экрана. Имя и размер программа видит свои.
    public sealed class Font : IDisposable
    {
        public Font(string familyName, float emSize)
            : this(familyName, emSize, FontStyle.Regular, GraphicsUnit.Point)
        {
        }

        public Font(string familyName, float emSize, FontStyle style)
            : this(familyName, emSize, style, GraphicsUnit.Point)
        {
        }

        public Font(string familyName, float emSize, FontStyle style, GraphicsUnit unit)
        {
            if (emSize <= 0 || float.IsInfinity(emSize) || float.IsNaN(emSize))
            {
                throw new ArgumentException("Value of '" + emSize + "' is not valid for 'emSize'. 'emSize' should be greater than 0 and less than or equal to System.Single.MaxValue.", "emSize");
            }
            Name = familyName;
            Size = emSize;
            Style = style;
            Unit = unit;
        }

        public Font(Font prototype, FontStyle newStyle)
            : this(prototype.Name, prototype.Size, newStyle, prototype.Unit)
        {
        }

        public string Name { get; }

        public float Size { get; }

        public float SizeInPoints => Size;

        public FontStyle Style { get; }

        public GraphicsUnit Unit { get; }

        public bool Bold => (Style & FontStyle.Bold) != 0;

        public bool Italic => (Style & FontStyle.Italic) != 0;

        public bool Underline => (Style & FontStyle.Underline) != 0;

        public int Height => FreeOsWindow.TextHeight();

        public void Dispose()
        {
        }

        public override string ToString() =>
            "[Font: Name=" + Name + ", Size=" + Size + ", Units=" + (int)Unit + ", GdiCharSet=1, GdiVerticalFont=False]";
    }

    // Рисование в окне формы. Координаты — от угла элемента, отсечение — его
    // видимая часть; то и другое в точках окна хранит `origin` и `clip`.
    public sealed class Graphics : IDisposable
    {
        private readonly int window;
        private readonly int originX;
        private readonly int originY;
        private readonly Rectangle clip;

        internal Graphics(int window, int originX, int originY, Rectangle clip)
        {
            this.window = window;
            this.originX = originX;
            this.originY = originY;
            this.clip = clip;
        }

        public RectangleF VisibleClipBounds => new RectangleF(clip.X - originX, clip.Y - originY, clip.Width, clip.Height);

        public void Dispose()
        {
        }

        private void Fill(int x, int y, int width, int height, Color color)
        {
            if (color.A == 0 || width <= 0 || height <= 0)
            {
                return;
            }
            Rectangle area = Rectangle.Intersect(new Rectangle(x + originX, y + originY, width, height), clip);
            if (area.Width > 0 && area.Height > 0)
            {
                FreeOsWindow.Fill(window, area.X, area.Y, area.Width, area.Height, color.ToArgb());
            }
        }

        public void Clear(Color color) => Fill(clip.X - originX, clip.Y - originY, clip.Width, clip.Height, color);

        public void FillRectangle(Brush brush, int x, int y, int width, int height) => Fill(x, y, width, height, Paint(brush));

        public void FillRectangle(Brush brush, Rectangle rect) => Fill(rect.X, rect.Y, rect.Width, rect.Height, Paint(brush));

        public void FillRectangle(Brush brush, float x, float y, float width, float height) =>
            Fill((int)x, (int)y, (int)width, (int)height, Paint(brush));

        public void FillRectangle(Brush brush, RectangleF rect) => FillRectangle(brush, rect.X, rect.Y, rect.Width, rect.Height);

        // Контур GDI+ шире прямоугольника на точку: правый и нижний край входят.
        public void DrawRectangle(Pen pen, int x, int y, int width, int height)
        {
            if (pen == null)
            {
                throw new ArgumentNullException("pen");
            }
            int thickness = Math.Max(1, (int)pen.Width);
            int inset = thickness / 2;
            int left = x - inset;
            int top = y - inset;
            int outerWidth = width + thickness;
            int outerHeight = height + thickness;
            Fill(left, top, outerWidth, thickness, pen.Color);
            Fill(left, top + outerHeight - thickness, outerWidth, thickness, pen.Color);
            Fill(left, top + thickness, thickness, outerHeight - 2 * thickness, pen.Color);
            Fill(left + outerWidth - thickness, top + thickness, thickness, outerHeight - 2 * thickness, pen.Color);
        }

        public void DrawRectangle(Pen pen, Rectangle rect) => DrawRectangle(pen, rect.X, rect.Y, rect.Width, rect.Height);

        public void DrawRectangle(Pen pen, float x, float y, float width, float height) =>
            DrawRectangle(pen, (int)x, (int)y, (int)width, (int)height);

        // Отрезок Брезенхэма квадратиками толщины пера.
        public void DrawLine(Pen pen, int x1, int y1, int x2, int y2)
        {
            if (pen == null)
            {
                throw new ArgumentNullException("pen");
            }
            int thickness = Math.Max(1, (int)pen.Width);
            int inset = thickness / 2;
            if (y1 == y2)
            {
                Fill(Math.Min(x1, x2), y1 - inset, Math.Abs(x2 - x1) + 1, thickness, pen.Color);
                return;
            }
            if (x1 == x2)
            {
                Fill(x1 - inset, Math.Min(y1, y2), thickness, Math.Abs(y2 - y1) + 1, pen.Color);
                return;
            }
            int dx = Math.Abs(x2 - x1);
            int dy = -Math.Abs(y2 - y1);
            int sx = x1 < x2 ? 1 : -1;
            int sy = y1 < y2 ? 1 : -1;
            int error = dx + dy;
            while (true)
            {
                Fill(x1 - inset, y1 - inset, thickness, thickness, pen.Color);
                if (x1 == x2 && y1 == y2)
                {
                    break;
                }
                int doubled = 2 * error;
                if (doubled >= dy)
                {
                    error += dy;
                    x1 += sx;
                }
                if (doubled <= dx)
                {
                    error += dx;
                    y1 += sy;
                }
            }
        }

        public void DrawLine(Pen pen, Point pt1, Point pt2) => DrawLine(pen, pt1.X, pt1.Y, pt2.X, pt2.Y);

        public void DrawLine(Pen pen, float x1, float y1, float x2, float y2) => DrawLine(pen, (int)x1, (int)y1, (int)x2, (int)y2);

        public void DrawString(string s, Font font, Brush brush, float x, float y)
        {
            if (brush == null)
            {
                throw new ArgumentNullException("brush");
            }
            if (font == null)
            {
                throw new ArgumentNullException("font");
            }
            if (string.IsNullOrEmpty(s))
            {
                return;
            }
            Color color = brush.Paint;
            if (color.A == 0)
            {
                return;
            }
            FreeOsWindow.Text(window, (int)x + originX, (int)y + originY, s, color.ToArgb(), clip.X, clip.Y, clip.Width, clip.Height);
        }

        public void DrawString(string s, Font font, Brush brush, PointF point) => DrawString(s, font, brush, point.X, point.Y);

        public void DrawString(string s, Font font, Brush brush, RectangleF layoutRectangle) =>
            DrawString(s, font, brush, layoutRectangle.X, layoutRectangle.Y);

        public SizeF MeasureString(string text, Font font)
        {
            if (string.IsNullOrEmpty(text))
            {
                return SizeF.Empty;
            }
            return new SizeF(FreeOsWindow.TextWidth(text), FreeOsWindow.TextHeight());
        }

        private static Color Paint(Brush brush)
        {
            if (brush == null)
            {
                throw new ArgumentNullException("brush");
            }
            return brush.Paint;
        }
    }
}
