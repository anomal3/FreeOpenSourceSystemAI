// System.Drawing.Drawing2D (фаза N9): матрица, путь и перечисления режимов
// рисования. Путь хранится так же, как у GDI+ — точки и их виды
// (`PathPointType`), — и эллипсы и дуги превращаются в кривые Безье уже при
// добавлении: программа, читающая `PathPoints`, видит те же 13 точек эллипса,
// что под Windows (сверено пробой). Разворачивает кривые растеризатор в Rust.

namespace System.Drawing.Drawing2D
{
    public enum SmoothingMode
    {
        Invalid = -1,
        Default = 0,
        HighSpeed = 1,
        HighQuality = 2,
        None = 3,
        AntiAlias = 4,
    }

    public enum PixelOffsetMode
    {
        Invalid = -1,
        Default = 0,
        HighSpeed = 1,
        HighQuality = 2,
        None = 3,
        Half = 4,
    }

    public enum CompositingMode
    {
        SourceOver = 0,
        SourceCopy = 1,
    }

    public enum CompositingQuality
    {
        Invalid = -1,
        Default = 0,
        HighSpeed = 1,
        HighQuality = 2,
        GammaCorrected = 3,
        AssumeLinear = 4,
    }

    public enum InterpolationMode
    {
        Invalid = -1,
        Default = 0,
        Low = 1,
        High = 2,
        Bilinear = 3,
        Bicubic = 4,
        NearestNeighbor = 5,
        HighQualityBilinear = 6,
        HighQualityBicubic = 7,
    }

    public enum FillMode
    {
        Alternate = 0,
        Winding = 1,
    }

    public enum MatrixOrder
    {
        Prepend = 0,
        Append = 1,
    }

    public enum LineJoin
    {
        Miter = 0,
        Bevel = 1,
        Round = 2,
        MiterClipped = 3,
    }

    public enum LineCap
    {
        Flat = 0,
        Square = 1,
        Round = 2,
        Triangle = 3,
        NoAnchor = 0x10,
        SquareAnchor = 0x11,
        RoundAnchor = 0x12,
        DiamondAnchor = 0x13,
        ArrowAnchor = 0x14,
        AnchorMask = 0xf0,
        Custom = 0xff,
    }

    public enum DashCap
    {
        Flat = 0,
        Round = 2,
        Triangle = 3,
    }

    public enum DashStyle
    {
        Solid = 0,
        Dash = 1,
        Dot = 2,
        DashDot = 3,
        DashDotDot = 4,
        Custom = 5,
    }

    public enum PenAlignment
    {
        Center = 0,
        Inset = 1,
        Outset = 2,
        Left = 3,
        Right = 4,
    }

    public enum PenType
    {
        SolidColor = 0,
        HatchFill = 1,
        TextureFill = 2,
        PathGradient = 3,
        LinearGradient = 4,
    }

    public enum WrapMode
    {
        Tile = 0,
        TileFlipX = 1,
        TileFlipY = 2,
        TileFlipXY = 3,
        Clamp = 4,
    }

    public enum LinearGradientMode
    {
        Horizontal = 0,
        Vertical = 1,
        ForwardDiagonal = 2,
        BackwardDiagonal = 3,
    }

    public enum CombineMode
    {
        Replace = 0,
        Intersect = 1,
        Union = 2,
        Xor = 3,
        Exclude = 4,
        Complement = 5,
    }

    public enum PathPointType
    {
        Start = 0,
        Line = 1,
        Bezier = 3,
        Bezier3 = 3,
        PathTypeMask = 0x07,
        DashMode = 0x10,
        PathMarker = 0x20,
        CloseSubpath = 0x80,
    }

    public enum CoordinateSpace
    {
        World = 0,
        Page = 1,
        Device = 2,
    }

    public sealed class GraphicsState
    {
        internal GraphicsState()
        {
        }

        internal Matrix Transform;
        internal Region Clip;
        internal SmoothingMode Smoothing;
        internal PixelOffsetMode PixelOffset;
        internal CompositingMode Compositing;
        internal CompositingQuality Quality;
        internal InterpolationMode Interpolation;
        internal System.Drawing.Text.TextRenderingHint TextHint;
    }

