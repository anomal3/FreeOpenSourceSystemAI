// System.Windows.Forms (фаза N6a): Application.Run, Form и её события, дерево
// элементов, рисование через Paint и щелчки мышью поверх окна программы FreeOS.
//
// Цикл сообщений — здесь, на C#: окно и его события даёт среда
// (`FreeOsWindow`), всё остальное — порядок событий, отсечение, кто получил
// щелчок — повторяет WinForms. Порядок проверен образцом `form` против
// настоящего WinForms: Load, Shown, первый Paint после Shown, FormClosing,
// FormClosed.

using System.Collections.Generic;
using System.ComponentModel;
using System.Drawing;
using System.Runtime.CompilerServices;
using System.Threading;

namespace System.ComponentModel
{
    public interface IComponent : IDisposable
    {
    }

    public interface IContainer : IDisposable
    {
        void Add(IComponent component);

        void Remove(IComponent component);
    }

    public class Container : IContainer
    {
        private readonly List<IComponent> components = new List<IComponent>();

        public void Add(IComponent component)
        {
            if (component != null)
            {
                components.Add(component);
            }
        }

        public void Remove(IComponent component) => components.Remove(component);

        public void Dispose()
        {
            Dispose(true);
        }

        protected virtual void Dispose(bool disposing)
        {
            if (!disposing)
            {
                return;
            }
            for (int i = components.Count - 1; i >= 0; i--)
            {
                components[i].Dispose();
            }
            components.Clear();
        }
    }

    public class Component : IComponent
    {
        private bool disposed;

        public event EventHandler Disposed;

        internal bool IsComponentDisposed => disposed;

        public void Dispose()
        {
            Dispose(true);
        }

        protected virtual void Dispose(bool disposing)
        {
            if (disposing && !disposed)
            {
                disposed = true;
                Disposed?.Invoke(this, EventArgs.Empty);
            }
        }
    }

    public class CancelEventArgs : EventArgs
    {
        public CancelEventArgs()
        {
        }

        public CancelEventArgs(bool cancel)
        {
            Cancel = cancel;
        }

        public bool Cancel { get; set; }
    }
}

namespace System.Windows.Forms
{
    // Окно программы, как его даёт среда. Номер окна — неотрицательный,
    // отрицательное — отказ.
    internal static class FreeOsWindow
    {
        internal const int EventNone = 0;
        internal const int EventKey = 1;
        internal const int EventPointer = 2;
        internal const int EventClose = 3;

        [MethodImpl(MethodImplOptions.InternalCall)]
        internal static extern int Open(string title, int width, int height);

        [MethodImpl(MethodImplOptions.InternalCall)]
        internal static extern void Fill(int window, int x, int y, int width, int height, int argb);

        [MethodImpl(MethodImplOptions.InternalCall)]
        internal static extern void Text(int window, int x, int y, string text, int argb, int clipX, int clipY, int clipWidth, int clipHeight);

        [MethodImpl(MethodImplOptions.InternalCall)]
        internal static extern int TextWidth(string text);

        [MethodImpl(MethodImplOptions.InternalCall)]
        internal static extern int TextHeight();

        [MethodImpl(MethodImplOptions.InternalCall)]
        internal static extern void Present(int window);

        // Вид события (`Event*`), для щелчка — точка и кнопки, для клавиши — символ.
        [MethodImpl(MethodImplOptions.InternalCall)]
        internal static extern int NextEvent(int window, out int x, out int y, out int code);

        [MethodImpl(MethodImplOptions.InternalCall)]
        internal static extern void Close(int window);
    }

    public enum HighDpiMode
    {
        DpiUnaware = 0,
        SystemAware = 1,
        PerMonitor = 2,
        PerMonitorV2 = 3,
        DpiUnawareGdiScaled = 4,
    }

    public enum AutoScaleMode
    {
        None = 0,
        Font = 1,
        Dpi = 2,
        Inherit = 3,
    }

    [Flags]
    public enum MouseButtons
    {
        None = 0,
        Left = 0x100000,
        Right = 0x200000,
        Middle = 0x400000,
        XButton1 = 0x800000,
        XButton2 = 0x1000000,
    }

