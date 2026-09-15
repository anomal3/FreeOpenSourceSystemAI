namespace NumbersApp;

// Обработчики — код, написанный руками. Самопроверка нажимает стрелки из кода,
// упирает значение в края и печатает decimal-арифметику, на которой поле
// считает: сложение, умножение, деление, округление, разбор и форматы.
public partial class Form1 : Form
{
    public Form1()
    {
        InitializeComponent();
    }

    private void numericUpDown1_ValueChanged(object sender, EventArgs e)
    {
        Console.WriteLine("value: " + numericUpDown1.Value + " '" + numericUpDown1.Text + "'");
        label1.Text = numericUpDown1.Value.ToString();
    }

    protected override void OnShown(EventArgs e)
    {
        base.OnShown(e);
        Console.WriteLine("shown: " + numericUpDown1.Value + " " + numericUpDown1.Minimum + " " + numericUpDown1.Maximum + " " + numericUpDown1.Increment + " '" + numericUpDown1.Text + "'"
            + " | " + numericUpDown2.Value + " " + numericUpDown2.Increment + " '" + numericUpDown2.Text + "' " + numericUpDown2.DecimalPlaces + " " + numericUpDown2.Hexadecimal
            + " " + numericUpDown2.ThousandsSeparator + " " + numericUpDown1.InterceptArrowKeys + " " + numericUpDown1.ReadOnly + " " + numericUpDown1.TextAlign);
        if (!Environment.GetCommandLineArgs().Contains("self-test"))
        {
            return;
        }
        numericUpDown1.UpButton();
        numericUpDown1.UpButton();
        numericUpDown1.Value = 50;
        numericUpDown1.UpButton();
        numericUpDown1.DownButton();
        try
        {
            numericUpDown1.Value = 51;
        }
        catch (ArgumentOutOfRangeException ex)
        {
            Console.WriteLine("range: " + ex.ParamName + " " + numericUpDown1.Value);
        }
        numericUpDown1.Maximum = 20;
        Console.WriteLine("max: " + numericUpDown1.Maximum + " " + numericUpDown1.Value);
        numericUpDown1.Minimum = 30;
        Console.WriteLine("min: " + numericUpDown1.Minimum + " " + numericUpDown1.Maximum + " " + numericUpDown1.Value);
        numericUpDown2.UpButton();
        Console.WriteLine("second: " + numericUpDown2.Value + " '" + numericUpDown2.Text + "'");
        numericUpDown2.Maximum = 5000;
        numericUpDown2.Value = 1234.5m;
        Console.WriteLine("thousands: " + numericUpDown2.Value + " '" + numericUpDown2.Text + "'");
        numericUpDown2.DecimalPlaces = 0;
        numericUpDown2.Hexadecimal = true;
        Console.WriteLine("hex: '" + numericUpDown2.Text + "'");

        decimal a = 1.1m;
        decimal b = 2.25m;
        Console.WriteLine("math: " + (a + b) + " " + (b - a) + " " + (a - b) + " " + (a * b) + " " + (b / a) + " " + (1m / 3m) + " " + (10m / 4m) + " " + (-a) + " " + (b % a));
        Console.WriteLine("compare: " + (a < b) + " " + (a == 1.10m) + " " + a.Equals(1.100m) + " " + decimal.Compare(b, a) + " " + Math.Max(a, b) + " " + (1.10m).ToString() + " " + (1.10m + 0m));
        Console.WriteLine("round: " + decimal.Round(2.25m, 1) + " " + decimal.Round(2.35m, 1) + " " + Math.Round(2.5m) + " " + Math.Round(3.5m) + " " + decimal.Truncate(-2.7m) + " " + decimal.Floor(-2.7m) + " " + decimal.Ceiling(2.1m) + " " + Math.Round(2.345m, 2, MidpointRounding.AwayFromZero));
        Console.WriteLine("convert: " + (int)b + " " + (long)(-a) + " " + (double)b + " " + (decimal)7 / 4 + " " + (decimal)0.1 + " " + (decimal)1.5f + " " + decimal.MaxValue + " " + decimal.MinusOne + " " + new decimal(123456789, 0, 0, true, 4));
        Console.WriteLine("parse: " + decimal.Parse("3.50") + " " + decimal.Parse("-0.001") + " " + decimal.TryParse("x", out decimal bad) + " " + bad + " " + decimal.Parse("1e3", System.Globalization.NumberStyles.Float));
        Console.WriteLine("format: " + 1234.5m.ToString("F1") + " " + 1234.5m.ToString("N2") + " " + 0.125m.ToString("P1") + " " + 42m.ToString("0000.00") + " " + (-3.14159m).ToString("G3"));
        Console.WriteLine("bits: " + string.Join(",", decimal.GetBits(1.5m)) + " " + string.Join(",", decimal.GetBits(-79228162514264337593543950335m)) + " " + (0.1m + 0.2m == 0.3m));
        Close();
    }

    protected override void OnFormClosed(FormClosedEventArgs e)
    {
        Console.WriteLine("closed: " + e.CloseReason + " " + label1.Text);
        base.OnFormClosed(e);
    }
}
