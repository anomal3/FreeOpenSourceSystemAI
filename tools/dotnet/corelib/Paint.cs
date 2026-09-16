// Кисти и перья (фаза N9). Кисть сама раскладывает себя для растеризатора
// (`Pack`): сплошная — цветом, градиент — опорными цветами и обратной матрицей
// из точек устройства в своё пространство, текстура — точками картинки. Так
// Graphics не знает, какие кисти бывают, а новая кисть — это новый класс здесь и
// новый вид в `raster::paint::Paint`.

using System.Drawing.Drawing2D;

namespace System.Drawing
{
    public abstract class Brush : IDisposable, ICloneable
    {
        public void Dispose()
        {
            Dispose(true);
        }

        protected virtual void Dispose(bool disposing)
        {
        }

        public abstract object Clone();

        // Цвет, если кисть сплошная: для быстрого пути заливки прямоугольника.
        internal virtual bool IsSolid(out Color color)
        {
            color = Color.Empty;
            return false;
        }

        // `device` — из координат программы в точки устройства.
        internal abstract void Pack(Matrix device, out int[] ints, out float[] floats, out int[] texture);

        // Обратная к «сначала своё преобразование кисти, потом устройство»;
        // вырожденная — `null`, и кисть рисует первым цветом.
        internal static float[] Inverse(Matrix own, Matrix device)
        {
            var combined = own == null ? device.Clone() : own.Clone();
            if (own != null)
            {
                combined.Multiply(device, MatrixOrder.Append);
            }
            if (!combined.IsInvertible)
            {
                return null;
            }
            combined.Invert();
            return combined.Elements;
        }
    }

    public sealed class SolidBrush : Brush
    {
        public SolidBrush(Color color)
        {
            Color = color;
        }

        public Color Color { get; set; }

        public override object Clone() => new SolidBrush(Color);

        internal override bool IsSolid(out Color color)
        {
            color = Color;
            return true;
        }

        internal override void Pack(Matrix device, out int[] ints, out float[] floats, out int[] texture)
        {
            ints = new int[] { 0, 0, Color.ToArgb() };
            floats = null;
            texture = null;
        }
    }

    public sealed class TextureBrush : Brush
    {
        private readonly Image image;
        private Matrix transform = new Matrix();

        public TextureBrush(Image bitmap)
            : this(bitmap, WrapMode.Tile)
        {
        }

        public TextureBrush(Image image, WrapMode wrapMode)
        {
            if (image == null)
            {
                throw new ArgumentNullException("image");
            }
            this.image = image;
            WrapMode = wrapMode;
        }

        public TextureBrush(Image image, Rectangle dstRect)
            : this(image, WrapMode.Tile, dstRect)
        {
        }

        public TextureBrush(Image image, WrapMode wrapMode, Rectangle dstRect)
        {
            if (image == null)
            {
                throw new ArgumentNullException("image");
            }
            var bitmap = image as Bitmap;
            this.image = bitmap == null ? image : bitmap.Clone(dstRect, Imaging.PixelFormat.Format32bppArgb);
            WrapMode = wrapMode;
        }

        public Image Image => image;

        public WrapMode WrapMode { get; set; }

        public Matrix Transform
        {
            get => transform.Clone();
            set
            {
                if (value == null)
                {
                    throw new ArgumentNullException("value");
                }
                transform = value.Clone();
            }
        }

        public void ResetTransform() => transform.Reset();

        public void MultiplyTransform(Matrix matrix) => transform.Multiply(matrix, MatrixOrder.Prepend);

        public void MultiplyTransform(Matrix matrix, MatrixOrder order) => transform.Multiply(matrix, order);

        public void TranslateTransform(float dx, float dy) => transform.Translate(dx, dy, MatrixOrder.Prepend);

        public void TranslateTransform(float dx, float dy, MatrixOrder order) => transform.Translate(dx, dy, order);

        public void ScaleTransform(float sx, float sy) => transform.Scale(sx, sy, MatrixOrder.Prepend);

        public void ScaleTransform(float sx, float sy, MatrixOrder order) => transform.Scale(sx, sy, order);

        public void RotateTransform(float angle) => transform.Rotate(angle, MatrixOrder.Prepend);

        public void RotateTransform(float angle, MatrixOrder order) => transform.Rotate(angle, order);

        public override object Clone()
        {
            var copy = new TextureBrush(image, WrapMode);
            copy.transform = transform.Clone();
            return copy;
        }

        internal override void Pack(Matrix device, out int[] ints, out float[] floats, out int[] texture)
        {
            float[] inverse = Inverse(transform, device);
            if (inverse == null)
            {
                ints = new int[] { 0, 0, 0 };
                floats = null;
                texture = null;
                return;
            }
            ints = new int[] { 2, (int)WrapMode, image.Width, image.Height };
            floats = inverse;
            texture = image.pixels;
        }
    }

}

namespace System.Drawing.Drawing2D
{
    public sealed class LinearGradientBrush : Brush
    {
        private PointF start;
        private PointF end;
        private Color[] colors;
        private Matrix transform = new Matrix();
        private Blend blend;
        private ColorBlend interpolation;
        private WrapMode wrapMode = WrapMode.Tile;

