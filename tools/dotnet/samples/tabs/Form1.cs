namespace TabsApp;

// Обработчики — код, написанный руками. Самопроверка печатает выбор вкладки,
// видимость страниц, строку состояния и подсказки после каждого шага; размеры
// вкладок не печатаются — их Windows берёт у темы оформления.
public partial class Form1 : Form
{
    public Form1()
    {
        InitializeComponent();
    }

    private string Pages()
    {
        string text = tabControl1.TabCount + " " + tabControl1.SelectedIndex + " " + (tabControl1.SelectedTab == null ? "null" : tabControl1.SelectedTab.Text) + " |";
        for (int i = 0; i < tabControl1.TabPages.Count; i++)
        {
            TabPage page = tabControl1.TabPages[i];
            text += " " + page.Text + ":" + page.Visible;
        }
        return text;
    }

    private void tabControl1_SelectedIndexChanged(object sender, EventArgs e)
    {
        Console.WriteLine("selected: " + Pages());
        toolStripStatusLabel1.Text = tabControl1.SelectedTab == null ? "No page" : "Page " + tabControl1.SelectedTab.Text;
    }

    private void checkBox1_CheckedChanged(object sender, EventArgs e)
    {
        Console.WriteLine("checked: " + checkBox1.Checked);
        toolStripStatusLabel1.Text = "Enabled: " + checkBox1.Checked;
    }

    protected override void OnShown(EventArgs e)
    {
        base.OnShown(e);
        Console.WriteLine("shown: " + Pages() + " | " + (tabPage2.Parent == tabControl1) + " " + tabPage1.Padding.All + " " + tabControl1.Alignment + " " + tabControl1.Appearance);
        Console.WriteLine("status: " + statusStrip1.Dock + " " + statusStrip1.Items.Count + " " + toolStripStatusLabel1.Text + " " + toolStripStatusLabel1.Spring + " " + statusStrip1.SizingGrip);
        Console.WriteLine("tips: '" + toolTip1.GetToolTip(label1) + "' '" + toolTip1.GetToolTip(checkBox1) + "' '" + toolTip1.GetToolTip(tabControl1) + "' "
            + toolTip1.Active + " " + toolTip1.AutomaticDelay + " " + toolTip1.AutoPopDelay + " " + toolTip1.InitialDelay + " " + toolTip1.ReshowDelay + " " + toolTip1.ShowAlways);
        if (!Environment.GetCommandLineArgs().Contains("self-test"))
        {
            return;
        }
        tabControl1.SelectedIndex = 1;
        tabControl1.SelectedIndex = 1;
        tabControl1.SelectedTab = tabPage1;
        checkBox1.Checked = true;
        tabControl1.SelectedIndex = 1;
        tabControl1.TabPages.Remove(tabPage2);
        Console.WriteLine("removed: " + Pages() + " | " + (tabPage2.Parent == null));
        tabControl1.TabPages.Add("Extra");
        tabControl1.TabPages.Add(tabPage2);
        Console.WriteLine("added: " + Pages() + " " + tabControl1.TabPages[1].Text + " " + tabControl1.TabPages[1].Name.Length);
        tabControl1.SelectedTab = tabPage2;
        try
        {
            tabControl1.SelectedIndex = -2;
        }
        catch (Exception ex)
        {
            Console.WriteLine("range: " + ex.GetType().Name);
        }
        toolTip1.SetToolTip(label1, null);
        toolTip1.SetToolTip(tabControl1, "Pages");
        Console.WriteLine("tips now: '" + toolTip1.GetToolTip(label1) + "' '" + toolTip1.GetToolTip(tabControl1) + "'");
        toolStripStatusLabel1.Spring = true;
        statusStrip1.Items.Add("More");
        Console.WriteLine("status now: " + statusStrip1.Items.Count + " " + statusStrip1.Items[1].GetType().Name + " " + statusStrip1.Items[1].Text + " " + toolStripStatusLabel1.Text);
        Close();
    }

    protected override void OnFormClosed(FormClosedEventArgs e)
    {
        Console.WriteLine("closed: " + e.CloseReason + " " + toolStripStatusLabel1.Text);
        base.OnFormClosed(e);
    }
}
