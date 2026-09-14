// Таймер и окна сообщений (фаза N7c): System.Windows.Forms.Timer, MessageBox
// и IButtonControl. Таймер тикает в цикле приложения, окно сообщения — модальная
// форма: пока оно открыто, остальные формы рисуются, но ввода не получают.
//
// Строки обходятся циклом по индексу, не foreach (см. IO.cs).

using System.ComponentModel;
using System.Drawing;

namespace System.Windows.Forms
{
    public interface IButtonControl
    {
        DialogResult DialogResult { get; set; }

        void NotifyDefault(bool value);

        void PerformClick();
    }

    public enum MessageBoxButtons
    {
        OK = 0,
        OKCancel = 1,
        AbortRetryIgnore = 2,
        YesNoCancel = 3,
        YesNo = 4,
        RetryCancel = 5,
        CancelTryContinue = 6,
    }

    // Порядок членов — как у .NET: у повторяющихся значений имя даёт первый
    // (`Warning` печатается `Exclamation`, сверено образцом `dialogs`).
    public enum MessageBoxIcon
    {
        None = 0,
        Hand = 16,
        Question = 32,
        Exclamation = 48,
        Asterisk = 64,
        Stop = 16,
        Error = 16,
        Warning = 48,
        Information = 64,
    }

    public enum MessageBoxDefaultButton
    {
        Button1 = 0,
        Button2 = 256,
        Button3 = 512,
        Button4 = 768,
    }

    public class Timer : Component
    {
        private int interval = 100;
        private bool enabled;
        private long due;

        public Timer()
        {
        }

        public Timer(IContainer container)
            : this()
        {
            if (container == null)
            {
                throw new ArgumentNullException("container");
            }
            container.Add(this);
        }

        public event EventHandler Tick;

        public object Tag { get; set; }

        public int Interval
        {
            get => interval;
            set
            {
                if (value < 1)
                {
                    throw new ArgumentOutOfRangeException("value", "Interval value '" + value + "' is not valid. Interval must be greater than 0.");
                }
                interval = value;
                if (enabled)
                {
                    due = Application.NowMs() + interval;
                }
            }
        }

        public virtual bool Enabled
        {
            get => enabled;
            set
            {
                if (enabled == value)
                {
                    return;
                }
                enabled = value;
                if (value)
                {
                    due = Application.NowMs() + interval;
                    Application.AddTimer(this);
                }
                else
                {
                    Application.RemoveTimer(this);
                }
            }
        }

        public void Start() => Enabled = true;

        public void Stop() => Enabled = false;

        protected virtual void OnTick(EventArgs e) => Tick?.Invoke(this, e);

        // Цикл приложения: пора ли тикнуть. Следующий срок считается от мига
        // тика, как у WM_TIMER, — пропущенные тики не копятся.
        internal bool FireIfDue(long now)
        {
            if (!enabled || now < due)
            {
                return false;
            }
            due = now + interval;
            OnTick(EventArgs.Empty);
            return true;
        }

        protected override void Dispose(bool disposing)
        {
            if (disposing)
            {
                Enabled = false;
            }
            base.Dispose(disposing);
        }

        public override string ToString() => "System.Windows.Forms.Timer, Interval: " + interval;
    }

    public static class MessageBox
    {
        public static DialogResult Show(string text) => Show(null, text, string.Empty, MessageBoxButtons.OK, MessageBoxIcon.None, MessageBoxDefaultButton.Button1);

        public static DialogResult Show(string text, string caption) =>
            Show(null, text, caption, MessageBoxButtons.OK, MessageBoxIcon.None, MessageBoxDefaultButton.Button1);

        public static DialogResult Show(string text, string caption, MessageBoxButtons buttons) =>
            Show(null, text, caption, buttons, MessageBoxIcon.None, MessageBoxDefaultButton.Button1);

        public static DialogResult Show(string text, string caption, MessageBoxButtons buttons, MessageBoxIcon icon) =>
            Show(null, text, caption, buttons, icon, MessageBoxDefaultButton.Button1);