        public LinearGradientBrush(PointF point1, PointF point2, Color color1, Color color2)
        {
            start = point1;
            end = point2;
            colors = new Color[] { color1, color2 };
            // Прямоугольник кисти у GDI+ — границы двух точек, а у отрезка вдоль
            // оси — квадрат со стороной в длину отрезка, поперёк оси по центру
            // (проба: (0, 0)–(40, 0) даёт {X=0,Y=-20,Width=40,Height=40}).
            float width = Math.Abs(point2.X - point1.X);
            float height = Math.Abs(point2.Y - point1.Y);
            float left = Math.Min(point1.X, point2.X);
            float top = Math.Min(point1.Y, point2.Y);
            if (height == 0)
            {
                Rectangle = new RectangleF(left, top - width / 2, width, width);
            }
            else if (width == 0)
            {
                Rectangle = new RectangleF(left - height / 2, top, height, height);
            }
            else
            {
                Rectangle = new RectangleF(left, top, width, height);
            }
        }

        public LinearGradientBrush(Point point1, Point point2, Color color1, Color color2)
            : this((PointF)point1, (PointF)point2, color1, color2)
        {
        }

        public LinearGradientBrush(RectangleF rect, Color color1, Color color2, LinearGradientMode linearGradientMode)
            : this(rect, color1, color2, ModeAngle(linearGradientMode))
        {
        }

        public LinearGradientBrush(Rectangle rect, Color color1, Color color2, LinearGradientMode linearGradientMode)
            : this((RectangleF)rect, color1, color2, ModeAngle(linearGradientMode))
        {
        }

        public LinearGradientBrush(RectangleF rect, Color color1, Color color2, float angle)
            : this(rect, color1, color2, angle, false)
        {
        }

        public LinearGradientBrush(Rectangle rect, Color color1, Color color2, float angle)
            : this((RectangleF)rect, color1, color2, angle, false)
        {
        }

        public LinearGradientBrush(Rectangle rect, Color color1, Color color2, float angle, bool isAngleScaleable)
            : this((RectangleF)rect, color1, color2, angle, isAngleScaleable)
        {
        }

        // Доля 0 проходит через угол прямоугольника, ближний по направлению
        // `angle`, доля 1 — через дальний: полосы градиента перпендикулярны
        // направлению и касаются углов.
        public LinearGradientBrush(RectangleF rect, Color color1, Color color2, float angle, bool isAngleScaleable)
        {
            if (rect.Width == 0 || rect.Height == 0)
            {
                throw new ArgumentException("Rectangle '" + rect + "' cannot have a width or height equal to 0.");
            }
            Rectangle = rect;
            colors = new Color[] { color1, color2 };
            double radians = angle * Math.PI / 180.0;
            double cx = Math.Cos(radians);
            double cy = Math.Sin(radians);
            double min = double.MaxValue;
            double max = double.MinValue;
            float[] xs = { rect.X, rect.X + rect.Width, rect.X, rect.X + rect.Width };
            float[] ys = { rect.Y, rect.Y, rect.Y + rect.Height, rect.Y + rect.Height };
            for (int i = 0; i < 4; i++)
            {
                double d = xs[i] * cx + ys[i] * cy;
                min = Math.Min(min, d);
                max = Math.Max(max, d);
            }
            start = new PointF((float)(min * cx), (float)(min * cy));
            end = new PointF((float)(max * cx), (float)(max * cy));
        }

        private static float ModeAngle(LinearGradientMode mode)
        {
            switch (mode)
            {
                case LinearGradientMode.Horizontal:
                    return 0;
                case LinearGradientMode.Vertical:
                    return 90;
                case LinearGradientMode.ForwardDiagonal:
                    return 45;
                case LinearGradientMode.BackwardDiagonal:
                    return 135;
                default:
                    throw new ArgumentException("The value of argument 'linearGradientMode' (" + (int)mode + ") is invalid for Enum type 'LinearGradientMode'.");
            }
        }

        public RectangleF Rectangle { get; }

        public Color[] LinearColors
        {
            get => new Color[] { colors[0], colors[1] };
            set
            {
                if (value == null || value.Length < 2)
                {
                    throw new ArgumentException("Parameter is not valid.");
                }
                colors = new Color[] { value[0], value[1] };
            }
        }

        public WrapMode WrapMode
        {
            get => wrapMode;
            set
            {
                if (value == WrapMode.Clamp)
                {
                    throw new ArgumentException("Parameter is not valid.");
                }
                wrapMode = value;
            }
        }

        public bool GammaCorrection { get; set; }

        public Blend Blend
        {
            get => blend;
            set
            {
                blend = value;
                interpolation = null;
            }
        }

        public ColorBlend InterpolationColors
        {
            get => interpolation;
            set
            {
                interpolation = value;
                blend = null;
            }
        }

        public Matrix Transform
        {
            get => transform.Clone();
            set
            {
                if (value == null)
                {
                    throw new ArgumentNullException("value");
                }
                transform = value.Clone();
            }
        }

        public void ResetTransform() => transform.Reset();

        public void MultiplyTransform(Matrix matrix) => transform.Multiply(matrix, MatrixOrder.Prepend);

        public void MultiplyTransform(Matrix matrix, MatrixOrder order) => transform.Multiply(matrix, order);

        public void TranslateTransform(float dx, float dy) => transform.Translate(dx, dy, MatrixOrder.Prepend);

