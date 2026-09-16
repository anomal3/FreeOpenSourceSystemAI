// Списки формы (фаза N7b): ListBox и ComboBox из дизайнера.
//
// Правила выбора при правке строк сняты с WinForms образцом `lists`: вставка
// перед выбранной строкой сдвигает SelectedIndex без события, удаление выбранной
// сбрасывает выбор с событием, удаление строки выше сдвигает без события,
// сортировка сохраняет выбранную строку, SelectedItem, которого нет, ничего не
// меняет, ComboBox.Items.Clear() сбрасывает выбор без события.
//
// Строки обходятся циклом по индексу, не foreach (см. IO.cs).

using System.Collections.Generic;
using System.Drawing;

namespace System.Collections
{
    public interface ICollection : IEnumerable
    {
        int Count { get; }

        // Фаза N10: чужой код реализует оба члена явно
        // (`object ICollection.SyncRoot`), и без них в интерфейсе не собирается.
        object SyncRoot { get; }

        bool IsSynchronized { get; }

        void CopyTo(Array array, int index);
    }

    public interface IList : ICollection
    {
        object this[int index] { get; set; }

        bool IsReadOnly { get; }

        int Add(object value);

        bool Contains(object value);

        void Clear();

        int IndexOf(object value);

        void Insert(int index, object value);

        void Remove(object value);

        void RemoveAt(int index);
    }
}

namespace System.Windows.Forms
{
    public enum SelectionMode
    {
        None = 0,
        One = 1,
        MultiSimple = 2,
        MultiExtended = 3,
    }

    public enum ComboBoxStyle
    {
        Simple = 0,
        DropDown = 1,
        DropDownList = 2,
    }

    public abstract class ListControl : Control
    {
        public event EventHandler SelectedIndexChanged;

        public event EventHandler SelectedValueChanged;

        public bool FormattingEnabled { get; set; }

        public string DisplayMember { get; set; } = string.Empty;

        public string ValueMember { get; set; } = string.Empty;

        public abstract int SelectedIndex { get; set; }

        public string GetItemText(object item) => item == null ? string.Empty : item.ToString() ?? string.Empty;

        protected virtual void OnSelectedIndexChanged(EventArgs e)
        {
            SelectedIndexChanged?.Invoke(this, e);
            OnSelectedValueChanged(e);
        }

        protected virtual void OnSelectedValueChanged(EventArgs e) => SelectedValueChanged?.Invoke(this, e);

        internal void RaiseSelectedIndexChanged() => OnSelectedIndexChanged(EventArgs.Empty);

        internal override bool Selectable => true;

        internal override Color AmbientBackColor => SystemColors.Window;

        // Строки и выбор, общие для ListBox и ComboBox.
        internal readonly List<object> Rows = new List<object>();

        internal int Selected = -1;

        internal bool SortedRows;

        internal int FindRow(string s, int startIndex, bool exact)
        {
            if (s == null || Rows.Count == 0)
            {
                return -1;
            }
            int count = Rows.Count;
            for (int step = 1; step <= count; step++)
            {
                int i = (startIndex + step) % count;
                if (startIndex < -1 || startIndex >= count)
                {
                    i = (step - 1) % count;
                }
                string text = GetItemText(Rows[i]);
                bool match = exact
                    ? string.Equals(text, s, StringComparison.OrdinalIgnoreCase)
                    : text.Length >= s.Length && string.Equals(text.Substring(0, s.Length), s, StringComparison.OrdinalIgnoreCase);
                if (match)
                {
                    return i;
                }
            }
            return -1;
        }

        internal int InsertRow(int index, object item)
        {
            if (item == null)
            {
                throw new ArgumentNullException("item");
            }
            if (SortedRows)
            {
                index = 0;
                string text = GetItemText(item);
                while (index < Rows.Count && string.Compare(GetItemText(Rows[index]), text) <= 0)
                {
                    index++;
                }
            }
            if (index < 0 || index > Rows.Count)
            {
                throw new ArgumentOutOfRangeException("index", "InvalidArgument=Value of '" + index + "' is not valid for 'index'.");
            }
            Rows.Insert(index, item);
            if (Selected >= index)
            {
                Selected++;
            }
            Invalidate();
            return index;
        }