        public static DialogResult Show(string text, string caption, MessageBoxButtons buttons, MessageBoxIcon icon, MessageBoxDefaultButton defaultButton) =>
            Show(null, text, caption, buttons, icon, defaultButton);

        public static DialogResult Show(IWin32Window owner, string text) =>
            Show(owner, text, string.Empty, MessageBoxButtons.OK, MessageBoxIcon.None, MessageBoxDefaultButton.Button1);

        public static DialogResult Show(IWin32Window owner, string text, string caption) =>
            Show(owner, text, caption, MessageBoxButtons.OK, MessageBoxIcon.None, MessageBoxDefaultButton.Button1);

        public static DialogResult Show(IWin32Window owner, string text, string caption, MessageBoxButtons buttons) =>
            Show(owner, text, caption, buttons, MessageBoxIcon.None, MessageBoxDefaultButton.Button1);

        public static DialogResult Show(IWin32Window owner, string text, string caption, MessageBoxButtons buttons, MessageBoxIcon icon) =>
            Show(owner, text, caption, buttons, icon, MessageBoxDefaultButton.Button1);

        public static DialogResult Show(IWin32Window owner, string text, string caption, MessageBoxButtons buttons, MessageBoxIcon icon, MessageBoxDefaultButton defaultButton)
        {
            if (buttons < MessageBoxButtons.OK || buttons > MessageBoxButtons.CancelTryContinue)
            {
                throw new ComponentModel.InvalidEnumArgumentException("buttons", (int)buttons, typeof(MessageBoxButtons));
            }
            var window = new MessageBoxWindow(text ?? string.Empty, caption ?? string.Empty, buttons, icon, defaultButton);
            return window.ShowDialog();
        }
    }

    public interface IWin32Window
    {
        IntPtr Handle { get; }
    }

    // Окно сообщения. Кнопки прижаты к правому нижнему углу с постоянными
    // отступами: размер окна зависит от ширины текста, а место кнопки от угла —
    // нет, и стенд целится в кнопку от угла (`Aim::ProgramCorner`).
    //
    // Середина последней кнопки — 52 точки левее и 26 выше правого нижнего угла
    // содержимого, каждая предыдущая — ещё на 88 левее.
    internal sealed class MessageBoxWindow : Form
    {
        internal const int Margin = 12;
        internal const int ButtonWidth = 80;
        internal const int ButtonHeight = 28;
        internal const int ButtonGap = 8;
        private const int IconSize = 32;

        private readonly string[] lines;
        private readonly MessageBoxIcon icon;

        internal MessageBoxWindow(string text, string caption, MessageBoxButtons buttons, MessageBoxIcon icon, MessageBoxDefaultButton defaultButton)
        {
            this.icon = icon;
            lines = text.Replace("\r\n", "\n").Split('\n');
            Text = caption;
            BackColor = SystemColors.Window;
            MaximizeBox = false;
            MinimizeBox = false;

            DialogResult[] results = Results(buttons);
            int lineHeight = FreeOsWindow.TextHeight();
            int widest = 0;
            for (int i = 0; i < lines.Length; i++)
            {
                widest = Math.Max(widest, FreeOsWindow.TextWidth(lines[i]));
            }
            int textLeft = icon == MessageBoxIcon.None ? Margin + 12 : Margin + 12 + IconSize + 12;
            int contentHeight = Math.Max(icon == MessageBoxIcon.None ? 0 : IconSize, lines.Length * lineHeight);
            int buttonsWidth = results.Length * ButtonWidth + (results.Length - 1) * ButtonGap;
            int width = Math.Max(Math.Max(textLeft + widest + 24, buttonsWidth + 2 * Margin), 220);
            int height = 20 + contentHeight + 20 + ButtonHeight + Margin;
            ClientSize = new Size(width, height);

            int x = width - Margin - buttonsWidth;
            int defaultIndex = Math.Min(results.Length - 1, (int)defaultButton / 256);
            for (int i = 0; i < results.Length; i++)
            {
                var button = new Button
                {
                    Text = Label(results[i]),
                    DialogResult = results[i],
                    Location = new Point(x, height - Margin - ButtonHeight),
                    Size = new Size(ButtonWidth, ButtonHeight),
                    TabIndex = i,
                    UseVisualStyleBackColor = true,
                };
                Controls.Add(button);
                if (i == defaultIndex)
                {
                    AcceptButton = button;
                    ActiveControl = null;
                    PreferredFocus = button;
                }
                if (results[i] == DialogResult.Cancel || (results.Length == 1 && results[i] == DialogResult.OK))
                {
                    CancelButton = button;
                }
                x += ButtonWidth + ButtonGap;
            }
        }