        public void TranslateTransform(float dx, float dy, MatrixOrder order) => transform.Translate(dx, dy, order);

        public void ScaleTransform(float sx, float sy) => transform.Scale(sx, sy, MatrixOrder.Prepend);

        public void ScaleTransform(float sx, float sy, MatrixOrder order) => transform.Scale(sx, sy, order);

        public void RotateTransform(float angle) => transform.Rotate(angle, MatrixOrder.Prepend);

        public void RotateTransform(float angle, MatrixOrder order) => transform.Rotate(angle, order);

        // Треугольник: от первого цвета к доле `scale` второго в `focus` и назад.
        public void SetBlendTriangularShape(float focus) => SetBlendTriangularShape(focus, 1.0f);

        public void SetBlendTriangularShape(float focus, float scale)
        {
            if (focus < 0 || focus > 1 || scale < 0 || scale > 1)
            {
                throw new ArgumentException("Parameter is not valid.");
            }
            var shape = new Blend(3);
            shape.Positions = new float[] { 0, focus, 1 };
            shape.Factors = new float[] { 0, scale, 0 };
            Blend = shape;
        }

        public override object Clone()
        {
            var copy = (LinearGradientBrush)MemberwiseCloneBrush();
            return copy;
        }

        private LinearGradientBrush MemberwiseCloneBrush()
        {
            var copy = new LinearGradientBrush(start, end, colors[0], colors[1]);
            copy.transform = transform.Clone();
            copy.blend = blend;
            copy.interpolation = interpolation;
            copy.wrapMode = wrapMode;
            return copy;
        }

        internal override void Pack(Matrix device, out int[] ints, out float[] floats, out int[] texture)
        {
            texture = null;
            float[] inverse = Inverse(transform, device);
            if (inverse == null)
            {
                ints = new int[] { 0, 0, colors[0].ToArgb() };
                floats = null;
                return;
            }
            float[] positions;
            int[] argb;
            if (interpolation != null && interpolation.Colors != null && interpolation.Positions != null)
            {
                int n = Math.Min(interpolation.Colors.Length, interpolation.Positions.Length);
                positions = new float[n];
                argb = new int[n];
                for (int i = 0; i < n; i++)
                {
                    positions[i] = interpolation.Positions[i];
                    argb[i] = interpolation.Colors[i].ToArgb();
                }
            }
            else if (blend != null && blend.Factors != null && blend.Positions != null)
            {
                int n = Math.Min(blend.Factors.Length, blend.Positions.Length);
                positions = new float[n];
                argb = new int[n];
                for (int i = 0; i < n; i++)
                {
                    positions[i] = blend.Positions[i];
                    argb[i] = Lerp(colors[0], colors[1], blend.Factors[i]);
                }
            }
            else
            {
                positions = new float[] { 0, 1 };
                argb = new int[] { colors[0].ToArgb(), colors[1].ToArgb() };
            }
            ints = new int[3 + argb.Length];
            ints[0] = 1;
            ints[1] = (int)wrapMode;
            ints[2] = argb.Length;
            for (int i = 0; i < argb.Length; i++)
            {
                ints[3 + i] = argb[i];
            }
            floats = new float[10 + positions.Length];
            for (int i = 0; i < 6; i++)
            {
                floats[i] = inverse[i];
            }
            floats[6] = start.X;
            floats[7] = start.Y;
            floats[8] = end.X;
            floats[9] = end.Y;
            for (int i = 0; i < positions.Length; i++)
            {
                floats[10 + i] = positions[i];
            }
        }

        private static int Lerp(Color a, Color b, float t)
        {
            int Mix(int x, int y) => (int)Math.Round(x + (y - x) * (double)t);
            return (Mix(a.A, b.A) << 24) | (Mix(a.R, b.R) << 16) | (Mix(a.G, b.G) << 8) | Mix(a.B, b.B);
        }
    }

    public sealed class Blend
    {
        public Blend()
            : this(1)
        {
        }

        public Blend(int count)
        {
            Factors = new float[count];
            Positions = new float[count];
        }

        public float[] Factors { get; set; }

        public float[] Positions { get; set; }
    }

    public sealed class ColorBlend
    {
        public ColorBlend()
            : this(1)
        {
        }

        public ColorBlend(int count)
        {
            Colors = new Color[count];
            Positions = new float[count];
        }

        public Color[] Colors { get; set; }

        public float[] Positions { get; set; }
    }

}

namespace System.Drawing
{
    public sealed class Pen : IDisposable, ICloneable
    {
        private Brush brush;
        private DashStyle dashStyle;
        private float[] dashPattern;

        public Pen(Color color)
            : this(color, 1)
        {
        }

        public Pen(Color color, float width)
        {
            brush = new SolidBrush(color);
            Width = width;
            MiterLimit = 10;
        }

        public Pen(Brush brush)
            : this(brush, 1)
        {
        }

        public Pen(Brush brush, float width)
        {
            if (brush == null)
            {
                throw new ArgumentNullException("brush");
            }
            this.brush = (Brush)brush.Clone();
            Width = width;
            MiterLimit = 10;
        }

        public float Width { get; set; }

        public Color Color
        {
            get => brush is SolidBrush solid ? solid.Color : Color.Empty;
            set => brush = new SolidBrush(value);
        }

