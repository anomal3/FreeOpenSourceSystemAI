namespace LayoutApp;

// Обработчики — код, написанный руками. Высоты меню и поля ввода зависят от
// шрифта, поэтому печатаются расстояния и равенства, а не они сами.
public partial class Form1 : Form
{
    public Form1()
    {
        InitializeComponent();
    }

    private string Report()
    {
        return "panel1 " + panel1.Left + " " + panel1.Width + " " + (panel1.Top == menuStrip1.Bottom) + " " + (panel1.Bottom == label1.Top)
            + " | panel2 " + panel2.Left + " " + panel2.Width + " " + (panel2.Top == menuStrip1.Bottom)
            + " | label " + label1.Left + " " + label1.Width + " " + label1.Height + " " + (label1.Bottom == ClientSize.Height)
            + " | menu " + menuStrip1.Left + " " + menuStrip1.Width
            + " | text " + textBox1.Left + " " + textBox1.Top + " " + textBox1.Width
            + " | button " + (panel2.Width - button1.Right) + " " + (panel2.Height - button1.Bottom) + " " + button1.Width + " " + button1.Height;
    }

    protected override void OnShown(EventArgs e)
    {
        base.OnShown(e);
        Console.WriteLine("shown: " + Report());
        Console.WriteLine("menu: " + menuStrip1.Dock + " " + menuStrip1.Items.Count + " " + menuStrip1.Items[0].Text + " "
            + fileToolStripMenuItem.DropDownItems.Count + " " + (openToolStripMenuItem.OwnerItem == fileToolStripMenuItem) + " "
            + openToolStripMenuItem.ShortcutKeys + " " + (MainMenuStrip == menuStrip1) + " " + wrapToolStripMenuItem.Checked + " "
            + toolStripSeparator1.GetType().Name + " " + button1.Anchor + " " + panel2.Dock);
        if (Environment.GetCommandLineArgs().Contains("self-test"))
        {
            button1.PerformClick();
            openToolStripMenuItem.PerformClick();
            wrapToolStripMenuItem.PerformClick();
            wrapToolStripMenuItem.PerformClick();
            panel1.Dock = DockStyle.Right;
            Console.WriteLine("right: " + panel1.Left + " " + panel1.Width + " " + panel2.Left + " " + panel2.Width);
            panel1.Visible = false;
            Console.WriteLine("hidden: " + panel2.Left + " " + panel2.Width + " " + textBox1.Width);
            panel1.Visible = true;
            button1.Anchor = AnchorStyles.Top | AnchorStyles.Left;
            ClientSize = new Size(400, 250);
            Console.WriteLine("shrunk: " + Report());
            exitToolStripMenuItem.PerformClick();
        }
    }

    private void button1_Click(object sender, EventArgs e)
    {
        ClientSize = new Size(ClientSize.Width + 100, ClientSize.Height + 50);
        Console.WriteLine("grown: " + ClientSize.Width + "x" + ClientSize.Height + " " + Report());
        label1.Text = "Grown to " + ClientSize.Width + "x" + ClientSize.Height;
    }

    private void openToolStripMenuItem_Click(object sender, EventArgs e)
    {
        Console.WriteLine("open: " + sender.GetType().Name + " " + ((ToolStripMenuItem)sender).Text);
        label1.Text = "Open clicked";
    }

    private void wrapToolStripMenuItem_CheckedChanged(object sender, EventArgs e)
    {
        Console.WriteLine("wrap: " + wrapToolStripMenuItem.Checked + " " + wrapToolStripMenuItem.CheckState);
        label1.Text = "Word wrap: " + wrapToolStripMenuItem.Checked;
    }

    private void exitToolStripMenuItem_Click(object sender, EventArgs e)
    {
        Console.WriteLine("exit");
        Close();
    }

    protected override void OnFormClosed(FormClosedEventArgs e)
    {
        Console.WriteLine("closed: " + e.CloseReason);
        base.OnFormClosed(e);
    }
}
