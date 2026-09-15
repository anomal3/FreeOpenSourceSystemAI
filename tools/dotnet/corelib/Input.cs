// Ввод с клавиатуры в форме (фаза N7a): Keys и KeyEventArgs, перевод события
// стола в клавишу и символ, поле ввода TextBox и флажок CheckBox.
//
// Стол отдаёт окну программы символ с учётом раскладки, а не код клавиши:
// русская буква приходит буквой. Поэтому KeyDown у буквы другой раскладки
// получает Keys.None — физической клавиши среда не знает, — а KeyPress несёт
// ту самую букву, что и под Windows.

using System.Drawing;

namespace System.Windows.Forms
{
    [Flags]
    public enum Keys
    {
        KeyCode = 0xFFFF,
        Modifiers = unchecked((int)0xFF000000) | 0x10000 | 0x20000 | 0x40000,
        None = 0,
        Back = 8,
        Tab = 9,
        Enter = 13,
        Return = 13,
        ShiftKey = 16,
        ControlKey = 17,
        Menu = 18,
        Escape = 27,
        Space = 32,
        PageUp = 33,
        Prior = 33,
        PageDown = 34,
        Next = 34,
        End = 35,
        Home = 36,
        Left = 37,
        Up = 38,
        Right = 39,
        Down = 40,
        Insert = 45,
        Delete = 46,
        D0 = 48,
        D1 = 49,
        D2 = 50,
        D3 = 51,
        D4 = 52,
        D5 = 53,
        D6 = 54,
        D7 = 55,
        D8 = 56,
        D9 = 57,
        A = 65,
        B = 66,
        C = 67,
        D = 68,
        E = 69,
        F = 70,
        G = 71,
        H = 72,
        I = 73,
        J = 74,
        K = 75,
        L = 76,
        M = 77,
        N = 78,
        O = 79,
        P = 80,
        Q = 81,
        R = 82,
        S = 83,
        T = 84,
        U = 85,
        V = 86,
        W = 87,
        X = 88,
        Y = 89,
        Z = 90,
        Apps = 93,
        F1 = 112,
        Shift = 0x10000,
        Control = 0x20000,
        Alt = 0x40000,
    }

    public class KeyEventArgs : EventArgs
    {
        private bool suppressKeyPress;

        public KeyEventArgs(Keys keyData)
        {
            KeyData = keyData;
        }

        public Keys KeyData { get; }

        public Keys KeyCode => KeyData & Keys.KeyCode;

        public Keys Modifiers => KeyData & ~Keys.KeyCode;

        public int KeyValue => (int)(KeyData & Keys.KeyCode);

        public bool Shift => (KeyData & Keys.Shift) != 0;

        public bool Control => (KeyData & Keys.Control) != 0;

        public bool Alt => (KeyData & Keys.Alt) != 0;

        public bool Handled { get; set; }

        public bool SuppressKeyPress
        {
            get => suppressKeyPress;
            set
            {
                suppressKeyPress = value;
                Handled = value;
            }
        }
    }

    public delegate void KeyEventHandler(object sender, KeyEventArgs e);

    // Событие стола — клавиша и символ WinForms.
    //
    // С фазы N7h у клавиши есть ещё маска модификаторов (1 Shift, 2 Ctrl, 4 Alt)
    // и латинская буква — см. `user_abi::WinEvent::y`. По букве `Keys` берутся
    // от клавиши, как у Windows: Ctrl+O остаётся Ctrl+O в русской раскладке, а
    // Ctrl+H не путается с Backspace, хотя символ у них один — `0x08`.
    internal static class KeyMap
    {
        // `WIN_KEY_NAMED` договора окон: выше — имена клавиш без символа.
        private const int Named = 0x01000000;

        private const int ModShift = 1;
        private const int ModControl = 2;
        private const int ModAlt = 4;

        // Клавишу прямо сейчас разбирает Tab (фаза N7h). Переключатель, в
        // который вошли по Tab, не отмечается, а стрелкой — отмечается: WinForms
        // различает это, спрашивая состояние клавиши Tab.
        internal static bool TabPressed;

        internal static Keys ToKeys(int code) => ToKeys(code, 0, 0);

        internal static Keys ToKeys(int code, int mods, int latin)
        {
            Keys keys = FromLatin(latin);
            if (keys == Keys.None)
            {
                keys = FromSymbol(code);
            }
            if ((mods & ModShift) != 0)
            {
                keys |= Keys.Shift;
            }
            if ((mods & ModControl) != 0)
            {
                keys |= Keys.Control;
            }
            if ((mods & ModAlt) != 0)
            {
                keys |= Keys.Alt;
            }
            return keys;
        }

