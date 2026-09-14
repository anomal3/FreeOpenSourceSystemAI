// Меню формы (фаза N7d): MenuStrip и его пункты из дизайнера — ToolStripMenuItem
// с вложенными DropDownItems и ToolStripSeparator.
//
// Полоса меню — элемент формы, прижатый к верху (Dock = Top). Открытый пункт
// рисуется всплывающим поверх формы, как выпадающий список ComboBox, и первым
// получает щелчок и стрелки. Сочетаний вроде Ctrl+O стол окну программы не
// отдаёт, поэтому ShortcutKeys только подписываются справа от пункта.

using System.Collections.Generic;
using System.ComponentModel;
using System.Drawing;

namespace System.Windows.Forms
{
    public enum ToolStripItemDisplayStyle
    {
        None = 0,
        Text = 1,
        Image = 2,
        ImageAndText = 3,
    }

    public enum ToolStripGripStyle
    {
        Hidden = 0,
        Visible = 1,
    }

    public enum ToolStripRenderMode
    {
        Custom = 0,
        System = 1,
        Professional = 2,
        ManagerRenderMode = 3,
    }

    public abstract class ToolStripItem : Component
    {
        private string text = string.Empty;
        private bool enabled = true;

        protected ToolStripItem()
        {
        }

        public string Name { get; set; } = string.Empty;

        public virtual string Text
        {
            get => text;
            set
            {
                text = value ?? string.Empty;
                Invalidate();
            }
        }

        public object Tag { get; set; }

        public virtual bool Enabled
        {
            get => enabled;
            set
            {
                enabled = value;
                Invalidate();
            }
        }

        public bool Visible { get; set; } = true;

        public bool Available
        {
            get => Visible;
            set => Visible = value;
        }

        public virtual Size Size { get; set; }

        public int Width => Size.Width;

        public int Height => Size.Height;

        public string ToolTipText { get; set; }

        public ToolStripItemDisplayStyle DisplayStyle { get; set; } = ToolStripItemDisplayStyle.ImageAndText;

        public ToolStrip Owner { get; internal set; }

        public ToolStripItem OwnerItem { get; internal set; }

        public event EventHandler Click;

        protected virtual void OnClick(EventArgs e) => Click?.Invoke(this, e);

        public void PerformClick()
        {
            if (Enabled && Available)
            {
                OnClick(EventArgs.Empty);
            }
        }

        // Полоса, на которой пункт в конце концов лежит.
        internal ToolStrip Root
        {
            get
            {
                ToolStripItem item = this;
                while (item.OwnerItem != null)
                {
                    item = item.OwnerItem;
                }
                return item.Owner;
            }
        }

        internal virtual bool IsSeparator => false;

        // Текст без знака мнемоники: `&File` рисуется как `File`, `&&` — как `&`.
        internal string PlainText
        {
            get
            {
                string source = Text;
                var result = new System.Text.StringBuilder();
                for (int i = 0; i < source.Length; i++)
                {
                    char c = source[i];
                    if (c == '&' && i + 1 < source.Length)
                    {
                        i++;
                        c = source[i];
                    }
                    else if (c == '&')
                    {
                        continue;
                    }
                    result.Append(c);
                }
                return result.ToString();
            }
        }

        internal void Invalidate()
        {
            ToolStrip strip = Root;
            if (strip != null)
            {
                strip.Invalidate();
            }
        }

        public override string ToString() => Text;
    }

    public class ToolStripSeparator : ToolStripItem
    {
        public ToolStripSeparator()
        {
        }

        internal override bool IsSeparator => true;
    }

    public abstract class ToolStripDropDownItem : ToolStripItem
    {
        protected ToolStripDropDownItem()
        {
            DropDownItems = new ToolStripItemCollection(null, this);
        }

        public ToolStripItemCollection DropDownItems { get; }

        public virtual bool HasDropDownItems => DropDownItems.Count > 0;

        public event EventHandler DropDownOpening;

        public event EventHandler DropDownOpened;

        public event EventHandler DropDownClosed;

        protected virtual void OnDropDownOpening(EventArgs e) => DropDownOpening?.Invoke(this, e);