        public Brush Brush
        {
            get => (Brush)brush.Clone();
            set
            {
                if (value == null)
                {
                    throw new ArgumentNullException("value");
                }
                brush = (Brush)value.Clone();
            }
        }

        public PenType PenType =>
            brush is SolidBrush ? PenType.SolidColor : brush is TextureBrush ? PenType.TextureFill : PenType.LinearGradient;

        public LineJoin LineJoin { get; set; }

        public LineCap StartCap { get; set; }

        public LineCap EndCap { get; set; }

        public DashCap DashCap { get; set; }

        public float MiterLimit { get; set; }

        public float DashOffset { get; set; }

        public PenAlignment Alignment { get; set; }

        public Matrix Transform { get; set; } = new Matrix();

        public DashStyle DashStyle
        {
            get => dashStyle;
            set
            {
                dashStyle = value;
                dashPattern = null;
            }
        }

        // Узор в толщинах пера; у готовых стилей — те же числа, что отдаёт GDI+.
        public float[] DashPattern
        {
            get
            {
                if (dashPattern != null)
                {
                    return Copy(dashPattern);
                }
                switch (dashStyle)
                {
                    case DashStyle.Dash:
                        return new float[] { 3, 1 };
                    case DashStyle.Dot:
                        return new float[] { 1, 1 };
                    case DashStyle.DashDot:
                        return new float[] { 3, 1, 1, 1 };
                    case DashStyle.DashDotDot:
                        return new float[] { 3, 1, 1, 1, 1, 1 };
                    default:
                        return new float[] { 1 };
                }
            }
            set
            {
                if (value == null || value.Length == 0)
                {
                    throw new ArgumentException("Parameter is not valid.");
                }
                for (int i = 0; i < value.Length; i++)
                {
                    if (value[i] <= 0)
                    {
                        throw new ArgumentException("Parameter is not valid.");
                    }
                }
                dashPattern = Copy(value);
                dashStyle = DashStyle.Custom;
            }
        }

        public void SetLineCap(LineCap startCap, LineCap endCap, DashCap dashCap)
        {
            StartCap = startCap;
            EndCap = endCap;
            DashCap = dashCap;
        }

        public void Dispose()
        {
        }

        public object Clone()
        {
            var copy = new Pen(brush, Width);
            copy.LineJoin = LineJoin;
            copy.StartCap = StartCap;
            copy.EndCap = EndCap;
            copy.DashCap = DashCap;
            copy.MiterLimit = MiterLimit;
            copy.DashOffset = DashOffset;
            copy.Alignment = Alignment;
            copy.dashStyle = dashStyle;
            copy.dashPattern = dashPattern;
            return copy;
        }

        private static float[] Copy(float[] values)
        {
            var copy = new float[values.Length];
            Array.CopyItems(values, copy, values.Length);
            return copy;
        }

        internal Brush PaintBrush => brush;

        internal bool IsSolidThin(out Color color)
        {
            color = Color.Empty;
            return Width <= 1 && dashStyle == DashStyle.Solid && brush.IsSolid(out color);
        }

        internal float[] PackFloats()
        {
            float[] dashes = dashStyle == DashStyle.Solid ? new float[0] : DashPattern;
            var packed = new float[3 + dashes.Length];
            packed[0] = Width;
            packed[1] = MiterLimit;
            packed[2] = DashOffset;
            for (int i = 0; i < dashes.Length; i++)
            {
                packed[3 + i] = dashes[i];
            }
            return packed;
        }

        internal int[] PackInts() => new int[] { (int)LineJoin, (int)StartCap, (int)EndCap };
    }

