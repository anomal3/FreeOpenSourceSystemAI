// Частые элементы формы (фаза N7e): переключатели RadioButton в рамке GroupBox,
// полоса хода ProgressBar и ползунок TrackBar из дизайнера.
//
// Правила сняты с WinForms образцом `choices`: отмеченный переключатель сначала
// снимает отметку с соседей — с их событиями — и только потом сообщает о себе;
// TabStop у переключателя следует за Checked; сужение диапазона ползунка или
// полосы хода сдвигает значение без события.

using System.Drawing;

namespace System.ComponentModel
{
    public interface ISupportInitialize
    {
        void BeginInit();

        void EndInit();
    }
}

namespace System.Windows.Forms
{
    public enum Orientation
    {
        Horizontal = 0,
        Vertical = 1,
    }

    public enum TickStyle
    {
        None = 0,
        TopLeft = 1,
        BottomRight = 2,
        Both = 3,
    }

    public enum ProgressBarStyle
    {
        Blocks = 0,
        Continuous = 1,
        Marquee = 2,
    }

    public class RadioButton : ButtonBase
    {
        private bool isChecked;
        private bool fitting;

        public RadioButton()
        {
            TextAlign = ContentAlignment.MiddleLeft;
            TabStop = false;
        }

        protected override Size DefaultSize => new Size(104, 24);

        public bool AutoCheck { get; set; } = true;

        public Appearance Appearance { get; set; }

        public ContentAlignment CheckAlign { get; set; } = ContentAlignment.MiddleLeft;

        public event EventHandler CheckedChanged;

        // Порядок WinForms: своя отметка и TabStop, затем соседи теряют отметку со
        // своими событиями, и последним — своё событие.
        public bool Checked
        {
            get => isChecked;
            set
            {
                if (isChecked == value)
                {
                    return;
                }
                isChecked = value;
                TabStop = value;
                if (value)
                {
                    UncheckSiblings();
                }
                Invalidate();
                OnCheckedChanged(EventArgs.Empty);
            }
        }

        protected virtual void OnCheckedChanged(EventArgs e) => CheckedChanged?.Invoke(this, e);

        private void UncheckSiblings()
        {
            if (Parent == null || !AutoCheck)
            {
                return;
            }
            for (int i = 0; i < Parent.Controls.Count; i++)
            {
                var other = Parent.Controls[i] as RadioButton;
                if (other != null && other != this && other.AutoCheck)
                {
                    other.Checked = false;
                }
            }
        }

        protected override void OnClick(EventArgs e)
        {
            if (AutoCheck)
            {
                Checked = true;
            }
            base.OnClick(e);
        }

        public void PerformClick()
        {
            if (CanSelect)
            {
                OnClick(EventArgs.Empty);
            }
        }

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
            Size = new Size(FreeOsWindow.TextWidth(Text) + 22, Math.Max(FreeOsWindow.TextHeight() + 4, 17));
            fitting = false;
        }

        internal override void DrawFace(Graphics g) => g.Clear(BackColor);

        // Полуширина строки круга радиуса r на расстоянии d от середины.
        private static int Half(double r, int d) => (int)Math.Sqrt(r * r - d * d);

