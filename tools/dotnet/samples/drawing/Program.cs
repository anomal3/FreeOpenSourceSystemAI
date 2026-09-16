// Образец для фазы N9: System.Drawing как в GDI+.
//
// Всё проверяемое рисуется в Bitmap и читается обратно через GetPixel: точку в
// памяти можно прочитать, точку в окне — нет. Строки выбраны так, чтобы
// совпадать с GDI+ до значения: заливка по центрам точек, тонкое перо с обоими
// концами, толстое перо, половинное покрытие сглаженного края на целой
// координате, округление смешивания, градиент, картинка с увеличением. Там, где
// GDI+ считает приближённо (край сглаженного круга), печатается не значение, а
// признак «край частично закрашен».
//
// Затем форма рисует то же в окно; с аргументом self-test закрывается сама.

using System.Drawing.Drawing2D;
using System.Drawing.Imaging;

namespace FreeOs.Samples.Drawing;

internal static class Program
{
    [STAThread]
    private static int Main(string[] args)
    {
        Console.WriteLine("drawing: start");
        Bitmaps();
        Fills();
        Strokes();
        Blending();
        Images();
        Matrices();
        Transforms();
        Paths();
        Clipping();
        Regions();
        Texts();

        ApplicationConfiguration.Initialize();
        var form = new CanvasForm(args.Length > 0 && args[0] == "self-test");
        Application.Run(form);
        Console.WriteLine("drawing: done");
        return 9;
    }

    internal static string C(Color c) => c.A + "," + c.R + "," + c.G + "," + c.B;

    private static Bitmap White(int width = 40, int height = 40)
    {
        var bitmap = new Bitmap(width, height);
        using (Graphics g = Graphics.FromImage(bitmap))
        {
            g.Clear(Color.White);
        }
        return bitmap;
    }

    // Канал точек строки: 'R', 'G' или 'B'.
    private static string Row(Bitmap bitmap, int y, int x0, int x1, char channel)
    {
        string text = "";
        for (int x = x0; x <= x1; x++)
        {
            Color c = bitmap.GetPixel(x, y);
            int value = channel == 'R' ? c.R : channel == 'G' ? c.G : c.B;
            text += (x == x0 ? "" : " ") + value;
        }
        return text;
    }

    // Закрашено ли (чёрным по белому) — для фигур, где важна граница.
    private static string Ink(Bitmap bitmap, params int[] xy)
    {
        string text = "";
        for (int i = 0; i + 1 < xy.Length; i += 2)
        {
            text += bitmap.GetPixel(xy[i], xy[i + 1]).R < 128 ? "#" : ".";
        }
        return text;
    }

    private static string R(float value) => (Math.Round(value, 3) + 0.0).ToString();

    private static void Bitmaps()
    {
        var bitmap = new Bitmap(64, 48);
        Console.WriteLine("bitmap: " + bitmap.Width + "x" + bitmap.Height + " " + bitmap.PixelFormat + " " + C(bitmap.GetPixel(0, 0)) + " " + bitmap.Size + " "
            + bitmap.HorizontalResolution);
        bitmap.SetPixel(3, 4, Color.FromArgb(10, 20, 30, 40));
        Color pixel = bitmap.GetPixel(3, 4);
        Console.WriteLine("pixel: " + C(pixel) + " " + pixel.Name + " " + pixel.IsKnownColor + " " + (pixel == Color.FromArgb(10, 20, 30, 40)));
        try
        {
            bitmap.GetPixel(64, 0);
        }
        catch (ArgumentOutOfRangeException)
        {
            Console.WriteLine("pixel: out of range");
        }
        var opaque = new Bitmap(4, 4, PixelFormat.Format24bppRgb);
        opaque.SetPixel(1, 1, Color.FromArgb(10, 20, 30, 40));
        Console.WriteLine("opaque: " + C(opaque.GetPixel(0, 0)) + " " + C(opaque.GetPixel(1, 1)));
    }