        // `true` — выбор сброшен, и событие за вызывающим.
        internal bool RemoveRow(int index)
        {
            if (index < 0 || index >= Rows.Count)
            {
                throw new ArgumentOutOfRangeException("index", "InvalidArgument=Value of '" + index + "' is not valid for 'index'.");
            }
            Rows.RemoveAt(index);
            Invalidate();
            if (Selected == index)
            {
                Selected = -1;
                return true;
            }
            if (Selected > index)
            {
                Selected--;
            }
            return false;
        }

        internal void SortRows()
        {
            object kept = Selected >= 0 ? Rows[Selected] : null;
            for (int i = 1; i < Rows.Count; i++)
            {
                object item = Rows[i];
                string text = GetItemText(item);
                int j = i - 1;
                while (j >= 0 && string.Compare(GetItemText(Rows[j]), text) > 0)
                {
                    Rows[j + 1] = Rows[j];
                    j--;
                }
                Rows[j + 1] = item;
            }
            if (kept != null)
            {
                Selected = Rows.IndexOf(kept);
            }
            Invalidate();
        }

        internal int RowHeight => FreeOsWindow.TextHeight() + 4;

        internal static ArgumentOutOfRangeException BadIndex(int value, string name) =>
            new ArgumentOutOfRangeException("value", "InvalidArgument=Value of '" + value + "' is not valid for '" + name + "'.");

        // Строки с выделенной, начиная с `top`, в прямоугольнике `area`.
        internal void DrawRows(Graphics g, Rectangle area, int top)
        {
            int rowHeight = RowHeight;
            int y = area.Y;
            for (int i = top; i < Rows.Count && y < area.Bottom; i++)
            {
                bool selected = i == Selected;
                if (selected)
                {
                    g.FillRectangle(new SolidBrush(Focused ? SystemColors.Highlight : SystemColors.ControlLight), area.X, y, area.Width, rowHeight);
                }
                g.DrawString(GetItemText(Rows[i]), Font, new SolidBrush(selected && Focused ? SystemColors.HighlightText : ForeColor), area.X + 3, y + 2);
                y += rowHeight;
            }
        }
    }

    public class ListBox : ListControl
    {
        private int top;

        public ListBox()
        {
            Items = new ObjectCollection(this);
        }

        protected override Size DefaultSize => new Size(120, 96);

        public ObjectCollection Items { get; }

        public SelectionMode SelectionMode { get; set; } = SelectionMode.One;

        public bool IntegralHeight { get; set; } = true;

        public int ItemHeight => RowHeight;

        public int TopIndex
        {
            get => top;
            set
            {
                top = Math.Max(0, Math.Min(value, Math.Max(0, Rows.Count - 1)));
                Invalidate();
            }
        }

        public bool Sorted
        {
            get => SortedRows;
            set
            {
                if (SortedRows == value)
                {
                    return;
                }
                SortedRows = value;
                if (value)
                {
                    SortRows();
                }
            }
        }

        public override int SelectedIndex
        {
            get => Selected;
            set
            {
                if (value < -1 || value >= Rows.Count)
                {
                    throw BadIndex(value, "SelectedIndex");
                }
                if (SelectionMode == SelectionMode.None)
                {
                    throw new ArgumentException("Cannot call this method when SelectionMode is SelectionMode.NONE.");
                }
                if (Selected == value)
                {
                    return;
                }
                Selected = value;
                Reveal();
                Invalidate();
                RaiseSelectedIndexChanged();
            }
        }

        public object SelectedItem
        {
            get => Selected >= 0 ? Rows[Selected] : null;
            set
            {
                int index = value == null ? -1 : Rows.IndexOf(value);
                if (index >= 0 || value == null)
                {
                    SelectedIndex = index;
                }
            }
        }

        public void ClearSelected() => SelectedIndex = -1;

        public int FindString(string s) => FindRow(s, -1, false);

        public int FindString(string s, int startIndex) => FindRow(s, startIndex, false);

        public int FindStringExact(string s) => FindRow(s, -1, true);

        public int FindStringExact(string s, int startIndex) => FindRow(s, startIndex, true);

        public int IndexFromPoint(int x, int y)
        {
            int index = top + (y - 1) / RowHeight;
            return y >= 1 && index < Rows.Count ? index : -1;
        }