        private static DialogResult[] Results(MessageBoxButtons buttons)
        {
            switch (buttons)
            {
                case MessageBoxButtons.OKCancel:
                    return new[] { DialogResult.OK, DialogResult.Cancel };
                case MessageBoxButtons.AbortRetryIgnore:
                    return new[] { DialogResult.Abort, DialogResult.Retry, DialogResult.Ignore };
                case MessageBoxButtons.YesNoCancel:
                    return new[] { DialogResult.Yes, DialogResult.No, DialogResult.Cancel };
                case MessageBoxButtons.YesNo:
                    return new[] { DialogResult.Yes, DialogResult.No };
                case MessageBoxButtons.RetryCancel:
                    return new[] { DialogResult.Retry, DialogResult.Cancel };
                case MessageBoxButtons.CancelTryContinue:
                    return new[] { DialogResult.Cancel, DialogResult.TryAgain, DialogResult.Continue };
                default:
                    return new[] { DialogResult.OK };
            }
        }

        private static string Label(DialogResult result)
        {
            switch (result)
            {
                case DialogResult.Cancel:
                    return "Cancel";
                case DialogResult.Abort:
                    return "Abort";
                case DialogResult.Retry:
                    return "Retry";
                case DialogResult.Ignore:
                    return "Ignore";
                case DialogResult.Yes:
                    return "Yes";
                case DialogResult.No:
                    return "No";
                case DialogResult.TryAgain:
                    return "Try Again";
                case DialogResult.Continue:
                    return "Continue";
                default:
                    return "OK";
            }
        }

        protected override void OnPaint(PaintEventArgs e)
        {
            Graphics g = e.Graphics;
            // Полоса под кнопками — как у окна сообщения Windows.
            int bandTop = ClientSize.Height - Margin - ButtonHeight - Margin;
            g.FillRectangle(new SolidBrush(SystemColors.Control), 0, bandTop, ClientSize.Width, ClientSize.Height - bandTop);
            int textLeft = Margin + 12;
            if (icon != MessageBoxIcon.None)
            {
                Color color;
                string mark;
                switch (icon)
                {
                    case MessageBoxIcon.Hand:
                        color = Color.FromArgb(232, 17, 35);
                        mark = "x";
                        break;
                    case MessageBoxIcon.Question:
                        color = Color.FromArgb(0, 120, 215);
                        mark = "?";
                        break;
                    case MessageBoxIcon.Exclamation:
                        color = Color.FromArgb(255, 185, 0);
                        mark = "!";
                        break;
                    default:
                        color = Color.FromArgb(0, 120, 215);
                        mark = "i";
                        break;
                }
                g.FillRectangle(new SolidBrush(color), textLeft, 20, IconSize, IconSize);
                g.DrawString(mark, Font, Brushes.White, textLeft + (IconSize - FreeOsWindow.TextWidth(mark)) / 2, 20 + (IconSize - FreeOsWindow.TextHeight()) / 2);
                textLeft += IconSize + 12;
            }
            int lineHeight = FreeOsWindow.TextHeight();
            for (int i = 0; i < lines.Length; i++)
            {
                g.DrawString(lines[i], Font, new SolidBrush(SystemColors.WindowText), textLeft, 20 + i * lineHeight);
            }
            base.OnPaint(e);
        }
    }
}

namespace System.ComponentModel
{
    public class InvalidEnumArgumentException : ArgumentException
    {
        public InvalidEnumArgumentException(string argumentName, int invalidValue, Type enumClass)
            : base("The value of argument '" + argumentName + "' (" + invalidValue + ") is invalid for Enum type '" + enumClass.Name + "'.", argumentName)
        {
        }
    }
}