        protected virtual void OnDropDownOpened(EventArgs e) => DropDownOpened?.Invoke(this, e);

        protected virtual void OnDropDownClosed(EventArgs e) => DropDownClosed?.Invoke(this, e);

        internal void RaiseDropDown(int stage)
        {
            if (stage == 0)
            {
                OnDropDownOpening(EventArgs.Empty);
            }
            else if (stage == 1)
            {
                OnDropDownOpened(EventArgs.Empty);
            }
            else
            {
                OnDropDownClosed(EventArgs.Empty);
            }
        }

        public void ShowDropDown()
        {
            var menu = Root as MenuStrip;
            if (menu != null && OwnerItem == null && HasDropDownItems)
            {
                menu.Open(this);
            }
        }

        public void HideDropDown()
        {
            var menu = Root as MenuStrip;
            if (menu != null && menu.OpenItem == this)
            {
                menu.Close();
            }
        }

        public bool Pressed
        {
            get
            {
                var menu = Root as MenuStrip;
                return menu != null && menu.OpenItem == this;
            }
        }
    }

    public class ToolStripMenuItem : ToolStripDropDownItem
    {
        private CheckState checkState;

        public ToolStripMenuItem()
        {
        }

        public ToolStripMenuItem(string text)
        {
            Text = text;
        }

        public bool CheckOnClick { get; set; }

        public bool Checked
        {
            get => checkState != CheckState.Unchecked;
            set => CheckState = value ? CheckState.Checked : CheckState.Unchecked;
        }

        // Как у ToolStripMenuItem в WinForms: любая смена состояния даёт оба
        // события, CheckedChanged первым.
        public CheckState CheckState
        {
            get => checkState;
            set
            {
                if (checkState == value)
                {
                    return;
                }
                checkState = value;
                OnCheckedChanged(EventArgs.Empty);
                OnCheckStateChanged(EventArgs.Empty);
                Invalidate();
            }
        }

        public Keys ShortcutKeys { get; set; }

        public bool ShowShortcutKeys { get; set; } = true;

        public string ShortcutKeyDisplayString { get; set; }

        public event EventHandler CheckedChanged;

        public event EventHandler CheckStateChanged;

        protected virtual void OnCheckedChanged(EventArgs e) => CheckedChanged?.Invoke(this, e);

        protected virtual void OnCheckStateChanged(EventArgs e) => CheckStateChanged?.Invoke(this, e);

        protected override void OnClick(EventArgs e)
        {
            if (CheckOnClick)
            {
                Checked = !Checked;
            }
            base.OnClick(e);
        }

        // Подпись сочетания: `Ctrl+O`, как рисует WinForms.
        internal string ShortcutText
        {
            get
            {
                if (!ShowShortcutKeys)
                {
                    return string.Empty;
                }
                if (ShortcutKeyDisplayString != null)
                {
                    return ShortcutKeyDisplayString;
                }
                if (ShortcutKeys == Keys.None)
                {
                    return string.Empty;
                }
                string prefix = string.Empty;
                if ((ShortcutKeys & Keys.Control) != 0)
                {
                    prefix += "Ctrl+";
                }
                if ((ShortcutKeys & Keys.Shift) != 0)
                {
                    prefix += "Shift+";
                }
                if ((ShortcutKeys & Keys.Alt) != 0)
                {
                    prefix += "Alt+";
                }
                Keys code = ShortcutKeys & Keys.KeyCode;
                return prefix + code.ToString();
            }
        }
    }

    public class ToolStripItemCollection : Layout.ArrangedElementCollection
    {
        private readonly List<ToolStripItem> items = new List<ToolStripItem>();
        private readonly ToolStrip owner;
        private readonly ToolStripItem ownerItem;

        internal ToolStripItemCollection(ToolStrip owner, ToolStripItem ownerItem)
        {
            this.owner = owner;
            this.ownerItem = ownerItem;
        }

        public override int Count => items.Count;

        public virtual ToolStripItem this[int index] => items[index];

        public virtual ToolStripItem this[string key]
        {
            get
            {
                for (int i = 0; i < items.Count; i++)
                {
                    if (string.Equals(items[i].Name, key, StringComparison.OrdinalIgnoreCase))
                    {
                        return items[i];
                    }
                }
                return null;
            }
        }