    private static void Fills()
    {
        Bitmap b = White();
        using (Graphics g = Graphics.FromImage(b))
        {
            g.FillRectangle(Brushes.Red, 10, 10, 20, 10);
        }
        Console.WriteLine("fill: " + Row(b, 10, 8, 12, 'G') + " | " + Row(b, 10, 28, 31, 'G') + " | " + Row(b, 19, 9, 10, 'G') + " | " + Row(b, 20, 9, 10, 'G'));

        b = White();
        using (Graphics g = Graphics.FromImage(b))
        {
            g.SmoothingMode = SmoothingMode.AntiAlias;
            g.FillRectangle(Brushes.Red, 10, 10, 20, 10);
            Console.WriteLine("smoothing: " + g.SmoothingMode + " " + g.PixelOffsetMode + " " + g.CompositingMode + " " + g.InterpolationMode);
        }
        Console.WriteLine("antialias: " + Row(b, 12, 8, 12, 'G') + " | " + Row(b, 12, 28, 31, 'G'));

        b = White();
        using (Graphics g = Graphics.FromImage(b))
        {
            g.SmoothingMode = SmoothingMode.AntiAlias;
            g.PixelOffsetMode = PixelOffsetMode.Half;
            g.FillRectangle(Brushes.Red, 10, 10, 20, 10);
        }
        Console.WriteLine("half: " + Row(b, 12, 8, 12, 'G') + " | " + Row(b, 12, 28, 31, 'G'));

        b = White();
        using (Graphics g = Graphics.FromImage(b))
        {
            g.FillPolygon(Brushes.Black, new[] { new Point(0, 0), new Point(30, 0), new Point(0, 30) });
        }
        Console.WriteLine("triangle: " + Ink(b, 0, 0, 29, 0, 30, 0, 24, 5, 25, 5, 14, 15, 15, 15, 0, 29, 0, 30));

        b = White();
        using (Graphics g = Graphics.FromImage(b))
        {
            g.FillEllipse(Brushes.Black, 5, 5, 30, 30);
        }
        Console.WriteLine("ellipse: " + Ink(b, 20, 20, 5, 20, 34, 20, 35, 20, 6, 6, 33, 33, 20, 7, 12, 12));

        b = White();
        using (Graphics g = Graphics.FromImage(b))
        {
            g.SmoothingMode = SmoothingMode.AntiAlias;
            g.FillEllipse(Brushes.Black, 5, 5, 30, 30);
        }
        int partial = 0;
        for (int x = 0; x < 40; x++)
        {
            int r = b.GetPixel(x, 20).R;
            if (r > 0 && r < 255)
            {
                partial++;
            }
        }
        Console.WriteLine("smooth ellipse: " + (partial == 2) + " " + b.GetPixel(20, 20).R + " " + b.GetPixel(2, 20).R);

        b = White();
        using (Graphics g = Graphics.FromImage(b))
        {
            g.FillPie(Brushes.Black, 0, 0, 40, 40, 0, 90);
            g.FillRectangles(Brushes.Black, new[] { new Rectangle(0, 0, 2, 2), new Rectangle(4, 0, 2, 2) });
        }
        Console.WriteLine("pie: " + Ink(b, 25, 25, 15, 25, 25, 15, 5, 5, 1, 1, 3, 1, 5, 1));
    }