        private int VisibleRows => Math.Max(1, (Height - 2) / RowHeight);

        private void Reveal()
        {
            if (Selected < 0)
            {
                return;
            }
            if (Selected < top)
            {
                top = Selected;
            }
            else if (Selected >= top + VisibleRows)
            {
                top = Selected - VisibleRows + 1;
            }
        }

        protected override void OnMouseDown(MouseEventArgs e)
        {
            int index = IndexFromPoint(e.X, e.Y);
            if (index >= 0)
            {
                SelectedIndex = index;
            }
            base.OnMouseDown(e);
        }

        // Стрелки и листание — выбор строки, а не переход к соседнему элементу
        // (фаза N7h).
        protected override bool IsInputKey(Keys keyData) => IsNavigationKey(keyData) || base.IsInputKey(keyData);

        internal override void ProcessKey(KeyEventArgs e)
        {
            int count = Rows.Count;
            if (count == 0)
            {
                return;
            }
            int next = Selected;
            switch (e.KeyCode)
            {
                case Keys.Down:
                    next = Math.Min(count - 1, Selected + 1);
                    break;
                case Keys.Up:
                    next = Math.Max(0, Selected - 1);
                    break;
                case Keys.Home:
                    next = 0;
                    break;
                case Keys.End:
                    next = count - 1;
                    break;
                case Keys.PageDown:
                    next = Math.Min(count - 1, Math.Max(Selected, 0) + VisibleRows - 1);
                    break;
                case Keys.PageUp:
                    next = Math.Max(0, Selected - VisibleRows + 1);
                    break;
                default:
                    return;
            }
            SelectedIndex = next;
        }

        protected override void OnPaintBackground(PaintEventArgs pevent)
        {
            Graphics g = pevent.Graphics;
            g.Clear(Focused ? SystemColors.Highlight : Color.FromArgb(122, 122, 122));
            g.FillRectangle(new SolidBrush(BackColor), 1, 1, Width - 2, Height - 2);
        }

        protected override void OnPaint(PaintEventArgs e)
        {
            DrawRows(e.Graphics, new Rectangle(1, 1, Width - 2, Height - 2), top);
            base.OnPaint(e);
        }

        public class ObjectCollection : Collections.IList
        {
            private readonly ListBox owner;

            object Collections.ICollection.SyncRoot => this;

            bool Collections.ICollection.IsSynchronized => false;

            public ObjectCollection(ListBox owner)
            {
                this.owner = owner;
            }

            public int Count => owner.Rows.Count;

            public bool IsReadOnly => false;

            public virtual object this[int index]
            {
                get
                {
                    if (index < 0 || index >= owner.Rows.Count)
                    {
                        throw BadIndex(index, "index");
                    }
                    return owner.Rows[index];
                }
                set
                {
                    if (index < 0 || index >= owner.Rows.Count)
                    {
                        throw BadIndex(index, "index");
                    }
                    owner.Rows[index] = value ?? throw new ArgumentNullException("value");
                    owner.Invalidate();
                }
            }

            public int Add(object item) => owner.InsertRow(owner.Rows.Count, item);

            public void AddRange(object[] items)
            {
                for (int i = 0; i < items.Length; i++)
                {
                    Add(items[i]);
                }
            }

            public void Insert(int index, object item) => owner.InsertRow(index, item);

            public void Remove(object value)
            {
                int index = owner.Rows.IndexOf(value);
                if (index >= 0)
                {
                    RemoveAt(index);
                }
            }

            public void RemoveAt(int index)
            {
                if (owner.RemoveRow(index))
                {
                    owner.RaiseSelectedIndexChanged();
                }
            }

            public void Clear()
            {
                bool hadSelection = owner.Selected >= 0;
                owner.Rows.Clear();
                owner.Selected = -1;
                owner.Invalidate();
                if (hadSelection)
                {
                    owner.RaiseSelectedIndexChanged();
                }
            }

            public bool Contains(object value) => owner.Rows.IndexOf(value) >= 0;

            public int IndexOf(object value) => owner.Rows.IndexOf(value);

            public void CopyTo(Array array, int index)
            {
                for (int i = 0; i < owner.Rows.Count; i++)
                {
                    ((object[])array)[index + i] = owner.Rows[i];
                }
            }

            public Collections.IEnumerator GetEnumerator() => owner.Rows.GetEnumerator();
        }
    }

