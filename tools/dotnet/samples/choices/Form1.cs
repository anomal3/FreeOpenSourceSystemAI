namespace ChoicesApp;

// Обработчики — код, написанный руками. Самопроверка печатает состояние
// переключателей, полосы хода и ползунка после каждого шага, а размеры — нет:
// их Windows берёт у темы оформления.
public partial class Form1 : Form
{
    public Form1()
    {
        InitializeComponent();
    }

    private string Radios() =>
        radioButton1.Checked + " " + radioButton2.Checked + " " + radioButton3.Checked
        + " | tabstop " + radioButton1.TabStop + " " + radioButton2.TabStop + " " + radioButton3.TabStop;

    private void radio_CheckedChanged(object sender, EventArgs e)
    {
        var radio = (RadioButton)sender;
        Console.WriteLine("radio: " + radio.Text + " " + radio.Checked + " | " + Radios());
        if (radio.Checked)
        {
            label1.Text = radio.Text;
        }
    }

    private void trackBar1_ValueChanged(object sender, EventArgs e)
    {
        Console.WriteLine("track: " + trackBar1.Value);
        progressBar1.Value = trackBar1.Value * 10;
        label1.Text = "Level " + trackBar1.Value;
    }

    protected override void OnShown(EventArgs e)
    {
        base.OnShown(e);
        Console.WriteLine("shown: " + Radios() + " | group " + groupBox1.Text + " " + groupBox1.Controls.Count + " " + groupBox1.TabStop
            + " | progress " + progressBar1.Minimum + " " + progressBar1.Maximum + " " + progressBar1.Value + " " + progressBar1.Step + " " + progressBar1.Style
            + " | track " + trackBar1.Minimum + " " + trackBar1.Maximum + " " + trackBar1.Value + " " + trackBar1.SmallChange + " "
            + trackBar1.LargeChange + " " + trackBar1.TickFrequency + " " + trackBar1.Orientation + " " + trackBar1.TickStyle);
        if (!Environment.GetCommandLineArgs().Contains("self-test"))
        {
            return;
        }
        radioButton2.Checked = true;
        radioButton3.PerformClick();
        radioButton3.Checked = false;
        Console.WriteLine("none: " + Radios());
        radioButton1.Checked = true;
        progressBar1.PerformStep();
        Console.WriteLine("step: " + progressBar1.Value);
        progressBar1.Increment(100);
        Console.WriteLine("increment: " + progressBar1.Value);
        progressBar1.Increment(-500);
        Console.WriteLine("decrement: " + progressBar1.Value);
        try
        {
            progressBar1.Value = 101;
        }
        catch (ArgumentOutOfRangeException ex)
        {
            Console.WriteLine("range: " + ex.GetType().Name + " " + ex.ParamName + " " + progressBar1.Value);
        }
        trackBar1.Value = 7;
        trackBar1.Value = 7;
        trackBar1.Maximum = 5;
        Console.WriteLine("max: " + trackBar1.Maximum + " " + trackBar1.Value + " " + progressBar1.Value);
        trackBar1.Minimum = 6;
        Console.WriteLine("min: " + trackBar1.Minimum + " " + trackBar1.Maximum + " " + trackBar1.Value);
        try
        {
            trackBar1.Value = 0;
        }
        catch (ArgumentOutOfRangeException ex)
        {
            Console.WriteLine("track range: " + ex.ParamName + " " + trackBar1.Value);
        }
        progressBar1.Maximum = 50;
        Console.WriteLine("progress max: " + progressBar1.Maximum + " " + progressBar1.Value);
        Close();
    }

    protected override void OnFormClosed(FormClosedEventArgs e)
    {
        Console.WriteLine("closed: " + e.CloseReason + " " + label1.Text);
        base.OnFormClosed(e);
    }
}