    public sealed class GraphicsContainer
    {
        internal GraphicsContainer(GraphicsState state)
        {
            State = state;
        }

        internal GraphicsState State { get; }
    }

    // Аффинное преобразование 3×2 в порядке GDI+: точка — строка слева,
    // `x' = x·M11 + y·M21 + OffsetX`. «Prepend» (по умолчанию) ставит новое
    // преобразование ПЕРЕД прежним: `Translate(10, 20)`, затем `Scale(2, 3)`
    // сначала масштабирует точку, потом сдвигает — элементы 2,0,0,3,10,20.
    public sealed class Matrix : IDisposable
    {
        internal float m11;
        internal float m12;
        internal float m21;
        internal float m22;
        internal float dx;
        internal float dy;

        public Matrix()
        {
            m11 = 1;
            m22 = 1;
        }

        public Matrix(float m11, float m12, float m21, float m22, float dx, float dy)
        {
            this.m11 = m11;
            this.m12 = m12;
            this.m21 = m21;
            this.m22 = m22;
            this.dx = dx;
            this.dy = dy;
        }

        // Прямоугольник в параллелограмм: три точки — верхний левый, верхний
        // правый и нижний левый углы.
        public Matrix(RectangleF rect, PointF[] plgpts)
        {
            if (plgpts == null)
            {
                throw new ArgumentNullException("plgpts");
            }
            if (plgpts.Length != 3)
            {
                throw new ArgumentException("Parameter is not valid.");
            }
            if (rect.Width == 0 || rect.Height == 0)
            {
                throw new ArgumentException("Parameter is not valid.");
            }
            m11 = (plgpts[1].X - plgpts[0].X) / rect.Width;
            m12 = (plgpts[1].Y - plgpts[0].Y) / rect.Width;
            m21 = (plgpts[2].X - plgpts[0].X) / rect.Height;
            m22 = (plgpts[2].Y - plgpts[0].Y) / rect.Height;
            dx = plgpts[0].X - rect.X * m11 - rect.Y * m21;
            dy = plgpts[0].Y - rect.X * m12 - rect.Y * m22;
        }

        public Matrix(Rectangle rect, Point[] plgpts)
            : this((RectangleF)rect, ToFloat(plgpts))
        {
        }

        private static PointF[] ToFloat(Point[] points)
        {
            if (points == null)
            {
                throw new ArgumentNullException("plgpts");
            }
            var result = new PointF[points.Length];
            for (int i = 0; i < points.Length; i++)
            {
                result[i] = points[i];
            }
            return result;
        }

        public float[] Elements => new float[] { m11, m12, m21, m22, dx, dy };

        public float OffsetX => dx;

        public float OffsetY => dy;

        public bool IsIdentity => m11 == 1 && m12 == 0 && m21 == 0 && m22 == 1 && dx == 0 && dy == 0;

        public bool IsInvertible
        {
            get
            {
                double det = (double)m11 * m22 - (double)m12 * m21;
                return det != 0 && !double.IsNaN(det) && !double.IsInfinity(det);
            }
        }

        public void Dispose()
        {
        }

        public Matrix Clone() => new Matrix(m11, m12, m21, m22, dx, dy);

        public void Reset()
        {
            m11 = 1;
            m12 = 0;
            m21 = 0;
            m22 = 1;
            dx = 0;
            dy = 0;
        }

        // `a`, потом `b`.
        internal static void Product(Matrix a, Matrix b, Matrix result)
        {
            double r11 = (double)a.m11 * b.m11 + (double)a.m12 * b.m21;
            double r12 = (double)a.m11 * b.m12 + (double)a.m12 * b.m22;
            double r21 = (double)a.m21 * b.m11 + (double)a.m22 * b.m21;
            double r22 = (double)a.m21 * b.m12 + (double)a.m22 * b.m22;
            double rdx = (double)a.dx * b.m11 + (double)a.dy * b.m21 + b.dx;
            double rdy = (double)a.dx * b.m12 + (double)a.dy * b.m22 + b.dy;
            result.m11 = (float)r11;
            result.m12 = (float)r12;
            result.m21 = (float)r21;
            result.m22 = (float)r22;
            result.dx = (float)rdx;
            result.dy = (float)rdy;
        }