    public enum CloseReason
    {
        None = 0,
        WindowsShutDown = 1,
        MdiFormClosing = 2,
        UserClosing = 3,
        TaskManagerClosing = 4,
        FormOwnerClosing = 5,
        ApplicationExitCall = 6,
    }

    public enum FormStartPosition
    {
        Manual = 0,
        CenterScreen = 1,
        WindowsDefaultLocation = 2,
        WindowsDefaultBounds = 3,
        CenterParent = 4,
    }

    public enum FormBorderStyle
    {
        None = 0,
        FixedSingle = 1,
        Fixed3D = 2,
        FixedDialog = 3,
        Sizable = 4,
        FixedToolWindow = 5,
        SizableToolWindow = 6,
    }

    public class PaintEventArgs : EventArgs, IDisposable
    {
        public PaintEventArgs(Graphics graphics, Rectangle clipRect)
        {
            Graphics = graphics;
            ClipRectangle = clipRect;
        }

        public Graphics Graphics { get; }

        public Rectangle ClipRectangle { get; }

        public void Dispose()
        {
        }
    }

    public delegate void PaintEventHandler(object sender, PaintEventArgs e);

    public class MouseEventArgs : EventArgs
    {
        public MouseEventArgs(MouseButtons button, int clicks, int x, int y, int delta)
        {
            Button = button;
            Clicks = clicks;
            X = x;
            Y = y;
            Delta = delta;
        }

        public MouseButtons Button { get; }

        public int Clicks { get; }

        public int X { get; }

        public int Y { get; }

        public int Delta { get; }

        public Point Location => new Point(X, Y);
    }

    public delegate void MouseEventHandler(object sender, MouseEventArgs e);

    public class KeyPressEventArgs : EventArgs
    {
        public KeyPressEventArgs(char keyChar)
        {
            KeyChar = keyChar;
        }

        public char KeyChar { get; set; }

        public bool Handled { get; set; }
    }

    public delegate void KeyPressEventHandler(object sender, KeyPressEventArgs e);

    public class FormClosingEventArgs : CancelEventArgs
    {
        public FormClosingEventArgs(CloseReason closeReason, bool cancel)
            : base(cancel)
        {
            CloseReason = closeReason;
        }

        public CloseReason CloseReason { get; }
    }

    public delegate void FormClosingEventHandler(object sender, FormClosingEventArgs e);

    public class FormClosedEventArgs : EventArgs
    {
        public FormClosedEventArgs(CloseReason closeReason)
        {
            CloseReason = closeReason;
        }

        public CloseReason CloseReason { get; }
    }

    public delegate void FormClosedEventHandler(object sender, FormClosedEventArgs e);

    public static class Application
    {
        private static readonly List<Form> openForms = new List<Form>();
        private static readonly Queue<Action> posted = new Queue<Action>();

        // Включённые таймеры (фаза N7c) и стопка модальных форм: ввод получает
        // только верхняя из них.
        private static readonly List<Timer> timers = new List<Timer>();
        private static readonly List<Form> modal = new List<Form>();

        // Сколько спать, когда делать нечего: окно отвечает на щелчок за кадр
        // стола, а холостой оборот интерпретатора не жжёт процессор.
        private const int IdleSleepMs = 20;

        public static void EnableVisualStyles()
        {
        }

        public static void SetCompatibleTextRenderingDefault(bool defaultValue)
        {
        }

        public static bool SetHighDpiMode(HighDpiMode highDpiMode) => true;

        internal static void Post(Action action) => posted.Enqueue(action);

        public static void Run(Form mainForm)
        {
            if (mainForm == null)
            {
                throw new ArgumentNullException("mainForm");
            }
            if (mainForm.IsDisposed)
            {
                throw new ObjectDisposedException("Form");
            }
            mainForm.Show();
            while (!mainForm.IsDisposed)
            {
                if (!DoEventsOnce())
                {
                    Thread.Sleep(IdleSleepMs);
                }
            }
            while (posted.Count > 0)
            {
                posted.Dequeue();
            }
        }

        public static void DoEvents() => DoEventsOnce();

