// Вкладки, строка состояния и подсказки (фаза N7f): TabControl со страницами
// TabPage, StatusStrip с ToolStripStatusLabel и компонент ToolTip из дизайнера.
//
// Правила сняты с WinForms образцом `tabs`: страницы, кроме выбранной, скрыты;
// удаление выбранной вкладки поднимает SelectedIndexChanged и выбирает
// оставшуюся; `Items.Add(string)` у строки состояния создаёт ToolStripStatusLabel.
// Подсказки хранятся и отдаются, но на FreeOS не всплывают: движения мыши над
// окном программы стол не присылает.

using System.Collections.Generic;
using System.ComponentModel;
using System.Drawing;

namespace System.Windows.Forms
{
    public struct Padding
    {
        public Padding(int all)
        {
            Left = all;
            Top = all;
            Right = all;
            Bottom = all;
        }

        public Padding(int left, int top, int right, int bottom)
        {
            Left = left;
            Top = top;
            Right = right;
            Bottom = bottom;
        }

        public static readonly Padding Empty = new Padding(0);

        public int Left { get; set; }

        public int Top { get; set; }

        public int Right { get; set; }

        public int Bottom { get; set; }

        // Как у WinForms: -1, если стороны разные.
        public int All
        {
            get => Left == Top && Left == Right && Left == Bottom ? Left : -1;
            set
            {
                Left = value;
                Top = value;
                Right = value;
                Bottom = value;
            }
        }

        public int Horizontal => Left + Right;

        public int Vertical => Top + Bottom;

        public Size Size => new Size(Horizontal, Vertical);

        public override string ToString() => "{Left=" + Left + ",Top=" + Top + ",Right=" + Right + ",Bottom=" + Bottom + "}";
    }

    public enum TabAlignment
    {
        Top = 0,
        Bottom = 1,
        Left = 2,
        Right = 3,
    }

    public enum TabAppearance
    {
        Normal = 0,
        Buttons = 1,
        FlatButtons = 2,
    }

    public enum ToolTipIcon
    {
        None = 0,
        Info = 1,
        Warning = 2,
        Error = 3,
    }

    public class TabPage : Panel
    {
        public TabPage()
        {
        }

        public TabPage(string text)
        {
            Text = text;
        }

        public bool UseVisualStyleBackColor { get; set; }

        public int ImageIndex { get; set; } = -1;

        public string ToolTipText { get; set; } = string.Empty;

        internal override Color AmbientBackColor => UseVisualStyleBackColor ? SystemColors.Window : base.AmbientBackColor;

        public override string Text
        {
            get => base.Text;
            set
            {
                base.Text = value;
                if (Parent != null)
                {
                    Parent.Invalidate();
                }
            }
        }
    }

    public class TabControl : Control
    {
        private int selected = -1;
        private TabPageCollection pages;

        public TabControl()
        {
        }

        protected override Size DefaultSize => new Size(200, 100);

        internal override bool Selectable => true;

        // Коллекция заводится при первом обращении: базовый конструктор Control
        // задаёт размер, а это зовёт OnResize, который раскладывает страницы,
        // ещё до тела конструктора TabControl.
        public TabPageCollection TabPages => pages ?? (pages = new TabPageCollection(this));

        public TabAlignment Alignment { get; set; } = TabAlignment.Top;

        public TabAppearance Appearance { get; set; } = TabAppearance.Normal;

        public bool Multiline { get; set; }

        // У TabControl Padding — поля заголовка вкладки, и тип у него Point, как
        // у WinForms.
        public new Point Padding { get; set; } = new Point(6, 3);

        public int TabCount => TabPages.Count;

        public event EventHandler SelectedIndexChanged;

        protected virtual void OnSelectedIndexChanged(EventArgs e) => SelectedIndexChanged?.Invoke(this, e);

        public int SelectedIndex
        {
            get => selected;
            set
            {
                if (value < -1)
                {
                    throw new ArgumentOutOfRangeException("value", "Value of '" + value + "' is not valid for 'SelectedIndex'. 'SelectedIndex' must be greater than or equal to -1.");
                }
                if (value >= TabCount || selected == value)
                {
                    return;
                }
                Select(value, true);
            }
        }

        public TabPage SelectedTab
        {
            get => selected >= 0 && selected < TabCount ? TabPages[selected] : null;
            set
            {
                int index = value == null ? -1 : TabPages.IndexOf(value);
                if (index >= 0)
                {
                    SelectedIndex = index;
                }
            }
        }

        public void SelectTab(int index) => SelectedIndex = index;

        public void SelectTab(TabPage tabPage) => SelectedTab = tabPage;

        // Выбранная страница видна, остальные скрыты — как у WinForms.
        private void Select(int index, bool raise)
        {
            selected = index;
            for (int i = 0; i < TabCount; i++)
            {
                TabPages[i].VisibleState = i == index;
            }
            Invalidate();
            if (raise)
            {
                OnSelectedIndexChanged(EventArgs.Empty);
            }
        }