    public class ComboBox : ListControl
    {
        private ComboBoxStyle style = ComboBoxStyle.DropDown;
        private string editText = string.Empty;
        private bool dropped;
        private int dropTop;

        public ComboBox()
        {
            Items = new ObjectCollection(this);
        }

        protected override Size DefaultSize => new Size(121, 23);

        public ObjectCollection Items { get; }

        public int MaxDropDownItems { get; set; } = 8;

        public int DropDownHeight { get; set; } = 106;

        public int DropDownWidth { get; set; }

        public bool Sorted
        {
            get => SortedRows;
            set
            {
                if (SortedRows != value)
                {
                    SortedRows = value;
                    if (value)
                    {
                        SortRows();
                    }
                }
            }
        }

        public ComboBoxStyle DropDownStyle
        {
            get => style;
            set
            {
                style = value;
                Invalidate();
            }
        }

        public bool DroppedDown
        {
            get => dropped;
            set
            {
                if (dropped == value)
                {
                    return;
                }
                dropped = value;
                Form form = FindForm();
                if (form != null)
                {
                    if (value && form.Popup != null)
                    {
                        form.Popup.ClosePopup();
                    }
                    form.Popup = value ? this : null;
                    form.NeedsPaint = true;
                }
                if (value)
                {
                    dropTop = Math.Max(0, Math.Min(Selected, Rows.Count - DropRows));
                }
            }
        }

        public override string Text
        {
            get => style == ComboBoxStyle.DropDownList ? (Selected >= 0 ? GetItemText(Rows[Selected]) : string.Empty) : editText;
            set
            {
                value = value ?? string.Empty;
                int index = FindRow(value, -1, true);
                if (index >= 0 && GetItemText(Rows[index]) == value)
                {
                    SelectedIndex = index;
                    return;
                }
                if (style != ComboBoxStyle.DropDownList && editText != value)
                {
                    editText = value;
                    OnTextChanged(EventArgs.Empty);
                    Invalidate();
                }
            }
        }

        public override int SelectedIndex
        {
            get => Selected;
            set
            {
                if (value < -1 || value >= Rows.Count)
                {
                    throw BadIndex(value, "SelectedIndex");
                }
                if (Selected == value)
                {
                    return;
                }
                Selected = value;
                if (value >= 0)
                {
                    editText = GetItemText(Rows[value]);
                }
                Invalidate();
                OnTextChanged(EventArgs.Empty);
                RaiseSelectedIndexChanged();
            }
        }

        public object SelectedItem
        {
            get => Selected >= 0 ? Rows[Selected] : null;
            set
            {
                int index = value == null ? -1 : Rows.IndexOf(value);
                if (index >= 0 || value == null)
                {
                    SelectedIndex = index;
                }
            }
        }

        public int FindString(string s) => FindRow(s, -1, false);

        public int FindString(string s, int startIndex) => FindRow(s, startIndex, false);

        public int FindStringExact(string s) => FindRow(s, -1, true);

        private int DropRows => Math.Max(1, Math.Min(MaxDropDownItems, Rows.Count));

        // Открытый список лежит под полем, в точках окна формы.
        internal override Rectangle PopupBounds
        {
            get
            {
                Point origin = OriginInForm();
                return new Rectangle(origin.X, origin.Y + Height, Math.Max(Width, DropDownWidth), DropRows * RowHeight + 2);
            }
        }

        internal override void PaintPopup(int window)
        {
            Rectangle bounds = PopupBounds;
            var g = new Graphics(window, bounds.X, bounds.Y, bounds);
            g.Clear(Color.FromArgb(122, 122, 122));
            g.FillRectangle(new SolidBrush(SystemColors.Window), 1, 1, bounds.Width - 2, bounds.Height - 2);
            DrawRows(g, new Rectangle(1, 1, bounds.Width - 2, bounds.Height - 2), dropTop);
        }

        internal override void ClosePopup() => DroppedDown = false;

        // Щелчок в открытом списке: строка выбирается, список закрывается.
        internal override void ClickPopup(int x, int y)
        {
            int localY = y - PopupBounds.Y;
            int index = dropTop + (localY - 1) / RowHeight;
            DroppedDown = false;
            if (localY >= 1 && index < Rows.Count)
            {
                SelectedIndex = index;
            }
        }