        // Один оборот цикла: отложенное, таймеры, события окон, перерисовка.
        // `false` — делать было нечего.
        private static bool DoEventsOnce()
        {
            bool busy = false;
            while (posted.Count > 0)
            {
                posted.Dequeue()();
                busy = true;
            }
            if (timers.Count > 0)
            {
                // Копия: обработчик тика вправе остановить и запустить таймеры.
                Timer[] due = timers.ToArray();
                long now = NowMs();
                for (int i = 0; i < due.Length; i++)
                {
                    if (due[i].FireIfDue(now))
                    {
                        busy = true;
                    }
                }
            }
            for (int i = openForms.Count - 1; i >= 0; i--)
            {
                if (i >= openForms.Count)
                {
                    continue;
                }
                Form form = openForms[i];
                // Под модальным окном форма рисуется, но ввода не получает:
                // её события выбираются и выбрасываются.
                bool blocked = modal.Count > 0 && modal[modal.Count - 1] != form;
                while (blocked ? form.DiscardEvent() : form.PumpEvent())
                {
                    busy = true;
                }
            }
            for (int i = openForms.Count - 1; i >= 0; i--)
            {
                if (i < openForms.Count && openForms[i].PaintIfNeeded())
                {
                    busy = true;
                }
            }
            return busy;
        }

        public static void Exit()
        {
            for (int i = openForms.Count - 1; i >= 0; i--)
            {
                if (i < openForms.Count)
                {
                    openForms[i].CloseFor(CloseReason.ApplicationExitCall);
                }
            }
        }

        internal static void Opened(Form form) => openForms.Add(form);

        internal static void Closed(Form form) => openForms.Remove(form);

        internal static long NowMs() => Diagnostics.Stopwatch.GetTimestamp() / 10000;

        internal static void AddTimer(Timer timer)
        {
            if (!timers.Contains(timer))
            {
                timers.Add(timer);
            }
        }

        internal static void RemoveTimer(Timer timer) => timers.Remove(timer);

        internal static bool IsModal(Form form) => modal.Contains(form);

        // Свой цикл событий для ShowDialog: пока форма открыта. Кнопка с
        // DialogResult закрывает её после обработчика, как в WinForms; отказ в
        // FormClosing возвращает DialogResult к None.
        internal static void RunModal(Form form)
        {
            modal.Add(form);
            try
            {
                while (!form.IsDisposed)
                {
                    bool busy = DoEventsOnce();
                    if (!form.IsDisposed && form.DialogResult != DialogResult.None)
                    {
                        form.CloseFor(CloseReason.None);
                        if (!form.IsDisposed)
                        {
                            form.DialogResult = DialogResult.None;
                        }
                        continue;
                    }
                    if (!busy)
                    {
                        Thread.Sleep(IdleSleepMs);
                    }
                }
            }
            finally
            {
                modal.Remove(form);
            }
        }
    }

    public class Control : Component
    {
        private static readonly Font defaultFont = new Font("Segoe UI", 9F);

        private string text = string.Empty;
        private int x;
        private int y;
        private int width;
        private int height;
        private Color backColor;
        private Color foreColor;
        private Font font;
        private int layoutSuspended;

        public Control()
        {
            Controls = new ControlCollection(this);
            // Флаг, а не свойство: свойство зовёт SetVisibleCore, а у формы это
            // показ окна, которого из базового конструктора быть не должно.
            VisibleState = true;
            Enabled = true;
            Size = DefaultSize;
        }

        public static Font DefaultFont => defaultFont;

        protected virtual Size DefaultSize => Size.Empty;

        public Control Parent { get; private set; }

        public ControlCollection Controls { get; }

        public string Name { get; set; } = string.Empty;

        public int TabIndex { get; set; }

        public bool TabStop { get; set; } = true;

        public object Tag { get; set; }

        public bool Enabled { get; set; }

        public virtual bool AutoSize { get; set; }

        public bool IsDisposed => IsComponentDisposed;

        internal bool VisibleState { get; set; }

        public bool Visible
        {
            get => VisibleState && (Parent == null || Parent.Visible);
            set
            {
                if (VisibleState != value)
                {
                    SetVisibleCore(value);
                }
            }
        }

        protected virtual void SetVisibleCore(bool value)
        {
            VisibleState = value;
            Invalidate();
        }

