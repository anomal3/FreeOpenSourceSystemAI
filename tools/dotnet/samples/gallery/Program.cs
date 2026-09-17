namespace Gallery;

// Витрина образцов: окно со списком кнопок, по кнопке — форма образца или
// окно с выводом консольного образца. Сделана для телефона: запускается с
// домашнего экрана, а у /bin/dotnet одна программа на запуск.

internal static class Program
{
    [STAThread]
    private static int Main(string[] args)
    {
        ApplicationConfiguration.Initialize();
        bool selfTest = args.Length > 0 && args[0] == "self-test";
        if (selfTest)
        {
            return SelfTest();
        }
        Application.Run(new GalleryForm());
        return 0;
    }

    // Самопроверка: всё, что есть в витрине, по очереди и без окна витрины.
    // Формы в режиме self-test закрываются сами; вывод консольных образцов
    // перехватывается и печатается с числом строк — так видно, что SetOut
    // поймал всё.
    private static int SelfTest()
    {
        Console.WriteLine("gallery: " + Samples.Forms.Length + " form(s), " + Samples.Consoles.Length + " console sample(s)");
        foreach (FormSample sample in Samples.Forms)
        {
            Console.WriteLine("== " + sample.Name);
            using (Form form = sample.Create())
            {
                form.ShowDialog();
            }
        }
        foreach (ConsoleSample sample in Samples.Consoles)
        {
            if (sample.Name == "files" || sample.Name == "gc")
            {
                // files пишет в домашний каталог, gc долгий: на стенде они
                // проверены своими запусками, здесь хватает остальных.
                continue;
            }
            Output output = Samples.Run(sample);
            Console.WriteLine("== " + sample.Name + ": code " + output.Code + ", " + output.Lines.Count + " line(s)");
            foreach (string line in output.Lines)
            {
                Console.WriteLine(line);
            }
        }
        return 5;
    }
}

internal sealed class FormSample
{
    public FormSample(string name, string about, Func<Form> create)
    {
        Name = name;
        About = about;
        Create = create;
    }

    public string Name { get; }

    public string About { get; }

    public Func<Form> Create { get; }
}

internal sealed class ConsoleSample
{
    public ConsoleSample(string name, string about, Func<int> main)
    {
        Name = name;
        About = about;
        Main = main;
    }

    public string Name { get; }

    public string About { get; }

    public Func<int> Main { get; }
}

internal sealed class Output
{
    public int Code;
    public List<string> Lines = new List<string>();
}

internal static class Samples
{
    public static readonly FormSample[] Forms =
    {
        new FormSample("winforms", "Кнопка и надпись", () => new WinFormsApp.Form1()),
        new FormSample("form", "Рисование и мышь", () => new FreeOs.Samples.Form.DemoForm(IsSelfTest())),
        new FormSample("controls", "Поле ввода, флажок", () => new ControlsApp.Form1()),
        new FormSample("lists", "Список, выпадающий список", () => new ListsApp.Form1()),
        new FormSample("dialogs", "Таймер, диалоги", () => new DialogsApp.Form1()),
        new FormSample("layout", "Меню, Anchor и Dock", () => new LayoutApp.Form1()),
        new FormSample("choices", "Переключатели, прогресс", () => new ChoicesApp.Form1()),
        new FormSample("tabs", "Вкладки, строка состояния", () => new TabsApp.Form1()),
        new FormSample("numbers", "Числа, NumericUpDown", () => new NumbersApp.Form1()),
        new FormSample("keys", "Клавиатура, подменю", () => new KeysApp.Form1()),
    };

    public static readonly ConsoleSample[] Consoles =
    {
        new ConsoleSample("objects", "Классы и интерфейсы", FreeOs.Samples.Objects.Program.Main),
        new ConsoleSample("exceptions", "Исключения", FreeOs.Samples.Exceptions.Program.Main),
        new ConsoleSample("generics", "Обобщения, делегаты", FreeOs.Samples.Generics.Program.Main),
        new ConsoleSample("text", "Строки и формат", FreeOs.Samples.Text.Program.Main),
        new ConsoleSample("collections", "Коллекции", FreeOs.Samples.Collections.Program.Main),
        new ConsoleSample("floats", "Дробные числа", FreeOs.Samples.Floats.Program.Main),
        new ConsoleSample("enums", "Перечисления", FreeOs.Samples.Enums.Program.Main),
        new ConsoleSample("linq", "LINQ", FreeOs.Samples.Linq.Program.Main),
        new ConsoleSample("pqueue", "Очередь с приоритетом", FreeOs.Samples.PQueue.Program.Main),
        new ConsoleSample("files", "Файлы и каталоги", FreeOs.Samples.Files.Program.Main),
        new ConsoleSample("gc", "Сборка мусора", FreeOs.Samples.Gc.Program.Main),
    };

    private static bool IsSelfTest() => Environment.GetCommandLineArgs().Contains("self-test");

