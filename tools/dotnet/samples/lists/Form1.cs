namespace ListsApp;

// Обработчики — код, написанный руками. Порядок и число событий при вставке,
// удалении и сортировке — то, что образец снимает с WinForms.
public partial class Form1 : Form
{
    public Form1()
    {
        InitializeComponent();
    }

    private void listBox1_SelectedIndexChanged(object sender, EventArgs e) =>
        Console.WriteLine("list: " + listBox1.SelectedIndex + " " + (listBox1.SelectedItem ?? "null") + " | " + Items(listBox1.Items));

    private void comboBox1_SelectedIndexChanged(object sender, EventArgs e)
    {
        label1.Text = comboBox1.Text;
        Console.WriteLine("combo: " + comboBox1.SelectedIndex + " " + (comboBox1.SelectedItem ?? "null") + " '" + comboBox1.Text + "' | " + Items(comboBox1.Items));
    }

    private static string Items(System.Collections.IList items)
    {
        var parts = new List<string>();
        foreach (object item in items)
        {
            parts.Add(item.ToString() ?? "");
        }
        return string.Join(",", parts);
    }

    protected override void OnShown(EventArgs e)
    {
        base.OnShown(e);
        Console.WriteLine("shown: " + listBox1.Items.Count + " " + listBox1.SelectedIndex + " " + listBox1.SelectionMode + " " + listBox1.Sorted + " | "
            + comboBox1.Items.Count + " " + comboBox1.SelectedIndex + " " + comboBox1.DropDownStyle + " '" + comboBox1.Text + "'");
        if (!Environment.GetCommandLineArgs().Contains("self-test"))
        {
            return;
        }
        listBox1.SelectedIndex = 1;
        listBox1.SelectedIndex = 1;
        listBox1.Items.Add("Tokyo");
        listBox1.Items.Insert(0, "Berlin");
        Console.WriteLine("after insert: " + listBox1.SelectedIndex + " " + listBox1.SelectedItem);
        Console.WriteLine("find: " + listBox1.FindString("pa") + " " + listBox1.FindStringExact("Oslo") + " " + listBox1.FindString("x") + " " + listBox1.Items.IndexOf("Tokyo") + " " + listBox1.Items.Contains("Rome"));
        listBox1.Items.Remove("Oslo");
        Console.WriteLine("after remove: " + listBox1.SelectedIndex);
        listBox1.SelectedItem = "Paris";
        listBox1.Sorted = true;
        Console.WriteLine("sorted: " + Items(listBox1.Items) + " " + listBox1.SelectedIndex);
        listBox1.Items.RemoveAt(0);
        listBox1.ClearSelected();
        comboBox1.SelectedIndex = 2;
        comboBox1.SelectedItem = "Small";
        comboBox1.SelectedItem = "Nothing";
        Console.WriteLine("combo text: '" + comboBox1.Text + "' " + comboBox1.SelectedIndex);
        comboBox1.Items.Clear();
        Console.WriteLine("cleared: " + comboBox1.SelectedIndex + " '" + comboBox1.Text + "' " + listBox1.Items.Count);
        try
        {
            listBox1.SelectedIndex = 10;
        }
        catch (ArgumentOutOfRangeException error)
        {
            Console.WriteLine("range: " + error.GetType().Name);
        }
        Close();
    }
}
