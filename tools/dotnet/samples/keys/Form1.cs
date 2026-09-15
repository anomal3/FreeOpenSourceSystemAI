namespace KeysApp;

// Обработчики — код, написанный руками. Каждый печатает то, что изменилось, —
// по этим строкам стенд сверяет настоящие нажатия на FreeOS с настоящими
// нажатиями в Windows. Самопроверка проводит клавиши через ProcessDialogKey и
// ProcessCmdKey: это те же ворота, через которые WinForms пропускает клавишу
// до элемента в фокусе.
public partial class Form1 : Form
{
    public Form1()
    {
        InitializeComponent();
    }

    private void open_Click(object sender, EventArgs e)
    {
        Console.WriteLine("open: " + openToolStripMenuItem.Text);
    }

    private void recent_Click(object sender, EventArgs e)
    {
        Console.WriteLine("recent: " + ((ToolStripItem)sender).Text);
    }

    private void exit_Click(object sender, EventArgs e)
    {
        Close();
    }

    private void numericUpDown1_ValueChanged(object sender, EventArgs e)
    {
        Console.WriteLine("value: " + numericUpDown1.Value);
    }

    private void button1_Click(object sender, EventArgs e)
    {
        Console.WriteLine("hello");
    }

    private void radio_CheckedChanged(object sender, EventArgs e)
    {
        var radio = (RadioButton)sender;
        Console.WriteLine("radio: " + radio.Text + " " + radio.Checked);
    }

    private void control_Enter(object sender, EventArgs e)
    {
        Console.WriteLine("focus: " + ((Control)sender).Name);
    }

    private void toolTip1_Popup(object sender, PopupEventArgs e)
    {
        Console.WriteLine("tip: " + e.AssociatedControl!.Name + " '" + toolTip1.GetToolTip(e.AssociatedControl) + "'");
    }

    private string Radios() => radioButton1.Checked + " " + radioButton2.Checked + " " + radioButton3.Checked;

    protected override void OnShown(EventArgs e)
    {
        base.OnShown(e);
        Console.WriteLine("shown: " + ActiveControl!.Name + " " + numericUpDown1.Value + " '" + numericUpDown1.Text + "' " + openToolStripMenuItem.ShortcutKeys
            + " " + recentToolStripMenuItem.DropDownItems.Count + " " + recentToolStripMenuItem.HasDropDownItems + " | " + Radios());
        if (!Environment.GetCommandLineArgs().Contains("self-test"))
        {
            return;
        }
        ProcessDialogKey(Keys.Tab);
        ProcessDialogKey(Keys.Tab);
        ProcessDialogKey(Keys.Down);
        ProcessDialogKey(Keys.Down);
        ProcessDialogKey(Keys.Down);
        Console.WriteLine("arrows: " + ActiveControl.Name + " " + Radios());
        ProcessDialogKey(Keys.Shift | Keys.Tab);
        Console.WriteLine("back: " + ActiveControl.Name);
        // Сообщение с окном формы: без него WinForms сочетаний не разбирает.
        var message = Message.Create(Handle, 0x100, default, default);
        Console.WriteLine("shortcuts: " + ProcessCmdKey(ref message, Keys.Control | Keys.O) + " " + ProcessCmdKey(ref message, Keys.Control | Keys.Shift | Keys.S)
            + " " + ProcessCmdKey(ref message, Keys.Control | Keys.P) + " " + ProcessCmdKey(ref message, Keys.O));
        numericUpDown1.Text = "42";
        Console.WriteLine("typed: " + numericUpDown1.Value + " '" + numericUpDown1.Text + "'");
        numericUpDown1.Text = "250";
        Console.WriteLine("clamped: " + numericUpDown1.Value + " '" + numericUpDown1.Text + "'");
        numericUpDown1.Text = "-3";
        Console.WriteLine("below: " + numericUpDown1.Value + " '" + numericUpDown1.Text + "'");
        numericUpDown1.Text = "abc";
        Console.WriteLine("bad: " + numericUpDown1.Value + " '" + numericUpDown1.Text + "'");
        Console.WriteLine("next: " + SelectNextControl(button1, true, true, true, true) + " " + ActiveControl.Name
            + " " + SelectNextControl(numericUpDown1, false, true, true, false) + " " + ActiveControl.Name);
        Close();
    }

    protected override void OnFormClosed(FormClosedEventArgs e)
    {
        Console.WriteLine("closed: " + e.CloseReason + " " + numericUpDown1.Value + " " + Radios());
        base.OnFormClosed(e);
    }
}
