// Graphics (фаза N9): рисование как в GDI+ поверх окна формы и поверх Bitmap.
//
// Состояние Graphics — преобразование мира, отсечение и режимы — живёт здесь, а
// каждая фигура уходит в растеризатор одним вызовом `GdiNative` с путём,
// кистью и целью (см. `crates/clr-vm/src/gdi.rs`). Отсечение хранится в
// координатах устройства, как у GDI+: `SetClip`, а потом `TranslateTransform`
// не сдвигает уже заданную область.
//
// Быстрые пути — ради элементов WinForms, которые перерисовывают сотни
// прямоугольников и линий в точку за кадр: сплошной прямоугольник без
// поворота и тонкая линия вдоль оси рисуются заливкой прямоугольника в
// целых точках, без путей и без массивов на каждый вызов.

using System.Drawing.Drawing2D;
using System.Drawing.Imaging;
using System.Drawing.Text;
using System.Runtime.CompilerServices;
using System.Windows.Forms;

namespace System.Drawing.Text
{
    public enum TextRenderingHint
    {
        SystemDefault = 0,
        SingleBitPerPixelGridFit = 1,
        SingleBitPerPixel = 2,
        AntiAliasGridFit = 3,
        AntiAlias = 4,
        ClearTypeGridFit = 5,
    }
}

namespace System.Drawing
{
    internal static class GdiNative
    {
        [MethodImpl(MethodImplOptions.InternalCall)]
        internal static extern void FillPath(int[] target, int[] image, int[] clipOps, float[] clipPoints, byte[] clipTypes, float[] points, byte[] types,
            float[] matrix, int fillMode, int[] paintInts, float[] paintFloats, int[] texture);

        [MethodImpl(MethodImplOptions.InternalCall)]
        internal static extern void StrokePath(int[] target, int[] image, int[] clipOps, float[] clipPoints, byte[] clipTypes, float[] points, byte[] types,
            float[] matrix, float[] pen, int[] penInts, int[] paintInts, float[] paintFloats, int[] texture);

        [MethodImpl(MethodImplOptions.InternalCall)]
        internal static extern void FillRect(int[] target, int[] image, int x, int y, int width, int height, int argb);

        [MethodImpl(MethodImplOptions.InternalCall)]
        internal static extern void DrawImage(int[] target, int[] image, int[] clipOps, float[] clipPoints, byte[] clipTypes, int[] source, int sourceWidth,
            int sourceHeight, float[] part, float[] toDevice, int interpolation, int opacity);

        [MethodImpl(MethodImplOptions.InternalCall)]
        internal static extern bool RegionContains(int[] ops, float[] points, byte[] types, float x, float y);

        [MethodImpl(MethodImplOptions.InternalCall)]
        internal static extern void DrawText(int[] target, int[] image, int[] clipOps, float[] clipPoints, byte[] clipTypes, string text, float[] place,
            int[] paintInts, float[] paintFloats, int[] texture);

        [MethodImpl(MethodImplOptions.InternalCall)]
        internal static extern int RegionBounds(int[] ops, float[] points, byte[] types, float[] bounds);
    }

    public sealed class Graphics : IDisposable
    {
        private const int FlagAntialias = 1;
        private const int FlagHalfPixel = 2;
        private const int FlagSourceCopy = 4;
        private const int FlagOpaque = 8;

        private static readonly byte[] RectangleTypes = { 0, 1, 1, 0x81 };
        private static readonly byte[] LineTypes = { 0, 1 };

        private readonly int window = -1;
        private readonly Image image;
        private readonly int originX;
        private readonly int originY;
        private readonly Rectangle surface;
        private Matrix transform = new Matrix();
        // Отсечение в точках устройства; `null` — бесконечное.
        private Region clip;
        private SmoothingMode smoothing = SmoothingMode.None;
        private PixelOffsetMode pixelOffset = PixelOffsetMode.Default;
        private CompositingMode compositing = CompositingMode.SourceOver;

        // Цель и матрица устройства пересобираются только после смены
        // состояния: форма рисует ими сотни раз за кадр.
        private int[] target;
        private int[] clipOps;
        private float[] clipPoints;
        private byte[] clipTypes;
        private Rectangle clipRect;
        private float[] device;
        private Matrix deviceMatrix;

        // Окно формы: координаты от угла элемента, рисовать можно в `clip` —
        // его видимой части в точках окна.
        internal Graphics(int window, int originX, int originY, Rectangle clip)
        {
            this.window = window;
            this.originX = originX;
            this.originY = originY;
            surface = clip;
        }

        private Graphics(Image image)
        {
            this.image = image;
            surface = new Rectangle(0, 0, image.Width, image.Height);
        }

        public static Graphics FromImage(Image image)
        {
            if (image == null)
            {
                throw new ArgumentNullException("image");
            }
            return new Graphics(image);
        }

        public void Dispose()
        {
        }

        public void Flush()
        {
        }

        public SmoothingMode SmoothingMode
        {
            get => smoothing;
            set
            {
                if (value == SmoothingMode.Invalid)
                {
                    throw new ArgumentException("Parameter is not valid.");
                }
                smoothing = value;
                target = null;
            }
        }

        public PixelOffsetMode PixelOffsetMode
        {
            get => pixelOffset;
            set
            {
                pixelOffset = value;
                target = null;
            }
        }

        public CompositingMode CompositingMode
        {
            get => compositing;
            set
            {
                compositing = value;
                target = null;
            }
        }

        public CompositingQuality CompositingQuality { get; set; }

        public InterpolationMode InterpolationMode { get; set; } = InterpolationMode.Bilinear;

        public TextRenderingHint TextRenderingHint { get; set; }

        public int TextContrast { get; set; } = 4;

        public GraphicsUnit PageUnit { get; set; } = GraphicsUnit.Display;

