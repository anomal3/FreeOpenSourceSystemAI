namespace DialogsApp;

// Обработчики — код, написанный руками. Самопроверка печатает счёт тиков и
// состояние таймера, а не время: сколько миллисекунд прошло, зависит от машины.
public partial class Form1 : Form
{
    private int ticks;
    private bool selfTest;

    public Form1()
    {
        InitializeComponent();
    }

    private void timer1_Tick(object sender, EventArgs e)
    {
        ticks++;
        Console.WriteLine("tick " + ticks + " " + timer1.Enabled + " " + timer1.Interval);
        label1.Text = "Ticks: " + ticks;
        if (ticks == 3)
        {
            timer1.Stop();
            Console.WriteLine("stopped: " + timer1.Enabled);
            if (selfTest)
            {
                timer1.Start();
                timer1.Enabled = false;
                Console.WriteLine("restart and disable: " + timer1.Enabled + " " + ticks);
                Close();
            }
        }
    }

    private void button1_Click(object sender, EventArgs e)
    {
        DialogResult answer = MessageBox.Show("Save the changes?", "Question", MessageBoxButtons.YesNo, MessageBoxIcon.Question);
        Console.WriteLine("answer: " + answer);
        label1.Text = "Answer: " + answer;
        DialogResult ok = MessageBox.Show("Done");
        Console.WriteLine("ok: " + ok);
    }

    protected override void OnShown(EventArgs e)
    {
        base.OnShown(e);
        selfTest = Environment.GetCommandLineArgs().Contains("self-test");
        Console.WriteLine("shown: " + timer1.Enabled + " " + timer1.Interval + " " + (timer1.Tag ?? "null") + " " + DialogResult + " "
            + MessageBoxButtons.YesNoCancel + " " + (int)DialogResult.Yes + " " + MessageBoxIcon.Warning + " " + (int)MessageBoxIcon.Warning);
        timer1.Start();
        Console.WriteLine("started: " + timer1.Enabled);
    }

    protected override void OnFormClosed(FormClosedEventArgs e)
    {
        Console.WriteLine("closed: " + e.CloseReason + " " + ticks);
        base.OnFormClosed(e);
    }
}
