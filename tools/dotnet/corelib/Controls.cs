// Элементы формы (фаза N6b): Button и Label — то, что кладёт на форму
// дизайнер шаблона `dotnet new winforms`. Рисуются в духе Windows 10: плоская
// светлая кнопка с серой рамкой и текстом по центру, надпись без фона.

using System.Drawing;

namespace System.Drawing
{
    public enum ContentAlignment
    {
        TopLeft = 1,
        TopCenter = 2,
        TopRight = 4,
        MiddleLeft = 16,
        MiddleCenter = 32,
        MiddleRight = 64,
        BottomLeft = 256,
        BottomCenter = 512,
        BottomRight = 1024,
    }
}

namespace System.Windows.Forms.Layout
{
    public class ArrangedElementCollection
    {
        public virtual int Count => 0;
    }
}

namespace System.Windows.Forms
{
    public enum FlatStyle
    {
        Flat = 0,
        Popup = 1,
        Standard = 2,
        System = 3,
    }

    public enum DialogResult
    {
        None = 0,
        OK = 1,
        Cancel = 2,
        Abort = 3,
        Retry = 4,
        Ignore = 5,
        Yes = 6,
        No = 7,
        TryAgain = 10,
        Continue = 11,
    }

    public abstract class ButtonBase : Control
    {
        protected ButtonBase()
        {
        }

        protected override Size DefaultSize => new Size(75, 23);

        public bool UseVisualStyleBackColor { get; set; }

        public FlatStyle FlatStyle { get; set; } = FlatStyle.Standard;

        public virtual ContentAlignment TextAlign { get; set; } = ContentAlignment.MiddleCenter;

        // Кнопка нажимается левой кнопкой мыши; правая до Click не доходит.
        internal override bool ClicksWithLeftOnly => true;

        internal override bool Selectable => true;

        protected override void OnPaintBackground(PaintEventArgs pevent) => DrawFace(pevent.Graphics);

        protected override void OnPaint(PaintEventArgs e)
        {
            DrawContent(e.Graphics);
            base.OnPaint(e);
        }

        // Рамка — синяя у кнопки с фокусом.
        internal virtual void DrawFace(Graphics g)
        {
            g.Clear(Focused ? SystemColors.Highlight : Color.FromArgb(173, 173, 173));
            Color face = UseVisualStyleBackColor ? Color.FromArgb(225, 225, 225) : BackColor;
            g.FillRectangle(new SolidBrush(face), 1, 1, Width - 2, Height - 2);
        }

        internal virtual void DrawContent(Graphics g)
        {
            string text = Text;
            if (text.Length > 0)
            {
                int textWidth = FreeOsWindow.TextWidth(text);
                int textHeight = FreeOsWindow.TextHeight();
                g.DrawString(text, Font, new SolidBrush(Enabled ? ForeColor : SystemColors.GrayText),
                    (Width - textWidth) / 2, (Height - textHeight) / 2);
            }
        }

        // Пробел нажимает кнопку и переключает флажок, как в WinForms.
        internal override void ProcessKey(KeyEventArgs e)
        {
            if (e.KeyCode == Keys.Space)
            {
                OnClick(EventArgs.Empty);
            }
        }
    }

    public class Button : ButtonBase
    {
        public DialogResult DialogResult { get; set; }

        // Как в WinForms: нажатие из кода — только у видимой и доступной кнопки.
        public void PerformClick()
        {
            if (Visible && Enabled)
            {
                OnClick(EventArgs.Empty);
            }
        }

        // У кнопки с фокусом Enter — тоже нажатие.
        internal override void ProcessKey(KeyEventArgs e)
        {
            if (e.KeyCode == Keys.Enter)
            {
                PerformClick();
                return;
            }
            base.ProcessKey(e);
        }
    }

    public class Label : Control
    {
        private bool fitting;

        public Label()
        {
            TabStop = false;
        }

        protected override Size DefaultSize => new Size(100, 23);

        public ContentAlignment TextAlign { get; set; } = ContentAlignment.TopLeft;

        // Надпись с AutoSize сама берёт размер по тексту, и размер из дизайнера
        // ей не указ — как в WinForms.
        public override bool AutoSize
        {
            get => base.AutoSize;
            set
            {
                base.AutoSize = value;
                Fit();
            }
        }

        public override string Text
        {
            get => base.Text;
            set
            {
                base.Text = value;
                Fit();
            }
        }

        protected override void OnResize(EventArgs e)
        {
            base.OnResize(e);
            Fit();
        }

        private void Fit()
        {
            if (!AutoSize || fitting)
            {
                return;
            }
            fitting = true;
            Size = new Size(FreeOsWindow.TextWidth(Text) + 3, FreeOsWindow.TextHeight() + 2);
            fitting = false;
        }

        protected override void OnPaint(PaintEventArgs e)
        {
            e.Graphics.DrawString(Text, Font, new SolidBrush(Enabled ? ForeColor : SystemColors.GrayText), 0, 1);
            base.OnPaint(e);
        }
    }
}