        public void Multiply(Matrix matrix) => Multiply(matrix, MatrixOrder.Prepend);

        public void Multiply(Matrix matrix, MatrixOrder order)
        {
            if (matrix == null)
            {
                throw new ArgumentNullException("matrix");
            }
            if (order == MatrixOrder.Prepend)
            {
                Product(matrix, this, this);
            }
            else
            {
                Product(this, matrix, this);
            }
        }

        public void Translate(float offsetX, float offsetY) => Translate(offsetX, offsetY, MatrixOrder.Prepend);

        public void Translate(float offsetX, float offsetY, MatrixOrder order) =>
            Multiply(new Matrix(1, 0, 0, 1, offsetX, offsetY), order);

        public void Scale(float scaleX, float scaleY) => Scale(scaleX, scaleY, MatrixOrder.Prepend);

        public void Scale(float scaleX, float scaleY, MatrixOrder order) => Multiply(new Matrix(scaleX, 0, 0, scaleY, 0, 0), order);

        public void Rotate(float angle) => Rotate(angle, MatrixOrder.Prepend);

        public void Rotate(float angle, MatrixOrder order) => Multiply(RotationOf(angle), order);

        internal static Matrix RotationOf(float angle)
        {
            double radians = angle * Math.PI / 180.0;
            float cos = (float)Math.Cos(radians);
            float sin = (float)Math.Sin(radians);
            return new Matrix(cos, sin, -sin, cos, 0, 0);
        }

        public void RotateAt(float angle, PointF point) => RotateAt(angle, point, MatrixOrder.Prepend);

        public void RotateAt(float angle, PointF point, MatrixOrder order)
        {
            var around = new Matrix(1, 0, 0, 1, -point.X, -point.Y);
            around.Multiply(RotationOf(angle), MatrixOrder.Append);
            around.Multiply(new Matrix(1, 0, 0, 1, point.X, point.Y), MatrixOrder.Append);
            Multiply(around, order);
        }

        public void Shear(float shearX, float shearY) => Shear(shearX, shearY, MatrixOrder.Prepend);

        public void Shear(float shearX, float shearY, MatrixOrder order) => Multiply(new Matrix(1, shearY, shearX, 1, 0, 0), order);

        public void Invert()
        {
            double det = (double)m11 * m22 - (double)m12 * m21;
            if (det == 0 || double.IsNaN(det) || double.IsInfinity(det))
            {
                throw new ArgumentException("Parameter is not valid.");
            }
            double i11 = m22 / det;
            double i12 = -m12 / det;
            double i21 = -m21 / det;
            double i22 = m11 / det;
            double idx = -(dx * i11 + dy * i21);
            double idy = -(dx * i12 + dy * i22);
            m11 = (float)i11;
            m12 = (float)i12;
            m21 = (float)i21;
            m22 = (float)i22;
            dx = (float)idx;
            dy = (float)idy;
        }

        internal PointF Apply(float x, float y) =>
            new PointF((float)((double)x * m11 + (double)y * m21 + dx), (float)((double)x * m12 + (double)y * m22 + dy));

        public void TransformPoints(PointF[] pts)
        {
            if (pts == null)
            {
                throw new ArgumentNullException("pts");
            }
            for (int i = 0; i < pts.Length; i++)
            {
                pts[i] = Apply(pts[i].X, pts[i].Y);
            }
        }

        public void TransformPoints(Point[] pts)
        {
            if (pts == null)
            {
                throw new ArgumentNullException("pts");
            }
            for (int i = 0; i < pts.Length; i++)
            {
                pts[i] = Point.Round(Apply(pts[i].X, pts[i].Y));
            }
        }

