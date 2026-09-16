// Image и Bitmap (фаза N9): картинка в памяти с чтением и записью точек.
// Точки лежат в `int[]` строками сверху вниз как ARGB без умножения на
// прозрачность — это и есть `Format32bppArgb` у GDI+, и `GetPixel` отдаёт ровно
// то, что записал `SetPixel` (проба: `(10, 20, 30, 40)` возвращается как есть).
// Рисует в этот массив растеризатор через `Graphics.FromImage`.

using System.Drawing.Imaging;

namespace System.Drawing
{
    public abstract class Image : IDisposable, ICloneable
    {
        internal int width;
        internal int height;
        internal int[] pixels;
        private float horizontalResolution = 96;
        private float verticalResolution = 96;

        public int Width => width;

        public int Height => height;

        public Size Size => new Size(width, height);

        public SizeF PhysicalDimension => new SizeF(width, height);

        public PixelFormat PixelFormat { get; internal set; }

        public float HorizontalResolution => horizontalResolution;

        public float VerticalResolution => verticalResolution;

        public int Flags => HasAlpha ? 2 : 0;

        internal bool HasAlpha =>
            PixelFormat == PixelFormat.Format32bppArgb || PixelFormat == PixelFormat.Format32bppPArgb || PixelFormat == PixelFormat.Format64bppArgb
            || PixelFormat == PixelFormat.Format16bppArgb1555;

        public void SetResolution(float xDpi, float yDpi)
        {
            if (xDpi <= 0 || yDpi <= 0)
            {
                throw new ArgumentException("Parameter is not valid.");
            }
            horizontalResolution = xDpi;
            verticalResolution = yDpi;
        }

        public RectangleF GetBounds(ref GraphicsUnit pageUnit)
        {
            pageUnit = GraphicsUnit.Pixel;
            return new RectangleF(0, 0, width, height);
        }

        public abstract object Clone();

        public void Dispose()
        {
            Dispose(true);
        }

        protected virtual void Dispose(bool disposing)
        {
        }

        public void RotateFlip(RotateFlipType rotateFlipType)
        {
            int kind = (int)rotateFlipType;
            int turns = kind & 3;
            bool flipX = (kind & 4) != 0;
            int w = width;
            int h = height;
            int nw = turns % 2 == 1 ? h : w;
            int nh = turns % 2 == 1 ? w : h;
            var result = new int[nw * nh];
            for (int y = 0; y < h; y++)
            {
                for (int x = 0; x < w; x++)
                {
                    int sx = x;
                    int sy = y;
                    int tx;
                    int ty;
                    switch (turns)
                    {
                        case 1:
                            tx = h - 1 - sy;
                            ty = sx;
                            break;
                        case 2:
                            tx = w - 1 - sx;
                            ty = h - 1 - sy;
                            break;
                        case 3:
                            tx = sy;
                            ty = w - 1 - sx;
                            break;
                        default:
                            tx = sx;
                            ty = sy;
                            break;
                    }
                    if (flipX)
                    {
                        tx = nw - 1 - tx;
                    }
                    result[ty * nw + tx] = pixels[y * w + x];
                }
            }
            pixels = result;
            width = nw;
            height = nh;
        }
    }

    public enum RotateFlipType
    {
        RotateNoneFlipNone = 0,
        Rotate90FlipNone = 1,
        Rotate180FlipNone = 2,
        Rotate270FlipNone = 3,
        RotateNoneFlipX = 4,
        Rotate90FlipX = 5,
        Rotate180FlipX = 6,
        Rotate270FlipX = 7,
        RotateNoneFlipY = 6,
        Rotate90FlipY = 7,
        Rotate180FlipY = 4,
        Rotate270FlipY = 5,
        RotateNoneFlipXY = 2,
        Rotate90FlipXY = 3,
        Rotate180FlipXY = 0,
        Rotate270FlipXY = 1,
    }

    public sealed class Bitmap : Image
    {
        public Bitmap(int width, int height)
            : this(width, height, PixelFormat.Format32bppArgb)
        {
        }

        public Bitmap(int width, int height, PixelFormat format)
        {
            if (width <= 0 || height <= 0)
            {
                throw new ArgumentException("Parameter is not valid.");
            }
            if (format != PixelFormat.Format32bppArgb && format != PixelFormat.Format32bppRgb && format != PixelFormat.Format24bppRgb
                && format != PixelFormat.Format32bppPArgb)
            {
                throw new ArgumentException("Parameter is not valid.");
            }
            this.width = width;
            this.height = height;
            PixelFormat = format;
            pixels = new int[width * height];
            if (!HasAlpha)
            {
                // Картинка без прозрачности начинается чёрной непрозрачной, а не
                // «пустой»: у неё нет способа сказать «здесь ничего».
                for (int i = 0; i < pixels.Length; i++)
                {
                    pixels[i] = unchecked((int)0xFF000000);
                }
            }
        }

