// System.Drawing.Region (фаза N9): область как программа операций над путями в
// обратной польской записи — фигура кладётся на стек, операция снимает две.
// Так выражается и `a.Union(b)`, где `b` сама собрана из нескольких фигур.
// Маску точек по программе строит растеризатор (`raster::region`), здесь —
// только запись операций и узнавание частого случая «это прямоугольник».

using System.Collections.Generic;
using System.Drawing.Drawing2D;

namespace System.Drawing
{
    public sealed class Region : IDisposable
    {
        private const int KindInfinite = 0;
        private const int KindEmpty = 1;
        private const int KindPath = 2;
        private const int KindCombine = 3;

        private sealed class Step
        {
            internal int Kind;
            internal int Combine;
            internal PointF[] Points;
            internal byte[] Types;
            internal int FillMode;
        }

        private List<Step> steps = new List<Step>();

        public Region()
        {
            MakeInfinite();
        }

        public Region(RectangleF rect)
        {
            steps.Add(RectangleStep(rect));
        }

        public Region(Rectangle rect)
            : this((RectangleF)rect)
        {
        }

        public Region(GraphicsPath path)
        {
            if (path == null)
            {
                throw new ArgumentNullException("path");
            }
            steps.Add(PathStep(path));
        }

        public void Dispose()
        {
        }

        public Region Clone()
        {
            var copy = new Region();
            copy.steps = new List<Step>();
            for (int i = 0; i < steps.Count; i++)
            {
                Step step = steps[i];
                copy.steps.Add(new Step { Kind = step.Kind, Combine = step.Combine, Points = CopyPoints(step.Points), Types = step.Types, FillMode = step.FillMode });
            }
            return copy;
        }

        private static PointF[] CopyPoints(PointF[] points)
        {
            if (points == null)
            {
                return null;
            }
            var copy = new PointF[points.Length];
            Array.CopyItems(points, copy, points.Length);
            return copy;
        }

        private static Step RectangleStep(RectangleF rect)
        {
            // Пустой прямоугольник — пустая область, а не вырожденный путь.
            if (rect.Width <= 0 || rect.Height <= 0)
            {
                return new Step { Kind = KindEmpty };
            }
            return new Step
            {
                Kind = KindPath,
                Points = new PointF[] { new PointF(rect.X, rect.Y), new PointF(rect.X + rect.Width, rect.Y), new PointF(rect.X + rect.Width, rect.Y + rect.Height), new PointF(rect.X, rect.Y + rect.Height) },
                Types = new byte[] { 0, 1, 1, 0x81 },
            };
        }

        private static Step PathStep(GraphicsPath path) =>
            new Step { Kind = KindPath, Points = path.PathPoints, Types = path.PathTypes, FillMode = (int)path.FillMode };

        public void MakeInfinite()
        {
            steps.Clear();
            steps.Add(new Step { Kind = KindInfinite });
        }

        public void MakeEmpty()
        {
            steps.Clear();
            steps.Add(new Step { Kind = KindEmpty });
        }

        private void Combine(List<Step> operand, CombineMode mode)
        {
            if (mode == CombineMode.Replace)
            {
                steps = operand;
                return;
            }
            steps.AddRange(operand);
            steps.Add(new Step { Kind = KindCombine, Combine = (int)mode });
        }

        private static List<Step> One(Step step)
        {
            var list = new List<Step>();
            list.Add(step);
            return list;
        }

        internal void Combine(RectangleF rect, CombineMode mode) => Combine(One(RectangleStep(rect)), mode);

        internal void Combine(GraphicsPath path, CombineMode mode)
        {
            if (path == null)
            {
                throw new ArgumentNullException("path");
            }
            Combine(One(PathStep(path)), mode);
        }

        internal void Combine(Region region, CombineMode mode)
        {
            if (region == null)
            {
                throw new ArgumentNullException("region");
            }
            Combine(region.Clone().steps, mode);
        }

        public void Intersect(Rectangle rect) => Combine(rect, CombineMode.Intersect);

        public void Intersect(RectangleF rect) => Combine(rect, CombineMode.Intersect);

        public void Intersect(GraphicsPath path) => Combine(path, CombineMode.Intersect);

        public void Intersect(Region region) => Combine(region, CombineMode.Intersect);

        public void Union(Rectangle rect) => Combine(rect, CombineMode.Union);

        public void Union(RectangleF rect) => Combine(rect, CombineMode.Union);

