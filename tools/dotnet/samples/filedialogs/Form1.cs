namespace FileDialogsApp;

// Три кнопки — три окна выбора. Ответ печатается: стенд ждёт эти строки.
public partial class Form1 : Form
{
    public Form1()
    {
        InitializeComponent();
    }

    private void openButton_Click(object sender, EventArgs e)
    {
        using var dialog = new OpenFileDialog
        {
            Title = "Open a text file",
            Filter = "Text files (*.txt)|*.txt|All files (*.*)|*.*",
            FilterIndex = 2,
        };
        DialogResult answer = dialog.ShowDialog(this);
        Console.WriteLine("open: " + answer + " " + (answer == DialogResult.OK ? dialog.FileName : "-"));
        label1.Text = "Open: " + (answer == DialogResult.OK ? dialog.FileName : answer.ToString());
    }

    private void saveButton_Click(object sender, EventArgs e)
    {
        using var dialog = new SaveFileDialog
        {
            Filter = "Text files (*.txt)|*.txt|All files (*.*)|*.*",
            FileName = "winforms-note",
            DefaultExt = "txt",
        };
        DialogResult answer = dialog.ShowDialog(this);
        if (answer == DialogResult.OK)
        {
            File.WriteAllText(dialog.FileName, "Saved from WinForms\n");
        }
        Console.WriteLine("save: " + answer + " " + (answer == DialogResult.OK ? dialog.FileName : "-"));
        label1.Text = "Save: " + (answer == DialogResult.OK ? dialog.FileName : answer.ToString());
    }

    private void folderButton_Click(object sender, EventArgs e)
    {
        using var dialog = new FolderBrowserDialog
        {
            Description = "Pick a folder",
        };
        DialogResult answer = dialog.ShowDialog(this);
        Console.WriteLine("folder: " + answer + " " + (answer == DialogResult.OK ? dialog.SelectedPath : "-"));
        label1.Text = "Folder: " + (answer == DialogResult.OK ? dialog.SelectedPath : answer.ToString());
    }

    protected override void OnShown(EventArgs e)
    {
        base.OnShown(e);
        Console.WriteLine("shown: file dialogs");
        if (Environment.GetCommandLineArgs().Contains("self-test"))
        {
            Close();
        }
    }

    protected override void OnFormClosed(FormClosedEventArgs e)
    {
        Console.WriteLine("closed: " + e.CloseReason);
        base.OnFormClosed(e);
    }
}