    public static class Brushes
    {
        public static Brush Transparent => new SolidBrush(Color.Transparent);
        public static Brush AliceBlue => new SolidBrush(Color.AliceBlue);
        public static Brush AntiqueWhite => new SolidBrush(Color.AntiqueWhite);
        public static Brush Aqua => new SolidBrush(Color.Aqua);
        public static Brush Aquamarine => new SolidBrush(Color.Aquamarine);
        public static Brush Azure => new SolidBrush(Color.Azure);
        public static Brush Beige => new SolidBrush(Color.Beige);
        public static Brush Bisque => new SolidBrush(Color.Bisque);
        public static Brush Black => new SolidBrush(Color.Black);
        public static Brush BlanchedAlmond => new SolidBrush(Color.BlanchedAlmond);
        public static Brush Blue => new SolidBrush(Color.Blue);
        public static Brush BlueViolet => new SolidBrush(Color.BlueViolet);
        public static Brush Brown => new SolidBrush(Color.Brown);
        public static Brush BurlyWood => new SolidBrush(Color.BurlyWood);
        public static Brush CadetBlue => new SolidBrush(Color.CadetBlue);
        public static Brush Chartreuse => new SolidBrush(Color.Chartreuse);
        public static Brush Chocolate => new SolidBrush(Color.Chocolate);
        public static Brush Coral => new SolidBrush(Color.Coral);
        public static Brush CornflowerBlue => new SolidBrush(Color.CornflowerBlue);
        public static Brush Cornsilk => new SolidBrush(Color.Cornsilk);
        public static Brush Crimson => new SolidBrush(Color.Crimson);
        public static Brush Cyan => new SolidBrush(Color.Cyan);
        public static Brush DarkBlue => new SolidBrush(Color.DarkBlue);
        public static Brush DarkCyan => new SolidBrush(Color.DarkCyan);
        public static Brush DarkGoldenrod => new SolidBrush(Color.DarkGoldenrod);
        public static Brush DarkGray => new SolidBrush(Color.DarkGray);
        public static Brush DarkGreen => new SolidBrush(Color.DarkGreen);
        public static Brush DarkKhaki => new SolidBrush(Color.DarkKhaki);
        public static Brush DarkMagenta => new SolidBrush(Color.DarkMagenta);
        public static Brush DarkOliveGreen => new SolidBrush(Color.DarkOliveGreen);
        public static Brush DarkOrange => new SolidBrush(Color.DarkOrange);
        public static Brush DarkOrchid => new SolidBrush(Color.DarkOrchid);
        public static Brush DarkRed => new SolidBrush(Color.DarkRed);
        public static Brush DarkSalmon => new SolidBrush(Color.DarkSalmon);
        public static Brush DarkSeaGreen => new SolidBrush(Color.DarkSeaGreen);
        public static Brush DarkSlateBlue => new SolidBrush(Color.DarkSlateBlue);
        public static Brush DarkSlateGray => new SolidBrush(Color.DarkSlateGray);
        public static Brush DarkTurquoise => new SolidBrush(Color.DarkTurquoise);
        public static Brush DarkViolet => new SolidBrush(Color.DarkViolet);
        public static Brush DeepPink => new SolidBrush(Color.DeepPink);
        public static Brush DeepSkyBlue => new SolidBrush(Color.DeepSkyBlue);
        public static Brush DimGray => new SolidBrush(Color.DimGray);
        public static Brush DodgerBlue => new SolidBrush(Color.DodgerBlue);
        public static Brush Firebrick => new SolidBrush(Color.Firebrick);
        public static Brush FloralWhite => new SolidBrush(Color.FloralWhite);
        public static Brush ForestGreen => new SolidBrush(Color.ForestGreen);
        public static Brush Fuchsia => new SolidBrush(Color.Fuchsia);
        public static Brush Gainsboro => new SolidBrush(Color.Gainsboro);
        public static Brush GhostWhite => new SolidBrush(Color.GhostWhite);
        public static Brush Gold => new SolidBrush(Color.Gold);
        public static Brush Goldenrod => new SolidBrush(Color.Goldenrod);
        public static Brush Gray => new SolidBrush(Color.Gray);
        public static Brush Green => new SolidBrush(Color.Green);
        public static Brush GreenYellow => new SolidBrush(Color.GreenYellow);
        public static Brush Honeydew => new SolidBrush(Color.Honeydew);
        public static Brush HotPink => new SolidBrush(Color.HotPink);
        public static Brush IndianRed => new SolidBrush(Color.IndianRed);
        public static Brush Indigo => new SolidBrush(Color.Indigo);
        public static Brush Ivory => new SolidBrush(Color.Ivory);
        public static Brush Khaki => new SolidBrush(Color.Khaki);
        public static Brush Lavender => new SolidBrush(Color.Lavender);
        public static Brush LavenderBlush => new SolidBrush(Color.LavenderBlush);
        public static Brush LawnGreen => new SolidBrush(Color.LawnGreen);
        public static Brush LemonChiffon => new SolidBrush(Color.LemonChiffon);
        public static Brush LightBlue => new SolidBrush(Color.LightBlue);
        public static Brush LightCoral => new SolidBrush(Color.LightCoral);
        public static Brush LightCyan => new SolidBrush(Color.LightCyan);
        public static Brush LightGoldenrodYellow => new SolidBrush(Color.LightGoldenrodYellow);
        public static Brush LightGray => new SolidBrush(Color.LightGray);
        public static Brush LightGreen => new SolidBrush(Color.LightGreen);
        public static Brush LightPink => new SolidBrush(Color.LightPink);
        public static Brush LightSalmon => new SolidBrush(Color.LightSalmon);
        public static Brush LightSeaGreen => new SolidBrush(Color.LightSeaGreen);
        public static Brush LightSkyBlue => new SolidBrush(Color.LightSkyBlue);
        public static Brush LightSlateGray => new SolidBrush(Color.LightSlateGray);
        public static Brush LightSteelBlue => new SolidBrush(Color.LightSteelBlue);
        public static Brush LightYellow => new SolidBrush(Color.LightYellow);
        public static Brush Lime => new SolidBrush(Color.Lime);
        public static Brush LimeGreen => new SolidBrush(Color.LimeGreen);
        public static Brush Linen => new SolidBrush(Color.Linen);
        public static Brush Magenta => new SolidBrush(Color.Magenta);
        public static Brush Maroon => new SolidBrush(Color.Maroon);
        public static Brush MediumAquamarine => new SolidBrush(Color.MediumAquamarine);
        public static Brush MediumBlue => new SolidBrush(Color.MediumBlue);
        public static Brush MediumOrchid => new SolidBrush(Color.MediumOrchid);
        public static Brush MediumPurple => new SolidBrush(Color.MediumPurple);
        public static Brush MediumSeaGreen => new SolidBrush(Color.MediumSeaGreen);
        public static Brush MediumSlateBlue => new SolidBrush(Color.MediumSlateBlue);
        public static Brush MediumSpringGreen => new SolidBrush(Color.MediumSpringGreen);
        public static Brush MediumTurquoise => new SolidBrush(Color.MediumTurquoise);
        public static Brush MediumVioletRed => new SolidBrush(Color.MediumVioletRed);
        public static Brush MidnightBlue => new SolidBrush(Color.MidnightBlue);
        public static Brush MintCream => new SolidBrush(Color.MintCream);
        public static Brush MistyRose => new SolidBrush(Color.MistyRose);
        public static Brush Moccasin => new SolidBrush(Color.Moccasin);
        public static Brush NavajoWhite => new SolidBrush(Color.NavajoWhite);
        public static Brush Navy => new SolidBrush(Color.Navy);
        public static Brush OldLace => new SolidBrush(Color.OldLace);
        public static Brush Olive => new SolidBrush(Color.Olive);
        public static Brush OliveDrab => new SolidBrush(Color.OliveDrab);
        public static Brush Orange => new SolidBrush(Color.Orange);
        public static Brush OrangeRed => new SolidBrush(Color.OrangeRed);
        public static Brush Orchid => new SolidBrush(Color.Orchid);
        public static Brush PaleGoldenrod => new SolidBrush(Color.PaleGoldenrod);
        public static Brush PaleGreen => new SolidBrush(Color.PaleGreen);
        public static Brush PaleTurquoise => new SolidBrush(Color.PaleTurquoise);
        public static Brush PaleVioletRed => new SolidBrush(Color.PaleVioletRed);
        public static Brush PapayaWhip => new SolidBrush(Color.PapayaWhip);
        public static Brush PeachPuff => new SolidBrush(Color.PeachPuff);
        public static Brush Peru => new SolidBrush(Color.Peru);
        public static Brush Pink => new SolidBrush(Color.Pink);
        public static Brush Plum => new SolidBrush(Color.Plum);
        public static Brush PowderBlue => new SolidBrush(Color.PowderBlue);
        public static Brush Purple => new SolidBrush(Color.Purple);
        public static Brush Red => new SolidBrush(Color.Red);
        public static Brush RosyBrown => new SolidBrush(Color.RosyBrown);
        public static Brush RoyalBlue => new SolidBrush(Color.RoyalBlue);
        public static Brush SaddleBrown => new SolidBrush(Color.SaddleBrown);
        public static Brush Salmon => new SolidBrush(Color.Salmon);
        public static Brush SandyBrown => new SolidBrush(Color.SandyBrown);
        public static Brush SeaGreen => new SolidBrush(Color.SeaGreen);
        public static Brush SeaShell => new SolidBrush(Color.SeaShell);
        public static Brush Sienna => new SolidBrush(Color.Sienna);
        public static Brush Silver => new SolidBrush(Color.Silver);
        public static Brush SkyBlue => new SolidBrush(Color.SkyBlue);
        public static Brush SlateBlue => new SolidBrush(Color.SlateBlue);
        public static Brush SlateGray => new SolidBrush(Color.SlateGray);
        public static Brush Snow => new SolidBrush(Color.Snow);
        public static Brush SpringGreen => new SolidBrush(Color.SpringGreen);
        public static Brush SteelBlue => new SolidBrush(Color.SteelBlue);
        public static Brush Tan => new SolidBrush(Color.Tan);
        public static Brush Teal => new SolidBrush(Color.Teal);
        public static Brush Thistle => new SolidBrush(Color.Thistle);
        public static Brush Tomato => new SolidBrush(Color.Tomato);
        public static Brush Turquoise => new SolidBrush(Color.Turquoise);
        public static Brush Violet => new SolidBrush(Color.Violet);
        public static Brush Wheat => new SolidBrush(Color.Wheat);
        public static Brush White => new SolidBrush(Color.White);
        public static Brush WhiteSmoke => new SolidBrush(Color.WhiteSmoke);
        public static Brush Yellow => new SolidBrush(Color.Yellow);
        public static Brush YellowGreen => new SolidBrush(Color.YellowGreen);
    }