        private static Keys FromLatin(int latin)
        {
            if (latin >= 'a' && latin <= 'z')
            {
                return (Keys)(latin - 32);
            }
            if (latin >= '0' && latin <= '9')
            {
                return (Keys)latin;
            }
            return Keys.None;
        }

        private static Keys FromSymbol(int code)
        {
            switch (code)
            {
                case 0x08:
                    return Keys.Back;
                case 0x09:
                    return Keys.Tab;
                case 0x0A:
                case 0x0D:
                    return Keys.Enter;
                case 0x1B:
                    return Keys.Escape;
                case 0x20:
                    return Keys.Space;
                case Named + 1:
                    return Keys.Left;
                case Named + 2:
                    return Keys.Right;
                case Named + 3:
                    return Keys.Up;
                case Named + 4:
                    return Keys.Down;
                case Named + 5:
                    return Keys.Home;
                case Named + 6:
                    return Keys.End;
                case Named + 7:
                    return Keys.PageUp;
                case Named + 8:
                    return Keys.PageDown;
                case Named + 9:
                    return Keys.Delete;
                case Named + 10:
                    return Keys.Apps;
            }
            if (code >= 'a' && code <= 'z')
            {
                return (Keys)(code - 32);
            }
            if (code >= 'A' && code <= 'Z')
            {
                return (Keys)code | Keys.Shift;
            }
            if (code >= '0' && code <= '9')
            {
                return (Keys)code;
            }
            return Keys.None;
        }

        // Символ для KeyPress. Enter стола — `\n`, у WinForms это `\r`. С Alt
        // KeyPress не приходит: у Windows это WM_SYSCHAR, элементу он не достаётся.
        internal static bool ToChar(int code, int mods, out char c)
        {
            c = '\0';
            if ((mods & ModAlt) != 0 || code < 0 || code > 0xFFFF)
            {
                return false;
            }
            c = code == 0x0A ? '\r' : (char)code;
            return true;
        }
    }

    public abstract class TextBoxBase : Control
    {
        private int selectionStart;
        private int selectionLength;
        private int maxLength = 32767;
        private bool keepSelection;
        private int scroll;
        private bool readOnly;

        protected TextBoxBase()
        {
        }

        internal override bool Selectable => true;

        internal override Color AmbientBackColor => SystemColors.Window;

        public virtual int MaxLength
        {
            get => maxLength;
            set
            {
                if (value < 0)
                {
                    throw new ArgumentOutOfRangeException("MaxLength", "'" + value + "' is not a valid value for 'MaxLength'. 'MaxLength' must be greater than or equal to 0.");
                }
                maxLength = value;
            }
        }

        public virtual bool Multiline { get; set; }

        public bool ReadOnly
        {
            get => readOnly;
            set
            {
                readOnly = value;
                Invalidate();
            }
        }

        public virtual int TextLength => Text.Length;

        // Текст из кода ставит каретку в начало, как WM_SETTEXT.
        public override string Text
        {
            get => base.Text;
            set
            {
                if (!keepSelection)
                {
                    selectionStart = 0;
                    selectionLength = 0;
                }
                base.Text = value;
                Clamp();
            }
        }

        public int SelectionStart
        {
            get => selectionStart;
            set
            {
                if (value < 0)
                {
                    throw new ArgumentOutOfRangeException("SelectionStart", "'" + value + "' is not a valid value for 'SelectionStart'. 'SelectionStart' must be greater than or equal to 0.");
                }
                Select(value, selectionLength);
            }
        }

        public virtual int SelectionLength
        {
            get => selectionLength;
            set
            {
                if (value < 0)
                {
                    throw new ArgumentOutOfRangeException("SelectionLength", "'" + value + "' is not a valid value for 'SelectionLength'. 'SelectionLength' must be greater than or equal to 0.");
                }
                Select(selectionStart, value);
            }
        }

        public virtual string SelectedText
        {
            get => Text.Substring(selectionStart, selectionLength);
            set => Replace(value ?? string.Empty);
        }

        public void Select(int start, int length)
        {
            if (start < 0)
            {
                throw new ArgumentOutOfRangeException("start", "'" + start + "' is not a valid value for 'start'. 'start' must be greater than or equal to 0.");
            }
            int textLength = TextLength;
            if (length < 0)
            {
                // Выделение назад: от `start + length` до `start`.
                int end = Math.Min(start, textLength);
                start = Math.Max(0, end + length);
                length = end - start;
            }
            selectionStart = Math.Min(start, textLength);
            selectionLength = Math.Max(0, Math.Min(length, textLength - selectionStart));
            Invalidate();
        }