        public int Add(ToolStripItem value)
        {
            if (value == null)
            {
                throw new ArgumentNullException("value");
            }
            items.Add(value);
            Adopt(value);
            return items.Count - 1;
        }

        public ToolStripItem Add(string text)
        {
            var item = new ToolStripMenuItem(text);
            Add(item);
            return item;
        }

        public void AddRange(ToolStripItem[] toolStripItems)
        {
            if (toolStripItems == null)
            {
                throw new ArgumentNullException("toolStripItems");
            }
            for (int i = 0; i < toolStripItems.Length; i++)
            {
                Add(toolStripItems[i]);
            }
        }

        public void Insert(int index, ToolStripItem value)
        {
            items.Insert(index, value);
            Adopt(value);
        }

        public void Remove(ToolStripItem value)
        {
            if (value != null && items.Remove(value))
            {
                value.Owner = null;
                value.OwnerItem = null;
                Changed();
            }
        }

        public void RemoveAt(int index) => Remove(items[index]);

        public void Clear()
        {
            for (int i = items.Count - 1; i >= 0; i--)
            {
                Remove(items[i]);
            }
        }

        public bool Contains(ToolStripItem value) => items.Contains(value);

        public int IndexOf(ToolStripItem value) => items.IndexOf(value);

        public Collections.IEnumerator GetEnumerator() => items.GetEnumerator();

        private void Adopt(ToolStripItem value)
        {
            value.Owner = owner;
            value.OwnerItem = ownerItem;
            Changed();
        }

        private void Changed()
        {
            ToolStrip strip = owner ?? (ownerItem != null ? ownerItem.Root : null);
            if (strip != null)
            {
                strip.Invalidate();
            }
        }
    }

    public class ToolStrip : ScrollableControl
    {
        internal const int ItemPadding = 8;

        public ToolStrip()
        {
            Items = new ToolStripItemCollection(this, null);
            Dock = DockStyle.Top;
            AutoSize = true;
            TabStop = false;
        }

        protected override Size DefaultSize => new Size(100, 25);

        public virtual ToolStripItemCollection Items { get; }

        public Size ImageScalingSize { get; set; } = new Size(16, 16);

        public ToolStripGripStyle GripStyle { get; set; } = ToolStripGripStyle.Visible;

        public ToolStripRenderMode RenderMode { get; set; } = ToolStripRenderMode.ManagerRenderMode;

        public bool Stretch { get; set; }

        public bool ShowItemToolTips { get; set; } = true;

        // Высота строки пункта — от системного шрифта, как у WinForms от своего.
        internal static int RowHeight => FreeOsWindow.TextHeight() + ItemPadding;

        internal override Size ConstrainSize(int width, int height) =>
            AutoSize ? new Size(width, RowHeight + 4) : new Size(width, height);

        internal int ItemWidth(ToolStripItem item) =>
            item.IsSeparator ? 6 : FreeOsWindow.TextWidth(item.PlainText) + 2 * ItemPadding;

        // Левый край пункта на полосе.
        internal int ItemLeft(int index)
        {
            int left = 2;
            for (int i = 0; i < index; i++)
            {
                if (Items[i].Available)
                {
                    left += ItemWidth(Items[i]);
                }
            }
            return left;
        }

        internal int ItemAt(int x)
        {
            int left = 2;
            for (int i = 0; i < Items.Count; i++)
            {
                ToolStripItem item = Items[i];
                if (!item.Available)
                {
                    continue;
                }
                int width = ItemWidth(item);
                if (x >= left && x < left + width)
                {
                    return i;
                }
                left += width;
            }
            return -1;
        }

        internal virtual bool IsHighlighted(ToolStripItem item) => false;

        protected override void OnPaintBackground(PaintEventArgs pevent)
        {
            pevent.Graphics.Clear(Color.FromArgb(249, 249, 249));
        }