    public static class Pens
    {
        public static Pen Transparent => new Pen(Color.Transparent);
        public static Pen AliceBlue => new Pen(Color.AliceBlue);
        public static Pen AntiqueWhite => new Pen(Color.AntiqueWhite);
        public static Pen Aqua => new Pen(Color.Aqua);
        public static Pen Aquamarine => new Pen(Color.Aquamarine);
        public static Pen Azure => new Pen(Color.Azure);
        public static Pen Beige => new Pen(Color.Beige);
        public static Pen Bisque => new Pen(Color.Bisque);
        public static Pen Black => new Pen(Color.Black);
        public static Pen BlanchedAlmond => new Pen(Color.BlanchedAlmond);
        public static Pen Blue => new Pen(Color.Blue);
        public static Pen BlueViolet => new Pen(Color.BlueViolet);
        public static Pen Brown => new Pen(Color.Brown);
        public static Pen BurlyWood => new Pen(Color.BurlyWood);
        public static Pen CadetBlue => new Pen(Color.CadetBlue);
        public static Pen Chartreuse => new Pen(Color.Chartreuse);
        public static Pen Chocolate => new Pen(Color.Chocolate);
        public static Pen Coral => new Pen(Color.Coral);
        public static Pen CornflowerBlue => new Pen(Color.CornflowerBlue);
        public static Pen Cornsilk => new Pen(Color.Cornsilk);
        public static Pen Crimson => new Pen(Color.Crimson);
        public static Pen Cyan => new Pen(Color.Cyan);
        public static Pen DarkBlue => new Pen(Color.DarkBlue);
        public static Pen DarkCyan => new Pen(Color.DarkCyan);
        public static Pen DarkGoldenrod => new Pen(Color.DarkGoldenrod);
        public static Pen DarkGray => new Pen(Color.DarkGray);
        public static Pen DarkGreen => new Pen(Color.DarkGreen);
        public static Pen DarkKhaki => new Pen(Color.DarkKhaki);
        public static Pen DarkMagenta => new Pen(Color.DarkMagenta);
        public static Pen DarkOliveGreen => new Pen(Color.DarkOliveGreen);
        public static Pen DarkOrange => new Pen(Color.DarkOrange);
        public static Pen DarkOrchid => new Pen(Color.DarkOrchid);
        public static Pen DarkRed => new Pen(Color.DarkRed);
        public static Pen DarkSalmon => new Pen(Color.DarkSalmon);
        public static Pen DarkSeaGreen => new Pen(Color.DarkSeaGreen);
        public static Pen DarkSlateBlue => new Pen(Color.DarkSlateBlue);
        public static Pen DarkSlateGray => new Pen(Color.DarkSlateGray);
        public static Pen DarkTurquoise => new Pen(Color.DarkTurquoise);
        public static Pen DarkViolet => new Pen(Color.DarkViolet);
        public static Pen DeepPink => new Pen(Color.DeepPink);
        public static Pen DeepSkyBlue => new Pen(Color.DeepSkyBlue);
        public static Pen DimGray => new Pen(Color.DimGray);
        public static Pen DodgerBlue => new Pen(Color.DodgerBlue);
        public static Pen Firebrick => new Pen(Color.Firebrick);
        public static Pen FloralWhite => new Pen(Color.FloralWhite);
        public static Pen ForestGreen => new Pen(Color.ForestGreen);
        public static Pen Fuchsia => new Pen(Color.Fuchsia);
        public static Pen Gainsboro => new Pen(Color.Gainsboro);
        public static Pen GhostWhite => new Pen(Color.GhostWhite);
        public static Pen Gold => new Pen(Color.Gold);
        public static Pen Goldenrod => new Pen(Color.Goldenrod);
        public static Pen Gray => new Pen(Color.Gray);
        public static Pen Green => new Pen(Color.Green);
        public static Pen GreenYellow => new Pen(Color.GreenYellow);
        public static Pen Honeydew => new Pen(Color.Honeydew);
        public static Pen HotPink => new Pen(Color.HotPink);
        public static Pen IndianRed => new Pen(Color.IndianRed);
        public static Pen Indigo => new Pen(Color.Indigo);
        public static Pen Ivory => new Pen(Color.Ivory);
        public static Pen Khaki => new Pen(Color.Khaki);
        public static Pen Lavender => new Pen(Color.Lavender);
        public static Pen LavenderBlush => new Pen(Color.LavenderBlush);
        public static Pen LawnGreen => new Pen(Color.LawnGreen);
        public static Pen LemonChiffon => new Pen(Color.LemonChiffon);
        public static Pen LightBlue => new Pen(Color.LightBlue);
        public static Pen LightCoral => new Pen(Color.LightCoral);
        public static Pen LightCyan => new Pen(Color.LightCyan);
        public static Pen LightGoldenrodYellow => new Pen(Color.LightGoldenrodYellow);
        public static Pen LightGray => new Pen(Color.LightGray);
        public static Pen LightGreen => new Pen(Color.LightGreen);
        public static Pen LightPink => new Pen(Color.LightPink);
        public static Pen LightSalmon => new Pen(Color.LightSalmon);
        public static Pen LightSeaGreen => new Pen(Color.LightSeaGreen);
        public static Pen LightSkyBlue => new Pen(Color.LightSkyBlue);
        public static Pen LightSlateGray => new Pen(Color.LightSlateGray);
        public static Pen LightSteelBlue => new Pen(Color.LightSteelBlue);
        public static Pen LightYellow => new Pen(Color.LightYellow);
        public static Pen Lime => new Pen(Color.Lime);
        public static Pen LimeGreen => new Pen(Color.LimeGreen);
        public static Pen Linen => new Pen(Color.Linen);
        public static Pen Magenta => new Pen(Color.Magenta);
        public static Pen Maroon => new Pen(Color.Maroon);
        public static Pen MediumAquamarine => new Pen(Color.MediumAquamarine);
        public static Pen MediumBlue => new Pen(Color.MediumBlue);
        public static Pen MediumOrchid => new Pen(Color.MediumOrchid);
        public static Pen MediumPurple => new Pen(Color.MediumPurple);
        public static Pen MediumSeaGreen => new Pen(Color.MediumSeaGreen);
        public static Pen MediumSlateBlue => new Pen(Color.MediumSlateBlue);
        public static Pen MediumSpringGreen => new Pen(Color.MediumSpringGreen);
        public static Pen MediumTurquoise => new Pen(Color.MediumTurquoise);
        public static Pen MediumVioletRed => new Pen(Color.MediumVioletRed);
        public static Pen MidnightBlue => new Pen(Color.MidnightBlue);
        public static Pen MintCream => new Pen(Color.MintCream);
        public static Pen MistyRose => new Pen(Color.MistyRose);
        public static Pen Moccasin => new Pen(Color.Moccasin);
        public static Pen NavajoWhite => new Pen(Color.NavajoWhite);
        public static Pen Navy => new Pen(Color.Navy);
        public static Pen OldLace => new Pen(Color.OldLace);
        public static Pen Olive => new Pen(Color.Olive);
        public static Pen OliveDrab => new Pen(Color.OliveDrab);
        public static Pen Orange => new Pen(Color.Orange);
        public static Pen OrangeRed => new Pen(Color.OrangeRed);
        public static Pen Orchid => new Pen(Color.Orchid);
        public static Pen PaleGoldenrod => new Pen(Color.PaleGoldenrod);
        public static Pen PaleGreen => new Pen(Color.PaleGreen);
        public static Pen PaleTurquoise => new Pen(Color.PaleTurquoise);
        public static Pen PaleVioletRed => new Pen(Color.PaleVioletRed);
        public static Pen PapayaWhip => new Pen(Color.PapayaWhip);
        public static Pen PeachPuff => new Pen(Color.PeachPuff);
        public static Pen Peru => new Pen(Color.Peru);
        public static Pen Pink => new Pen(Color.Pink);
        public static Pen Plum => new Pen(Color.Plum);
        public static Pen PowderBlue => new Pen(Color.PowderBlue);
        public static Pen Purple => new Pen(Color.Purple);
        public static Pen Red => new Pen(Color.Red);
        public static Pen RosyBrown => new Pen(Color.RosyBrown);
        public static Pen RoyalBlue => new Pen(Color.RoyalBlue);
        public static Pen SaddleBrown => new Pen(Color.SaddleBrown);
        public static Pen Salmon => new Pen(Color.Salmon);
        public static Pen SandyBrown => new Pen(Color.SandyBrown);
        public static Pen SeaGreen => new Pen(Color.SeaGreen);
        public static Pen SeaShell => new Pen(Color.SeaShell);
        public static Pen Sienna => new Pen(Color.Sienna);
        public static Pen Silver => new Pen(Color.Silver);
        public static Pen SkyBlue => new Pen(Color.SkyBlue);
        public static Pen SlateBlue => new Pen(Color.SlateBlue);
        public static Pen SlateGray => new Pen(Color.SlateGray);
        public static Pen Snow => new Pen(Color.Snow);
        public static Pen SpringGreen => new Pen(Color.SpringGreen);
        public static Pen SteelBlue => new Pen(Color.SteelBlue);
        public static Pen Tan => new Pen(Color.Tan);
        public static Pen Teal => new Pen(Color.Teal);
        public static Pen Thistle => new Pen(Color.Thistle);
        public static Pen Tomato => new Pen(Color.Tomato);
        public static Pen Turquoise => new Pen(Color.Turquoise);
        public static Pen Violet => new Pen(Color.Violet);
        public static Pen Wheat => new Pen(Color.Wheat);
        public static Pen White => new Pen(Color.White);
        public static Pen WhiteSmoke => new Pen(Color.WhiteSmoke);
        public static Pen Yellow => new Pen(Color.Yellow);
        public static Pen YellowGreen => new Pen(Color.YellowGreen);
    }