        public void SelectAll() => Select(0, TextLength);

        public void AppendText(string text)
        {
            if (string.IsNullOrEmpty(text))
            {
                return;
            }
            Select(TextLength, 0);
            Replace(text);
        }

        public void Clear() => Text = null;

        private void Clamp()
        {
            int textLength = TextLength;
            selectionStart = Math.Min(selectionStart, textLength);
            selectionLength = Math.Min(selectionLength, textLength - selectionStart);
        }

        // Заменить выделение и поставить каретку за вставленным.
        private void Replace(string insert)
        {
            string text = Text;
            string next = text.Substring(0, selectionStart) + insert + text.Substring(selectionStart + selectionLength);
            selectionStart += insert.Length;
            selectionLength = 0;
            keepSelection = true;
            Text = next;
            keepSelection = false;
        }

        private void MoveCaret(int position)
        {
            selectionStart = Math.Max(0, Math.Min(position, TextLength));
            selectionLength = 0;
            Invalidate();
        }

        // Стрелки, Home, End и листание разбирает само поле: форма не уводит по
        // ним фокус (фаза N7h).
        protected override bool IsInputKey(Keys keyData) => IsNavigationKey(keyData) || base.IsInputKey(keyData);

        internal override void ProcessKey(KeyEventArgs e)
        {
            switch (e.KeyCode)
            {
                case Keys.Left:
                    MoveCaret(selectionLength > 0 ? selectionStart : selectionStart - 1);
                    break;
                case Keys.Right:
                    MoveCaret(selectionLength > 0 ? selectionStart + selectionLength : selectionStart + 1);
                    break;
                case Keys.Home:
                    MoveCaret(0);
                    break;
                case Keys.End:
                    MoveCaret(TextLength);
                    break;
                case Keys.Delete:
                    if (readOnly)
                    {
                        break;
                    }
                    if (selectionLength == 0 && selectionStart < TextLength)
                    {
                        selectionLength = 1;
                    }
                    if (selectionLength > 0)
                    {
                        Replace(string.Empty);
                    }
                    break;
            }
        }

        internal override void ProcessChar(char c)
        {
            if (readOnly)
            {
                return;
            }
            if (c == '\b')
            {
                if (selectionLength == 0 && selectionStart > 0)
                {
                    selectionStart--;
                    selectionLength = 1;
                }
                if (selectionLength > 0)
                {
                    Replace(string.Empty);
                }
                return;
            }
            if (c == '\r')
            {
                if (Multiline)
                {
                    Insert("\r\n");
                }
                return;
            }
            if (c < ' ' || c == '')
            {
                return;
            }
            Insert(c.ToString());
        }

        // Набор, в отличие от Text из кода, упирается в MaxLength.
        private void Insert(string s)
        {
            int room = maxLength - (TextLength - selectionLength);
            if (room <= 0)
            {
                return;
            }
            if (s.Length > room)
            {
                s = s.Substring(0, room);
            }
            Replace(s);
        }

        internal virtual string DisplayText => Text;

        protected override void OnPaintBackground(PaintEventArgs pevent)
        {
            Graphics g = pevent.Graphics;
            g.Clear(Focused ? SystemColors.Highlight : Color.FromArgb(122, 122, 122));
            g.FillRectangle(new SolidBrush(readOnly ? SystemColors.Control : BackColor), 1, 1, Width - 2, Height - 2);
        }

        protected override void OnPaint(PaintEventArgs e)
        {
            Graphics g = e.Graphics;
            string shown = DisplayText;
            int lineHeight = FreeOsWindow.TextHeight();
            int top = Multiline ? 3 : (Height - lineHeight) / 2;
            int caret = FreeOsWindow.TextWidth(shown.Substring(0, selectionStart + selectionLength));
            int room = Width - 8;
            // Каретка всегда видна: текст уезжает влево, когда она дошла до края.
            if (caret - scroll > room)
            {
                scroll = caret - room;
            }
            if (caret - scroll < 0)
            {
                scroll = caret;
            }
            int left = 4 - scroll;
            Brush ink = new SolidBrush(Enabled ? ForeColor : SystemColors.GrayText);
            if (selectionLength > 0)
            {
                string before = shown.Substring(0, selectionStart);
                string selected = shown.Substring(selectionStart, selectionLength);
                int x = left + FreeOsWindow.TextWidth(before);
                int width = FreeOsWindow.TextWidth(selected);
                g.DrawString(before, Font, ink, left, top);
                g.FillRectangle(new SolidBrush(Focused ? SystemColors.Highlight : SystemColors.ControlLight), x, top, width, lineHeight);
                g.DrawString(selected, Font, new SolidBrush(Focused ? SystemColors.HighlightText : ForeColor), x, top);
                g.DrawString(shown.Substring(selectionStart + selectionLength), Font, ink, x + width, top);
            }
            else
            {
                g.DrawString(shown, Font, ink, left, top);
                if (Focused && !readOnly)
                {
                    g.FillRectangle(new SolidBrush(ForeColor), left + caret, top, 1, lineHeight);
                }
            }
            base.OnPaint(e);
        }
    }