        public void TransformVectors(PointF[] pts)
        {
            if (pts == null)
            {
                throw new ArgumentNullException("pts");
            }
            for (int i = 0; i < pts.Length; i++)
            {
                float x = pts[i].X;
                float y = pts[i].Y;
                pts[i] = new PointF(x * m11 + y * m21, x * m12 + y * m22);
            }
        }

        public void TransformVectors(Point[] pts)
        {
            if (pts == null)
            {
                throw new ArgumentNullException("pts");
            }
            for (int i = 0; i < pts.Length; i++)
            {
                float x = pts[i].X;
                float y = pts[i].Y;
                pts[i] = Point.Round(new PointF(x * m11 + y * m21, x * m12 + y * m22));
            }
        }

        public void VectorTransformPoints(Point[] pts) => TransformVectors(pts);

        public override bool Equals(object obj) =>
            obj is Matrix other && other.m11 == m11 && other.m12 == m12 && other.m21 == m21 && other.m22 == m22 && other.dx == dx && other.dy == dy;

        public override int GetHashCode() => m11.GetHashCode() ^ m12.GetHashCode() ^ m21.GetHashCode() ^ m22.GetHashCode() ^ dx.GetHashCode() ^ dy.GetHashCode();
    }

    public sealed class PathData
    {
        public PointF[] Points { get; set; }

        public byte[] Types { get; set; }
    }

    // Путь GDI+. Правила добавления сняты пробой под Windows:
    //  * `AddLine`, начатая в последней точке фигуры, её не повторяет;
    //  * эллипс, прямоугольник, многоугольник и сектор — всегда новая замкнутая
    //    фигура; эллипс — 13 точек от правого края по часовой;
    //  * дуга делится на кривые не больше чем по 90°, 200° — три кривые;
    //  * `AddCurve` — кардинальный сплайн с натяжением 0.5, у крайних точек
    //    соседом считается сама точка.
    public sealed class GraphicsPath : IDisposable, ICloneable
    {
        private PointF[] points;
        private byte[] types;
        private int count;
        private bool startNew = true;

        public GraphicsPath()
            : this(FillMode.Alternate)
        {
        }

        public GraphicsPath(FillMode fillMode)
        {
            FillMode = fillMode;
            points = new PointF[16];
            types = new byte[16];
        }

        public GraphicsPath(PointF[] pts, byte[] types)
            : this(pts, types, FillMode.Alternate)
        {
        }

        public GraphicsPath(PointF[] pts, byte[] types, FillMode fillMode)
            : this(fillMode)
        {
            if (pts == null)
            {
                throw new ArgumentNullException("pts");
            }
            if (types == null || types.Length != pts.Length)
            {
                throw new ArgumentException("Parameter is not valid.");
            }
            for (int i = 0; i < pts.Length; i++)
            {
                Append(pts[i], types[i]);
            }
            startNew = count == 0 || (types[count - 1] & (byte)PathPointType.CloseSubpath) != 0;
        }

        public FillMode FillMode { get; set; }

        public int PointCount => count;

        public PointF[] PathPoints
        {
            get
            {
                var copy = new PointF[count];
                Array.CopyItems(points, copy, count);
                return copy;
            }
        }

        public byte[] PathTypes
        {
            get
            {
                var copy = new byte[count];
                Array.CopyItems(types, copy, count);
                return copy;
            }
        }

        public PathData PathData => new PathData { Points = PathPoints, Types = PathTypes };

        public void Dispose()
        {
        }

        public object Clone()
        {
            var copy = new GraphicsPath(FillMode);
            for (int i = 0; i < count; i++)
            {
                copy.Append(points[i], types[i]);
            }
            copy.startNew = startNew;
            return copy;
        }

        public void Reset()
        {
            count = 0;
            startNew = true;
            FillMode = FillMode.Alternate;
        }

        private void Append(PointF point, byte type)
        {
            if (count == points.Length)
            {
                Array.Resize(ref points, count * 2);
                Array.Resize(ref types, count * 2);
            }
            points[count] = point;
            types[count] = type;
            count++;
        }