    // Выполнить консольный образец, собрав его вывод по строкам. Исключение
    // образца — тоже вывод: витрина не должна падать вместе с ним.
    public static Output Run(ConsoleSample sample)
    {
        var output = new Output();
        var writer = new LineWriter(output.Lines);
        TextWriter previous = Console.Out;
        Console.SetOut(writer);
        try
        {
            output.Code = sample.Main();
        }
        catch (Exception error)
        {
            writer.WriteLine("unhandled: " + error.GetType().Name + ": " + error.Message);
            output.Code = -1;
        }
        finally
        {
            writer.Finish();
            Console.SetOut(previous);
        }
        return output;
    }
}

// Писатель, режущий текст на строки. Перевод строки — '\n'; '\r' перед ним
// отбрасывается, чтобы вывод под Windows и у своей среды совпадал.
internal sealed class LineWriter : TextWriter
{
    private readonly List<string> lines;
    private readonly System.Text.StringBuilder current = new System.Text.StringBuilder();

    public LineWriter(List<string> lines)
    {
        this.lines = lines;
    }

    public override System.Text.Encoding Encoding => System.Text.Encoding.UTF8;

    public override void Write(char value)
    {
        if (value == '\n')
        {
            lines.Add(current.ToString());
            current.Clear();
        }
        else if (value != '\r')
        {
            current.Append(value);
        }
    }

    public override void Write(string? value)
    {
        if (value == null)
        {
            return;
        }
        for (int i = 0; i < value.Length; i++)
        {
            Write(value[i]);
        }
    }

    public void Finish()
    {
        if (current.Length > 0)
        {
            lines.Add(current.ToString());
            current.Clear();
        }
    }
}

// Окно витрины: две колонки кнопок — формы слева, консольные образцы справа.
internal sealed class GalleryForm : Form
{
    private const int Row = 44;
    private const int Gap = 6;
    private const int Column = 172;
    private readonly Label status;

    public GalleryForm()
    {
        SuspendLayout();
        AutoScaleMode = AutoScaleMode.None;
        Text = "Примеры";
        int rows = Math.Max(Samples.Forms.Length, Samples.Consoles.Length);
        int top = 36;
        ClientSize = new Size(12 + Column + 8 + Column + 12, top + rows * (Row + Gap) + 40);

        AddCaption("Окна WinForms", 12);
        AddCaption("Консоль", 12 + Column + 8);
        for (int i = 0; i < Samples.Forms.Length; i++)
        {
            FormSample sample = Samples.Forms[i];
            AddButton(sample.Name, sample.About, 12, top + i * (Row + Gap), (sender, e) => OpenForm(sample));
        }
        for (int i = 0; i < Samples.Consoles.Length; i++)
        {
            ConsoleSample sample = Samples.Consoles[i];
            AddButton(sample.Name, sample.About, 12 + Column + 8, top + i * (Row + Gap), (sender, e) => RunConsole(sample));
        }

        status = new Label();
        status.AutoSize = false;
        status.Location = new Point(12, top + rows * (Row + Gap) + 6);
        status.Size = new Size(Column * 2 + 8, 24);
        status.Text = "Нажмите на пример";
        Controls.Add(status);
        ResumeLayout(false);
    }

    private void AddCaption(string text, int x)
    {
        var label = new Label();
        label.AutoSize = false;
        label.Location = new Point(x, 8);
        label.Size = new Size(Column, 24);
        label.Text = text;
        Controls.Add(label);
    }

    private void AddButton(string name, string about, int x, int y, EventHandler click)
    {
        var button = new Button();
        button.Location = new Point(x, y);
        button.Size = new Size(Column, Row);
        button.Text = name;
        button.Click += click;
        Controls.Add(button);
        var tip = about;
        button.MouseEnter += (sender, e) => status.Text = name + " — " + tip;
    }

    private void OpenForm(FormSample sample)
    {
        status.Text = "Открыт " + sample.Name;
        using (Form form = sample.Create())
        {
            form.ShowDialog(this);
        }
        status.Text = "Закрыт " + sample.Name;
    }

    private void RunConsole(ConsoleSample sample)
    {
        status.Text = "Выполняется " + sample.Name + "…";
        status.Update();
        Output output = Samples.Run(sample);
        status.Text = sample.Name + ": код " + output.Code + ", строк " + output.Lines.Count;
        using (var window = new OutputForm(sample, output))
        {
            window.ShowDialog(this);
        }
    }
}

// Вывод консольного образца: строки списком, код возврата в заголовке.
internal sealed class OutputForm : Form
{
    public OutputForm(ConsoleSample sample, Output output)
    {
        SuspendLayout();
        AutoScaleMode = AutoScaleMode.None;
        Text = sample.Name + " — код " + output.Code;
        ClientSize = new Size(360, 520);

        var list = new ListBox();
        list.Dock = DockStyle.Fill;
        list.IntegralHeight = false;
        foreach (string line in output.Lines)
        {
            list.Items.Add(line.Length == 0 ? " " : line);
        }
        Controls.Add(list);

        var close = new Button();
        close.Dock = DockStyle.Bottom;
        close.Height = 40;
        close.Text = "Закрыть";
        close.Click += (sender, e) => Close();
        Controls.Add(close);
        ResumeLayout(false);
    }
}
