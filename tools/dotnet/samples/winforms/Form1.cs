namespace WinFormsApp;

// Обработчик кнопки — единственный код, написанный руками. Размеры не
// печатаются: с AutoScaleMode.Font из шаблона они зависят от DPI экрана.
public partial class Form1 : Form
{
    private int clicks;

    public Form1()
    {
        InitializeComponent();
    }

    private void button1_Click(object sender, EventArgs e)
    {
        clicks++;
        label1.Text = "Clicked " + clicks + (clicks == 1 ? " time" : " times");
        Console.WriteLine("button1: " + label1.Text);
    }

    protected override void OnShown(EventArgs e)
    {
        base.OnShown(e);
        Console.WriteLine("shown: " + Text + " | " + button1.Text + " | " + label1.Text + " | " + Controls.Count + " "
            + (Controls[0] == label1) + " " + button1.TabIndex + " " + label1.AutoSize + " " + button1.Parent?.Name);
        if (Environment.GetCommandLineArgs().Contains("self-test"))
        {
            button1.PerformClick();
            button1.PerformClick();
            Close();
        }
    }

    protected override void OnFormClosed(FormClosedEventArgs e)
    {
        Console.WriteLine("closed: " + e.CloseReason + " " + label1.Text);
        base.OnFormClosed(e);
    }
}