        // Первая точка очередного куска фигуры: начинает фигуру, если та
        // закончена, и не повторяет последнюю точку, если совпала с ней.
        private void AddFirst(float x, float y)
        {
            if (startNew)
            {
                Append(new PointF(x, y), (byte)PathPointType.Start);
                startNew = false;
                return;
            }
            if (count > 0 && points[count - 1].X == x && points[count - 1].Y == y)
            {
                return;
            }
            Append(new PointF(x, y), (byte)PathPointType.Line);
        }

        public void StartFigure()
        {
            startNew = true;
        }

        public void CloseFigure()
        {
            if (count > 0)
            {
                types[count - 1] |= (byte)PathPointType.CloseSubpath;
            }
            startNew = true;
        }

        public void CloseAllFigures()
        {
            for (int i = 1; i < count; i++)
            {
                if (types[i] == (byte)PathPointType.Start)
                {
                    types[i - 1] |= (byte)PathPointType.CloseSubpath;
                }
            }
            CloseFigure();
        }

        public PointF GetLastPoint()
        {
            if (count == 0)
            {
                throw new ArgumentException("Parameter is not valid.");
            }
            return points[count - 1];
        }

        public void AddLine(float x1, float y1, float x2, float y2)
        {
            AddFirst(x1, y1);
            Append(new PointF(x2, y2), (byte)PathPointType.Line);
        }

        public void AddLine(int x1, int y1, int x2, int y2) => AddLine((float)x1, y1, x2, y2);

        public void AddLine(PointF pt1, PointF pt2) => AddLine(pt1.X, pt1.Y, pt2.X, pt2.Y);

        public void AddLine(Point pt1, Point pt2) => AddLine((float)pt1.X, pt1.Y, pt2.X, pt2.Y);

        public void AddLines(PointF[] points)
        {
            if (points == null)
            {
                throw new ArgumentNullException("points");
            }
            if (points.Length == 0)
            {
                throw new ArgumentException("Parameter is not valid.");
            }
            AddFirst(points[0].X, points[0].Y);
            for (int i = 1; i < points.Length; i++)
            {
                Append(points[i], (byte)PathPointType.Line);
            }
        }

        public void AddLines(Point[] points) => AddLines(Floats(points));

        internal static PointF[] Floats(Point[] points)
        {
            if (points == null)
            {
                throw new ArgumentNullException("points");
            }
            var result = new PointF[points.Length];
            for (int i = 0; i < points.Length; i++)
            {
                result[i] = points[i];
            }
            return result;
        }

        public void AddBezier(float x1, float y1, float x2, float y2, float x3, float y3, float x4, float y4)
        {
            AddFirst(x1, y1);
            Append(new PointF(x2, y2), (byte)PathPointType.Bezier);
            Append(new PointF(x3, y3), (byte)PathPointType.Bezier);
            Append(new PointF(x4, y4), (byte)PathPointType.Bezier);
        }

        public void AddBezier(int x1, int y1, int x2, int y2, int x3, int y3, int x4, int y4) =>
            AddBezier((float)x1, y1, x2, y2, x3, y3, x4, y4);

        public void AddBezier(PointF pt1, PointF pt2, PointF pt3, PointF pt4) => AddBezier(pt1.X, pt1.Y, pt2.X, pt2.Y, pt3.X, pt3.Y, pt4.X, pt4.Y);

        public void AddBezier(Point pt1, Point pt2, Point pt3, Point pt4) =>
            AddBezier((float)pt1.X, pt1.Y, pt2.X, pt2.Y, pt3.X, pt3.Y, pt4.X, pt4.Y);

        public void AddBeziers(PointF[] points)
        {
            if (points == null)
            {
                throw new ArgumentNullException("points");
            }
            if (points.Length < 4 || (points.Length - 1) % 3 != 0)
            {
                throw new ArgumentException("Parameter is not valid.");
            }
            AddFirst(points[0].X, points[0].Y);
            for (int i = 1; i < points.Length; i++)
            {
                Append(points[i], (byte)PathPointType.Bezier);
            }
        }

        public void AddBeziers(params Point[] points) => AddBeziers(Floats(points));

