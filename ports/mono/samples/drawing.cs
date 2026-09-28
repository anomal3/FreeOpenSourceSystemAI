// System.Drawing под Mono во FreeOS (фаза 61b): Graphics рисует через
// libgdiplus, та — через cairo, pixman и freetype, а шрифт находит fontconfig.
//
// Как и hello.cs, собирается чужим компилятором (csc из .NET Framework 4) и
// сверяется чужим рантаймом: тот же drawing.exe под .NET Framework на Windows
// рисует настоящим GDI+ и обязан напечатать в stdout ровно те же строки.
// Поэтому строки — о фактах, общих для двух разных растеризаторов: цвет точки
// в глубине фигуры, наличие полутона на краю круга, пометки текста в своей
// полосе, — а не о точном рисунке сглаживания, который у GDI+ и cairo свой.
//
// Имя найденного шрифта — в stderr: под Windows это Microsoft Sans Serif, во
// FreeOS — DejaVu Sans, единственный шрифт системы. Сценарий ищет его
// отдельно: это и есть проверка, что fontconfig выбрал шрифт по имени.
//
// Язык — C# 5: другого у csc из .NET Framework нет.

using System;
using System.Drawing;
using System.Drawing.Drawing2D;
using System.Drawing.Imaging;
using System.Globalization;
using System.IO;

static class Program
{
    const int W = 64;
    const int H = 48;
    static int failures;

    static void Check(bool passed, string what)
    {
        Console.WriteLine("drawing: " + (passed ? "ok " : "FAILED ") + what);
        if (!passed)
            failures++;
    }

    static string Hex(Color color)
    {
        return color.ToArgb().ToString("x8", CultureInfo.InvariantCulture);
    }

    static bool IsWhite(Color color) { return color.ToArgb() == Color.White.ToArgb(); }

    static int Main()
    {
        using (Bitmap bitmap = new Bitmap(W, H, PixelFormat.Format32bppArgb))
        {
            Console.WriteLine("drawing: bitmap " + bitmap.Width + "x" + bitmap.Height + " " + bitmap.PixelFormat);
            using (Graphics graphics = Graphics.FromImage(bitmap))
            {
                graphics.Clear(Color.White);

                // Прямоугольник без сглаживания: целые координаты, точки
                // 4..23 по x и 4..13 по y — ровно его цвета, соседи — фон.
                using (SolidBrush red = new SolidBrush(Color.FromArgb(255, 200, 0, 0)))
                    graphics.FillRectangle(red, 4, 4, 20, 10);
                Color inside = bitmap.GetPixel(13, 8);
                Console.WriteLine("drawing: rectangle " + Hex(inside));
                Check(Hex(bitmap.GetPixel(4, 4)) == "ffc80000" && Hex(bitmap.GetPixel(23, 13)) == "ffc80000"
                      && IsWhite(bitmap.GetPixel(3, 8)) && IsWhite(bitmap.GetPixel(24, 8))
                      && IsWhite(bitmap.GetPixel(13, 3)) && IsWhite(bitmap.GetPixel(13, 14)),
                      "a filled rectangle keeps to its edges");

                // Круг со сглаживанием: центр залит целиком, а на пути от фона
                // к центру есть точка, которая ни фон, ни синий, — полутон.
                graphics.SmoothingMode = SmoothingMode.AntiAlias;
                graphics.FillEllipse(Brushes.Blue, 32, 4, 24, 24);
                Console.WriteLine("drawing: circle centre " + Hex(bitmap.GetPixel(44, 16)));
                bool soft = false;
                for (int x = 30; x <= 44; x++)
                {
                    Color c = bitmap.GetPixel(x, 16);
                    if (!IsWhite(c) && c.ToArgb() != Color.Blue.ToArgb())
                        soft = true;
                }
                Check(soft, "an antialiased circle has a soft edge");

                // Текст шрифтом «без засечек»: имя семейства спрашивается у
                // системы, а не зашито. Пометки — только в полосе под фигурами.
                using (Font font = new Font(FontFamily.GenericSansSerif, 14, FontStyle.Regular, GraphicsUnit.Pixel))
                {
                    Console.Error.WriteLine("drawing: font family " + font.FontFamily.Name);
                    graphics.DrawString("FreeOS", font, Brushes.Black, 2, 28);
                    SizeF size = graphics.MeasureString("FreeOS", font);
                    Check(size.Width > size.Height && size.Height > 10, "text measures wider than tall");
                }
            }

            int marked = 0;
            int stray = 0;
            for (int y = 0; y < H; y++)
            {
                for (int x = 0; x < W; x++)
                {
                    Color c = bitmap.GetPixel(x, y);
                    if (c.R < 128 && c.G < 128 && c.B < 128)
                    {
                        if (y >= 28)
                            marked++;
                        else
                            stray++;
                    }
                }
            }
            Check(marked > 20 && stray == 0, "text leaves dark marks in its own band only");

            // PNG туда и обратно: записан кодеком библиотеки, прочитан им же —
            // и совпадает с оригиналом точка в точку.
            using (MemoryStream stream = new MemoryStream())
            {
                bitmap.Save(stream, ImageFormat.Png);
                stream.Position = 0;
                using (Bitmap back = new Bitmap(stream))
                {
                    int same = 0;
                    for (int y = 0; y < H; y++)
                        for (int x = 0; x < W; x++)
                            if (back.GetPixel(x, y).ToArgb() == bitmap.GetPixel(x, y).ToArgb())
                                same++;
                    Console.WriteLine("drawing: png " + back.Width + "x" + back.Height + ", " + same + " pixels match");
                    Check(same == W * H, "a PNG reads back pixel for pixel");
                }
            }
        }
        Console.WriteLine("drawing: done, " + failures + " check(s) failed");
        return failures == 0 ? 0 : 1;
    }
}