        internal static int HeaderHeight => FreeOsWindow.TextHeight() + 10;

        // Прямоугольник страниц: под заголовками, с полями по четыре точки.
        public override Rectangle DisplayRectangle => new Rectangle(4, HeaderHeight + 4, Math.Max(0, Width - 8), Math.Max(0, Height - HeaderHeight - 8));

        private void PlacePages()
        {
            Rectangle area = DisplayRectangle;
            for (int i = 0; i < TabCount; i++)
            {
                TabPages[i].Place(area.X, area.Y, area.Width, area.Height);
            }
        }

        protected override void OnResize(EventArgs e)
        {
            PlacePages();
            base.OnResize(e);
        }

        protected override void OnControlAdded(ControlEventArgs e)
        {
            base.OnControlAdded(e);
            var page = e.Control as TabPage;
            if (page == null)
            {
                return;
            }
            Rectangle area = DisplayRectangle;
            page.Place(area.X, area.Y, area.Width, area.Height);
            if (selected < 0)
            {
                Select(0, false);
            }
            else
            {
                page.VisibleState = TabPages.IndexOf(page) == selected;
            }
        }

        protected override void OnControlRemoved(ControlEventArgs e)
        {
            base.OnControlRemoved(e);
            if (!(e.Control is TabPage))
            {
                return;
            }
            // Удалённую выбранную страницу сменяет та, что встала на её место или
            // перед ним; WinForms сообщает об этом событием.
            int count = TabCount;
            int next = count == 0 ? -1 : Math.Min(selected, count - 1);
            Select(next, true);
        }

        private int TabWidth(int index) => FreeOsWindow.TextWidth(TabPages[index].Text) + 16;

        private int TabAt(int x)
        {
            int left = 2;
            for (int i = 0; i < TabCount; i++)
            {
                int width = TabWidth(i);
                if (x >= left && x < left + width)
                {
                    return i;
                }
                left += width;
            }
            return -1;
        }

        protected override void OnMouseDown(MouseEventArgs e)
        {
            if (e.Y < HeaderHeight + 2)
            {
                int index = TabAt(e.X);
                if (index >= 0)
                {
                    SelectedIndex = index;
                }
            }
            base.OnMouseDown(e);
        }

        internal override void ProcessKey(KeyEventArgs e)
        {
            if (TabCount == 0)
            {
                return;
            }
            if (e.KeyCode == Keys.Right)
            {
                SelectedIndex = (selected + 1) % TabCount;
            }
            else if (e.KeyCode == Keys.Left)
            {
                SelectedIndex = (selected + TabCount - 1) % TabCount;
            }
        }

        protected override void OnPaint(PaintEventArgs e)
        {
            Graphics g = e.Graphics;
            var edge = new Pen(Color.FromArgb(172, 172, 172));
            int header = HeaderHeight;
            g.FillRectangle(new SolidBrush(SystemColors.Window), 0, header + 1, Width, Height - header - 1);
            g.DrawRectangle(edge, 0, header, Width - 1, Height - header - 1);
            int left = 2;
            int textHeight = FreeOsWindow.TextHeight();
            for (int i = 0; i < TabCount; i++)
            {
                int width = TabWidth(i);
                bool current = i == selected;
                int top = current ? 0 : 2;
                g.FillRectangle(new SolidBrush(current ? SystemColors.Window : Color.FromArgb(240, 240, 240)), left, top, width, header - top + (current ? 1 : 0));
                g.DrawLine(edge, left, top, left + width - 1, top);
                g.DrawLine(edge, left, top, left, header);
                g.DrawLine(edge, left + width - 1, top, left + width - 1, header);
                if (current && Focused)
                {
                    g.DrawRectangle(new Pen(SystemColors.Highlight), left + 3, top + 3, width - 7, header - top - 6);
                }
                g.DrawString(TabPages[i].Text, Font, new SolidBrush(Enabled ? ForeColor : SystemColors.GrayText), left + 8, top + (header - top - textHeight) / 2);
                left += width;
            }
            base.OnPaint(e);
        }

        public class TabPageCollection
        {
            private readonly TabControl owner;

            public TabPageCollection(TabControl owner)
            {
                this.owner = owner;
            }

            public int Count
            {
                get
                {
                    int count = 0;
                    for (int i = 0; i < owner.Controls.Count; i++)
                    {
                        if (owner.Controls[i] is TabPage)
                        {
                            count++;
                        }
                    }
                    return count;
                }
            }

            public virtual TabPage this[int index]
            {
                get
                {
                    int seen = 0;
                    for (int i = 0; i < owner.Controls.Count; i++)
                    {
                        var page = owner.Controls[i] as TabPage;
                        if (page != null)
                        {
                            if (seen == index)
                            {
                                return page;
                            }
                            seen++;
                        }
                    }
                    throw new ArgumentOutOfRangeException("index", "InvalidArgument=Value of '" + index + "' is not valid for 'index'.");
                }
            }