    private static void Strokes()
    {
        Bitmap b = White();
        using (Graphics g = Graphics.FromImage(b))
        {
            g.DrawRectangle(Pens.Blue, 5, 5, 10, 10);
        }
        Console.WriteLine("rectangle: " + Row(b, 5, 3, 17, 'R') + " | " + Row(b, 10, 3, 17, 'R') + " | " + Row(b, 16, 3, 17, 'R'));

        b = White();
        using (Graphics g = Graphics.FromImage(b))
        {
            g.DrawLine(Pens.Black, 2, 10, 30, 10);
            g.DrawLine(Pens.Black, 2, 20, 30, 27);
        }
        string diagonal = "";
        for (int y = 20; y <= 27; y++)
        {
            int first = -1;
            int last = -1;
            for (int x = 0; x < 40; x++)
            {
                if (b.GetPixel(x, y).R == 0)
                {
                    first = first < 0 ? x : first;
                    last = x;
                }
            }
            diagonal += " " + first + "-" + last;
        }
        Console.WriteLine("line: " + Row(b, 10, 0, 4, 'R') + " | " + Row(b, 10, 28, 32, 'R') + " |" + diagonal);

        foreach (float width in new[] { 5f, 4f })
        {
            b = White();
            using (Graphics g = Graphics.FromImage(b))
            using (var pen = new Pen(Color.Black, width))
            {
                g.DrawLine(pen, 5, 20, 35, 20);
            }
            string rows = "";
            for (int y = 16; y <= 24; y++)
            {
                rows += b.GetPixel(6, y).R == 0 ? "#" : ".";
            }
            Console.WriteLine("wide " + width + ": " + rows + " " + Row(b, 20, 3, 6, 'R'));
        }

        b = White();
        using (Graphics g = Graphics.FromImage(b))
        using (var pen = new Pen(Color.Black, 5))
        {
            g.SmoothingMode = SmoothingMode.AntiAlias;
            g.DrawLine(pen, 5, 20, 35, 20);
        }
        Console.WriteLine("smooth wide: " + Row(b, 20, 3, 6, 'R') + " " + Row(b, 17, 6, 6, 'R') + " " + Row(b, 18, 6, 6, 'R') + " " + Row(b, 22, 6, 6, 'R') + " "
            + Row(b, 23, 6, 6, 'R'));

        b = White();
        using (Graphics g = Graphics.FromImage(b))
        using (var pen = new Pen(Color.Black, 2))
        {
            pen.DashStyle = DashStyle.Dash;
            g.DrawLine(pen, 0, 20, 40, 20);
            Console.WriteLine("dash pattern: " + string.Join(",", pen.DashPattern) + " " + pen.DashStyle + " " + pen.LineJoin + " " + pen.StartCap + " " + pen.MiterLimit
                + " " + pen.Alignment + " " + pen.PenType);
        }
        string dashes = "";
        for (int x = 0; x < 16; x++)
        {
            dashes += b.GetPixel(x, 20).R == 0 ? "#" : ".";
        }
        Console.WriteLine("dashes: " + dashes);

        b = White();
        using (Graphics g = Graphics.FromImage(b))
        using (var pen = new Pen(Color.Black, 6))
        {
            pen.LineJoin = LineJoin.Round;
            pen.StartCap = LineCap.Round;
            pen.EndCap = LineCap.Square;
            g.DrawLines(pen, new[] { new Point(8, 8), new Point(30, 8), new Point(30, 30) });
            g.DrawEllipse(Pens.Black, 2, 32, 6, 6);
        }
        Console.WriteLine("joins: " + Ink(b, 5, 8, 3, 8, 30, 8, 30, 30, 30, 32, 30, 35, 33, 5, 2, 35, 5, 35));
    }

    private static void Blending()
    {
        Bitmap b = White();
        using (Graphics g = Graphics.FromImage(b))
        {
            g.FillRectangle(new SolidBrush(Color.FromArgb(128, 0, 0, 255)), 0, 0, 10, 10);
            g.FillRectangle(new SolidBrush(Color.FromArgb(100, 255, 0, 0)), 20, 0, 10, 10);
            g.FillRectangle(new SolidBrush(Color.FromArgb(200, 0, 128, 0)), 20, 0, 10, 10);
        }
        var clear = new Bitmap(10, 10);
        using (Graphics g = Graphics.FromImage(clear))
        {
            g.FillRectangle(new SolidBrush(Color.FromArgb(128, 0, 0, 255)), 0, 0, 10, 10);
            Console.Write("blend: " + C(clear.GetPixel(5, 5)));
            g.FillRectangle(new SolidBrush(Color.FromArgb(100, 255, 0, 0)), 0, 0, 10, 10);
        }
        Console.WriteLine(" " + C(clear.GetPixel(5, 5)) + " " + C(b.GetPixel(5, 5)) + " " + C(b.GetPixel(25, 5)));

        using (Graphics g = Graphics.FromImage(clear))
        {
            g.CompositingMode = CompositingMode.SourceCopy;
            g.FillRectangle(new SolidBrush(Color.FromArgb(50, 1, 2, 3)), 0, 0, 5, 5);
            g.CompositingMode = CompositingMode.SourceOver;
            g.Clear(Color.Transparent);
        }
        Console.WriteLine("copy: " + C(clear.GetPixel(0, 0)) + " " + clear.GetPixel(9, 9).A);

        b = White();
        using (Graphics g = Graphics.FromImage(b))
        {
            var gradient = new LinearGradientBrush(new Point(0, 0), new Point(40, 0), Color.Black, Color.White);
            g.FillRectangle(gradient, 0, 0, 40, 10);
            var vertical = new LinearGradientBrush(new Rectangle(0, 20, 10, 20), Color.Red, Color.Blue, LinearGradientMode.Vertical);
            g.FillRectangle(vertical, 0, 20, 10, 20);
            Console.WriteLine("gradient brush: " + gradient.WrapMode + " " + gradient.Rectangle + " " + C(gradient.LinearColors[0]) + " " + vertical.Rectangle);
        }
        Console.WriteLine("gradient: " + b.GetPixel(0, 5).R + " " + b.GetPixel(10, 5).R + " " + b.GetPixel(20, 5).R + " " + b.GetPixel(30, 5).R + " " + b.GetPixel(39, 5).R + " | "
            + (b.GetPixel(5, 21).R > b.GetPixel(5, 38).R) + " " + (b.GetPixel(5, 38).B > 200));

        var tile = new Bitmap(2, 2);
        tile.SetPixel(0, 0, Color.Red);
        tile.SetPixel(1, 0, Color.Lime);
        tile.SetPixel(0, 1, Color.Blue);
        tile.SetPixel(1, 1, Color.Black);
        b = White();
        using (Graphics g = Graphics.FromImage(b))
        using (var texture = new TextureBrush(tile))
        {
            g.FillRectangle(texture, 0, 0, 8, 8);
        }
        Console.WriteLine("texture: " + C(b.GetPixel(0, 0)) + " " + C(b.GetPixel(3, 0)) + " " + C(b.GetPixel(2, 3)) + " " + C(b.GetPixel(7, 7)));
    }