        public void AddRectangle(RectangleF rect)
        {
            if (rect.Width <= 0 || rect.Height <= 0)
            {
                return;
            }
            StartFigure();
            AddFirst(rect.X, rect.Y);
            Append(new PointF(rect.X + rect.Width, rect.Y), (byte)PathPointType.Line);
            Append(new PointF(rect.X + rect.Width, rect.Y + rect.Height), (byte)PathPointType.Line);
            Append(new PointF(rect.X, rect.Y + rect.Height), (byte)PathPointType.Line);
            CloseFigure();
        }

        public void AddRectangle(Rectangle rect) => AddRectangle((RectangleF)rect);

        public void AddRectangles(RectangleF[] rects)
        {
            if (rects == null)
            {
                throw new ArgumentNullException("rects");
            }
            for (int i = 0; i < rects.Length; i++)
            {
                AddRectangle(rects[i]);
            }
        }

        public void AddRectangles(Rectangle[] rects)
        {
            if (rects == null)
            {
                throw new ArgumentNullException("rects");
            }
            for (int i = 0; i < rects.Length; i++)
            {
                AddRectangle(rects[i]);
            }
        }

        public void AddEllipse(float x, float y, float width, float height)
        {
            StartFigure();
            AddArcSegments(x, y, width, height, 0, 360);
            CloseFigure();
        }

        public void AddEllipse(RectangleF rect) => AddEllipse(rect.X, rect.Y, rect.Width, rect.Height);

        public void AddEllipse(Rectangle rect) => AddEllipse(rect.X, rect.Y, rect.Width, rect.Height);

        public void AddEllipse(int x, int y, int width, int height) => AddEllipse((float)x, y, width, height);

        public void AddArc(float x, float y, float width, float height, float startAngle, float sweepAngle) =>
            AddArcSegments(x, y, width, height, startAngle, sweepAngle);

        public void AddArc(RectangleF rect, float startAngle, float sweepAngle) => AddArc(rect.X, rect.Y, rect.Width, rect.Height, startAngle, sweepAngle);

        public void AddArc(Rectangle rect, float startAngle, float sweepAngle) => AddArc(rect.X, rect.Y, rect.Width, rect.Height, startAngle, sweepAngle);

        public void AddArc(int x, int y, int width, int height, float startAngle, float sweepAngle) =>
            AddArc((float)x, y, width, height, startAngle, sweepAngle);

        public void AddPie(float x, float y, float width, float height, float startAngle, float sweepAngle)
        {
            StartFigure();
            AddFirst(x + width / 2, y + height / 2);
            AddArcSegments(x, y, width, height, startAngle, sweepAngle);
            CloseFigure();
        }

        public void AddPie(Rectangle rect, float startAngle, float sweepAngle) => AddPie(rect.X, rect.Y, rect.Width, rect.Height, startAngle, sweepAngle);

        public void AddPie(int x, int y, int width, int height, float startAngle, float sweepAngle) =>
            AddPie((float)x, y, width, height, startAngle, sweepAngle);