        public Bitmap(int width, int height, Graphics g)
            : this(width, height)
        {
            if (g == null)
            {
                throw new ArgumentNullException("g");
            }
        }

        public Bitmap(Image original)
            : this(original, original == null ? 0 : original.Width, original == null ? 0 : original.Height)
        {
        }

        public Bitmap(Image original, Size newSize)
            : this(original, newSize.Width, newSize.Height)
        {
        }

        public Bitmap(Image original, int width, int height)
            : this(width, height)
        {
            if (original == null)
            {
                throw new ArgumentNullException("original");
            }
            using (Graphics g = Graphics.FromImage(this))
            {
                g.DrawImage(original, 0, 0, width, height);
            }
        }

        public override object Clone()
        {
            var copy = new Bitmap(width, height, PixelFormat);
            Array.CopyItems(pixels, copy.pixels, pixels.Length);
            copy.SetResolution(HorizontalResolution, VerticalResolution);
            return copy;
        }

        public Bitmap Clone(Rectangle rect, PixelFormat format)
        {
            if (rect.Width <= 0 || rect.Height <= 0 || rect.X < 0 || rect.Y < 0 || rect.Right > width || rect.Bottom > height)
            {
                // GDI+ отвечает на прямоугольник за краем картинки «нехваткой
                // памяти»; своего конструктора с текстом у исключения нет.
                throw new OutOfMemoryException();
            }
            var copy = new Bitmap(rect.Width, rect.Height, format);
            for (int y = 0; y < rect.Height; y++)
            {
                for (int x = 0; x < rect.Width; x++)
                {
                    int argb = pixels[(rect.Y + y) * width + rect.X + x];
                    copy.pixels[y * rect.Width + x] = copy.HasAlpha ? argb : argb | unchecked((int)0xFF000000);
                }
            }
            return copy;
        }

        public Bitmap Clone(RectangleF rect, PixelFormat format) => Clone(Rectangle.Truncate(rect), format);

        public Color GetPixel(int x, int y)
        {
            if (x < 0 || x >= width)
            {
                throw new ArgumentOutOfRangeException("x", "Parameter must be positive and < Width.");
            }
            if (y < 0 || y >= height)
            {
                throw new ArgumentOutOfRangeException("y", "Parameter must be positive and < Height.");
            }
            return Color.FromArgb(pixels[y * width + x]);
        }

        public void SetPixel(int x, int y, Color color)
        {
            if (x < 0 || x >= width)
            {
                throw new ArgumentOutOfRangeException("x", "Parameter must be positive and < Width.");
            }
            if (y < 0 || y >= height)
            {
                throw new ArgumentOutOfRangeException("y", "Parameter must be positive and < Height.");
            }
            int argb = color.ToArgb();
            pixels[y * width + x] = HasAlpha ? argb : argb | unchecked((int)0xFF000000);
        }

        // Прозрачным становится каждый точно такой же цвет; без аргумента — цвет
        // левой нижней точки, как у GDI+.
        public void MakeTransparent() => MakeTransparent(GetPixel(0, height - 1));

        public void MakeTransparent(Color transparentColor)
        {
            if (!HasAlpha)
            {
                PixelFormat = PixelFormat.Format32bppArgb;
            }
            int key = transparentColor.ToArgb() | unchecked((int)0xFF000000);
            for (int i = 0; i < pixels.Length; i++)
            {
                if ((pixels[i] | unchecked((int)0xFF000000)) == key && ((pixels[i] >> 24) & 0xFF) == transparentColor.A)
                {
                    pixels[i] = 0;
                }
            }
        }
    }
}

namespace System.Drawing.Imaging
{
    public enum PixelFormat
    {
        Indexed = 0x00010000,
        Gdi = 0x00020000,
        Alpha = 0x00040000,
        PAlpha = 0x00080000,
        Extended = 0x00100000,
        Canonical = 0x00200000,
        Undefined = 0,
        DontCare = 0,
        Format1bppIndexed = 196865,
        Format4bppIndexed = 197634,
        Format8bppIndexed = 198659,
        Format16bppGrayScale = 1052676,
        Format16bppRgb555 = 135173,
        Format16bppRgb565 = 135174,
        Format16bppArgb1555 = 397319,
        Format24bppRgb = 137224,
        Format32bppRgb = 139273,
        Format32bppArgb = 2498570,
        Format32bppPArgb = 925707,
        Format48bppRgb = 1060876,
        Format64bppArgb = 3424269,
        Format64bppPArgb = 1851406,
        Max = 15,
    }
}