    private static void Images()
    {
        var source = new Bitmap(10, 10);
        using (Graphics g = Graphics.FromImage(source))
        {
            g.Clear(Color.Red);
        }
        source.SetPixel(0, 0, Color.Blue);

        Bitmap b = White();
        using (Graphics g = Graphics.FromImage(b))
        {
            g.DrawImage(source, 5, 5);
        }
        Console.WriteLine("image: " + Row(b, 5, 3, 16, 'G') + " " + C(b.GetPixel(5, 5)) + " " + C(b.GetPixel(6, 6)));

        b = White();
        using (Graphics g = Graphics.FromImage(b))
        {
            g.DrawImage(source, new Rectangle(0, 0, 20, 20));
        }
        Console.WriteLine("scaled: " + Row(b, 10, 17, 21, 'G') + " " + C(b.GetPixel(0, 0)) + " " + C(b.GetPixel(1, 1)) + " " + C(b.GetPixel(2, 2)));

        b = White();
        using (Graphics g = Graphics.FromImage(b))
        {
            g.InterpolationMode = InterpolationMode.NearestNeighbor;
            g.DrawImage(source, new Rectangle(0, 0, 20, 20));
        }
        Console.WriteLine("nearest: " + Row(b, 10, 17, 21, 'G') + " " + C(b.GetPixel(0, 0)) + " " + C(b.GetPixel(1, 1)) + " " + C(b.GetPixel(2, 2)));

        b = White();
        using (Graphics g = Graphics.FromImage(b))
        {
            g.DrawImage(source, new Rectangle(20, 20, 10, 10), new Rectangle(5, 5, 5, 5), GraphicsUnit.Pixel);
            g.DrawImageUnscaled(source, 0, 30);
        }
        Console.WriteLine("part: " + C(b.GetPixel(25, 25)) + " " + C(b.GetPixel(15, 25)) + " " + C(b.GetPixel(0, 30)) + " " + C(b.GetPixel(5, 35)));

        var copy = (Bitmap)source.Clone();
        copy.RotateFlip(RotateFlipType.Rotate90FlipNone);
        var part = source.Clone(new Rectangle(0, 0, 3, 3), PixelFormat.Format32bppArgb);
        copy.MakeTransparent(Color.Red);
        Console.WriteLine("clone: " + C(copy.GetPixel(9, 0)) + " " + C(copy.GetPixel(0, 0)) + " " + part.Width + " " + C(part.GetPixel(0, 0)));
    }

    private static string Elements(Matrix m)
    {
        string text = "";
        float[] elements = m.Elements;
        for (int i = 0; i < elements.Length; i++)
        {
            text += (i == 0 ? "" : ",") + R(elements[i]);
        }
        return text;
    }

    private static void Matrices()
    {
        var m = new Matrix();
        m.Translate(10, 20);
        m.Scale(2, 3);
        var points = new[] { new PointF(1, 1) };
        m.TransformPoints(points);
        Console.WriteLine("matrix: " + Elements(m) + " " + points[0] + " " + m.IsIdentity + " " + m.IsInvertible + " " + m.OffsetX);
        m.Rotate(30);
        Console.WriteLine("rotate: " + Elements(m));
        m.Invert();
        Console.WriteLine("invert: " + Elements(m));
        var at = new Matrix();
        at.RotateAt(90, new PointF(5, 5));
        Console.WriteLine("rotate at: " + Elements(at));
        at.Shear(1, 0.5f);
        at.Multiply(new Matrix(1, 2, 3, 4, 5, 6), MatrixOrder.Append);
        Console.WriteLine("shear: " + Elements(at));
        var vectors = new[] { new PointF(1, 0) };
        new Matrix(2, 0, 0, 2, 100, 100).TransformVectors(vectors);
        var singular = new Matrix(0, 0, 0, 0, 1, 1);
        Console.WriteLine("vectors: " + vectors[0] + " " + singular.IsInvertible);
    }