        protected override void OnPaint(PaintEventArgs e)
        {
            Graphics g = e.Graphics;
            int textTop = (Height - FreeOsWindow.TextHeight()) / 2;
            for (int i = 0; i < Items.Count; i++)
            {
                ToolStripItem item = Items[i];
                if (!item.Available)
                {
                    continue;
                }
                int left = ItemLeft(i);
                int width = ItemWidth(item);
                if (item.IsSeparator)
                {
                    g.FillRectangle(new SolidBrush(Color.FromArgb(215, 215, 215)), left + 2, 4, 1, Height - 8);
                    continue;
                }
                if (IsHighlighted(item))
                {
                    g.FillRectangle(new SolidBrush(Color.FromArgb(153, 209, 255)), left, 2, width, Height - 4);
                    g.FillRectangle(new SolidBrush(Color.FromArgb(204, 232, 255)), left + 1, 3, width - 2, Height - 6);
                }
                g.DrawString(item.PlainText, Font, new SolidBrush(item.Enabled ? ForeColor : SystemColors.GrayText), left + ItemPadding, textTop);
            }
            base.OnPaint(e);
        }
    }

    public class MenuStrip : ToolStrip
    {
        private const int CheckMargin = 28;
        private const int SeparatorHeight = 7;

        private ToolStripDropDownItem open;
        private int hot = -1;

        public MenuStrip()
        {
            GripStyle = ToolStripGripStyle.Hidden;
        }

        protected override Size DefaultSize => new Size(200, 24);

        internal ToolStripDropDownItem OpenItem => open;

        internal override bool IsHighlighted(ToolStripItem item) => item == open;

        protected override void OnMouseDown(MouseEventArgs e)
        {
            int index = ItemAt(e.X);
            var item = index >= 0 ? Items[index] as ToolStripDropDownItem : null;
            if (item == null || item == open || !item.Enabled)
            {
                Close();
            }
            else if (item.HasDropDownItems)
            {
                Open(item);
            }
            else
            {
                Close();
                item.PerformClick();
            }
            base.OnMouseDown(e);
        }

        internal void Open(ToolStripDropDownItem item)
        {
            if (open == item)
            {
                return;
            }
            Close();
            item.RaiseDropDown(0);
            open = item;
            hot = -1;
            Form form = FindForm();
            if (form != null)
            {
                if (form.Popup != null)
                {
                    form.Popup.ClosePopup();
                }
                form.Popup = this;
                form.NeedsPaint = true;
            }
            item.RaiseDropDown(1);
        }

        internal void Close()
        {
            if (open == null)
            {
                return;
            }
            ToolStripDropDownItem was = open;
            open = null;
            hot = -1;
            Form form = FindForm();
            if (form != null)
            {
                if (form.Popup == this)
                {
                    form.Popup = null;
                }
                form.NeedsPaint = true;
            }
            was.RaiseDropDown(2);
        }

        private static int RowOf(ToolStripItem item) => item.IsSeparator ? SeparatorHeight : RowHeight;

        internal override Rectangle PopupBounds
        {
            get
            {
                if (open == null)
                {
                    return Rectangle.Empty;
                }
                Point origin = OriginInForm();
                int width = 120;
                int height = 4;
                ToolStripItemCollection rows = open.DropDownItems;
                for (int i = 0; i < rows.Count; i++)
                {
                    ToolStripItem row = rows[i];
                    if (!row.Available)
                    {
                        continue;
                    }
                    height += RowOf(row);
                    if (!row.IsSeparator)
                    {
                        string shortcut = row is ToolStripMenuItem menuItem ? menuItem.ShortcutText : string.Empty;
                        int need = CheckMargin + FreeOsWindow.TextWidth(row.PlainText) + 24
                            + (shortcut.Length > 0 ? FreeOsWindow.TextWidth(shortcut) + 24 : 0);
                        width = Math.Max(width, need);
                    }
                }
                return new Rectangle(origin.X + ItemLeft(Items.IndexOf(open)), origin.Y + Height, width, height);
            }
        }

        // Строка всплывающего по высоте от его верха; -1 — рамка.
        private int RowAt(int localY)
        {
            int top = 2;
            ToolStripItemCollection rows = open.DropDownItems;
            for (int i = 0; i < rows.Count; i++)
            {
                if (!rows[i].Available)
                {
                    continue;
                }
                int height = RowOf(rows[i]);
                if (localY >= top && localY < top + height)
                {
                    return i;
                }
                top += height;
            }
            return -1;
        }