        public virtual string Text
        {
            get => text;
            set
            {
                value = value ?? string.Empty;
                if (text == value)
                {
                    return;
                }
                text = value;
                OnTextChanged(EventArgs.Empty);
                Invalidate();
            }
        }

        public Color BackColor
        {
            get
            {
                if (!backColor.IsEmpty)
                {
                    return backColor;
                }
                return AmbientBackColor;
            }
            set
            {
                backColor = value;
                Invalidate();
            }
        }

        public Color ForeColor
        {
            get
            {
                if (!foreColor.IsEmpty)
                {
                    return foreColor;
                }
                return Parent != null ? Parent.ForeColor : SystemColors.ControlText;
            }
            set
            {
                foreColor = value;
                Invalidate();
            }
        }

        public Font Font
        {
            get => font ?? (Parent != null ? Parent.Font : defaultFont);
            set
            {
                font = value;
                Invalidate();
            }
        }

        public int Left
        {
            get => x;
            set => SetBounds(value, y, width, height);
        }

        public int Top
        {
            get => y;
            set => SetBounds(x, value, width, height);
        }

        public int Width
        {
            get => width;
            set => SetBounds(x, y, value, height);
        }

        public int Height
        {
            get => height;
            set => SetBounds(x, y, width, value);
        }

        public int Right => x + width;

        public int Bottom => y + height;

        public Point Location
        {
            get => new Point(x, y);
            set => SetBounds(value.X, value.Y, width, height);
        }

        public Size Size
        {
            get => new Size(width, height);
            set => SetBounds(x, y, value.Width, value.Height);
        }

        public Rectangle Bounds
        {
            get => new Rectangle(x, y, width, height);
            set => SetBounds(value.X, value.Y, value.Width, value.Height);
        }

        public virtual Size ClientSize
        {
            get => new Size(width, height);
            set => SetBounds(x, y, value.Width, value.Height);
        }

        public Rectangle ClientRectangle => new Rectangle(0, 0, ClientSize.Width, ClientSize.Height);

        public Rectangle DisplayRectangle => ClientRectangle;

        public void SetBounds(int x, int y, int width, int height)
        {
            if (this.x == x && this.y == y && this.width == width && this.height == height)
            {
                return;
            }
            bool resized = this.width != width || this.height != height;
            this.x = x;
            this.y = y;
            this.width = width;
            this.height = height;
            if (resized)
            {
                OnResize(EventArgs.Empty);
            }
            Invalidate();
        }

        public event EventHandler Click;

        public event EventHandler TextChanged;

        public event EventHandler Resize;

        public event MouseEventHandler MouseClick;

        public event MouseEventHandler MouseDown;

        public event MouseEventHandler MouseUp;

        public event PaintEventHandler Paint;

        public event KeyPressEventHandler KeyPress;

        protected virtual void OnClick(EventArgs e) => Click?.Invoke(this, e);

        protected virtual void OnTextChanged(EventArgs e) => TextChanged?.Invoke(this, e);

        protected virtual void OnResize(EventArgs e) => Resize?.Invoke(this, e);

        protected virtual void OnMouseClick(MouseEventArgs e) => MouseClick?.Invoke(this, e);

        protected virtual void OnMouseDown(MouseEventArgs e) => MouseDown?.Invoke(this, e);

        protected virtual void OnMouseUp(MouseEventArgs e) => MouseUp?.Invoke(this, e);

        protected virtual void OnKeyPress(KeyPressEventArgs e) => KeyPress?.Invoke(this, e);

        protected virtual void OnPaint(PaintEventArgs e) => Paint?.Invoke(this, e);

        protected virtual void OnPaintBackground(PaintEventArgs pevent) => pevent.Graphics.Clear(BackColor);

        public void SuspendLayout() => layoutSuspended++;

        public void ResumeLayout() => ResumeLayout(true);

        public void ResumeLayout(bool performLayout)
        {
            if (layoutSuspended > 0)
            {
                layoutSuspended--;
            }
            if (performLayout && layoutSuspended == 0)
            {
                PerformLayout();
            }
        }

        public void PerformLayout()
        {
        }