    private static void Transforms()
    {
        Bitmap b = White();
        using (Graphics g = Graphics.FromImage(b))
        {
            g.TranslateTransform(20, 10);
            g.FillRectangle(Brushes.Black, 0, 0, 5, 5);
            Console.Write("translate: " + Elements(g.Transform));
            GraphicsState state = g.Save();
            g.ScaleTransform(2, 2);
            Console.Write(" | " + Elements(g.Transform));
            g.Restore(state);
            Console.WriteLine(" | " + Elements(g.Transform) + " " + Row(b, 12, 18, 26, 'R'));

            g.ResetTransform();
            g.ScaleTransform(2, 2);
            g.FillRectangle(Brushes.Black, 1, 10, 5, 5);
            g.ResetTransform();
            g.RotateTransform(90);
            g.TranslateTransform(30, 0, MatrixOrder.Append);
            Console.Write("rotate transform: " + Elements(g.Transform));
            g.FillRectangle(Brushes.Black, 0, 0, 10, 5);
            g.ResetTransform();
        }
        Console.WriteLine(" " + Ink(b, 3, 21, 11, 29, 13, 21, 1, 21, 27, 5, 24, 5, 27, 11));

        b = White();
        using (Graphics g = Graphics.FromImage(b))
        using (var pen = new Pen(Color.Black, 1))
        {
            g.ScaleTransform(4, 4);
            g.DrawLine(pen, 1, 5, 9, 5);
        }
        string rows = "";
        for (int y = 17; y <= 23; y++)
        {
            rows += b.GetPixel(20, y).R == 0 ? "#" : ".";
        }
        Console.WriteLine("scaled pen: " + rows);
    }

    private static void Paths()
    {
        var path = new GraphicsPath();
        path.AddRectangle(new Rectangle(0, 0, 30, 30));
        path.AddEllipse(5, 5, 20, 20);
        Console.WriteLine("path: " + path.PointCount + " " + string.Join(",", path.PathTypes) + " " + path.FillMode + " " + path.GetBounds() + " " + path.IsVisible(15, 15) + " "
            + path.IsVisible(2, 2) + " " + path.PathPoints[4] + " " + path.PathPoints[7]);

        Bitmap b = White();
        using (Graphics g = Graphics.FromImage(b))
        {
            g.FillPath(Brushes.Black, path);
            path.FillMode = FillMode.Winding;
            g.TranslateTransform(0, 0);
        }
        Bitmap w = White();
        using (Graphics g = Graphics.FromImage(w))
        {
            g.FillPath(Brushes.Black, path);
        }
        Console.WriteLine("fill mode: " + Ink(b, 2, 2, 15, 15) + " " + Ink(w, 2, 2, 15, 15) + " " + path.IsVisible(15, 15));

        var arc = new GraphicsPath();
        arc.AddArc(0, 0, 20, 20, 0, 90);
        int arcCount = arc.PointCount;
        arc.AddArc(0, 0, 20, 20, 90, 200);
        var pie = new GraphicsPath();
        pie.AddPie(0, 0, 20, 20, 0, 90);
        Console.WriteLine("arcs: " + arcCount + " " + string.Join(",", arc.PathTypes) + " | " + pie.PointCount + " " + string.Join(",", pie.PathTypes));

        var lines = new GraphicsPath();
        lines.AddLine(0, 0, 10, 0);
        lines.AddLine(10, 0, 10, 10);
        lines.AddLine(20, 20, 30, 30);
        lines.CloseFigure();
        lines.AddBezier(0, 0, 1, 1, 2, 2, 3, 3);
        lines.StartFigure();
        lines.AddLines(new[] { new PointF(1, 1), new PointF(2, 2) });
        lines.AddPolygon(new[] { new Point(0, 0), new Point(5, 0), new Point(0, 5) });
        Console.WriteLine("lines: " + lines.PointCount + " " + string.Join(",", lines.PathTypes) + " " + lines.GetLastPoint());

        var curve = new GraphicsPath();
        curve.AddCurve(new[] { new PointF(0, 0), new PointF(12, 12), new PointF(24, 0) });
        PointF[] spline = curve.PathPoints;
        Console.WriteLine("curve: " + curve.PointCount + " " + R(spline[1].X) + "," + R(spline[1].Y) + " " + R(spline[2].X) + "," + R(spline[2].Y) + " " + R(spline[4].X) + "," + R(spline[4].Y));

        var moved = (GraphicsPath)path.Clone();
        moved.Transform(new Matrix(1, 0, 0, 1, 5, 5));
        Console.WriteLine("path transform: " + moved.GetBounds() + " " + path.GetBounds());

        b = White();
        using (Graphics g = Graphics.FromImage(b))
        using (var pen = new Pen(Color.Black, 3))
        {
            var star = new GraphicsPath(FillMode.Winding);
            star.AddLines(new[] { new PointF(20, 2), new PointF(31, 36), new PointF(2, 14), new PointF(38, 14), new PointF(9, 36) });
            star.CloseFigure();
            g.FillPath(Brushes.Black, star);
            g.DrawBezier(pen, 2, 38, 10, 30, 30, 30, 38, 38);
        }
        Console.WriteLine("star: " + Ink(b, 20, 20, 20, 5, 2, 2, 20, 33, 20, 32, 20, 29));
    }

