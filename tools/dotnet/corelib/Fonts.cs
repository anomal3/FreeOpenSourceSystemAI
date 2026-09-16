// Семейства шрифтов и раскладка строки (фаза N9c).
//
// Шрифт у FreeOS один — системный шрифт форм, растеризованный заранее. Имя
// семейства программа видит своё, а кегль честный: строка шрифтом в 20 пунктов
// вдвое шире и выше, чем в 10, — глифы системного шрифта масштабируются
// растеризатором. Кегль, в котором системный шрифт нарисован без масштаба, —
// `Font.BasePixels`: шрифт форм WinForms по умолчанию (Segoe UI 9 pt при
// 96 dpi), так что элементы форм пишут тем же текстом, что и до N9c.

namespace System.Drawing.Text
{
    public enum GenericFontFamilies
    {
        Serif = 0,
        SansSerif = 1,
        Monospace = 2,
    }
}

namespace System.Drawing
{
    public sealed class FontFamily : IDisposable
    {
        public FontFamily(string name)
        {
            if (string.IsNullOrEmpty(name))
            {
                throw new ArgumentException("Value of '' is not valid for 'name'.");
            }
            Name = name;
        }

        public FontFamily(Text.GenericFontFamilies genericFamily)
            : this(genericFamily == Text.GenericFontFamilies.Serif ? "Times New Roman" : genericFamily == Text.GenericFontFamilies.Monospace ? "Courier New" : "Microsoft Sans Serif")
        {
        }

        public string Name { get; }

        public static FontFamily GenericSansSerif => new FontFamily("Microsoft Sans Serif");

        public static FontFamily GenericSerif => new FontFamily("Times New Roman");

        public static FontFamily GenericMonospace => new FontFamily("Courier New");

        public bool IsStyleAvailable(FontStyle style) => true;

        public void Dispose()
        {
        }

        public override bool Equals(object obj) => obj is FontFamily other && other.Name == Name;

        public override int GetHashCode() => Name.GetHashCode();

        public override string ToString() => "[FontFamily: Name=" + Name + "]";
    }

    public enum StringAlignment
    {
        Near = 0,
        Center = 1,
        Far = 2,
    }

    [Flags]
    public enum StringFormatFlags
    {
        DirectionRightToLeft = 0x0001,
        DirectionVertical = 0x0002,
        FitBlackBox = 0x0004,
        DisplayFormatControl = 0x0020,
        NoFontFallback = 0x0400,
        MeasureTrailingSpaces = 0x0800,
        NoWrap = 0x1000,
        LineLimit = 0x2000,
        NoClip = 0x4000,
    }

    public enum StringTrimming
    {
        None = 0,
        Character = 1,
        Word = 2,
        EllipsisCharacter = 3,
        EllipsisWord = 4,
        EllipsisPath = 5,
    }

    public sealed class StringFormat : IDisposable, ICloneable
    {
        public StringFormat()
        {
        }

        public StringFormat(StringFormatFlags options)
        {
            FormatFlags = options;
        }

        public StringFormat(StringFormat format)
        {
            if (format == null)
            {
                throw new ArgumentNullException("format");
            }
            Alignment = format.Alignment;
            LineAlignment = format.LineAlignment;
            FormatFlags = format.FormatFlags;
            Trimming = format.Trimming;
        }

        public StringAlignment Alignment { get; set; }

        public StringAlignment LineAlignment { get; set; }

        public StringFormatFlags FormatFlags { get; set; }

        public StringTrimming Trimming { get; set; } = StringTrimming.Character;

        public static StringFormat GenericDefault => new StringFormat();

        public static StringFormat GenericTypographic => new StringFormat(StringFormatFlags.FitBlackBox | StringFormatFlags.LineLimit | StringFormatFlags.NoClip);

        public object Clone() => new StringFormat(this);

        public void Dispose()
        {
        }
    }

    // Строки текста после переноса: разбиение по `\n` и, если задана ширина, по
    // словам — слово, не влезающее целиком, остаётся на своей строке.
    internal static class TextLayout
    {
        internal static float Width(string text, Font font) => Windows.Forms.FreeOsWindow.TextWidth(text) * font.Scale;

        internal static Collections.Generic.List<string> Lines(string text, Font font, float width, StringFormat format)
        {
            var lines = new Collections.Generic.List<string>();
            bool wrap = width > 0 && (format == null || (format.FormatFlags & StringFormatFlags.NoWrap) == 0);
            string[] paragraphs = text.Replace("\r", "").Split('\n');
            for (int p = 0; p < paragraphs.Length; p++)
            {
                string paragraph = paragraphs[p];
                if (!wrap || Width(paragraph, font) <= width)
                {
                    lines.Add(paragraph);
                    continue;
                }
                string[] words = paragraph.Split(' ');
                string current = "";
                for (int i = 0; i < words.Length; i++)
                {
                    string candidate = current.Length == 0 ? words[i] : current + " " + words[i];
                    if (current.Length > 0 && Width(candidate, font) > width)
                    {
                        lines.Add(current);
                        current = words[i];
                    }
                    else
                    {
                        current = candidate;
                    }
                }
                lines.Add(current);
            }
            return lines;
        }
    }
}