        // Дуга эллипса кривыми Безье не длиннее 90° каждая. Угол у GDI+ —
        // настоящий угол луча из центра, а не параметр эллипса, поэтому у
        // сплюснутого эллипса он пересчитывается в параметр.
        private void AddArcSegments(float x, float y, float width, float height, float startAngle, float sweepAngle)
        {
            double rx = width / 2.0;
            double ry = height / 2.0;
            double cx = x + rx;
            double cy = y + ry;
            if (sweepAngle > 360)
            {
                sweepAngle = 360;
            }
            if (sweepAngle < -360)
            {
                sweepAngle = -360;
            }
            double start = startAngle;
            double remaining = sweepAngle;
            bool first = true;
            while (true)
            {
                double step = remaining > 90 ? 90 : remaining < -90 ? -90 : remaining;
                double t1 = Parameter(start, rx, ry);
                double t2 = Parameter(start + step, rx, ry);
                // Параметр обязан идти в ту же сторону, что и угол: у 360°
                // atan2 вернул бы тот же конец, что и у 0°.
                if (step > 0 && t2 <= t1)
                {
                    t2 += 2 * Math.PI;
                }
                if (step < 0 && t2 >= t1)
                {
                    t2 -= 2 * Math.PI;
                }
                double alpha = 4.0 / 3.0 * Math.Tan((t2 - t1) / 4);
                double x0 = cx + rx * Math.Cos(t1);
                double y0 = cy + ry * Math.Sin(t1);
                double x3 = cx + rx * Math.Cos(t2);
                double y3 = cy + ry * Math.Sin(t2);
                if (first)
                {
                    // Начало дуги, в отличие от `AddLine`, не сливается с концом
                    // фигуры даже при совпадении: две дуги подряд у GDI+ дают
                    // 4 + 10 точек с отрезком между ними (проба).
                    if (startNew)
                    {
                        AddFirst((float)x0, (float)y0);
                    }
                    else
                    {
                        Append(new PointF((float)x0, (float)y0), (byte)PathPointType.Line);
                    }
                    first = false;
                }
                Append(new PointF((float)(x0 - alpha * rx * Math.Sin(t1)), (float)(y0 + alpha * ry * Math.Cos(t1))), (byte)PathPointType.Bezier);
                Append(new PointF((float)(x3 + alpha * rx * Math.Sin(t2)), (float)(y3 - alpha * ry * Math.Cos(t2))), (byte)PathPointType.Bezier);
                Append(new PointF((float)x3, (float)y3), (byte)PathPointType.Bezier);
                start += step;
                remaining -= step;
                if (remaining == 0 || step == 0)
                {
                    break;
                }
            }
        }

        private static double Parameter(double angle, double rx, double ry)
        {
            double radians = angle * Math.PI / 180.0;
            if (rx == ry || rx == 0 || ry == 0)
            {
                return radians;
            }
            // Целые обороты сохраняются: atan2 знает только один.
            double turns = Math.Floor(radians / (2 * Math.PI));
            double within = radians - turns * 2 * Math.PI;
            double t = Math.Atan2(rx * Math.Sin(within), ry * Math.Cos(within));
            if (t < 0)
            {
                t += 2 * Math.PI;
            }
            return t + turns * 2 * Math.PI;
        }

        public void AddPolygon(PointF[] points)
        {
            if (points == null)
            {
                throw new ArgumentNullException("points");
            }
            if (points.Length < 3)
            {
                throw new ArgumentException("Parameter is not valid.");
            }
            StartFigure();
            AddFirst(points[0].X, points[0].Y);
            for (int i = 1; i < points.Length; i++)
            {
                Append(points[i], (byte)PathPointType.Line);
            }
            CloseFigure();
        }

        public void AddPolygon(Point[] points) => AddPolygon(Floats(points));

        public void AddCurve(PointF[] points) => AddCurve(points, 0.5f);

        public void AddCurve(PointF[] points, float tension)
        {
            if (points == null)
            {
                throw new ArgumentNullException("points");
            }
            if (points.Length < 2)
            {
                throw new ArgumentException("Parameter is not valid.");
            }
            AddFirst(points[0].X, points[0].Y);
            AddSpline(points, tension, false);
        }

        public void AddCurve(Point[] points) => AddCurve(Floats(points), 0.5f);

        public void AddCurve(Point[] points, float tension) => AddCurve(Floats(points), tension);

        public void AddClosedCurve(PointF[] points) => AddClosedCurve(points, 0.5f);

        public void AddClosedCurve(PointF[] points, float tension)
        {
            if (points == null)
            {
                throw new ArgumentNullException("points");
            }
            if (points.Length < 3)
            {
                throw new ArgumentException("Parameter is not valid.");
            }
            StartFigure();
            AddFirst(points[0].X, points[0].Y);
            AddSpline(points, tension, true);
            CloseFigure();
        }

        public void AddClosedCurve(Point[] points) => AddClosedCurve(Floats(points), 0.5f);