    private static void Clipping()
    {
        Bitmap b = White();
        using (Graphics g = Graphics.FromImage(b))
        {
            g.SetClip(new Rectangle(0, 0, 10, 10));
            g.FillRectangle(Brushes.Black, 0, 0, 40, 40);
            Console.WriteLine("clip: " + g.ClipBounds + " " + g.IsVisible(5, 5) + " " + g.IsVisible(15, 15) + " " + g.IsClipEmpty + " " + g.VisibleClipBounds + " "
                + g.Clip.GetBounds(g) + " " + Ink(b, 5, 5, 15, 15));

            g.ResetClip();
            Console.WriteLine("reset: " + g.ClipBounds + " " + g.IsClipEmpty + " " + g.Clip.IsInfinite(g) + " " + g.VisibleClipBounds);

            g.SetClip(new Rectangle(10, 10, 5, 5));
            g.IntersectClip(new Rectangle(12, 12, 10, 10));
            Console.WriteLine("intersect: " + g.ClipBounds);
            g.SetClip(new Rectangle(0, 0, 40, 40));
            g.ExcludeClip(new Rectangle(10, 10, 20, 20));
            g.Clear(Color.Red);
            Console.WriteLine("exclude: " + g.ClipBounds + " " + C(b.GetPixel(5, 20)) + " " + C(b.GetPixel(20, 20)));
        }

        b = White();
        using (Graphics g = Graphics.FromImage(b))
        {
            var circle = new GraphicsPath();
            circle.AddEllipse(0, 0, 40, 40);
            g.SetClip(circle);
            g.FillRectangle(Brushes.Black, 0, 0, 40, 40);
            g.ResetClip();
            g.TranslateTransform(30, 30);
            g.SetClip(new Rectangle(0, 0, 5, 5));
            g.ResetTransform();
            g.FillRectangle(Brushes.Blue, 0, 0, 40, 40);
        }
        Console.WriteLine("clip path: " + Ink(b, 20, 20, 1, 1, 38, 38, 2, 20) + " " + C(b.GetPixel(32, 32)) + " " + C(b.GetPixel(28, 32)));
    }

    // Черные точки строки: их число и рамка. Шрифты у GDI+ и у FreeOS разные,
    // поэтому печатаются отношения — «вдвое крупнее», «выше, чем шире после
    // поворота», «по центру прямоугольника», — а не ширина в точках.
    private static Rectangle InkBox(Bitmap bitmap, out int count)
    {
        int left = int.MaxValue;
        int top = int.MaxValue;
        int right = -1;
        int bottom = -1;
        count = 0;
        for (int y = 0; y < bitmap.Height; y++)
        {
            for (int x = 0; x < bitmap.Width; x++)
            {
                if (bitmap.GetPixel(x, y).R < 160)
                {
                    count++;
                    left = Math.Min(left, x);
                    top = Math.Min(top, y);
                    right = Math.Max(right, x);
                    bottom = Math.Max(bottom, y);
                }
            }
        }
        return count == 0 ? Rectangle.Empty : Rectangle.FromLTRB(left, top, right + 1, bottom + 1);
    }