            public void Add(TabPage value)
            {
                if (value == null)
                {
                    throw new ArgumentNullException("value");
                }
                owner.Controls.Add(value);
            }

            public void Add(string text) => Add(new TabPage(text));

            public void AddRange(TabPage[] pages)
            {
                for (int i = 0; i < pages.Length; i++)
                {
                    Add(pages[i]);
                }
            }

            public void Remove(TabPage value) => owner.Controls.Remove(value);

            public void RemoveAt(int index) => Remove(this[index]);

            public void Clear()
            {
                for (int i = Count - 1; i >= 0; i--)
                {
                    RemoveAt(i);
                }
            }

            public bool Contains(TabPage page) => IndexOf(page) >= 0;

            public int IndexOf(TabPage page)
            {
                for (int i = 0; i < Count; i++)
                {
                    if (this[i] == page)
                    {
                        return i;
                    }
                }
                return -1;
            }
        }
    }

    public class ToolStripLabel : ToolStripItem
    {
        public ToolStripLabel()
        {
        }

        public ToolStripLabel(string text)
        {
            Text = text;
        }

        public bool IsLink { get; set; }
    }

    public class ToolStripStatusLabel : ToolStripLabel
    {
        private bool spring;

        public ToolStripStatusLabel()
        {
        }

        public ToolStripStatusLabel(string text)
            : base(text)
        {
        }

        // Растянутая надпись забирает всё свободное место строки.
        public bool Spring
        {
            get => spring;
            set
            {
                spring = value;
                Invalidate();
            }
        }
    }

    public class StatusStrip : ToolStrip
    {
        private const int GripWidth = 12;

        public StatusStrip()
        {
            Dock = DockStyle.Bottom;
            GripStyle = ToolStripGripStyle.Hidden;
        }

        protected override Size DefaultSize => new Size(200, 22);

        public bool SizingGrip { get; set; } = true;

        internal override ToolStripItem CreateDefaultItem(string text) => new ToolStripStatusLabel(text);

        internal override int ItemWidth(ToolStripItem item)
        {
            var label = item as ToolStripStatusLabel;
            int natural = base.ItemWidth(item);
            if (label == null || !label.Spring)
            {
                return natural;
            }
            int others = 0;
            for (int i = 0; i < Items.Count; i++)
            {
                if (Items[i] != item && Items[i].Available)
                {
                    others += base.ItemWidth(Items[i]);
                }
            }
            return Math.Max(natural, Width - 4 - others - (SizingGrip ? GripWidth : 0));
        }

        protected override void OnPaintBackground(PaintEventArgs pevent)
        {
            pevent.Graphics.Clear(Color.FromArgb(240, 240, 240));
        }
    }

    public class ToolTip : Component
    {
        private readonly Dictionary<Control, string> tips = new Dictionary<Control, string>();
        private int automaticDelay = 500;

        public ToolTip()
        {
        }

        public ToolTip(IContainer cont)
            : this()
        {
            if (cont == null)
            {
                throw new ArgumentNullException("cont");
            }
            cont.Add(this);
        }

        public bool Active { get; set; } = true;

        // Как у WinForms: общая задержка задаёт три остальные.
        public int AutomaticDelay
        {
            get => automaticDelay;
            set
            {
                if (value < 0)
                {
                    throw new ArgumentOutOfRangeException("value");
                }
                automaticDelay = value;
                AutoPopDelay = value * 10;
                InitialDelay = value;
                ReshowDelay = value / 5;
            }
        }

        public int AutoPopDelay { get; set; } = 5000;

        public int InitialDelay { get; set; } = 500;

        public int ReshowDelay { get; set; } = 100;

        public bool ShowAlways { get; set; }

        public bool IsBalloon { get; set; }

        public bool UseAnimation { get; set; } = true;

        public bool UseFading { get; set; } = true;

        public string ToolTipTitle { get; set; } = string.Empty;

        public ToolTipIcon ToolTipIcon { get; set; }

        public object Tag { get; set; }

        public void SetToolTip(Control control, string caption)
        {
            if (control == null)
            {
                throw new ArgumentNullException("control");
            }
            if (string.IsNullOrEmpty(caption))
            {
                tips.Remove(control);
            }
            else
            {
                tips[control] = caption;
            }
        }

        public string GetToolTip(Control control)
        {
            if (control != null && tips.TryGetValue(control, out string caption))
            {
                return caption;
            }
            return string.Empty;
        }

        public void RemoveAll() => tips.Clear();

        protected override void Dispose(bool disposing)
        {
            if (disposing)
            {
                tips.Clear();
            }
            base.Dispose(disposing);
        }
    }
}