    public static class SystemBrushes
    {
        public static Brush Control => new SolidBrush(SystemColors.Control);
        public static Brush ControlDark => new SolidBrush(SystemColors.ControlDark);
        public static Brush ControlDarkDark => new SolidBrush(SystemColors.ControlDarkDark);
        public static Brush ControlLight => new SolidBrush(SystemColors.ControlLight);
        public static Brush ControlLightLight => new SolidBrush(SystemColors.ControlLightLight);
        public static Brush ControlText => new SolidBrush(SystemColors.ControlText);
        public static Brush GrayText => new SolidBrush(SystemColors.GrayText);
        public static Brush Highlight => new SolidBrush(SystemColors.Highlight);
        public static Brush HighlightText => new SolidBrush(SystemColors.HighlightText);
        public static Brush Window => new SolidBrush(SystemColors.Window);
        public static Brush WindowText => new SolidBrush(SystemColors.WindowText);
        public static Brush ButtonFace => new SolidBrush(SystemColors.ButtonFace);
        public static Brush ButtonShadow => new SolidBrush(SystemColors.ButtonShadow);
    }

    public static class SystemPens
    {
        public static Pen Control => new Pen(SystemColors.Control);
        public static Pen ControlDark => new Pen(SystemColors.ControlDark);
        public static Pen ControlDarkDark => new Pen(SystemColors.ControlDarkDark);
        public static Pen ControlLight => new Pen(SystemColors.ControlLight);
        public static Pen ControlLightLight => new Pen(SystemColors.ControlLightLight);
        public static Pen ControlText => new Pen(SystemColors.ControlText);
        public static Pen GrayText => new Pen(SystemColors.GrayText);
        public static Pen Highlight => new Pen(SystemColors.Highlight);
        public static Pen HighlightText => new Pen(SystemColors.HighlightText);
        public static Pen Window => new Pen(SystemColors.Window);
        public static Pen WindowText => new Pen(SystemColors.WindowText);
        public static Pen ButtonFace => new Pen(SystemColors.ButtonFace);
        public static Pen ButtonShadow => new Pen(SystemColors.ButtonShadow);
    }
}
