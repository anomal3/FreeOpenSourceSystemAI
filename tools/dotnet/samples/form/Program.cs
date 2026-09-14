// Образец для фазы N6a: окно WinForms — Application.Run, события формы,
// рисование в Paint, щелчки мышью и закрытие.
//
// Масштабирование выключено (AutoScaleMode.None): с ним размеры зависели бы от
// DPI экрана машины, на которой снят эталон. Размер печатается только
// клиентский: внешний у Windows включает рамку своей темы.

namespace FreeOs.Samples.Form;

internal static class Program
{
    [STAThread]
    private static int Main(string[] args)
    {
        ApplicationConfiguration.Initialize();
        Console.WriteLine("form: start");

        // System.Drawing без окна.
        var box = new Rectangle(10, 20, 100, 50);
        Rectangle inflated = Rectangle.Inflate(box, 5, 5);
        Console.WriteLine(box + " " + box.Right + " " + box.Bottom + " " + box.Contains(new Point(50, 40)) + " " + box.Contains(5, 5) + " "
            + inflated + " " + Rectangle.Intersect(box, new Rectangle(60, 0, 100, 40)) + " " + box.IntersectsWith(new Rectangle(200, 0, 1, 1)));
        Console.WriteLine(new Point(3, 4) + " " + (new Point(3, 4) + new Size(10, 20)) + " " + new Size(7, 8) + " " + new SizeF(8F, 20F) + " "
            + new PointF(1.5F, 2F) + " " + Point.Empty.IsEmpty + " " + new Rectangle(new Point(1, 2), new Size(3, 4)).Location);
        Color steel = Color.SteelBlue;
        Color custom = Color.FromArgb(128, 10, 20, 30);
        Console.WriteLine(steel + " " + steel.ToArgb() + " " + steel.R + "," + steel.G + "," + steel.B + " " + steel.IsKnownColor + " " + custom + " "
            + custom.A + " " + custom.Name + " " + Color.FromArgb(255, 70, 130, 180).Equals(steel) + " " + (Color.FromArgb(steel.ToArgb()) == Color.FromArgb(255, 70, 130, 180)) + " "
            + Color.Empty.IsEmpty + " " + Color.White.Name + " " + SystemColors.Control);

        var form = new DemoForm(args.Length > 0 && args[0] == "self-test");
        Application.Run(form);
        Console.WriteLine("form: Run returned, disposed " + form.IsDisposed);
        return 17;
    }
}

internal sealed class DemoForm : System.Windows.Forms.Form
{
    private readonly bool selfTest;
    private int paints;

    public DemoForm(bool selfTest)
    {
        this.selfTest = selfTest;
        SuspendLayout();
        AutoScaleMode = AutoScaleMode.None;
        ClientSize = new Size(480, 320);
        Name = "DemoForm";
        Text = "FreeOS Form";
        BackColor = Color.FromArgb(240, 244, 248);
        MouseClick += OnMouseClicked;
        FormClosing += (sender, e) => Console.WriteLine("closing: " + e.CloseReason + " " + e.Cancel);
        FormClosed += (sender, e) => Console.WriteLine("closed: " + e.CloseReason);
        ResumeLayout(false);
        Console.WriteLine("ctor: " + ClientSize.Width + "x" + ClientSize.Height + " '" + Text + "' " + Name + " " + Visible + " " + BackColor + " "
            + Font.Name + " " + Font.Size + " " + Font.Style + " " + ClientRectangle);
    }

    protected override void OnLoad(EventArgs e)
    {
        Console.WriteLine("load: " + ClientSize);
        base.OnLoad(e);
    }

    protected override void OnShown(EventArgs e)
    {
        base.OnShown(e);
        Console.WriteLine("shown: " + Visible);
        if (selfTest)
        {
            Invalidate();
            Update();
            Console.WriteLine("update: " + paints + " paint(s)");
            Close();
        }
    }

    protected override void OnPaint(PaintEventArgs e)
    {
        base.OnPaint(e);
        paints++;
        Console.WriteLine("paint " + paints + ": " + e.ClipRectangle);
        Graphics g = e.Graphics;
        g.FillRectangle(Brushes.SteelBlue, 20, 20, 440, 80);
        using (var brush = new SolidBrush(Color.White))
        {
            g.DrawString("Hello from WinForms on FreeOS", Font, brush, 32, 48);
        }
        using (var pen = new Pen(Color.DarkOrange, 2))
        {
            g.DrawRectangle(pen, 20, 120, 200, 100);
        }
        g.FillRectangle(new SolidBrush(Color.FromArgb(46, 139, 87)), new Rectangle(240, 120, 220, 100));
        g.DrawString("Click anywhere", Font, Brushes.Black, new PointF(20, 240));
    }

    private void OnMouseClicked(object? sender, MouseEventArgs e)
    {
        Console.WriteLine("mouse: " + e.Button + " at " + e.X + "," + e.Y + " clicks " + e.Clicks);
    }
}
