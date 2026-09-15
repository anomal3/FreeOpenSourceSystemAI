// Поле со стрелками NumericUpDown (фаза N7g) — число decimal в пределах
// Minimum..Maximum, шаг Increment, запись с DecimalPlaces знаками, разделителем
// разрядов или шестнадцатеричная.
//
// Иерархия — как у WinForms, и это не украшение: ReadOnly, TextAlign,
// InterceptArrowKeys и Text объявлены у UpDownBase, и программа, собранная
// против настоящей библиотеки, ссылается именно на этот класс.
//
// Правила сняты с WinForms образцом `numbers`: ValueChanged приходит раньше,
// чем обновится Text; стрелка у края значение не меняет и события не даёт;
// сужение Maximum или Minimum подрезает значение с событием (у TrackBar — без).

using System.Drawing;

namespace System.Windows.Forms
{
    public enum HorizontalAlignment
    {
        Left = 0,
        Right = 1,
        Center = 2,
    }

    public enum LeftRightAlignment
    {
        Left = 0,
        Right = 1,
    }

    public abstract class UpDownBase : ContainerControl
    {
        internal const int ButtonWidth = 17;

        protected UpDownBase()
        {
        }

        protected override Size DefaultSize => new Size(120, 23);

        internal override bool Selectable => true;

        internal override Color AmbientBackColor => SystemColors.Window;

        public bool InterceptArrowKeys { get; set; } = true;

        public bool ReadOnly { get; set; }

        public HorizontalAlignment TextAlign { get; set; } = HorizontalAlignment.Left;

        public LeftRightAlignment UpDownAlign { get; set; } = LeftRightAlignment.Right;

        public BorderStyle BorderStyle { get; set; } = BorderStyle.Fixed3D;

        public override string Text
        {
            get => base.Text;
            set => base.Text = value;
        }

        public abstract void UpButton();

        public abstract void DownButton();

        protected abstract void UpdateEditText();

        internal override void ProcessKey(KeyEventArgs e)
        {
            if (!InterceptArrowKeys)
            {
                return;
            }
            if (e.KeyCode == Keys.Up)
            {
                UpButton();
            }
            else if (e.KeyCode == Keys.Down)
            {
                DownButton();
            }
        }

        // Щелчок по стрелкам у правого края: верхняя половина — вверх.
        protected override void OnMouseDown(MouseEventArgs e)
        {
            if (e.X >= Width - ButtonWidth)
            {
                if (e.Y < Height / 2)
                {
                    UpButton();
                }
                else
                {
                    DownButton();
                }
            }
            base.OnMouseDown(e);
        }

        protected override void OnPaintBackground(PaintEventArgs pevent)
        {
            Graphics g = pevent.Graphics;
            g.Clear(Focused ? SystemColors.Highlight : Color.FromArgb(122, 122, 122));
            g.FillRectangle(new SolidBrush(BackColor), 1, 1, Width - 2, Height - 2);
        }

        protected override void OnPaint(PaintEventArgs e)
        {
            Graphics g = e.Graphics;
            string text = Text;
            int textWidth = FreeOsWindow.TextWidth(text);
            int area = Width - ButtonWidth - 6;
            int left = TextAlign == HorizontalAlignment.Right ? 3 + area - textWidth : TextAlign == HorizontalAlignment.Center ? 3 + (area - textWidth) / 2 : 3;
            g.DrawString(text, Font, new SolidBrush(Enabled ? ForeColor : SystemColors.GrayText), left, (Height - FreeOsWindow.TextHeight()) / 2);
            // Две кнопки со стрелками у правого края.
            int x = Width - ButtonWidth - 1;
            int half = (Height - 2) / 2;
            var face = new SolidBrush(Color.FromArgb(225, 225, 225));
            var arrow = new SolidBrush(Color.FromArgb(51, 51, 51));
            g.FillRectangle(face, x, 1, ButtonWidth, half);
            g.FillRectangle(face, x, 1 + half, ButtonWidth, Height - 2 - half);
            g.FillRectangle(new SolidBrush(Color.FromArgb(173, 173, 173)), x, 1 + half, ButtonWidth, 1);
            int cx = x + ButtonWidth / 2;
            for (int i = 0; i < 3; i++)
            {
                g.FillRectangle(arrow, cx - i, 1 + half / 2 - 1 + i, 2 * i + 1, 1);
                g.FillRectangle(arrow, cx - i, 1 + half + (Height - 2 - half) / 2 + 1 - i, 2 * i + 1, 1);
            }
            base.OnPaint(e);
        }
    }