        public float PageScale { get; set; } = 1;

        public float DpiX => 96;

        public float DpiY => 96;

        public Point RenderingOrigin { get; set; }

        private bool Antialias => smoothing == SmoothingMode.AntiAlias || smoothing == SmoothingMode.HighQuality;

        private bool HalfPixel => pixelOffset == PixelOffsetMode.Half || pixelOffset == PixelOffsetMode.HighQuality;

        // ---- Преобразование -------------------------------------------------

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
                device = null;
            }
        }

        public void ResetTransform()
        {
            transform.Reset();
            device = null;
        }

        public void MultiplyTransform(Matrix matrix) => MultiplyTransform(matrix, MatrixOrder.Prepend);

        public void MultiplyTransform(Matrix matrix, MatrixOrder order)
        {
            transform.Multiply(matrix, order);
            device = null;
        }

        public void TranslateTransform(float dx, float dy) => TranslateTransform(dx, dy, MatrixOrder.Prepend);

        public void TranslateTransform(float dx, float dy, MatrixOrder order)
        {
            transform.Translate(dx, dy, order);
            device = null;
        }

        public void ScaleTransform(float sx, float sy) => ScaleTransform(sx, sy, MatrixOrder.Prepend);

        public void ScaleTransform(float sx, float sy, MatrixOrder order)
        {
            transform.Scale(sx, sy, order);
            device = null;
        }

        public void RotateTransform(float angle) => RotateTransform(angle, MatrixOrder.Prepend);

        public void RotateTransform(float angle, MatrixOrder order)
        {
            transform.Rotate(angle, order);
            device = null;
        }

        // Из координат программы в точки устройства: мир, потом угол элемента.
        private Matrix Device()
        {
            if (device == null)
            {
                deviceMatrix = transform.Clone();
                deviceMatrix.Translate(originX, originY, MatrixOrder.Append);
                device = deviceMatrix.Elements;
            }
            return deviceMatrix;
        }

        private float[] DeviceElements()
        {
            Device();
            return device;
        }

        private bool TranslationOnly(out float dx, out float dy)
        {
            Matrix m = Device();
            dx = m.dx;
            dy = m.dy;
            return m.m11 == 1 && m.m12 == 0 && m.m21 == 0 && m.m22 == 1;
        }

        public GraphicsState Save()
        {
            var state = new GraphicsState();
            state.Transform = transform.Clone();
            state.Clip = clip == null ? null : clip.Clone();
            state.Smoothing = smoothing;
            state.PixelOffset = pixelOffset;
            state.Compositing = compositing;
            state.Quality = CompositingQuality;
            state.Interpolation = InterpolationMode;
            state.TextHint = TextRenderingHint;
            return state;
        }

        public void Restore(GraphicsState gstate)
        {
            if (gstate == null)
            {
                return;
            }
            transform = gstate.Transform.Clone();
            clip = gstate.Clip == null ? null : gstate.Clip.Clone();
            smoothing = gstate.Smoothing;
            pixelOffset = gstate.PixelOffset;
            compositing = gstate.Compositing;
            CompositingQuality = gstate.Quality;
            InterpolationMode = gstate.Interpolation;
            TextRenderingHint = gstate.TextHint;
            device = null;
            target = null;
        }

        // Контейнер — сохранение состояния; вложенного пространства координат,
        // которое у GDI+ даёт `BeginContainer(dst, src, unit)`, здесь нет.
        public GraphicsContainer BeginContainer() => new GraphicsContainer(Save());

        public void EndContainer(GraphicsContainer container)
        {
            if (container == null)
            {
                throw new ArgumentNullException("container");
            }
            Restore(container.State);
        }

        // ---- Отсечение ------------------------------------------------------

        public Region Clip
        {
            get
            {
                if (clip == null)
                {
                    return new Region();
                }
                Region world = clip.Clone();
                Matrix inverse = Device().Clone();
                if (inverse.IsInvertible)
                {
                    inverse.Invert();
                    world.Transform(inverse);
                }
                return world;
            }
            set => SetClip(value, CombineMode.Replace);
        }

        public void ResetClip()
        {
            clip = null;
            target = null;
        }

        public void SetClip(Graphics g) => SetClip(g, CombineMode.Replace);

        public void SetClip(Graphics g, CombineMode combineMode)
        {
            if (g == null)
            {
                throw new ArgumentNullException("g");
            }
            CombineDevice(g.clip == null ? new Region() : g.clip.Clone(), combineMode);
        }

        public void SetClip(Rectangle rect) => SetClip((RectangleF)rect, CombineMode.Replace);

        public void SetClip(Rectangle rect, CombineMode combineMode) => SetClip((RectangleF)rect, combineMode);

        public void SetClip(RectangleF rect) => SetClip(rect, CombineMode.Replace);

        public void SetClip(RectangleF rect, CombineMode combineMode)
        {
            var region = new Region(rect);
            region.Transform(Device());
            CombineDevice(region, combineMode);
        }

        public void SetClip(GraphicsPath path) => SetClip(path, CombineMode.Replace);

        public void SetClip(GraphicsPath path, CombineMode combineMode)
        {
            if (path == null)
            {
                throw new ArgumentNullException("path");
            }
            var region = new Region(path);
            region.Transform(Device());
            CombineDevice(region, combineMode);
        }

        public void SetClip(Region region, CombineMode combineMode)
        {
            if (region == null)
            {
                throw new ArgumentNullException("region");
            }
            Region copy = region.Clone();
            copy.Transform(Device());
            CombineDevice(copy, combineMode);
        }

        public void IntersectClip(Rectangle rect) => SetClip((RectangleF)rect, CombineMode.Intersect);

        public void IntersectClip(RectangleF rect) => SetClip(rect, CombineMode.Intersect);

        public void IntersectClip(Region region) => SetClip(region, CombineMode.Intersect);

        public void ExcludeClip(Rectangle rect) => SetClip((RectangleF)rect, CombineMode.Exclude);

        public void ExcludeClip(Region region) => SetClip(region, CombineMode.Exclude);

        public void TranslateClip(float dx, float dy)
        {
            if (clip == null)
            {
                return;
            }
            Matrix m = Device();
            clip.Translate(dx * m.m11 + dy * m.m21, dx * m.m12 + dy * m.m22);
            target = null;
        }

        public void TranslateClip(int dx, int dy) => TranslateClip((float)dx, dy);

        private void CombineDevice(Region operand, CombineMode mode)
        {
            if (mode == CombineMode.Replace)
            {
                clip = operand;
            }
            else
            {
                Region current = clip == null ? new Region() : clip;
                current.Combine(operand, mode);
                clip = current;
            }
            target = null;
        }

        public RectangleF ClipBounds
        {
            get
            {
                if (clip == null)
                {
                    return new RectangleF(-4194304, -4194304, 8388608, 8388608);
                }
                return ToWorld(clip.GetBounds(this));
            }
        }

        public RectangleF VisibleClipBounds
        {
            get
            {
                RectangleF visible = surface;
                if (clip != null)
                {
                    visible = RectangleF.Intersect(visible, clip.GetBounds(this));
                }
                return ToWorld(visible);
            }
        }

        public bool IsClipEmpty => clip != null && clip.IsEmpty(this);

        public bool IsVisibleClipEmpty => VisibleClipBounds.IsEmpty;

        // Прямоугольник устройства в координатах программы — границы четырёх
        // углов после обратного преобразования.
        private RectangleF ToWorld(RectangleF rect)
        {
            Matrix inverse = Device().Clone();
            if (!inverse.IsInvertible)
            {
                return RectangleF.Empty;
            }
            inverse.Invert();
            var corners = new PointF[] { new PointF(rect.X, rect.Y), new PointF(rect.Right, rect.Y), new PointF(rect.X, rect.Bottom), new PointF(rect.Right, rect.Bottom) };
            inverse.TransformPoints(corners);
            float minX = corners[0].X;
            float minY = corners[0].Y;
            float maxX = corners[0].X;
            float maxY = corners[0].Y;
            for (int i = 1; i < 4; i++)
            {
                minX = Math.Min(minX, corners[i].X);
                minY = Math.Min(minY, corners[i].Y);
                maxX = Math.Max(maxX, corners[i].X);
                maxY = Math.Max(maxY, corners[i].Y);
            }
            return new RectangleF(minX, minY, maxX - minX, maxY - minY);
        }

        public bool IsVisible(float x, float y)
        {
            PointF p = Device().Apply(x, y);
            if (!new RectangleF(surface.X, surface.Y, surface.Width, surface.Height).Contains(p))
            {
                return false;
            }
            return clip == null || clip.IsVisible(p.X, p.Y);
        }

        public bool IsVisible(int x, int y) => IsVisible((float)x, y);

        public bool IsVisible(PointF point) => IsVisible(point.X, point.Y);

        public bool IsVisible(Point point) => IsVisible((float)point.X, point.Y);

        public bool IsVisible(RectangleF rect)
        {
            RectangleF visible = VisibleClipBounds;
            return rect.IntersectsWith(visible);
        }

        public bool IsVisible(Rectangle rect) => IsVisible((RectangleF)rect);

        // ---- Цель растеризатора --------------------------------------------

        private int[] Target()
        {
            if (target != null)
            {
                return target;
            }
            clipOps = null;
            clipPoints = null;
            clipTypes = null;
            Rectangle area = surface;
            if (clip != null)
            {
                if (clip.TryGetRectangle(out RectangleF rect, out bool infinite))
                {
                    if (!infinite)
                    {
                        area = Rectangle.Intersect(area, PixelRectangle(rect));
                    }
                }
                else
                {
                    clipOps = clip.Program(out clipPoints, out clipTypes);
                }
            }
            if (area.Width <= 0 || area.Height <= 0)
            {
                area = Rectangle.Empty;
            }
            clipRect = area;
            int flags = (Antialias ? FlagAntialias : 0) | (HalfPixel ? FlagHalfPixel : 0) | (compositing == CompositingMode.SourceCopy ? FlagSourceCopy : 0);
            if (image != null && !image.HasAlpha)
            {
                flags |= FlagOpaque;
            }
            target = new int[] { window, image == null ? 0 : image.Width, image == null ? 0 : image.Height, area.X, area.Y, area.Width, area.Height, flags };
            return target;
        }

        // Точки, чей центр внутри прямоугольника устройства, — по тому же
        // правилу, что у растеризатора (левый и верхний край входят).
        private Rectangle PixelRectangle(RectangleF rect)
        {
            double shift = HalfPixel ? 0.0 : 0.5;
            int x0 = Clamp(Math.Ceiling(rect.X - 0.5 + shift));
            int y0 = Clamp(Math.Ceiling(rect.Y - 0.5 + shift));
            int x1 = Clamp(Math.Ceiling(rect.X + rect.Width - 0.5 + shift));
            int y1 = Clamp(Math.Ceiling(rect.Y + rect.Height - 0.5 + shift));
            return new Rectangle(x0, y0, Math.Max(0, x1 - x0), Math.Max(0, y1 - y0));
        }

        private static int Clamp(double value) => value < -16777216 ? -16777216 : value > 16777216 ? 16777216 : (int)value;

        private int[] Pixels => image == null ? null : image.pixels;

        private void FillPoints(Brush brush, float[] points, byte[] types, FillMode mode)
        {
            if (brush == null)
            {
                throw new ArgumentNullException("brush");
            }
            int[] t = Target();
            if (clipRect.Width == 0)
            {
                return;
            }
            brush.Pack(Device(), out int[] paintInts, out float[] paintFloats, out int[] texture);
            GdiNative.FillPath(t, Pixels, clipOps, clipPoints, clipTypes, points, types, DeviceElements(), (int)mode, paintInts, paintFloats, texture);
        }

        private void StrokePoints(Pen pen, float[] points, byte[] types)
        {
            if (pen == null)
            {
                throw new ArgumentNullException("pen");
            }
            int[] t = Target();
            if (clipRect.Width == 0)
            {
                return;
            }
            pen.PaintBrush.Pack(Device(), out int[] paintInts, out float[] paintFloats, out int[] texture);
            GdiNative.StrokePath(t, Pixels, clipOps, clipPoints, clipTypes, points, types, DeviceElements(), pen.PackFloats(), pen.PackInts(), paintInts, paintFloats, texture);
        }

        // Сплошной прямоугольник в целых точках устройства — без пути.
        private bool TryFillPixels(Color color, float x, float y, float width, float height)
        {
            int[] t = Target();
            if (clipOps != null || !TranslationOnly(out float dx, out float dy))
            {
                return false;
            }
            float left = x + dx;
            float top = y + dy;
            float right = left + width;
            float bottom = top + height;
            if (Antialias && !(HalfPixel && IsWhole(left) && IsWhole(top) && IsWhole(right) && IsWhole(bottom)))
            {
                return false;
            }
            if (width <= 0 || height <= 0)
            {
                return true;
            }
            Rectangle pixels = PixelRectangle(new RectangleF(left, top, width, height));
            if (pixels.Width > 0 && pixels.Height > 0 && clipRect.Width > 0)
            {
                GdiNative.FillRect(t, Pixels, pixels.X, pixels.Y, pixels.Width, pixels.Height, color.ToArgb());
            }
            return true;
        }

        private static bool IsWhole(float value) => value == (int)value;

        private static float[] RectanglePoints(float x, float y, float width, float height) =>
            new float[] { x, y, x + width, y, x + width, y + height, x, y + height };

        private static float[] Flatten(PointF[] points)
        {
            var flat = new float[points.Length * 2];
            for (int i = 0; i < points.Length; i++)
            {
                flat[i * 2] = points[i].X;
                flat[i * 2 + 1] = points[i].Y;
            }
            return flat;
        }

        private static byte[] OpenTypes(int count)
        {
            var types = new byte[count];
            for (int i = 1; i < count; i++)
            {
                types[i] = 1;
            }
            return types;
        }

        private static byte[] ClosedTypes(int count)
        {
            byte[] types = OpenTypes(count);
            if (count > 0)
            {
                types[count - 1] |= 0x80;
            }
            return types;
        }

        // ---- Заливка --------------------------------------------------------

        public void Clear(Color color)
        {
            int[] t = Target();
            if (clipRect.Width == 0)
            {
                return;
            }
            // Clear пишет цвет как есть, без смешивания и без преобразования:
            // `Clear(Color.Transparent)` делает картинку прозрачной.
            int flags = t[7];
            t[7] = flags | FlagSourceCopy;
            try
            {
                if (clipOps == null)
                {
                    GdiNative.FillRect(t, Pixels, clipRect.X, clipRect.Y, clipRect.Width, clipRect.Height, color.ToArgb());
                }
                else
                {
                    var identity = new float[] { 1, 0, 0, 1, 0, 0 };
                    GdiNative.FillPath(t, Pixels, clipOps, clipPoints, clipTypes, RectanglePoints(clipRect.X, clipRect.Y, clipRect.Width, clipRect.Height), RectangleTypes,
                        identity, 0, new int[] { 0, 0, color.ToArgb() }, null, null);
                }
            }
            finally
            {
                t[7] = flags;
            }
        }

        public void FillRectangle(Brush brush, float x, float y, float width, float height)
        {
            if (brush == null)
            {
                throw new ArgumentNullException("brush");
            }
            if (brush.IsSolid(out Color color) && TryFillPixels(color, x, y, width, height))
            {
                return;
            }
            if (width <= 0 || height <= 0)
            {
                return;
            }
            FillPoints(brush, RectanglePoints(x, y, width, height), RectangleTypes, FillMode.Alternate);
        }

        public void FillRectangle(Brush brush, RectangleF rect) => FillRectangle(brush, rect.X, rect.Y, rect.Width, rect.Height);

        public void FillRectangle(Brush brush, Rectangle rect) => FillRectangle(brush, rect.X, rect.Y, rect.Width, rect.Height);

        public void FillRectangle(Brush brush, int x, int y, int width, int height) => FillRectangle(brush, (float)x, y, width, height);

        public void FillRectangles(Brush brush, RectangleF[] rects)
        {
            if (rects == null)
            {
                throw new ArgumentNullException("rects");
            }
            for (int i = 0; i < rects.Length; i++)
            {
                FillRectangle(brush, rects[i]);
            }
        }

        public void FillRectangles(Brush brush, Rectangle[] rects)
        {
            if (rects == null)
            {
                throw new ArgumentNullException("rects");
            }
            for (int i = 0; i < rects.Length; i++)
            {
                FillRectangle(brush, rects[i]);
            }
        }

        public void FillPolygon(Brush brush, PointF[] points) => FillPolygon(brush, points, FillMode.Alternate);

        public void FillPolygon(Brush brush, PointF[] points, FillMode fillMode)
        {
            if (points == null)
            {
                throw new ArgumentNullException("points");
            }
            FillPoints(brush, Flatten(points), ClosedTypes(points.Length), fillMode);
        }

        public void FillPolygon(Brush brush, Point[] points) => FillPolygon(brush, GraphicsPath.Floats(points), FillMode.Alternate);

        public void FillPolygon(Brush brush, Point[] points, FillMode fillMode) => FillPolygon(brush, GraphicsPath.Floats(points), fillMode);

        public void FillEllipse(Brush brush, float x, float y, float width, float height)
        {
            var path = new GraphicsPath();
            path.AddEllipse(x, y, width, height);
            FillPath(brush, path);
        }

        public void FillEllipse(Brush brush, RectangleF rect) => FillEllipse(brush, rect.X, rect.Y, rect.Width, rect.Height);

        public void FillEllipse(Brush brush, Rectangle rect) => FillEllipse(brush, rect.X, rect.Y, rect.Width, rect.Height);

        public void FillEllipse(Brush brush, int x, int y, int width, int height) => FillEllipse(brush, (float)x, y, width, height);

        public void FillPie(Brush brush, float x, float y, float width, float height, float startAngle, float sweepAngle)
        {
            var path = new GraphicsPath();
            path.AddPie(x, y, width, height, startAngle, sweepAngle);
            FillPath(brush, path);
        }

        public void FillPie(Brush brush, Rectangle rect, float startAngle, float sweepAngle) =>
            FillPie(brush, rect.X, rect.Y, rect.Width, rect.Height, startAngle, sweepAngle);

        public void FillPie(Brush brush, RectangleF rect, float startAngle, float sweepAngle) =>
            FillPie(brush, rect.X, rect.Y, rect.Width, rect.Height, startAngle, sweepAngle);

        public void FillPie(Brush brush, int x, int y, int width, int height, int startAngle, int sweepAngle) =>
            FillPie(brush, (float)x, y, width, height, startAngle, sweepAngle);

        public void FillClosedCurve(Brush brush, PointF[] points) => FillClosedCurve(brush, points, FillMode.Alternate, 0.5f);

        public void FillClosedCurve(Brush brush, PointF[] points, FillMode fillmode) => FillClosedCurve(brush, points, fillmode, 0.5f);

        public void FillClosedCurve(Brush brush, PointF[] points, FillMode fillmode, float tension)
        {
            var path = new GraphicsPath(fillmode);
            path.AddClosedCurve(points, tension);
            FillPath(brush, path);
        }

        public void FillClosedCurve(Brush brush, Point[] points) => FillClosedCurve(brush, GraphicsPath.Floats(points), FillMode.Alternate, 0.5f);

        public void FillPath(Brush brush, GraphicsPath path)
        {
            if (path == null)
            {
                throw new ArgumentNullException("path");
            }
            FillPoints(brush, path.Points(), path.Types(), path.FillMode);
        }

        // Область — отсечением: заливается её рамка, а лишнее срезает маска.
        public void FillRegion(Brush brush, Region region)
        {
            if (brush == null)
            {
                throw new ArgumentNullException("brush");
            }
            if (region == null)
            {
                throw new ArgumentNullException("region");
            }
            Region saved = clip == null ? null : clip.Clone();
            SetClip(region, CombineMode.Intersect);
            RectangleF bounds = region.GetBounds(this);
            RectangleF visible = VisibleClipBounds;
            RectangleF area = RectangleF.Intersect(bounds, visible);
            if (area.Width > 0 && area.Height > 0)
            {
                FillPoints(brush, RectanglePoints(area.X - 1, area.Y - 1, area.Width + 2, area.Height + 2), RectangleTypes, FillMode.Alternate);
            }
            clip = saved;
            target = null;
        }

        // ---- Обводка --------------------------------------------------------

        public void DrawLine(Pen pen, float x1, float y1, float x2, float y2)
        {
            if (pen == null)
            {
                throw new ArgumentNullException("pen");
            }
            // Тонкая линия вдоль оси в целых точках — прямоугольник в одну точку;
            // оба конца входят, как у косметического пера GDI+.
            if (!Antialias && !HalfPixel && pen.IsSolidThin(out Color color) && color.A == 255 && (x1 == x2 || y1 == y2)
                && TranslationOnly(out float dx, out float dy))
            {
                Target();
                float ax = x1 + dx;
                float ay = y1 + dy;
                float bx = x2 + dx;
                float by = y2 + dy;
                if (clipOps == null && IsWhole(ax) && IsWhole(ay) && IsWhole(bx) && IsWhole(by))
                {
                    int left = (int)Math.Min(ax, bx);
                    int top = (int)Math.Min(ay, by);
                    if (clipRect.Width > 0)
                    {
                        GdiNative.FillRect(target, Pixels, left, top, (int)Math.Abs(bx - ax) + 1, (int)Math.Abs(by - ay) + 1, color.ToArgb());
                    }
                    return;
                }
            }
            StrokePoints(pen, new float[] { x1, y1, x2, y2 }, LineTypes);
        }

        public void DrawLine(Pen pen, int x1, int y1, int x2, int y2) => DrawLine(pen, (float)x1, y1, x2, y2);

        public void DrawLine(Pen pen, PointF pt1, PointF pt2) => DrawLine(pen, pt1.X, pt1.Y, pt2.X, pt2.Y);

        public void DrawLine(Pen pen, Point pt1, Point pt2) => DrawLine(pen, (float)pt1.X, pt1.Y, pt2.X, pt2.Y);

        public void DrawLines(Pen pen, PointF[] points)
        {
            if (points == null)
            {
                throw new ArgumentNullException("points");
            }
            if (points.Length < 2)
            {
                throw new ArgumentException("Parameter is not valid.");
            }
            StrokePoints(pen, Flatten(points), OpenTypes(points.Length));
        }

        public void DrawLines(Pen pen, Point[] points) => DrawLines(pen, GraphicsPath.Floats(points));

        public void DrawRectangle(Pen pen, float x, float y, float width, float height)
        {
            if (pen == null)
            {
                throw new ArgumentNullException("pen");
            }
            if (!Antialias && !HalfPixel && pen.IsSolidThin(out Color color) && color.A == 255 && width > 0 && height > 0
                && TranslationOnly(out float dx, out float dy))
            {
                Target();
                float left = x + dx;
                float top = y + dy;
                if (clipOps == null && IsWhole(left) && IsWhole(top) && IsWhole(width) && IsWhole(height))
                {
                    if (clipRect.Width > 0)
                    {
                        int l = (int)left;
                        int t = (int)top;
                        int w = (int)width;
                        int h = (int)height;
                        int argb = color.ToArgb();
                        GdiNative.FillRect(target, Pixels, l, t, w + 1, 1, argb);
                        GdiNative.FillRect(target, Pixels, l, t + h, w + 1, 1, argb);
                        GdiNative.FillRect(target, Pixels, l, t + 1, 1, h - 1, argb);
                        GdiNative.FillRect(target, Pixels, l + w, t + 1, 1, h - 1, argb);
                    }
                    return;
                }
            }
            StrokePoints(pen, RectanglePoints(x, y, width, height), RectangleTypes);
        }

        public void DrawRectangle(Pen pen, Rectangle rect) => DrawRectangle(pen, rect.X, rect.Y, rect.Width, rect.Height);

        public void DrawRectangle(Pen pen, RectangleF rect) => DrawRectangle(pen, rect.X, rect.Y, rect.Width, rect.Height);

        public void DrawRectangle(Pen pen, int x, int y, int width, int height) => DrawRectangle(pen, (float)x, y, width, height);

        public void DrawRectangles(Pen pen, RectangleF[] rects)
        {
            if (rects == null)
            {
                throw new ArgumentNullException("rects");
            }
            for (int i = 0; i < rects.Length; i++)
            {
                DrawRectangle(pen, rects[i]);
            }
        }

        public void DrawRectangles(Pen pen, Rectangle[] rects)
        {
            if (rects == null)
            {
                throw new ArgumentNullException("rects");
            }
            for (int i = 0; i < rects.Length; i++)
            {
                DrawRectangle(pen, rects[i]);
            }
        }

        public void DrawPolygon(Pen pen, PointF[] points)
        {
            if (points == null)
            {
                throw new ArgumentNullException("points");
            }
            StrokePoints(pen, Flatten(points), ClosedTypes(points.Length));
        }

        public void DrawPolygon(Pen pen, Point[] points) => DrawPolygon(pen, GraphicsPath.Floats(points));

        public void DrawEllipse(Pen pen, float x, float y, float width, float height)
        {
            var path = new GraphicsPath();
            path.AddEllipse(x, y, width, height);
            DrawPath(pen, path);
        }

        public void DrawEllipse(Pen pen, RectangleF rect) => DrawEllipse(pen, rect.X, rect.Y, rect.Width, rect.Height);

        public void DrawEllipse(Pen pen, Rectangle rect) => DrawEllipse(pen, rect.X, rect.Y, rect.Width, rect.Height);

        public void DrawEllipse(Pen pen, int x, int y, int width, int height) => DrawEllipse(pen, (float)x, y, width, height);

        public void DrawArc(Pen pen, float x, float y, float width, float height, float startAngle, float sweepAngle)
        {
            var path = new GraphicsPath();
            path.AddArc(x, y, width, height, startAngle, sweepAngle);
            DrawPath(pen, path);
        }

        public void DrawArc(Pen pen, RectangleF rect, float startAngle, float sweepAngle) =>
            DrawArc(pen, rect.X, rect.Y, rect.Width, rect.Height, startAngle, sweepAngle);

        public void DrawArc(Pen pen, Rectangle rect, float startAngle, float sweepAngle) =>
            DrawArc(pen, rect.X, rect.Y, rect.Width, rect.Height, startAngle, sweepAngle);

        public void DrawArc(Pen pen, int x, int y, int width, int height, int startAngle, int sweepAngle) =>
            DrawArc(pen, (float)x, y, width, height, startAngle, sweepAngle);

        public void DrawPie(Pen pen, float x, float y, float width, float height, float startAngle, float sweepAngle)
        {
            var path = new GraphicsPath();
            path.AddPie(x, y, width, height, startAngle, sweepAngle);
            DrawPath(pen, path);
        }

        public void DrawPie(Pen pen, RectangleF rect, float startAngle, float sweepAngle) =>
            DrawPie(pen, rect.X, rect.Y, rect.Width, rect.Height, startAngle, sweepAngle);

        public void DrawPie(Pen pen, Rectangle rect, float startAngle, float sweepAngle) =>
            DrawPie(pen, rect.X, rect.Y, rect.Width, rect.Height, startAngle, sweepAngle);

        public void DrawPie(Pen pen, int x, int y, int width, int height, int startAngle, int sweepAngle) =>
            DrawPie(pen, (float)x, y, width, height, startAngle, sweepAngle);

        public void DrawBezier(Pen pen, float x1, float y1, float x2, float y2, float x3, float y3, float x4, float y4) =>
            StrokePoints(pen, new float[] { x1, y1, x2, y2, x3, y3, x4, y4 }, new byte[] { 0, 3, 3, 3 });

        public void DrawBezier(Pen pen, PointF pt1, PointF pt2, PointF pt3, PointF pt4) =>
            DrawBezier(pen, pt1.X, pt1.Y, pt2.X, pt2.Y, pt3.X, pt3.Y, pt4.X, pt4.Y);

        public void DrawBezier(Pen pen, Point pt1, Point pt2, Point pt3, Point pt4) =>
            DrawBezier(pen, (float)pt1.X, pt1.Y, pt2.X, pt2.Y, pt3.X, pt3.Y, pt4.X, pt4.Y);

        public void DrawBeziers(Pen pen, PointF[] points)
        {
            var path = new GraphicsPath();
            path.AddBeziers(points);
            DrawPath(pen, path);
        }

        public void DrawBeziers(Pen pen, Point[] points) => DrawBeziers(pen, GraphicsPath.Floats(points));

        public void DrawCurve(Pen pen, PointF[] points) => DrawCurve(pen, points, 0.5f);

        public void DrawCurve(Pen pen, PointF[] points, float tension)
        {
            var path = new GraphicsPath();
            path.AddCurve(points, tension);
            DrawPath(pen, path);
        }

        public void DrawCurve(Pen pen, Point[] points) => DrawCurve(pen, GraphicsPath.Floats(points), 0.5f);

        public void DrawCurve(Pen pen, Point[] points, float tension) => DrawCurve(pen, GraphicsPath.Floats(points), tension);

        public void DrawClosedCurve(Pen pen, PointF[] points)
        {
            var path = new GraphicsPath();
            path.AddClosedCurve(points, 0.5f);
            DrawPath(pen, path);
        }

        public void DrawClosedCurve(Pen pen, Point[] points) => DrawClosedCurve(pen, GraphicsPath.Floats(points));

        public void DrawPath(Pen pen, GraphicsPath path)
        {
            if (path == null)
            {
                throw new ArgumentNullException("path");
            }
            StrokePoints(pen, path.Points(), path.Types());
        }

        // ---- Картинки -------------------------------------------------------

        // Размер картинки в координатах программы: у GDI+ он физический, по
        // разрешению картинки, и Bitmap с 96 dpi на экране 96 dpi ложится
        // точка в точку.
        public void DrawImage(Image image, float x, float y)
        {
            if (image == null)
            {
                throw new ArgumentNullException("image");
            }
            DrawImage(image, x, y, image.Width * 96f / image.HorizontalResolution, image.Height * 96f / image.VerticalResolution);
        }

        public void DrawImage(Image image, int x, int y) => DrawImage(image, (float)x, y);

        public void DrawImage(Image image, PointF point) => DrawImage(image, point.X, point.Y);

        public void DrawImage(Image image, Point point) => DrawImage(image, (float)point.X, point.Y);

        public void DrawImage(Image image, float x, float y, float width, float height)
        {
            if (image == null)
            {
                throw new ArgumentNullException("image");
            }
            DrawImage(image, new RectangleF(x, y, width, height), new RectangleF(0, 0, image.Width, image.Height), GraphicsUnit.Pixel);
        }

        public void DrawImage(Image image, int x, int y, int width, int height) => DrawImage(image, (float)x, y, width, height);

        public void DrawImage(Image image, RectangleF rect) => DrawImage(image, rect.X, rect.Y, rect.Width, rect.Height);

        public void DrawImage(Image image, Rectangle rect) => DrawImage(image, (float)rect.X, rect.Y, rect.Width, rect.Height);

        public void DrawImage(Image image, Rectangle destRect, Rectangle srcRect, GraphicsUnit srcUnit) =>
            DrawImage(image, (RectangleF)destRect, (RectangleF)srcRect, srcUnit);

        public void DrawImage(Image image, Rectangle destRect, int srcX, int srcY, int srcWidth, int srcHeight, GraphicsUnit srcUnit) =>
            DrawImage(image, (RectangleF)destRect, new RectangleF(srcX, srcY, srcWidth, srcHeight), srcUnit);

        public void DrawImage(Image image, Rectangle destRect, float srcX, float srcY, float srcWidth, float srcHeight, GraphicsUnit srcUnit) =>
            DrawImage(image, (RectangleF)destRect, new RectangleF(srcX, srcY, srcWidth, srcHeight), srcUnit);

        public void DrawImage(Image image, RectangleF destRect, RectangleF srcRect, GraphicsUnit srcUnit)
        {
            if (image == null)
            {
                throw new ArgumentNullException("image");
            }
            if (srcRect.Width == 0 || srcRect.Height == 0)
            {
                return;
            }
            float sx = destRect.Width / srcRect.Width;
            float sy = destRect.Height / srcRect.Height;
            var place = new Matrix(sx, 0, 0, sy, destRect.X - srcRect.X * sx, destRect.Y - srcRect.Y * sy);
            DrawImagePlaced(image, srcRect, place);
        }

        // Параллелограмм: верхний левый, верхний правый и нижний левый углы.
        public void DrawImage(Image image, PointF[] destPoints)
        {
            if (image == null)
            {
                throw new ArgumentNullException("image");
            }
            if (destPoints == null)
            {
                throw new ArgumentNullException("destPoints");
            }
            if (destPoints.Length != 3)
            {
                throw new ArgumentException("Parameter is not valid.");
            }
            var place = new Matrix(new RectangleF(0, 0, image.Width, image.Height), destPoints);
            DrawImagePlaced(image, new RectangleF(0, 0, image.Width, image.Height), place);
        }

        public void DrawImage(Image image, Point[] destPoints) => DrawImage(image, GraphicsPath.Floats(destPoints));

        public void DrawImageUnscaled(Image image, int x, int y)
        {
            if (image == null)
            {
                throw new ArgumentNullException("image");
            }
            DrawImage(image, x, y, image.Width, image.Height);
        }

        public void DrawImageUnscaled(Image image, Point point) => DrawImageUnscaled(image, point.X, point.Y);

        public void DrawImageUnscaled(Image image, Rectangle rect) => DrawImageUnscaled(image, rect.X, rect.Y);

        public void DrawImageUnscaled(Image image, int x, int y, int width, int height) => DrawImageUnscaled(image, x, y);

        public void DrawImageUnscaledAndClipped(Image image, Rectangle rect)
        {
            if (image == null)
            {
                throw new ArgumentNullException("image");
            }
            int width = Math.Min(rect.Width, image.Width);
            int height = Math.Min(rect.Height, image.Height);
            DrawImage(image, new RectangleF(rect.X, rect.Y, width, height), new RectangleF(0, 0, width, height), GraphicsUnit.Pixel);
        }

        private void DrawImagePlaced(Image source, RectangleF part, Matrix place)
        {
            int[] t = Target();
            if (clipRect.Width == 0)
            {
                return;
            }
            place.Multiply(Device(), MatrixOrder.Append);
            int interpolation = InterpolationMode == InterpolationMode.NearestNeighbor ? 5 : 3;
            GdiNative.DrawImage(t, Pixels, clipOps, clipPoints, clipTypes, source.pixels, source.Width, source.Height,
                new float[] { part.X, part.Y, part.Width, part.Height }, place.Elements, interpolation, 255);
        }

        // ---- Текст ----------------------------------------------------------

        public void DrawString(string s, Font font, Brush brush, float x, float y) => DrawString(s, font, brush, new RectangleF(x, y, 0, 0), null);

        public void DrawString(string s, Font font, Brush brush, float x, float y, StringFormat format) =>
            DrawString(s, font, brush, new RectangleF(x, y, 0, 0), format);

        public void DrawString(string s, Font font, Brush brush, PointF point) => DrawString(s, font, brush, new RectangleF(point.X, point.Y, 0, 0), null);

        public void DrawString(string s, Font font, Brush brush, PointF point, StringFormat format) =>
            DrawString(s, font, brush, new RectangleF(point.X, point.Y, 0, 0), format);

        public void DrawString(string s, Font font, Brush brush, RectangleF layoutRectangle) => DrawString(s, font, brush, layoutRectangle, null);

        // Строки раскладываются здесь: перенос по ширине прямоугольника,
        // выравнивание по `Alignment` и `LineAlignment`. У точки вместо
        // прямоугольника «по центру» значит «центр строки в этой точке».
        public void DrawString(string s, Font font, Brush brush, RectangleF layoutRectangle, StringFormat format)
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
            int[] t = Target();
            if (clipRect.Width == 0)
            {
                return;
            }
            var lines = TextLayout.Lines(s, font, layoutRectangle.Width, format);
            float lineHeight = font.GetHeight();
            float total = lines.Count * lineHeight;
            StringAlignment horizontal = format == null ? StringAlignment.Near : format.Alignment;
            StringAlignment vertical = format == null ? StringAlignment.Near : format.LineAlignment;
            float top = layoutRectangle.Y + Offset(vertical, layoutRectangle.Height, total);
            bool legacy = window >= 0 && font.PixelSize == Font.BasePixels && clipOps == null && brush.IsSolid(out Color solid) && solid.A == 255
                && TranslationOnly(out float dx, out float dy);
            for (int i = 0; i < lines.Count; i++)
            {
                string line = lines[i];
                if (line.Length == 0)
                {
                    continue;
                }
                float left = layoutRectangle.X + Offset(horizontal, layoutRectangle.Width, TextLayout.Width(line, font));
                float lineTop = top + i * lineHeight;
                if (legacy)
                {
                    // Шрифт форм без масштаба и без поворота — тем же путём, что до
                    // N9c: элементы WinForms пишут текст тысячами строк, и
                    // рисование глифа прямо в окно дешевле маски.
                    brush.IsSolid(out Color color);
                    TranslationOnly(out float ox, out float oy);
                    FreeOsWindow.Text(window, (int)(left + ox), (int)(lineTop + oy), line, color.ToArgb(), clipRect.X, clipRect.Y, clipRect.Width, clipRect.Height);
                    continue;
                }
                float scale = font.Scale;
                var place = new Matrix(scale, 0, 0, scale, left, lineTop);
                place.Multiply(Device(), MatrixOrder.Append);
                brush.Pack(Device(), out int[] paintInts, out float[] paintFloats, out int[] texture);
                GdiNative.DrawText(t, Pixels, clipOps, clipPoints, clipTypes, line, place.Elements, paintInts, paintFloats, texture);
            }
        }

        private static float Offset(StringAlignment alignment, float room, float size)
        {
            if (alignment == StringAlignment.Near)
            {
                return 0;
            }
            float free = room > 0 ? room - size : -size;
            return alignment == StringAlignment.Center ? free / 2 : free;
        }

        public SizeF MeasureString(string text, Font font) => MeasureString(text, font, 0, null);

        public SizeF MeasureString(string text, Font font, int width) => MeasureString(text, font, (float)width, null);

        public SizeF MeasureString(string text, Font font, SizeF layoutArea) => MeasureString(text, font, layoutArea.Width, null);

        public SizeF MeasureString(string text, Font font, SizeF layoutArea, StringFormat stringFormat) =>
            MeasureString(text, font, layoutArea.Width, stringFormat);

        public SizeF MeasureString(string text, Font font, int width, StringFormat format) => MeasureString(text, font, (float)width, format);

        public SizeF MeasureString(string text, Font font, PointF origin, StringFormat stringFormat) => MeasureString(text, font, 0, stringFormat);

        private SizeF MeasureString(string text, Font font, float width, StringFormat format)
        {
            if (font == null)
            {
                throw new ArgumentNullException("font");
            }
            if (string.IsNullOrEmpty(text))
            {
                return SizeF.Empty;
            }
            var lines = TextLayout.Lines(text, font, width, format);
            float widest = 0;
            for (int i = 0; i < lines.Count; i++)
            {
                widest = Math.Max(widest, TextLayout.Width(lines[i], font));
            }
            return new SizeF(widest, lines.Count * font.GetHeight());
        }
    }
}