        // Кардинальный сплайн кривыми Безье: опоры отрезка i → i+1 —
        // `p[i] + (p[i+1] − p[i−1])·t/3` и `p[i+1] − (p[i+2] − p[i])·t/3`.
        private void AddSpline(PointF[] p, float tension, bool closed)
        {
            int n = p.Length;
            int segments = closed ? n : n - 1;
            // Последний знак опор у GDI+ плавает (1.9999999 вместо 2 у точек
            // (0,0)-(12,12)-(24,0)), и простой формулой он не повторяется:
            // совпадает всё до шестого знака.
            float k = tension / 3;
            for (int i = 0; i < segments; i++)
            {
                PointF prev = closed ? p[(i + n - 1) % n] : p[Math.Max(i - 1, 0)];
                PointF a = p[i];
                PointF b = p[(i + 1) % n];
                PointF next = closed ? p[(i + 2) % n] : p[Math.Min(i + 2, n - 1)];
                Append(new PointF(a.X + (b.X - prev.X) * k, a.Y + (b.Y - prev.Y) * k), (byte)PathPointType.Bezier);
                Append(new PointF(b.X - (next.X - a.X) * k, b.Y - (next.Y - a.Y) * k), (byte)PathPointType.Bezier);
                Append(b, (byte)PathPointType.Bezier);
            }
        }

        public void AddPath(GraphicsPath addingPath, bool connect)
        {
            if (addingPath == null)
            {
                throw new ArgumentNullException("addingPath");
            }
            for (int i = 0; i < addingPath.count; i++)
            {
                byte type = addingPath.types[i];
                if (i == 0 && connect && !startNew)
                {
                    type = (byte)PathPointType.Line;
                }
                else if (i == 0)
                {
                    type = (byte)PathPointType.Start;
                }
                Append(addingPath.points[i], type);
            }
            startNew = count == 0 || (types[count - 1] & (byte)PathPointType.CloseSubpath) != 0;
        }

        public RectangleF GetBounds() => GetBounds(null);

        public RectangleF GetBounds(Matrix matrix)
        {
            if (count == 0)
            {
                return RectangleF.Empty;
            }
            float minX = float.MaxValue;
            float minY = float.MaxValue;
            float maxX = float.MinValue;
            float maxY = float.MinValue;
            for (int i = 0; i < count; i++)
            {
                PointF p = matrix == null ? points[i] : matrix.Apply(points[i].X, points[i].Y);
                minX = Math.Min(minX, p.X);
                minY = Math.Min(minY, p.Y);
                maxX = Math.Max(maxX, p.X);
                maxY = Math.Max(maxY, p.Y);
            }
            return new RectangleF(minX, minY, maxX - minX, maxY - minY);
        }

        public void Transform(Matrix matrix)
        {
            if (matrix == null)
            {
                throw new ArgumentNullException("matrix");
            }
            for (int i = 0; i < count; i++)
            {
                points[i] = matrix.Apply(points[i].X, points[i].Y);
            }
        }

        public void Reverse()
        {
            for (int i = 0, j = count - 1; i < j; i++, j--)
            {
                PointF point = points[i];
                points[i] = points[j];
                points[j] = point;
            }
        }

        public bool IsVisible(float x, float y) => IsVisible(x, y, null);

        public bool IsVisible(float x, float y, Graphics graphics) => GdiNative.RegionContains(Program(), Points(), Types(), x, y);

        public bool IsVisible(PointF point) => IsVisible(point.X, point.Y);

        public bool IsVisible(int x, int y) => IsVisible((float)x, y);

        public bool IsVisible(Point point) => IsVisible((float)point.X, point.Y);

        // Для растеризатора: точки парами и виды — ровно `PointCount` штук.
        internal float[] Points()
        {
            var flat = new float[count * 2];
            for (int i = 0; i < count; i++)
            {
                flat[i * 2] = points[i].X;
                flat[i * 2 + 1] = points[i].Y;
            }
            return flat;
        }

        internal byte[] Types()
        {
            var copy = new byte[count];
            Array.CopyItems(types, copy, count);
            return copy;
        }

        private int[] Program() => new int[] { 2, 0, 0, count, (int)FillMode };
    }
}