        // Левый верхний угол элемента в точках окна формы.
        internal Point OriginInForm()
        {
            int ox = 0;
            int oy = 0;
            Control control = this;
            while (control != null && !(control is Form))
            {
                ox += control.x;
                oy += control.y;
                control = control.Parent;
            }
            return new Point(ox, oy);
        }

        public Form FindForm()
        {
            Control control = this;
            while (control != null && !(control is Form))
            {
                control = control.Parent;
            }
            return (Form)control;
        }

        public void Invalidate()
        {
            Form form = FindForm();
            if (form != null)
            {
                form.NeedsPaint = true;
            }
        }

        public void Invalidate(Rectangle rc) => Invalidate();

        // Нарисовать отложенное сейчас, не дожидаясь цикла.
        public void Update()
        {
            // Не `?.`: условный вызов метода, возвращающего bool, компилятор
            // собирает через Nullable<bool>, и без его конструктора падает сам
            // (см. память «Ловушки corelib»).
            Form form = FindForm();
            if (form != null)
            {
                form.PaintIfNeeded();
            }
        }

        public virtual void Refresh()
        {
            Invalidate();
            Update();
        }

        public void BringToFront()
        {
            if (Parent != null)
            {
                Parent.Controls.Remove(this);
                Parent.Controls.Insert(0, this);
            }
        }

        // Точка окна → самый верхний видимый потомок под ней.
        internal Control ChildAt(int px, int py, out int localX, out int localY)
        {
            for (int i = 0; i < Controls.Count; i++)
            {
                Control child = Controls[i];
                if (child.VisibleState && child.Bounds.Contains(px, py))
                {
                    return child.ChildAt(px - child.x, py - child.y, out localX, out localY);
                }
            }
            localX = px;
            localY = py;
            return this;
        }

        // Доходит ли до Click щелчок не левой кнопкой (у кнопки — нет).
        internal virtual bool ClicksWithLeftOnly => false;

        // Щелчок в порядке WinForms: MouseDown, Click, MouseClick, MouseUp.
        internal void DeliverClick(MouseButtons button, int localX, int localY)
        {
            if (!Enabled)
            {
                return;
            }
            var args = new MouseEventArgs(button, 1, localX, localY, 0);
            OnMouseDown(args);
            if (button == MouseButtons.Left || !ClicksWithLeftOnly)
            {
                OnClick(EventArgs.Empty);
            }
            OnMouseClick(args);
            OnMouseUp(args);
        }

        // Фон, если его не задали: у большинства элементов — родительский, у
        // поля ввода — белый.
        internal virtual Color AmbientBackColor => Parent != null ? Parent.BackColor : SystemColors.Control;

        // Фаза N7a: фокус. Принимают его только элементы, которые умеют ввод.
        internal virtual bool Selectable => false;

        public bool CanFocus => Selectable && Enabled && Visible;

        public bool CanSelect => CanFocus;

        public bool Focused
        {
            get
            {
                Form form = FindForm();
                return form != null && form.ActiveControl == this;
            }
        }

        public bool ContainsFocus => Focused;

        public bool Focus()
        {
            Form form = FindForm();
            if (form == null || !CanFocus)
            {
                return false;
            }
            form.ActiveControl = this;
            return form.ActiveControl == this;
        }

        public void Select() => Focus();

        public bool Contains(Control ctl)
        {
            while (ctl != null)
            {
                ctl = ctl.Parent;
                if (ctl == this)
                {
                    return true;
                }
            }
            return false;
        }

        public event EventHandler Enter;

        public event EventHandler Leave;

        public event EventHandler GotFocus;

        public event EventHandler LostFocus;

        public event KeyEventHandler KeyDown;

        public event KeyEventHandler KeyUp;

        protected virtual void OnEnter(EventArgs e) => Enter?.Invoke(this, e);

        protected virtual void OnLeave(EventArgs e) => Leave?.Invoke(this, e);

        protected virtual void OnGotFocus(EventArgs e) => GotFocus?.Invoke(this, e);

        protected virtual void OnLostFocus(EventArgs e) => LostFocus?.Invoke(this, e);

        protected virtual void OnKeyDown(KeyEventArgs e) => KeyDown?.Invoke(this, e);