        public void Union(GraphicsPath path) => Combine(path, CombineMode.Union);

        public void Union(Region region) => Combine(region, CombineMode.Union);

        public void Xor(Rectangle rect) => Combine(rect, CombineMode.Xor);

        public void Xor(RectangleF rect) => Combine(rect, CombineMode.Xor);

        public void Xor(GraphicsPath path) => Combine(path, CombineMode.Xor);

        public void Xor(Region region) => Combine(region, CombineMode.Xor);

        public void Exclude(Rectangle rect) => Combine(rect, CombineMode.Exclude);

        public void Exclude(RectangleF rect) => Combine(rect, CombineMode.Exclude);

        public void Exclude(GraphicsPath path) => Combine(path, CombineMode.Exclude);

        public void Exclude(Region region) => Combine(region, CombineMode.Exclude);

        public void Complement(Rectangle rect) => Combine(rect, CombineMode.Complement);

        public void Complement(RectangleF rect) => Combine(rect, CombineMode.Complement);

        public void Complement(GraphicsPath path) => Combine(path, CombineMode.Complement);

        public void Complement(Region region) => Combine(region, CombineMode.Complement);

        public void Translate(float dx, float dy) => Transform(new Matrix(1, 0, 0, 1, dx, dy));

        public void Translate(int dx, int dy) => Translate((float)dx, dy);

        public void Transform(Matrix matrix)
        {
            if (matrix == null)
            {
                throw new ArgumentNullException("matrix");
            }
            for (int i = 0; i < steps.Count; i++)
            {
                PointF[] points = steps[i].Points;
                if (points == null)
                {
                    continue;
                }
                for (int j = 0; j < points.Length; j++)
                {
                    points[j] = matrix.Apply(points[j].X, points[j].Y);
                }
            }
        }

        // Бесконечная область у GDI+ — только та, что ни с чем не сложена.
        public bool IsInfinite(Graphics g) => steps.Count == 1 && steps[0].Kind == KindInfinite;

        public bool IsEmpty(Graphics g) => Bounds(out RectangleF bounds) == 1;

        public bool IsVisible(float x, float y) => GdiNative.RegionContains(Program(out float[] points, out byte[] types), points, types, x, y);

        public bool IsVisible(float x, float y, Graphics g) => IsVisible(x, y);

        public bool IsVisible(PointF point) => IsVisible(point.X, point.Y);

        public bool IsVisible(PointF point, Graphics g) => IsVisible(point.X, point.Y);

        public bool IsVisible(int x, int y, Graphics g) => IsVisible((float)x, y);

        public bool IsVisible(Point point) => IsVisible((float)point.X, point.Y);

        public bool IsVisible(Point point, Graphics g) => IsVisible((float)point.X, point.Y);

        // Прямоугольник виден, если видна хоть одна его угловая или средняя
        // точка — приближение: область, пересекающая прямоугольник только
        // узкой полосой между ними, сочтётся невидимой.
        public bool IsVisible(RectangleF rect)
        {
            if (rect.Width <= 0 || rect.Height <= 0)
            {
                return false;
            }
            RectangleF bounds;
            int kind = Bounds(out bounds);
            if (kind == 1 || (kind == 2 && !bounds.IntersectsWith(rect)))
            {
                return false;
            }
            for (int i = 0; i <= 4; i++)
            {
                for (int j = 0; j <= 4; j++)
                {
                    float x = rect.X + Math.Min(rect.Width * i / 4, rect.Width - 0.01f);
                    float y = rect.Y + Math.Min(rect.Height * j / 4, rect.Height - 0.01f);
                    if (IsVisible(x, y))
                    {
                        return true;
                    }
                }
            }
            return false;
        }

        public bool IsVisible(Rectangle rect) => IsVisible((RectangleF)rect);

        public bool IsVisible(RectangleF rect, Graphics g) => IsVisible(rect);

        public bool IsVisible(Rectangle rect, Graphics g) => IsVisible((RectangleF)rect);

        public RectangleF GetBounds(Graphics g)
        {
            if (g == null)
            {
                throw new ArgumentNullException("g");
            }
            RectangleF bounds;
            switch (Bounds(out bounds))
            {
                case 0:
                    return new RectangleF(-4194304, -4194304, 8388608, 8388608);
                case 1:
                    return RectangleF.Empty;
                default:
                    return bounds;
            }
        }

