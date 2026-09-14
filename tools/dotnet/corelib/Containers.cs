// Контейнеры формы (фаза N7d): Panel — прямоугольник, в который дизайнер
// кладёт другие элементы и который прижимают к краю через Dock.

using System.Drawing;

namespace System.Windows.Forms
{
    public enum BorderStyle
    {
        None = 0,
        FixedSingle = 1,
        Fixed3D = 2,
    }

    public class Panel : ScrollableControl
    {
        private BorderStyle borderStyle;

        public Panel()
        {
            TabStop = false;
        }

        protected override Size DefaultSize => new Size(200, 100);

        public BorderStyle BorderStyle
        {
            get => borderStyle;
            set
            {
                borderStyle = value;
                Invalidate();
            }
        }

        protected override void OnPaint(PaintEventArgs e)
        {
            if (borderStyle != BorderStyle.None)
            {
                Color edge = borderStyle == BorderStyle.FixedSingle ? Color.FromArgb(100, 100, 100) : Color.FromArgb(160, 160, 160);
                e.Graphics.DrawRectangle(new Pen(edge), 0, 0, Width - 1, Height - 1);
            }
            base.OnPaint(e);
        }
    }
}