        protected virtual void OnKeyUp(KeyEventArgs e) => KeyUp?.Invoke(this, e);

        // Для ContainerControl: чужие protected-члены зовут только отсюда.
        internal void RaiseFocus(bool entering)
        {
            if (entering)
            {
                OnEnter(EventArgs.Empty);
                OnGotFocus(EventArgs.Empty);
            }
            else
            {
                OnLeave(EventArgs.Empty);
                OnLostFocus(EventArgs.Empty);
            }
            Invalidate();
        }

        // Клавиша стола: KeyDown, то, что элемент делает с ней сам, KeyPress у
        // символа, ввод символа, KeyUp — порядок WinForms.
        internal void DeliverKey(int code)
        {
            if (!Enabled)
            {
                return;
            }
            Keys keys = KeyMap.ToKeys(code);
            var down = new KeyEventArgs(keys);
            OnKeyDown(down);
            if (!down.Handled)
            {
                ProcessKey(down);
            }
            if (!down.SuppressKeyPress && KeyMap.ToChar(code, out char c))
            {
                var press = new KeyPressEventArgs(c);
                OnKeyPress(press);
                if (!press.Handled)
                {
                    ProcessChar(press.KeyChar);
                }
            }
            OnKeyUp(new KeyEventArgs(keys));
        }

        internal virtual void ProcessKey(KeyEventArgs e)
        {
        }

        internal virtual void ProcessChar(char c)
        {
        }

        // Первый по TabIndex элемент, который примет фокус, — как при показе
        // формы в WinForms.
        internal Control FirstFocusable()
        {
            Control best = null;
            for (int i = 0; i < Controls.Count; i++)
            {
                Control child = Controls[i];
                Control candidate = child.CanFocus ? child : child.FirstFocusable();
                if (candidate != null && (best == null || child.TabIndex < best.TabIndex))
                {
                    best = candidate;
                }
            }
            return best;
        }

        // Фон, Paint и потомки снизу вверх: первый в Controls — самый верхний.
        internal void PaintTree(int window, int originX, int originY, Rectangle visible)
        {
            Rectangle area = Rectangle.Intersect(visible, new Rectangle(originX, originY, ClientSize.Width, ClientSize.Height));
            if (area.Width <= 0 || area.Height <= 0)
            {
                return;
            }
            var graphics = new Graphics(window, originX, originY, area);
            var args = new PaintEventArgs(graphics, new Rectangle(area.X - originX, area.Y - originY, area.Width, area.Height));
            OnPaintBackground(args);
            OnPaint(args);
            for (int i = Controls.Count - 1; i >= 0; i--)
            {
                Control child = Controls[i];
                if (child.VisibleState)
                {
                    child.PaintTree(window, originX + child.x, originY + child.y, area);
                }
            }
        }

        protected override void Dispose(bool disposing)
        {
            if (disposing)
            {
                for (int i = Controls.Count - 1; i >= 0; i--)
                {
                    Controls[i].Dispose();
                }
            }
            base.Dispose(disposing);
        }

        // База — ArrangedElementCollection, как у WinForms: компилятор берёт
        // `Controls.Count` у неё, а индексатор — у этого класса.
        public class ControlCollection : Layout.ArrangedElementCollection
        {
            private readonly List<Control> items = new List<Control>();

            public ControlCollection(Control owner)
            {
                Owner = owner;
            }

            public Control Owner { get; }

            public override int Count => items.Count;

            public Control this[int index] => items[index];

            public virtual void Add(Control value)
            {
                if (value == null)
                {
                    return;
                }
                value.Parent?.Controls.Remove(value);
                items.Add(value);
                value.Parent = Owner;
                Owner.Invalidate();
            }

            public void AddRange(Control[] controls)
            {
                for (int i = 0; i < controls.Length; i++)
                {
                    Add(controls[i]);
                }
            }

            internal void Insert(int index, Control value)
            {
                items.Insert(index, value);
                value.Parent = Owner;
            }

            public virtual void Remove(Control value)
            {
                if (value != null && items.Remove(value))
                {
                    value.Parent = null;
                    Owner.Invalidate();
                }
            }

            public bool Contains(Control control) => items.Contains(control);