    public class TextBox : TextBoxBase
    {
        protected override Size DefaultSize => new Size(100, 23);

        public char PasswordChar { get; set; }

        public bool UseSystemPasswordChar { get; set; }

        internal override string DisplayText
        {
            get
            {
                char mask = UseSystemPasswordChar ? '●' : PasswordChar;
                return mask == '\0' ? Text : new string(mask, Text.Length);
            }
        }
    }

    public enum CheckState
    {
        Unchecked = 0,
        Checked = 1,
        Indeterminate = 2,
    }

    public enum Appearance
    {
        Normal = 0,
        Button = 1,
    }

    public class CheckBox : ButtonBase
    {
        private CheckState checkState;
        private bool fitting;

        public CheckBox()
        {
            TextAlign = ContentAlignment.MiddleLeft;
        }

        protected override Size DefaultSize => new Size(104, 24);

        public bool AutoCheck { get; set; } = true;

        public bool ThreeState { get; set; }

        public Appearance Appearance { get; set; }

        public ContentAlignment CheckAlign { get; set; } = ContentAlignment.MiddleLeft;

        public event EventHandler CheckedChanged;

        public event EventHandler CheckStateChanged;

        public bool Checked
        {
            get => checkState != CheckState.Unchecked;
            set
            {
                if (value != Checked)
                {
                    CheckState = value ? CheckState.Checked : CheckState.Unchecked;
                }
            }
        }

        // Как в WinForms: CheckedChanged — только когда поменялось «отмечен ли»,
        // CheckStateChanged — при всякой смене состояния, и после него.
        public CheckState CheckState
        {
            get => checkState;
            set
            {
                if (checkState == value)
                {
                    return;
                }
                bool was = Checked;
                checkState = value;
                if (was != Checked)
                {
                    OnCheckedChanged(EventArgs.Empty);
                }
                OnCheckStateChanged(EventArgs.Empty);
                Invalidate();
            }
        }

        protected virtual void OnCheckedChanged(EventArgs e) => CheckedChanged?.Invoke(this, e);

        protected virtual void OnCheckStateChanged(EventArgs e) => CheckStateChanged?.Invoke(this, e);

        protected override void OnClick(EventArgs e)
        {
            if (AutoCheck)
            {
                if (checkState == CheckState.Unchecked)
                {
                    CheckState = CheckState.Checked;
                }
                else if (checkState == CheckState.Checked && ThreeState)
                {
                    CheckState = CheckState.Indeterminate;
                }
                else
                {
                    CheckState = CheckState.Unchecked;
                }
            }
            base.OnClick(e);
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

        internal override void DrawContent(Graphics g)
        {
            int top = (Height - 13) / 2;
            g.FillRectangle(new SolidBrush(Focused ? SystemColors.Highlight : Color.FromArgb(51, 51, 51)), 1, top, 13, 13);
            g.FillRectangle(new SolidBrush(Enabled ? SystemColors.Window : SystemColors.Control), 2, top + 1, 11, 11);
            if (checkState == CheckState.Checked)
            {
                var pen = new Pen(Color.FromArgb(51, 51, 51), 2);
                g.DrawLine(pen, 4, top + 6, 6, top + 9);
                g.DrawLine(pen, 6, top + 9, 11, top + 3);
            }
            else if (checkState == CheckState.Indeterminate)
            {
                g.FillRectangle(new SolidBrush(Color.FromArgb(51, 51, 51)), 4, top + 3, 7, 7);
            }
            if (Text.Length > 0)
            {
                g.DrawString(Text, Font, new SolidBrush(Enabled ? ForeColor : SystemColors.GrayText), 19, (Height - FreeOsWindow.TextHeight()) / 2);
            }
        }
    }
}