    private static void Texts()
    {
        var small = new Font("Arial", 10);
        var big = new Font(new FontFamily("Arial"), 20, FontStyle.Bold);
        Bitmap b = White(120, 60);
        using (Graphics g = Graphics.FromImage(b))
        {
            SizeF one = g.MeasureString("Hello", small);
            SizeF twice = g.MeasureString("Hello", big);
            SizeF lines = g.MeasureString("Hello\nWorld", small);
            SizeF wrapped = g.MeasureString("Hello World Hello World", small, 60);
            Console.WriteLine("measure: " + (twice.Width > one.Width * 1.7f && twice.Width < one.Width * 2.3f) + " " + (twice.Height > one.Height * 1.7f) + " "
                + (lines.Height > one.Height * 1.7f && lines.Width < one.Width * 1.3f) + " " + (wrapped.Width <= 60 && wrapped.Height > one.Height * 1.7f) + " "
                + g.MeasureString("", small) + " " + (small.Height > 10 && small.Height < 22) + " " + (big.GetHeight() > small.GetHeight() * 1.7f));
            Console.WriteLine("font: " + small.Name + " " + small.Size + " " + small.Unit + " " + small.SizeInPoints + " " + big.Style + " " + big.Bold + " "
                + big.FontFamily.Name + " " + new Font("Arial", 16, GraphicsUnit.Pixel).SizeInPoints + " " + FontFamily.GenericMonospace.Name);
            g.DrawString("Hi", big, Brushes.Black, 5, 5);
        }
        Rectangle plain = InkBox(b, out int plainInk);
        Console.WriteLine("text: " + (plainInk > 20) + " " + (plain.X >= 5 && plain.Y >= 5) + " " + (plain.Right < 60 && plain.Bottom < 50));

        b = White(60, 120);
        using (Graphics g = Graphics.FromImage(b))
        {
            g.TranslateTransform(40, 10);
            g.RotateTransform(90);
            g.DrawString("Hello", small, Brushes.Black, 0, 0);
        }
        Rectangle turned = InkBox(b, out int turnedInk);
        Console.WriteLine("rotated text: " + (turnedInk > 10) + " " + (turned.Height > turned.Width * 2) + " " + (turned.Right <= 41));

        // Строка печатается одним вызовом: пока считается рамка, в журнал FreeOS
        // успевают вклиниться строки служб, и половинки строки стенд не узнает.
        string alignment;
        b = White(120, 60);
        using (Graphics g = Graphics.FromImage(b))
        using (var format = new StringFormat())
        {
            format.Alignment = StringAlignment.Center;
            format.LineAlignment = StringAlignment.Center;
            g.DrawString("Hi", big, Brushes.Black, new RectangleF(0, 0, 120, 60), format);
            alignment = format.Alignment + " " + format.LineAlignment;
        }
        Rectangle centered = InkBox(b, out int centeredInk);
        int middleX = centered.X + centered.Width / 2;
        int middleY = centered.Y + centered.Height / 2;
        Console.WriteLine("centered text: " + alignment + " " + (centeredInk > 20) + " " + (middleX > 50 && middleX < 70) + " " + (middleY > 20 && middleY < 40));

        b = White(120, 60);
        using (Graphics g = Graphics.FromImage(b))
        {
            g.SetClip(new Rectangle(0, 0, 12, 60));
            g.DrawString("Hello World", big, Brushes.Black, 2, 5);
        }
        Rectangle clipped = InkBox(b, out int clippedInk);
        Console.WriteLine("clipped text: " + (clippedInk > 0) + " " + (clipped.Right <= 12));
    }

    private static void Regions()
    {
        var region = new Region(new Rectangle(0, 0, 20, 20));
        region.Union(new Rectangle(10, 10, 20, 20));
        Console.Write("region: " + region.IsVisible(25, 25) + " " + region.IsVisible(25, 5));
        region.Intersect(new Rectangle(0, 0, 15, 15));
        Console.WriteLine(" " + region.IsVisible(12, 12) + " " + region.IsVisible(25, 25));

        var xor = new Region(new Rectangle(0, 0, 10, 10));
        xor.Xor(new Rectangle(5, 5, 10, 10));
        Console.Write("xor: " + xor.IsVisible(7, 7) + " " + xor.IsVisible(2, 2) + " " + xor.IsVisible(12, 12));
        xor.Exclude(new Rectangle(0, 0, 3, 3));
        Console.WriteLine(" " + xor.IsVisible(2, 2) + " " + xor.IsVisible(4, 4));

        Bitmap b = White();
        using (Graphics g = Graphics.FromImage(b))
        {
            var infinite = new Region();
            Console.Write("infinite: " + infinite.IsVisible(1000, 1000) + " " + infinite.IsEmpty(g) + " " + infinite.IsInfinite(g));
            infinite.MakeEmpty();
            Console.Write(" " + infinite.IsVisible(0, 0) + " " + infinite.IsEmpty(g));
            infinite.Complement(new Rectangle(0, 0, 5, 5));
            Console.WriteLine(" " + infinite.IsVisible(1, 1));
            Console.WriteLine("bounds: " + region.GetBounds(g) + " " + xor.GetBounds(g));

            var nested = new Region(new Rectangle(20, 20, 20, 20));
            nested.Exclude(new Rectangle(25, 25, 10, 10));
            var outer = new Region(new Rectangle(0, 0, 10, 10));
            outer.Union(nested);
            g.FillRegion(Brushes.Black, outer);
        }
        Console.WriteLine("fill region: " + Ink(b, 5, 5, 15, 15, 22, 22, 30, 30, 38, 38));
    }
}