            public int IndexOf(Control control) => items.IndexOf(control);

            public void Clear()
            {
                for (int i = items.Count - 1; i >= 0; i--)
                {
                    Remove(items[i]);
                }
            }

            public IEnumerator<Control> GetEnumerator() => items.GetEnumerator();
        }
    }

    public class ScrollableControl : Control
    {
        public virtual bool AutoScroll { get; set; }
    }

    public class ContainerControl : ScrollableControl
    {
        public SizeF AutoScaleDimensions { get; set; }

        public AutoScaleMode AutoScaleMode { get; set; } = AutoScaleMode.Inherit;

        private Control active;

        // Смена фокуса: Leave и LostFocus у прежнего, Enter и GotFocus у нового.
        public Control ActiveControl
        {
            get => active;
            set
            {
                if (active == value || (value != null && (!value.CanFocus || !Contains(value))))
                {
                    return;
                }
                Control previous = active;
                active = value;
                if (previous != null)
                {
                    previous.RaiseFocus(false);
                }
                if (value != null)
                {
                    value.RaiseFocus(true);
                }
            }
        }
    }

    public class Form : ContainerControl
    {
        private int window = -1;
        private bool closing;
        private bool loaded;

        public Form()
        {
            // Форма появляется только по Show или Application.Run.
            VisibleState = false;
        }

        protected override Size DefaultSize => new Size(300, 300);

        internal bool NeedsPaint { get; set; }

        // Открытый выпадающий список (фаза N7b): рисуется поверх всех элементов
        // и первым получает щелчок.
        internal ComboBox OpenDropDown { get; set; }

        // Диалог (фаза N7c). У модальной формы значение, отличное от None,
        // закрывает её — это проверяет цикл Application.RunModal.
        public DialogResult DialogResult { get; set; }

        public IButtonControl AcceptButton { get; set; }

        public IButtonControl CancelButton { get; set; }

        // Кому отдать фокус при показе вместо первого элемента по TabIndex.
        internal Control PreferredFocus { get; set; }

        public DialogResult ShowDialog() => ShowDialog(null);

        public DialogResult ShowDialog(IWin32Window owner)
        {
            if (Application.IsModal(this))
            {
                throw new InvalidOperationException("Form that is already displayed modally cannot be displayed as a modal dialog box. Close the form before calling showDialog.");
            }
            DialogResult = DialogResult.None;
            Show();
            Application.RunModal(this);
            return DialogResult;
        }

        public FormStartPosition StartPosition { get; set; } = FormStartPosition.WindowsDefaultLocation;

        public FormBorderStyle FormBorderStyle { get; set; } = FormBorderStyle.Sizable;

        public bool MaximizeBox { get; set; } = true;

        public bool MinimizeBox { get; set; } = true;

        public bool ShowInTaskbar { get; set; } = true;

        public event EventHandler Load;

        public event EventHandler Shown;

        public event FormClosingEventHandler FormClosing;

        public event FormClosedEventHandler FormClosed;

        protected virtual void OnLoad(EventArgs e) => Load?.Invoke(this, e);

        protected virtual void OnShown(EventArgs e) => Shown?.Invoke(this, e);

        protected virtual void OnFormClosing(FormClosingEventArgs e) => FormClosing?.Invoke(this, e);

        protected virtual void OnFormClosed(FormClosedEventArgs e) => FormClosed?.Invoke(this, e);

        public void Show() => Visible = true;

        // Первый показ: Load, окно, отложенный Shown — как у WinForms, где Shown
        // приходит сообщением и потому раньше первой перерисовки.
        protected override void SetVisibleCore(bool value)
        {
            if (!value || window >= 0)
            {
                base.SetVisibleCore(value);
                return;
            }
            if (!loaded)
            {
                loaded = true;
                OnLoad(EventArgs.Empty);
            }
            window = FreeOsWindow.Open(Text, ClientSize.Width, ClientSize.Height);
            if (window < 0)
            {
                throw new InvalidOperationException("FreeOS gave the form no window (code " + window + ").");
            }
            Application.Opened(this);
            base.SetVisibleCore(true);
            if (ActiveControl == null)
            {
                ActiveControl = PreferredFocus ?? FirstFocusable();
            }
            Application.Post(() => OnShown(EventArgs.Empty));
        }