        internal override void DrawContent(Graphics g)
        {
            int top = (Height - 13) / 2;
            var edge = new SolidBrush(Focused ? SystemColors.Highlight : Color.FromArgb(51, 51, 51));
            var face = new SolidBrush(Enabled ? SystemColors.Window : SystemColors.Control);
            for (int row = 0; row < 13; row++)
            {
                int half = Half(6.5, row - 6);
                g.FillRectangle(edge, 7 - half, top + row, 2 * half + 1, 1);
                if (row > 0 && row < 12)
                {
                    g.FillRectangle(face, 8 - half, top + row, 2 * half - 1, 1);
                }
            }
            if (isChecked)
            {
                var dot = new SolidBrush(Color.FromArgb(51, 51, 51));
                for (int row = 0; row < 7; row++)
                {
                    int half = Half(3.5, row - 3);
                    g.FillRectangle(dot, 7 - half, top + 3 + row, 2 * half + 1, 1);
                }
            }
            if (Text.Length > 0)
            {
                g.DrawString(Text, Font, new SolidBrush(Enabled ? ForeColor : SystemColors.GrayText), 19, (Height - FreeOsWindow.TextHeight()) / 2);
            }
        }
    }

    public class GroupBox : Control
    {
        public GroupBox()
        {
            TabStop = false;
        }

        protected override Size DefaultSize => new Size(200, 100);

        public FlatStyle FlatStyle { get; set; } = FlatStyle.Standard;

        public bool UseCompatibleTextRendering { get; set; }

        // Рамка на середине строки заголовка, заголовок — поверх неё.
        protected override void OnPaint(PaintEventArgs e)
        {
            Graphics g = e.Graphics;
            int textHeight = FreeOsWindow.TextHeight();
            int lineY = textHeight / 2;
            g.DrawRectangle(new Pen(Color.FromArgb(220, 220, 220)), 0, lineY, Width - 1, Height - lineY - 1);
            string text = Text;
            if (text.Length > 0)
            {
                g.FillRectangle(new SolidBrush(BackColor), 6, 0, FreeOsWindow.TextWidth(text) + 4, textHeight);
                g.DrawString(text, Font, new SolidBrush(Enabled ? ForeColor : SystemColors.GrayText), 8, 0);
            }
            base.OnPaint(e);
        }
    }

    public class ProgressBar : Control
    {
        private int minimum;
        private int maximum = 100;
        private int current;

        public ProgressBar()
        {
            TabStop = false;
        }

        protected override Size DefaultSize => new Size(100, 23);

        public int Step { get; set; } = 10;

        public ProgressBarStyle Style { get; set; } = ProgressBarStyle.Blocks;

        public int MarqueeAnimationSpeed { get; set; } = 100;

        public int Minimum
        {
            get => minimum;
            set
            {
                if (minimum == value)
                {
                    return;
                }
                if (value < 0)
                {
                    throw new ArgumentOutOfRangeException("value", "Value of '" + value + "' is not valid for 'Minimum'. 'Minimum' must be greater than or equal to 0.");
                }
                if (maximum < value)
                {
                    maximum = value;
                }
                minimum = value;
                if (current < minimum)
                {
                    current = minimum;
                }
                Invalidate();
            }
        }

        public int Maximum
        {
            get => maximum;
            set
            {
                if (maximum == value)
                {
                    return;
                }
                if (value < 0)
                {
                    throw new ArgumentOutOfRangeException("value", "Value of '" + value + "' is not valid for 'Maximum'. 'Maximum' must be greater than or equal to 0.");
                }
                if (minimum > value)
                {
                    minimum = value;
                }
                maximum = value;
                if (current > maximum)
                {
                    current = maximum;
                }
                Invalidate();
            }
        }

        public int Value
        {
            get => current;
            set
            {
                if (current == value)
                {
                    return;
                }
                if (value < minimum || value > maximum)
                {
                    throw new ArgumentOutOfRangeException("value", "Value of '" + value + "' is not valid for 'Value'. 'Value' should be between 'minimum' and 'maximum'.");
                }
                current = value;
                Invalidate();
            }
        }

        public void Increment(int value)
        {
            current = Math.Max(minimum, Math.Min(maximum, current + value));
            Invalidate();
        }

        public void PerformStep() => Increment(Step);

        protected override void OnPaintBackground(PaintEventArgs pevent)
        {
            Graphics g = pevent.Graphics;
            g.Clear(Color.FromArgb(188, 188, 188));
            g.FillRectangle(new SolidBrush(Color.FromArgb(230, 230, 230)), 1, 1, Width - 2, Height - 2);
        }

        protected override void OnPaint(PaintEventArgs e)
        {
            int range = maximum - minimum;
            if (range > 0 && current > minimum)
            {
                int filled = (int)((long)(Width - 2) * (current - minimum) / range);
                e.Graphics.FillRectangle(new SolidBrush(Color.FromArgb(6, 176, 37)), 1, 1, filled, Height - 2);
            }
            base.OnPaint(e);
        }
    }

    public class TrackBar : Control, ComponentModel.ISupportInitialize
    {
        private const int Inset = 8;

        private int minimum;
        private int maximum = 10;
        private int current;
        private bool initializing;

        public TrackBar()
        {
            TabStop = true;
        }

        protected override Size DefaultSize => new Size(104, 45);

        internal override bool Selectable => true;

        public int SmallChange { get; set; } = 1;

        public int LargeChange { get; set; } = 5;

        public int TickFrequency { get; set; } = 1;

        public Orientation Orientation { get; set; }

        public TickStyle TickStyle { get; set; } = TickStyle.BottomRight;

        public event EventHandler ValueChanged;

        public event EventHandler Scroll;

        protected virtual void OnValueChanged(EventArgs e) => ValueChanged?.Invoke(this, e);

        protected virtual void OnScroll(EventArgs e) => Scroll?.Invoke(this, e);

        // Пока дизайнер заполняет свойства, значение за диапазоном не отказ:
        // диапазон могут задать позже значения.
        public void BeginInit() => initializing = true;

        public void EndInit()
        {
            initializing = false;
            current = Math.Max(minimum, Math.Min(maximum, current));
        }

        public int Minimum
        {
            get => minimum;
            set
            {
                if (minimum != value)
                {
                    SetRange(value, Math.Max(maximum, value));
                }
            }
        }

        public int Maximum
        {
            get => maximum;
            set
            {
                if (maximum != value)
                {
                    SetRange(Math.Min(minimum, value), value);
                }
            }
        }

        // Значение за новым диапазоном подрезается молча — без ValueChanged,
        // как в WinForms.
        public void SetRange(int minValue, int maxValue)
        {
            if (minimum == minValue && maximum == maxValue)
            {
                return;
            }
            if (minValue > maxValue)
            {
                maxValue = minValue;
            }
            minimum = minValue;
            maximum = maxValue;
            if (!initializing)
            {
                current = Math.Max(minimum, Math.Min(maximum, current));
            }
            Invalidate();
        }

        public int Value
        {
            get => current;
            set
            {
                if (current == value)
                {
                    return;
                }
                if (!initializing && (value < minimum || value > maximum))
                {
                    throw new ArgumentOutOfRangeException("value", "Value of '" + value + "' is not valid for 'Value'. 'Value' should be between 'Minimum' and 'Maximum'.");
                }
                current = value;
                Invalidate();
                OnValueChanged(EventArgs.Empty);
            }
        }

        // Ход человека: значение подрезается к диапазону, потом Scroll.
        private void MoveTo(int value)
        {
            Value = Math.Max(minimum, Math.Min(maximum, value));
            OnScroll(EventArgs.Empty);
        }

        internal override void ProcessKey(KeyEventArgs e)
        {
            switch (e.KeyCode)
            {
                case Keys.Left:
                case Keys.Down:
                    MoveTo(current - SmallChange);
                    break;
                case Keys.Right:
                case Keys.Up:
                    MoveTo(current + SmallChange);
                    break;
                case Keys.PageDown:
                    MoveTo(current - LargeChange);
                    break;
                case Keys.PageUp:
                    MoveTo(current + LargeChange);
                    break;
                case Keys.Home:
                    MoveTo(minimum);
                    break;
                case Keys.End:
                    MoveTo(maximum);
                    break;
            }
        }

        private int Track => Math.Max(1, Width - 2 * Inset);

        private int ThumbX => Inset + (maximum > minimum ? (int)((long)Track * (current - minimum) / (maximum - minimum)) : 0);

        protected override void OnMouseDown(MouseEventArgs e)
        {
            int range = maximum - minimum;
            int value = minimum + (range > 0 ? (int)(((long)(e.X - Inset) * range + Track / 2) / Track) : 0);
            MoveTo(value);
            base.OnMouseDown(e);
        }

        protected override void OnPaint(PaintEventArgs e)
        {
            Graphics g = e.Graphics;
            g.FillRectangle(new SolidBrush(Color.FromArgb(214, 214, 214)), Inset, 12, Track, 4);
            g.FillRectangle(new SolidBrush(Color.FromArgb(231, 234, 234)), Inset + 1, 13, Track - 2, 2);
            if (TickStyle != TickStyle.None && TickFrequency > 0 && maximum > minimum)
            {
                var tick = new SolidBrush(Color.FromArgb(160, 160, 160));
                for (int v = minimum; v <= maximum; v += TickFrequency)
                {
                    int x = Inset + (int)((long)Track * (v - minimum) / (maximum - minimum));
                    g.FillRectangle(tick, x, 30, 1, 4);
                }
            }
            g.FillRectangle(new SolidBrush(Focused ? SystemColors.Highlight : Color.FromArgb(0, 122, 217)), ThumbX - 5, 4, 11, 21);
            base.OnPaint(e);
        }
    }
}
