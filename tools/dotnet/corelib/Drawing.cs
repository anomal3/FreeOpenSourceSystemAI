// System.Drawing (фаза N6a): точки, размеры, прямоугольники, цвета и шрифты.
// Кисти и перья — в Paint.cs, Graphics — в Graphics.cs (фаза N9): с N9 рисует
// растеризатор `raster`, и Graphics стал общим для окна и Bitmap.

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

        public static Point Round(PointF value) => new Point((int)Math.Round(value.X), (int)Math.Round(value.Y));

        public static Point Truncate(PointF value) => new Point((int)value.X, (int)value.Y);

        public static Point Ceiling(PointF value) => new Point((int)Math.Ceiling(value.X), (int)Math.Ceiling(value.Y));

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

        public static PointF operator +(PointF pt, SizeF sz) => new PointF(pt.X + sz.Width, pt.Y + sz.Height);

        public static PointF operator -(PointF pt, SizeF sz) => new PointF(pt.X - sz.Width, pt.Y - sz.Height);

        public static PointF Add(PointF pt, SizeF sz) => pt + sz;

        public static PointF Subtract(PointF pt, SizeF sz) => pt - sz;

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

        public PointF ToPointF() => new PointF(Width, Height);

        public static SizeF operator +(SizeF sz1, SizeF sz2) => new SizeF(sz1.Width + sz2.Width, sz1.Height + sz2.Height);

        public static SizeF operator -(SizeF sz1, SizeF sz2) => new SizeF(sz1.Width - sz2.Width, sz1.Height - sz2.Height);

        public static SizeF operator *(SizeF left, float right) => new SizeF(left.Width * right, left.Height * right);

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

        public static Rectangle Round(RectangleF value) =>
            new Rectangle((int)Math.Round(value.X), (int)Math.Round(value.Y), (int)Math.Round(value.Width), (int)Math.Round(value.Height));

        public static Rectangle Truncate(RectangleF value) => new Rectangle((int)value.X, (int)value.Y, (int)value.Width, (int)value.Height);

        public static Rectangle Ceiling(RectangleF value) =>
            new Rectangle((int)Math.Ceiling(value.X), (int)Math.Ceiling(value.Y), (int)Math.Ceiling(value.Width), (int)Math.Ceiling(value.Height));

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

        public PointF Location
        {
            get => new PointF(X, Y);
            set
            {
                X = value.X;
                Y = value.Y;
            }
        }

        public SizeF Size
        {
            get => new SizeF(Width, Height);
            set
            {
                Width = value.Width;
                Height = value.Height;
            }
        }

        public static RectangleF FromLTRB(float left, float top, float right, float bottom) => new RectangleF(left, top, right - left, bottom - top);

        public bool Contains(float x, float y) => X <= x && x < X + Width && Y <= y && y < Y + Height;

        public bool Contains(PointF pt) => Contains(pt.X, pt.Y);

        public bool Contains(RectangleF rect) =>
            X <= rect.X && rect.X + rect.Width <= X + Width && Y <= rect.Y && rect.Y + rect.Height <= Y + Height;

        public bool IntersectsWith(RectangleF rect) =>
            rect.X < X + Width && X < rect.X + rect.Width && rect.Y < Y + Height && Y < rect.Y + rect.Height;

        public static RectangleF Intersect(RectangleF a, RectangleF b)
        {
            float x1 = Math.Max(a.X, b.X);
            float x2 = Math.Min(a.X + a.Width, b.X + b.Width);
            float y1 = Math.Max(a.Y, b.Y);
            float y2 = Math.Min(a.Y + a.Height, b.Y + b.Height);
            if (x2 >= x1 && y2 >= y1)
            {
                return new RectangleF(x1, y1, x2 - x1, y2 - y1);
            }
            return Empty;
        }

        public void Intersect(RectangleF rect)
        {
            RectangleF result = Intersect(rect, this);
            X = result.X;
            Y = result.Y;
            Width = result.Width;
            Height = result.Height;
        }

        public static RectangleF Union(RectangleF a, RectangleF b)
        {
            float x1 = Math.Min(a.X, b.X);
            float x2 = Math.Max(a.X + a.Width, b.X + b.Width);
            float y1 = Math.Min(a.Y, b.Y);
            float y2 = Math.Max(a.Y + a.Height, b.Y + b.Height);
            return new RectangleF(x1, y1, x2 - x1, y2 - y1);
        }

        public static RectangleF Inflate(RectangleF rect, float x, float y)
        {
            RectangleF result = rect;
            result.Inflate(x, y);
            return result;
        }

        public void Inflate(float x, float y)
        {
            X -= x;
            Y -= y;
            Width += 2 * x;
            Height += 2 * y;
        }

        public void Inflate(SizeF size) => Inflate(size.Width, size.Height);

        public void Offset(float x, float y)
        {
            X += x;
            Y += y;
        }

        public void Offset(PointF pos) => Offset(pos.X, pos.Y);

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
        public static Color AntiqueWhite => Known(0xFFFAEBD7, "AntiqueWhite");
        public static Color Aqua => Known(0xFF00FFFF, "Aqua");
        public static Color Aquamarine => Known(0xFF7FFFD4, "Aquamarine");
        public static Color Azure => Known(0xFFF0FFFF, "Azure");
        public static Color Beige => Known(0xFFF5F5DC, "Beige");
        public static Color Bisque => Known(0xFFFFE4C4, "Bisque");
        public static Color Black => Known(0xFF000000, "Black");
        public static Color BlanchedAlmond => Known(0xFFFFEBCD, "BlanchedAlmond");
        public static Color Blue => Known(0xFF0000FF, "Blue");
        public static Color BlueViolet => Known(0xFF8A2BE2, "BlueViolet");
        public static Color Brown => Known(0xFFA52A2A, "Brown");
        public static Color BurlyWood => Known(0xFFDEB887, "BurlyWood");
        public static Color CadetBlue => Known(0xFF5F9EA0, "CadetBlue");
        public static Color Chartreuse => Known(0xFF7FFF00, "Chartreuse");
        public static Color Chocolate => Known(0xFFD2691E, "Chocolate");
        public static Color Coral => Known(0xFFFF7F50, "Coral");
        public static Color CornflowerBlue => Known(0xFF6495ED, "CornflowerBlue");
        public static Color Cornsilk => Known(0xFFFFF8DC, "Cornsilk");
        public static Color Crimson => Known(0xFFDC143C, "Crimson");
        public static Color Cyan => Known(0xFF00FFFF, "Cyan");
        public static Color DarkBlue => Known(0xFF00008B, "DarkBlue");
        public static Color DarkCyan => Known(0xFF008B8B, "DarkCyan");
        public static Color DarkGoldenrod => Known(0xFFB8860B, "DarkGoldenrod");
        public static Color DarkGray => Known(0xFFA9A9A9, "DarkGray");
        public static Color DarkGreen => Known(0xFF006400, "DarkGreen");
        public static Color DarkKhaki => Known(0xFFBDB76B, "DarkKhaki");
        public static Color DarkMagenta => Known(0xFF8B008B, "DarkMagenta");
        public static Color DarkOliveGreen => Known(0xFF556B2F, "DarkOliveGreen");
        public static Color DarkOrange => Known(0xFFFF8C00, "DarkOrange");
        public static Color DarkOrchid => Known(0xFF9932CC, "DarkOrchid");
        public static Color DarkRed => Known(0xFF8B0000, "DarkRed");
        public static Color DarkSalmon => Known(0xFFE9967A, "DarkSalmon");
        public static Color DarkSeaGreen => Known(0xFF8FBC8F, "DarkSeaGreen");
        public static Color DarkSlateBlue => Known(0xFF483D8B, "DarkSlateBlue");
        public static Color DarkSlateGray => Known(0xFF2F4F4F, "DarkSlateGray");
        public static Color DarkTurquoise => Known(0xFF00CED1, "DarkTurquoise");
        public static Color DarkViolet => Known(0xFF9400D3, "DarkViolet");
        public static Color DeepPink => Known(0xFFFF1493, "DeepPink");
        public static Color DeepSkyBlue => Known(0xFF00BFFF, "DeepSkyBlue");
        public static Color DimGray => Known(0xFF696969, "DimGray");
        public static Color DodgerBlue => Known(0xFF1E90FF, "DodgerBlue");
        public static Color Firebrick => Known(0xFFB22222, "Firebrick");
        public static Color FloralWhite => Known(0xFFFFFAF0, "FloralWhite");
        public static Color ForestGreen => Known(0xFF228B22, "ForestGreen");
        public static Color Fuchsia => Known(0xFFFF00FF, "Fuchsia");
        public static Color Gainsboro => Known(0xFFDCDCDC, "Gainsboro");
        public static Color GhostWhite => Known(0xFFF8F8FF, "GhostWhite");
        public static Color Gold => Known(0xFFFFD700, "Gold");
        public static Color Goldenrod => Known(0xFFDAA520, "Goldenrod");
        public static Color Gray => Known(0xFF808080, "Gray");
        public static Color Green => Known(0xFF008000, "Green");
        public static Color GreenYellow => Known(0xFFADFF2F, "GreenYellow");
        public static Color Honeydew => Known(0xFFF0FFF0, "Honeydew");
        public static Color HotPink => Known(0xFFFF69B4, "HotPink");
        public static Color IndianRed => Known(0xFFCD5C5C, "IndianRed");
        public static Color Indigo => Known(0xFF4B0082, "Indigo");
        public static Color Ivory => Known(0xFFFFFFF0, "Ivory");
        public static Color Khaki => Known(0xFFF0E68C, "Khaki");
        public static Color Lavender => Known(0xFFE6E6FA, "Lavender");
        public static Color LavenderBlush => Known(0xFFFFF0F5, "LavenderBlush");
        public static Color LawnGreen => Known(0xFF7CFC00, "LawnGreen");
        public static Color LemonChiffon => Known(0xFFFFFACD, "LemonChiffon");
        public static Color LightBlue => Known(0xFFADD8E6, "LightBlue");
        public static Color LightCoral => Known(0xFFF08080, "LightCoral");
        public static Color LightCyan => Known(0xFFE0FFFF, "LightCyan");
        public static Color LightGoldenrodYellow => Known(0xFFFAFAD2, "LightGoldenrodYellow");
        public static Color LightGray => Known(0xFFD3D3D3, "LightGray");
        public static Color LightGreen => Known(0xFF90EE90, "LightGreen");
        public static Color LightPink => Known(0xFFFFB6C1, "LightPink");
        public static Color LightSalmon => Known(0xFFFFA07A, "LightSalmon");
        public static Color LightSeaGreen => Known(0xFF20B2AA, "LightSeaGreen");
        public static Color LightSkyBlue => Known(0xFF87CEFA, "LightSkyBlue");
        public static Color LightSlateGray => Known(0xFF778899, "LightSlateGray");
        public static Color LightSteelBlue => Known(0xFFB0C4DE, "LightSteelBlue");
        public static Color LightYellow => Known(0xFFFFFFE0, "LightYellow");
        public static Color Lime => Known(0xFF00FF00, "Lime");
        public static Color LimeGreen => Known(0xFF32CD32, "LimeGreen");
        public static Color Linen => Known(0xFFFAF0E6, "Linen");
        public static Color Magenta => Known(0xFFFF00FF, "Magenta");
        public static Color Maroon => Known(0xFF800000, "Maroon");
        public static Color MediumAquamarine => Known(0xFF66CDAA, "MediumAquamarine");
        public static Color MediumBlue => Known(0xFF0000CD, "MediumBlue");
        public static Color MediumOrchid => Known(0xFFBA55D3, "MediumOrchid");
        public static Color MediumPurple => Known(0xFF9370DB, "MediumPurple");
        public static Color MediumSeaGreen => Known(0xFF3CB371, "MediumSeaGreen");
        public static Color MediumSlateBlue => Known(0xFF7B68EE, "MediumSlateBlue");
        public static Color MediumSpringGreen => Known(0xFF00FA9A, "MediumSpringGreen");
        public static Color MediumTurquoise => Known(0xFF48D1CC, "MediumTurquoise");
        public static Color MediumVioletRed => Known(0xFFC71585, "MediumVioletRed");
        public static Color MidnightBlue => Known(0xFF191970, "MidnightBlue");
        public static Color MintCream => Known(0xFFF5FFFA, "MintCream");
        public static Color MistyRose => Known(0xFFFFE4E1, "MistyRose");
        public static Color Moccasin => Known(0xFFFFE4B5, "Moccasin");
        public static Color NavajoWhite => Known(0xFFFFDEAD, "NavajoWhite");
        public static Color Navy => Known(0xFF000080, "Navy");
        public static Color OldLace => Known(0xFFFDF5E6, "OldLace");
        public static Color Olive => Known(0xFF808000, "Olive");
        public static Color OliveDrab => Known(0xFF6B8E23, "OliveDrab");
        public static Color Orange => Known(0xFFFFA500, "Orange");
        public static Color OrangeRed => Known(0xFFFF4500, "OrangeRed");
        public static Color Orchid => Known(0xFFDA70D6, "Orchid");
        public static Color PaleGoldenrod => Known(0xFFEEE8AA, "PaleGoldenrod");
        public static Color PaleGreen => Known(0xFF98FB98, "PaleGreen");
        public static Color PaleTurquoise => Known(0xFFAFEEEE, "PaleTurquoise");
        public static Color PaleVioletRed => Known(0xFFDB7093, "PaleVioletRed");
        public static Color PapayaWhip => Known(0xFFFFEFD5, "PapayaWhip");
        public static Color PeachPuff => Known(0xFFFFDAB9, "PeachPuff");
        public static Color Peru => Known(0xFFCD853F, "Peru");
        public static Color Pink => Known(0xFFFFC0CB, "Pink");
        public static Color Plum => Known(0xFFDDA0DD, "Plum");
        public static Color PowderBlue => Known(0xFFB0E0E6, "PowderBlue");
        public static Color Purple => Known(0xFF800080, "Purple");
        public static Color Red => Known(0xFFFF0000, "Red");
        public static Color RosyBrown => Known(0xFFBC8F8F, "RosyBrown");
        public static Color RoyalBlue => Known(0xFF4169E1, "RoyalBlue");
        public static Color SaddleBrown => Known(0xFF8B4513, "SaddleBrown");
        public static Color Salmon => Known(0xFFFA8072, "Salmon");
        public static Color SandyBrown => Known(0xFFF4A460, "SandyBrown");
        public static Color SeaGreen => Known(0xFF2E8B57, "SeaGreen");
        public static Color SeaShell => Known(0xFFFFF5EE, "SeaShell");
        public static Color Sienna => Known(0xFFA0522D, "Sienna");
        public static Color Silver => Known(0xFFC0C0C0, "Silver");
        public static Color SkyBlue => Known(0xFF87CEEB, "SkyBlue");
        public static Color SlateBlue => Known(0xFF6A5ACD, "SlateBlue");
        public static Color SlateGray => Known(0xFF708090, "SlateGray");
        public static Color Snow => Known(0xFFFFFAFA, "Snow");
        public static Color SpringGreen => Known(0xFF00FF7F, "SpringGreen");
        public static Color SteelBlue => Known(0xFF4682B4, "SteelBlue");
        public static Color Tan => Known(0xFFD2B48C, "Tan");
        public static Color Teal => Known(0xFF008080, "Teal");
        public static Color Thistle => Known(0xFFD8BFD8, "Thistle");
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
                case "Transparent": return Color.Transparent;
                case "AliceBlue": return Color.AliceBlue;
                case "AntiqueWhite": return Color.AntiqueWhite;
                case "Aqua": return Color.Aqua;
                case "Aquamarine": return Color.Aquamarine;
                case "Azure": return Color.Azure;
                case "Beige": return Color.Beige;
                case "Bisque": return Color.Bisque;
                case "Black": return Color.Black;
                case "BlanchedAlmond": return Color.BlanchedAlmond;
                case "Blue": return Color.Blue;
                case "BlueViolet": return Color.BlueViolet;
                case "Brown": return Color.Brown;
                case "BurlyWood": return Color.BurlyWood;
                case "CadetBlue": return Color.CadetBlue;
                case "Chartreuse": return Color.Chartreuse;
                case "Chocolate": return Color.Chocolate;
                case "Coral": return Color.Coral;
                case "CornflowerBlue": return Color.CornflowerBlue;
                case "Cornsilk": return Color.Cornsilk;
                case "Crimson": return Color.Crimson;
                case "Cyan": return Color.Cyan;
                case "DarkBlue": return Color.DarkBlue;
                case "DarkCyan": return Color.DarkCyan;
                case "DarkGoldenrod": return Color.DarkGoldenrod;
                case "DarkGray": return Color.DarkGray;
                case "DarkGreen": return Color.DarkGreen;
                case "DarkKhaki": return Color.DarkKhaki;
                case "DarkMagenta": return Color.DarkMagenta;
                case "DarkOliveGreen": return Color.DarkOliveGreen;
                case "DarkOrange": return Color.DarkOrange;
                case "DarkOrchid": return Color.DarkOrchid;
                case "DarkRed": return Color.DarkRed;
                case "DarkSalmon": return Color.DarkSalmon;
                case "DarkSeaGreen": return Color.DarkSeaGreen;
                case "DarkSlateBlue": return Color.DarkSlateBlue;
                case "DarkSlateGray": return Color.DarkSlateGray;
                case "DarkTurquoise": return Color.DarkTurquoise;
                case "DarkViolet": return Color.DarkViolet;
                case "DeepPink": return Color.DeepPink;
                case "DeepSkyBlue": return Color.DeepSkyBlue;
                case "DimGray": return Color.DimGray;
                case "DodgerBlue": return Color.DodgerBlue;
                case "Firebrick": return Color.Firebrick;
                case "FloralWhite": return Color.FloralWhite;
                case "ForestGreen": return Color.ForestGreen;
                case "Fuchsia": return Color.Fuchsia;
                case "Gainsboro": return Color.Gainsboro;
                case "GhostWhite": return Color.GhostWhite;
                case "Gold": return Color.Gold;
                case "Goldenrod": return Color.Goldenrod;
                case "Gray": return Color.Gray;
                case "Green": return Color.Green;
                case "GreenYellow": return Color.GreenYellow;
                case "Honeydew": return Color.Honeydew;
                case "HotPink": return Color.HotPink;
                case "IndianRed": return Color.IndianRed;
                case "Indigo": return Color.Indigo;
                case "Ivory": return Color.Ivory;
                case "Khaki": return Color.Khaki;
                case "Lavender": return Color.Lavender;
                case "LavenderBlush": return Color.LavenderBlush;
                case "LawnGreen": return Color.LawnGreen;
                case "LemonChiffon": return Color.LemonChiffon;
                case "LightBlue": return Color.LightBlue;
                case "LightCoral": return Color.LightCoral;
                case "LightCyan": return Color.LightCyan;
                case "LightGoldenrodYellow": return Color.LightGoldenrodYellow;
                case "LightGray": return Color.LightGray;
                case "LightGreen": return Color.LightGreen;
                case "LightPink": return Color.LightPink;
                case "LightSalmon": return Color.LightSalmon;
                case "LightSeaGreen": return Color.LightSeaGreen;
                case "LightSkyBlue": return Color.LightSkyBlue;
                case "LightSlateGray": return Color.LightSlateGray;
                case "LightSteelBlue": return Color.LightSteelBlue;
                case "LightYellow": return Color.LightYellow;
                case "Lime": return Color.Lime;
                case "LimeGreen": return Color.LimeGreen;
                case "Linen": return Color.Linen;
                case "Magenta": return Color.Magenta;
                case "Maroon": return Color.Maroon;
                case "MediumAquamarine": return Color.MediumAquamarine;
                case "MediumBlue": return Color.MediumBlue;
                case "MediumOrchid": return Color.MediumOrchid;
                case "MediumPurple": return Color.MediumPurple;
                case "MediumSeaGreen": return Color.MediumSeaGreen;
                case "MediumSlateBlue": return Color.MediumSlateBlue;
                case "MediumSpringGreen": return Color.MediumSpringGreen;
                case "MediumTurquoise": return Color.MediumTurquoise;
                case "MediumVioletRed": return Color.MediumVioletRed;
                case "MidnightBlue": return Color.MidnightBlue;
                case "MintCream": return Color.MintCream;
                case "MistyRose": return Color.MistyRose;
                case "Moccasin": return Color.Moccasin;
                case "NavajoWhite": return Color.NavajoWhite;
                case "Navy": return Color.Navy;
                case "OldLace": return Color.OldLace;
                case "Olive": return Color.Olive;
                case "OliveDrab": return Color.OliveDrab;
                case "Orange": return Color.Orange;
                case "OrangeRed": return Color.OrangeRed;
                case "Orchid": return Color.Orchid;
                case "PaleGoldenrod": return Color.PaleGoldenrod;
                case "PaleGreen": return Color.PaleGreen;
                case "PaleTurquoise": return Color.PaleTurquoise;
                case "PaleVioletRed": return Color.PaleVioletRed;
                case "PapayaWhip": return Color.PapayaWhip;
                case "PeachPuff": return Color.PeachPuff;
                case "Peru": return Color.Peru;
                case "Pink": return Color.Pink;
                case "Plum": return Color.Plum;
                case "PowderBlue": return Color.PowderBlue;
                case "Purple": return Color.Purple;
                case "Red": return Color.Red;
                case "RosyBrown": return Color.RosyBrown;
                case "RoyalBlue": return Color.RoyalBlue;
                case "SaddleBrown": return Color.SaddleBrown;
                case "Salmon": return Color.Salmon;
                case "SandyBrown": return Color.SandyBrown;
                case "SeaGreen": return Color.SeaGreen;
                case "SeaShell": return Color.SeaShell;
                case "Sienna": return Color.Sienna;
                case "Silver": return Color.Silver;
                case "SkyBlue": return Color.SkyBlue;
                case "SlateBlue": return Color.SlateBlue;
                case "SlateGray": return Color.SlateGray;
                case "Snow": return Color.Snow;
                case "SpringGreen": return Color.SpringGreen;
                case "SteelBlue": return Color.SteelBlue;
                case "Tan": return Color.Tan;
                case "Teal": return Color.Teal;
                case "Thistle": return Color.Thistle;
                case "Tomato": return Color.Tomato;
                case "Turquoise": return Color.Turquoise;
                case "Violet": return Color.Violet;
                case "Wheat": return Color.Wheat;
                case "White": return Color.White;
                case "WhiteSmoke": return Color.WhiteSmoke;
                case "Yellow": return Color.Yellow;
                case "YellowGreen": return Color.YellowGreen;
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

}