        public void Close() => CloseFor(CloseReason.UserClosing);

        internal void CloseFor(CloseReason reason)
        {
            if (closing || IsDisposed)
            {
                return;
            }
            if (window < 0)
            {
                Dispose();
                return;
            }
            // Модальную форму закрыли не кнопкой — как в WinForms, это Cancel.
            if (reason != CloseReason.None && DialogResult == DialogResult.None && Application.IsModal(this))
            {
                DialogResult = DialogResult.Cancel;
            }
            closing = true;
            var args = new FormClosingEventArgs(reason, false);
            OnFormClosing(args);
            if (args.Cancel)
            {
                closing = false;
                return;
            }
            OnFormClosed(new FormClosedEventArgs(reason));
            FreeOsWindow.Close(window);
            window = -1;
            Application.Closed(this);
            VisibleState = false;
            Dispose();
        }

        internal bool PumpEvent()
        {
            if (window < 0)
            {
                return false;
            }
            int kind = FreeOsWindow.NextEvent(window, out int px, out int py, out int code);
            switch (kind)
            {
                case FreeOsWindow.EventNone:
                    return false;
                case FreeOsWindow.EventPointer:
                    if (OpenDropDown != null)
                    {
                        // Щелчок в открытом списке выбирает строку; мимо — только
                        // закрывает список, как в Windows.
                        ComboBox combo = OpenDropDown;
                        Point origin = combo.OriginInForm();
                        Rectangle drop = combo.DropDownBounds(origin.X, origin.Y);
                        if (drop.Contains(px, py))
                        {
                            combo.ClickDropDown(py - drop.Y);
                            return true;
                        }
                        if (!new Rectangle(origin.X, origin.Y, combo.Width, combo.Height).Contains(px, py))
                        {
                            combo.DroppedDown = false;
                            return true;
                        }
                    }
                    Control target = ChildAt(px, py, out int localX, out int localY);
                    // Фокус переходит при нажатии, до MouseDown, как в WinForms.
                    if (target.CanFocus)
                    {
                        ActiveControl = target;
                    }
                    target.DeliverClick(code == 2 ? MouseButtons.Right : MouseButtons.Left, localX, localY);
                    return true;
                case FreeOsWindow.EventKey:
                    Control focused = ActiveControl != null && ActiveControl.CanFocus ? ActiveControl : this;
                    Keys key = KeyMap.ToKeys(code);
                    // Enter нажимает кнопку по умолчанию, если фокус не на
                    // другой кнопке, Escape — кнопку отмены.
                    if (key == Keys.Enter && AcceptButton != null && !(focused is IButtonControl))
                    {
                        AcceptButton.PerformClick();
                        return true;
                    }
                    if (key == Keys.Escape && CancelButton != null)
                    {
                        CancelButton.PerformClick();
                        return true;
                    }
                    focused.DeliverKey(code);
                    return true;
                case FreeOsWindow.EventClose:
                    CloseFor(CloseReason.UserClosing);
                    return true;
                default:
                    return true;
            }
        }

        // Форма под модальным окном: событие выбрано и выброшено.
        internal bool DiscardEvent()
        {
            return window >= 0 && FreeOsWindow.NextEvent(window, out int px, out int py, out int code) != FreeOsWindow.EventNone;
        }

        internal bool PaintIfNeeded()
        {
            if (!NeedsPaint || window < 0 || !VisibleState)
            {
                return false;
            }
            NeedsPaint = false;
            PaintTree(window, 0, 0, new Rectangle(0, 0, ClientSize.Width, ClientSize.Height));
            if (OpenDropDown != null)
            {
                Point origin = OpenDropDown.OriginInForm();
                OpenDropDown.PaintDropDown(window, origin.X, origin.Y);
            }
            FreeOsWindow.Present(window);
            return true;
        }

        protected override void Dispose(bool disposing)
        {
            if (disposing && window >= 0)
            {
                FreeOsWindow.Close(window);
                Application.Closed(this);
                window = -1;
            }
            base.Dispose(disposing);
        }
    }
}