        internal override void PaintPopup(int window)
        {
            if (open == null)
            {
                return;
            }
            Rectangle bounds = PopupBounds;
            var g = new Graphics(window, bounds.X, bounds.Y, bounds);
            g.Clear(Color.FromArgb(160, 160, 160));
            g.FillRectangle(new SolidBrush(Color.FromArgb(242, 242, 242)), 1, 1, bounds.Width - 2, bounds.Height - 2);
            int top = 2;
            int textHeight = FreeOsWindow.TextHeight();
            ToolStripItemCollection rows = open.DropDownItems;
            for (int i = 0; i < rows.Count; i++)
            {
                ToolStripItem row = rows[i];
                if (!row.Available)
                {
                    continue;
                }
                int height = RowOf(row);
                if (row.IsSeparator)
                {
                    g.FillRectangle(new SolidBrush(Color.FromArgb(215, 215, 215)), CheckMargin, top + 3, bounds.Width - CheckMargin - 4, 1);
                    top += height;
                    continue;
                }
                if (i == hot && row.Enabled)
                {
                    g.FillRectangle(new SolidBrush(Color.FromArgb(145, 201, 247)), 3, top, bounds.Width - 6, height);
                }
                var brush = new SolidBrush(row.Enabled ? ForeColor : SystemColors.GrayText);
                int textTop = top + (height - textHeight) / 2;
                var menuItem = row as ToolStripMenuItem;
                if (menuItem != null && menuItem.Checked)
                {
                    // Галочка из двух штрихов.
                    int cx = 9;
                    int cy = top + height / 2;
                    g.DrawLine(new Pen(ForeColor), cx, cy, cx + 3, cy + 3);
                    g.DrawLine(new Pen(ForeColor), cx + 3, cy + 3, cx + 9, cy - 3);
                }
                g.DrawString(row.PlainText, Font, brush, CheckMargin + 4, textTop);
                string shortcut = menuItem != null ? menuItem.ShortcutText : string.Empty;
                if (shortcut.Length > 0)
                {
                    g.DrawString(shortcut, Font, brush, bounds.Width - 16 - FreeOsWindow.TextWidth(shortcut), textTop);
                }
                top += height;
            }
        }

        internal override void ClickPopup(int x, int y)
        {
            int index = RowAt(y - PopupBounds.Y);
            if (index < 0)
            {
                return;
            }
            Activate(open.DropDownItems[index]);
        }

        // Пункт выбран: меню закрывается до обработчика, как в Windows.
        private void Activate(ToolStripItem row)
        {
            if (row.IsSeparator || !row.Enabled)
            {
                return;
            }
            Close();
            row.PerformClick();
        }

        internal override bool KeyPopup(Keys key)
        {
            if (open == null)
            {
                return false;
            }
            ToolStripItemCollection rows = open.DropDownItems;
            switch (key)
            {
                case Keys.Down:
                case Keys.Up:
                    int step = key == Keys.Down ? 1 : rows.Count - 1;
                    for (int n = 0; n < rows.Count; n++)
                    {
                        hot = hot < 0 ? (key == Keys.Down ? 0 : rows.Count - 1) : (hot + step) % rows.Count;
                        if (!rows[hot].IsSeparator && rows[hot].Available && rows[hot].Enabled)
                        {
                            break;
                        }
                    }
                    Invalidate();
                    return true;
                case Keys.Left:
                case Keys.Right:
                    int index = Items.IndexOf(open);
                    for (int n = 1; n < Items.Count; n++)
                    {
                        int next = (index + (key == Keys.Right ? n : Items.Count - n)) % Items.Count;
                        var candidate = Items[next] as ToolStripDropDownItem;
                        if (candidate != null && candidate.Available && candidate.Enabled && candidate.HasDropDownItems)
                        {
                            Open(candidate);
                            break;
                        }
                    }
                    return true;
                case Keys.Enter:
                    if (hot >= 0 && hot < rows.Count)
                    {
                        Activate(rows[hot]);
                    }
                    return true;
                case Keys.Escape:
                    Close();
                    return true;
                default:
                    return true;
            }
        }

        internal override void ClosePopup() => Close();
    }
}
