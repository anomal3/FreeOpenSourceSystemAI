namespace ControlsApp;

// Обработчики — код, написанный руками; остальное — из шаблона и дизайнера.
// Фокус и нажатия клавиш из кода не проверяются: под Windows они зависят от
// того, активно ли окно, а стенд проверяет их настоящей клавиатурой.
public partial class Form1 : Form
{
    private int changes;

    public Form1()
    {
        InitializeComponent();
        checkBox1.CheckStateChanged += (sender, e) => Console.WriteLine("state: " + checkBox1.CheckState);
    }

    private void textBox1_TextChanged(object sender, EventArgs e)
    {
        changes++;
        Report("text");
    }

    private void checkBox1_CheckedChanged(object sender, EventArgs e) => Report("checked");

    private void Report(string what)
    {
        label1.Text = textBox1.Text + " / " + checkBox1.CheckState;
        Console.WriteLine(what + ": " + textBox1.Text + " | " + checkBox1.Checked + " " + checkBox1.CheckState + " | " + changes);
    }

    protected override void OnShown(EventArgs e)
    {
        base.OnShown(e);
        Console.WriteLine("shown: " + textBox1.Text.Length + " " + textBox1.MaxLength + " " + textBox1.Multiline + " " + textBox1.ReadOnly + " "
            + checkBox1.Checked + " " + checkBox1.CheckState + " " + checkBox1.ThreeState + " " + checkBox1.AutoCheck + " " + label1.Text.Length);
        if (!Environment.GetCommandLineArgs().Contains("self-test"))
        {
            return;
        }
        textBox1.Text = "hello";
        textBox1.Text = "hello";
        textBox1.AppendText(" world");
        Console.WriteLine("caret: " + textBox1.SelectionStart + " " + textBox1.SelectionLength + " " + textBox1.TextLength);
        textBox1.Select(1, 3);
        Console.WriteLine("selected: " + textBox1.SelectedText + " " + textBox1.SelectionStart + " " + textBox1.SelectionLength);
        textBox1.MaxLength = 5;
        textBox1.Text = "abcdefgh";
        textBox1.Clear();
        checkBox1.Checked = true;
        checkBox1.Checked = true;
        checkBox1.ThreeState = true;
        checkBox1.CheckState = CheckState.Indeterminate;
        checkBox1.CheckState = CheckState.Unchecked;
        Console.WriteLine("label: " + label1.Text);
        Close();
    }
}