    public class NumericUpDown : UpDownBase, ComponentModel.ISupportInitialize
    {
        private decimal current = decimal.Zero;
        private decimal minimum = decimal.Zero;
        private decimal maximum = new decimal(100);
        private decimal increment = decimal.One;
        private int decimalPlaces;
        private bool hexadecimal;
        private bool thousands;
        private bool initializing;

        public NumericUpDown()
        {
            UpdateEditText();
        }

        public event EventHandler ValueChanged;

        protected virtual void OnValueChanged(EventArgs e) => ValueChanged?.Invoke(this, e);

        public decimal Increment
        {
            get => increment;
            set
            {
                if (value < decimal.Zero)
                {
                    throw new ArgumentOutOfRangeException("value", "Value of '" + value + "' is not valid for 'Increment'. 'Increment' must be greater than or equal to 0.");
                }
                increment = value;
            }
        }

        public int DecimalPlaces
        {
            get => decimalPlaces;
            set
            {
                if (value < 0 || value > 99)
                {
                    throw new ArgumentOutOfRangeException("value", "Value of '" + value + "' is not valid for 'DecimalPlaces'. 'DecimalPlaces' should be between 0 and 99.");
                }
                decimalPlaces = value;
                UpdateEditText();
            }
        }

        public bool Hexadecimal
        {
            get => hexadecimal;
            set
            {
                hexadecimal = value;
                UpdateEditText();
            }
        }

        public bool ThousandsSeparator
        {
            get => thousands;
            set
            {
                thousands = value;
                UpdateEditText();
            }
        }

        public decimal Minimum
        {
            get => minimum;
            set
            {
                minimum = value;
                if (minimum > maximum)
                {
                    maximum = minimum;
                }
                Value = Constrain(current);
            }
        }

        public decimal Maximum
        {
            get => maximum;
            set
            {
                maximum = value;
                if (minimum > maximum)
                {
                    minimum = maximum;
                }
                Value = Constrain(current);
            }
        }

        public decimal Value
        {
            get => current;
            set
            {
                if (value == current)
                {
                    return;
                }
                if (!initializing && (value < minimum || value > maximum))
                {
                    throw new ArgumentOutOfRangeException("value", "Value of '" + value + "' is not valid for 'Value'. 'Value' should be between 'Minimum' and 'Maximum'.");
                }
                current = value;
                OnValueChanged(EventArgs.Empty);
                UpdateEditText();
            }
        }

        public override void UpButton()
        {
            decimal next;
            try
            {
                next = current + increment;
                if (next > maximum)
                {
                    next = maximum;
                }
            }
            catch (OverflowException)
            {
                next = maximum;
            }
            Value = next;
        }

        public override void DownButton()
        {
            decimal next;
            try
            {
                next = current - increment;
                if (next < minimum)
                {
                    next = minimum;
                }
            }
            catch (OverflowException)
            {
                next = minimum;
            }
            Value = next;
        }

        public void BeginInit() => initializing = true;

        public void EndInit()
        {
            initializing = false;
            Value = Constrain(current);
            UpdateEditText();
        }

        private decimal Constrain(decimal value)
        {
            if (value < minimum)
            {
                value = minimum;
            }
            if (value > maximum)
            {
                value = maximum;
            }
            return value;
        }

        protected override void UpdateEditText()
        {
            if (initializing)
            {
                return;
            }
            Text = hexadecimal ? ((long)current).ToString("X") : current.ToString((thousands ? "N" : "F") + decimalPlaces);
        }
    }
}