        protected override void OnMouseDown(MouseEventArgs e)
        {
            DroppedDown = !dropped;
            base.OnMouseDown(e);
        }

        protected override bool IsInputKey(Keys keyData) => IsNavigationKey(keyData) || base.IsInputKey(keyData);

        internal override void ProcessKey(KeyEventArgs e)
        {
            int count = Rows.Count;
            switch (e.KeyCode)
            {
                case Keys.Down:
                    if (count > 0)
                    {
                        SelectedIndex = Math.Min(count - 1, Selected + 1);
                    }
                    break;
                case Keys.Up:
                    if (count > 0)
                    {
                        SelectedIndex = Math.Max(0, Selected - 1);
                    }
                    break;
                case Keys.Escape:
                case Keys.Enter:
                    DroppedDown = false;
                    break;
            }
        }

        protected override void OnPaintBackground(PaintEventArgs pevent)
        {
            Graphics g = pevent.Graphics;
            g.Clear(Focused ? SystemColors.Highlight : Color.FromArgb(122, 122, 122));
            Color face = style == ComboBoxStyle.DropDownList ? Color.FromArgb(225, 225, 225) : BackColor;
            g.FillRectangle(new SolidBrush(face), 1, 1, Width - 2, Height - 2);
        }

        protected override void OnPaint(PaintEventArgs e)
        {
            Graphics g = e.Graphics;
            int textTop = (Height - FreeOsWindow.TextHeight()) / 2;
            g.DrawString(Text, Font, new SolidBrush(Enabled ? ForeColor : SystemColors.GrayText), 4, textTop);
            // Стрелка: треугольник из убывающих полос у правого края.
            int arrowX = Width - 14;
            int arrowY = Height / 2 - 2;
            for (int i = 0; i < 4; i++)
            {
                g.FillRectangle(new SolidBrush(Color.FromArgb(51, 51, 51)), arrowX + i, arrowY + i, 7 - 2 * i, 1);
            }
            base.OnPaint(e);
        }

        public class ObjectCollection : Collections.IList
        {
            private readonly ComboBox owner;

            object Collections.ICollection.SyncRoot => this;

            bool Collections.ICollection.IsSynchronized => false;

            public ObjectCollection(ComboBox owner)
            {
                this.owner = owner;
            }

            public int Count => owner.Rows.Count;

            public bool IsReadOnly => false;

            public virtual object this[int index]
            {
                get
                {
                    if (index < 0 || index >= owner.Rows.Count)
                    {
                        throw BadIndex(index, "index");
                    }
                    return owner.Rows[index];
                }
                set
                {
                    if (index < 0 || index >= owner.Rows.Count)
                    {
                        throw BadIndex(index, "index");
                    }
                    owner.Rows[index] = value ?? throw new ArgumentNullException("value");
                    owner.Invalidate();
                }
            }

            public int Add(object item) => owner.InsertRow(owner.Rows.Count, item);

            public void AddRange(object[] items)
            {
                for (int i = 0; i < items.Length; i++)
                {
                    Add(items[i]);
                }
            }

            public void Insert(int index, object item) => owner.InsertRow(index, item);

            public void Remove(object value)
            {
                int index = owner.Rows.IndexOf(value);
                if (index >= 0)
                {
                    RemoveAt(index);
                }
            }

            public void RemoveAt(int index)
            {
                if (owner.RemoveRow(index))
                {
                    owner.editText = string.Empty;
                    owner.RaiseSelectedIndexChanged();
                }
            }

            // Как в WinForms: выбор и текст сбрасываются без события.
            public void Clear()
            {
                owner.Rows.Clear();
                owner.Selected = -1;
                owner.editText = string.Empty;
                owner.DroppedDown = false;
                owner.Invalidate();
            }

            public bool Contains(object value) => owner.Rows.IndexOf(value) >= 0;

            public int IndexOf(object value) => owner.Rows.IndexOf(value);

            public void CopyTo(Array array, int index)
            {
                for (int i = 0; i < owner.Rows.Count; i++)
                {
                    ((object[])array)[index + i] = owner.Rows[i];
                }
            }

            public Collections.IEnumerator GetEnumerator() => owner.Rows.GetEnumerator();
        }
    }
}