        // 0 — бесконечна, 1 — пуста, 2 — границы в `bounds`.
        private int Bounds(out RectangleF bounds)
        {
            var box = new float[4];
            int kind = GdiNative.RegionBounds(Program(out float[] points, out byte[] types), points, types, box);
            bounds = new RectangleF(box[0], box[1], box[2], box[3]);
            return kind;
        }

        // Программа для растеризатора: по пять чисел на шаг и общие точки.
        internal int[] Program(out float[] points, out byte[] types)
        {
            int total = 0;
            for (int i = 0; i < steps.Count; i++)
            {
                if (steps[i].Points != null)
                {
                    total += steps[i].Points.Length;
                }
            }
            points = new float[total * 2];
            types = new byte[total];
            var ops = new int[steps.Count * 5];
            int at = 0;
            for (int i = 0; i < steps.Count; i++)
            {
                Step step = steps[i];
                int count = step.Points == null ? 0 : step.Points.Length;
                ops[i * 5] = step.Kind;
                ops[i * 5 + 1] = step.Combine;
                ops[i * 5 + 2] = at;
                ops[i * 5 + 3] = count;
                ops[i * 5 + 4] = step.FillMode;
                for (int j = 0; j < count; j++)
                {
                    points[(at + j) * 2] = step.Points[j].X;
                    points[(at + j) * 2 + 1] = step.Points[j].Y;
                    types[at + j] = step.Types[j];
                }
                at += count;
            }
            return ops;
        }

        // Прямоугольник ли область — тогда отсечение обходится без маски.
        // Узнаются бесконечность, прямоугольник со сторонами вдоль осей и
        // пересечения таких; всё остальное — «не прямоугольник», даже если
        // геометрически он (объединение двух половин, например).
        internal bool TryGetRectangle(out RectangleF rect, out bool infinite)
        {
            var stack = new List<RectangleF>();
            var flags = new List<bool>();
            rect = RectangleF.Empty;
            infinite = false;
            for (int i = 0; i < steps.Count; i++)
            {
                Step step = steps[i];
                if (step.Kind == KindInfinite)
                {
                    stack.Add(RectangleF.Empty);
                    flags.Add(true);
                }
                else if (step.Kind == KindEmpty)
                {
                    stack.Add(RectangleF.Empty);
                    flags.Add(false);
                }
                else if (step.Kind == KindPath)
                {
                    if (!IsAxisRectangle(step.Points, step.Types, out RectangleF r))
                    {
                        return false;
                    }
                    stack.Add(r);
                    flags.Add(false);
                }
                else
                {
                    if (step.Combine != (int)CombineMode.Intersect || stack.Count < 2)
                    {
                        return false;
                    }
                    RectangleF b = stack[stack.Count - 1];
                    bool bInfinite = flags[flags.Count - 1];
                    RectangleF a = stack[stack.Count - 2];
                    bool aInfinite = flags[flags.Count - 2];
                    stack.RemoveAt(stack.Count - 1);
                    flags.RemoveAt(flags.Count - 1);
                    RectangleF result = aInfinite ? b : bInfinite ? a : RectangleF.Intersect(a, b);
                    stack[stack.Count - 1] = result;
                    flags[flags.Count - 1] = aInfinite && bInfinite;
                }
            }
            if (stack.Count != 1)
            {
                return false;
            }
            rect = stack[0];
            infinite = flags[0];
            return true;
        }

        private static bool IsAxisRectangle(PointF[] p, byte[] types, out RectangleF rect)
        {
            rect = RectangleF.Empty;
            if (p == null || p.Length != 4 || types[1] != 1 || types[2] != 1 || (types[3] & 7) != 1)
            {
                return false;
            }
            bool horizontalFirst = p[0].Y == p[1].Y && p[1].X == p[2].X && p[2].Y == p[3].Y && p[3].X == p[0].X;
            bool verticalFirst = p[0].X == p[1].X && p[1].Y == p[2].Y && p[2].X == p[3].X && p[3].Y == p[0].Y;
            if (!horizontalFirst && !verticalFirst)
            {
                return false;
            }
            float x0 = Math.Min(p[0].X, p[2].X);
            float y0 = Math.Min(p[0].Y, p[2].Y);
            rect = new RectangleF(x0, y0, Math.Max(p[0].X, p[2].X) - x0, Math.Max(p[0].Y, p[2].Y) - y0);
            return true;
        }
    }
}