internal sealed class CanvasForm : Form
{
    private readonly bool selfTest;
    private readonly Bitmap buffer;

    public CanvasForm(bool selfTest)
    {
        this.selfTest = selfTest;
        SuspendLayout();
        AutoScaleMode = AutoScaleMode.None;
        ClientSize = new Size(360, 240);
        Name = "CanvasForm";
        Text = "Drawing";
        BackColor = Color.White;
        FormClosed += (sender, e) => Console.WriteLine("closed: " + e.CloseReason);
        ResumeLayout(false);

        // Двойная буферизация руками: картинка рисуется один раз в Bitmap и
        // кладётся в окно — так делают чужие программы, чтобы окно не мигало.
        buffer = new Bitmap(160, 120);
        using (Graphics g = Graphics.FromImage(buffer))
        {
            g.SmoothingMode = SmoothingMode.AntiAlias;
            g.Clear(Color.FromArgb(240, 244, 248));
            using (var gradient = new LinearGradientBrush(new Rectangle(0, 0, 160, 120), Color.SteelBlue, Color.White, LinearGradientMode.ForwardDiagonal))
            {
                g.FillEllipse(gradient, 10, 10, 140, 100);
            }
            using (var pen = new Pen(Color.DarkOrange, 4))
            {
                pen.LineJoin = LineJoin.Round;
                g.DrawPolygon(pen, new[] { new Point(80, 18), new Point(140, 100), new Point(20, 100) });
            }
        }
        Color middle = buffer.GetPixel(80, 60);
        Color edge = buffer.GetPixel(80, 106);
        Console.WriteLine("buffer: " + (middle.B > middle.R) + " " + (edge.R > middle.R) + " " + C(buffer.GetPixel(1, 1)) + " " + C(buffer.GetPixel(80, 19)));
    }

    private static string C(Color c) => Program.C(c);

    protected override void OnShown(EventArgs e)
    {
        base.OnShown(e);
        Console.WriteLine("shown: " + ClientSize.Width + "x" + ClientSize.Height);
        if (selfTest)
        {
            Invalidate();
            Update();
            Close();
        }
    }

    protected override void OnPaint(PaintEventArgs e)
    {
        base.OnPaint(e);
        Graphics g = e.Graphics;
        g.DrawImage(buffer, 10, 10);
        g.SmoothingMode = SmoothingMode.AntiAlias;
        GraphicsState state = g.Save();
        g.TranslateTransform(260, 70);
        for (int i = 0; i < 12; i++)
        {
            g.RotateTransform(30);
            using (var brush = new SolidBrush(Color.FromArgb(160, 30 + i * 18, 80, 200 - i * 12)))
            {
                g.FillRectangle(brush, 10, -4, 50, 8);
            }
        }
        g.Restore(state);
        var clip = new GraphicsPath();
        clip.AddEllipse(190, 150, 140, 80);
        g.SetClip(clip);
        using (var hatch = new Pen(Color.SeaGreen, 3))
        {
            for (int x = 170; x < 360; x += 12)
            {
                g.DrawLine(hatch, x, 140, x + 60, 240);
            }
        }
        g.ResetClip();
        using (var pen = new Pen(Color.Navy, 2))
        {
            pen.DashStyle = DashStyle.DashDot;
            g.DrawBezier(pen, 20, 220, 60, 150, 120, 250, 170, 170);
        }
        using (var font = new Font("Arial", 14, FontStyle.Bold))
        {
            GraphicsState text = g.Save();
            g.TranslateTransform(345, 12);
            g.RotateTransform(90);
            g.DrawString("GDI+ on FreeOS", font, Brushes.SteelBlue, 0, 0);
            g.Restore(text);
            g.DrawString("System.Drawing", Font, Brushes.Black, 12, 138);
        }
        Console.WriteLine("paint: " + e.ClipRectangle + " " + g.IsVisible(5, 5) + " " + g.Transform.IsIdentity);
    }
}
