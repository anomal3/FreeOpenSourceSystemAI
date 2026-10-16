// WinForms под Mono во FreeOS, фаза 62b: то, что у формы бывает поверх неё и
// за её краем. Выпадающий список, контекстное меню, подсказка и главное меню
// старого образца — всплывающие окна внутри формы; выделение текста мышью —
// перетаскивание с захватом указателя (кнопку отпускают за краем формы);
// кнопка, привязанная к углу, — смена размера формы столом; `MessageBox` —
// отдельное окно стола. Фаза 62c: щелчок по другому окну стола закрывает
// открытые меню (`WIN_FOCUS`), клавиша приходит нажатием и отпусканием порознь
// (`WIN_KEYUP`), заголовок меняется на ходу (`freeos_window_title`). Каждое
// действие печатается строкой, по ним сверяет сценарий `mono-popups`.
//
// Раскладка задана числами: стенд целится в точки содержимого окна
// (`Aim::Program`), а они — клиентские точки отсюда плюс высота полосы меню.
// Тот же файл под .NET Framework на Windows открывает ту же форму.
//
// Язык — C# 5: другого у csc из .NET Framework нет.

using System;
using System.Drawing;
using System.Windows.Forms;

class PopupsForm : Form
{
    const string Sample = "drag to select this text";

    readonly ComboBox combo;
    readonly TextBox box;
    readonly Button corner;
    bool shown;

    public PopupsForm ()
    {
        Text = "Mono Popups";
        ClientSize = new Size (400, 240);

        // Главное меню старого образца: полоса над клиентской областью.
        MainMenu menu = new MainMenu ();
        MenuItem file = new MenuItem ("File");
        MenuItem hello = new MenuItem ("Hello");
        hello.Click += delegate { Console.WriteLine ("popups: menu Hello"); };
        file.MenuItems.Add (hello);
        file.MenuItems.Add (new MenuItem ("Other"));
        file.Popup += delegate { Console.WriteLine ("popups: menu opened"); };
        menu.MenuItems.Add (file);
        // Со счётом: меню сворачивается и после выбора пункта, и сценарий
        // ждёт именно то сворачивание, которое вызвал щелчок по другому окну.
        int collapses = 0;
        menu.Collapse += delegate { Console.WriteLine ("popups: menu collapsed " + ++collapses); };
        Menu = menu;

        // Выпадающий список: 12,12 размером 160x21.
        combo = new ComboBox ();
        combo.DropDownStyle = ComboBoxStyle.DropDownList;
        combo.Location = new Point (12, 12);
        combo.Size = new Size (160, 21);
        combo.Items.AddRange (new object[] { "one", "two", "three" });
        combo.SelectedIndex = 0;
        combo.DropDown += delegate { Console.WriteLine ("popups: combo dropped, item height " + combo.ItemHeight); };
        combo.SelectedIndexChanged += delegate { Console.WriteLine ("popups: combo " + combo.SelectedItem); };

        // Поле для выделения мышью: 12,50 размером 220x24.
        box = new TextBox ();
        box.Location = new Point (12, 50);
        box.Size = new Size (220, 24);
        box.Text = Sample;
        box.MouseUp += delegate { Console.WriteLine ("popups: selected " + box.SelectionLength); };
        box.KeyDown += delegate (object sender, KeyEventArgs e) { Console.WriteLine ("popups: key down " + e.KeyCode); };
        box.KeyUp += delegate (object sender, KeyEventArgs e) { Console.WriteLine ("popups: key up " + e.KeyCode); };

        // Вопрос отдельным окном стола: 12,90 размером 90x30.
        Button ask = new Button ();
        ask.Text = "Ask";
        ask.Location = new Point (12, 90);
        ask.Size = new Size (90, 30);
        ask.Click += delegate {
            DialogResult answer = MessageBox.Show (this, "Continue?", "Question", MessageBoxButtons.YesNo);
            Console.WriteLine ("popups: answer " + answer);
        };

        // Новое имя окна: 110,90 размером 90x30.
        Button rename = new Button ();
        rename.Text = "Rename";
        rename.Location = new Point (110, 90);
        rename.Size = new Size (90, 30);
        rename.Click += delegate {
            Text = "Mono Popups renamed";
            Console.WriteLine ("popups: renamed");
        };

        // Кнопка у правого нижнего угла: едет вместе с углом формы.
        corner = new Button ();
        corner.Text = "Corner";
        corner.Size = new Size (90, 30);
        corner.Location = new Point (ClientSize.Width - 102, ClientSize.Height - 42);
        corner.Anchor = AnchorStyles.Bottom | AnchorStyles.Right;

        ToolTip tip = new ToolTip ();
        tip.InitialDelay = 300;
        tip.SetToolTip (corner, "Anchored to the corner");
        tip.Popup += delegate { Console.WriteLine ("popups: tooltip shown"); };

        // Контекстное меню формы — правой кнопкой по пустому месту.
        ContextMenuStrip context = new ContextMenuStrip ();
        context.Items.Add ("Copy").Click += delegate { Console.WriteLine ("popups: context Copy"); };
        context.Items.Add ("Paste");
        context.Opened += delegate { Console.WriteLine ("popups: context opened"); };
        context.Closed += delegate (object sender, ToolStripDropDownClosedEventArgs e) {
            Console.WriteLine ("popups: context closed " + e.CloseReason);
        };
        // «Открыто» печатается до первой перерисовки меню, а первая отрисовка
        // ToolStrip в отладочной Mono — это ещё и компиляция всего его
        // оформления; снимок ждёт, пока меню нарисуется.
        bool context_painted = false;
        context.Paint += delegate {
            if (!context_painted) {
                context_painted = true;
                Console.WriteLine ("popups: context painted");
            }
        };
        ContextMenuStrip = context;

        Controls.Add (combo);
        Controls.Add (box);
        Controls.Add (ask);
        Controls.Add (rename);
        Controls.Add (corner);

        Shown += delegate {
            shown = true;
            Console.WriteLine ("popups: shown " + ClientSize.Width + "x" + ClientSize.Height);
        };
        // «Готова» — когда очередь сообщений впервые опустела после показа:
        // к этому времени форма нарисована и лежит на столе. Показ сам по себе
        // этого не значит — первая перерисовка в отладочной сборке Mono идёт
        // секундами.
        bool ready = false;
        Application.Idle += delegate {
            if (shown && !ready) {
                ready = true;
                Console.WriteLine ("popups: ready");
            }
        };
        Resize += delegate {
            if (shown)
                Console.WriteLine ("popups: resized " + ClientSize.Width + "x" + ClientSize.Height
                    + ", corner at " + corner.Left + "," + corner.Top);
        };
        Activated += delegate { if (shown) Console.WriteLine ("popups: activated"); };
        Deactivate += delegate { if (shown) Console.WriteLine ("popups: deactivated"); };
        FormClosed += delegate { Console.WriteLine ("popups: closed"); };
    }

    [STAThread]
    static int Main ()
    {
        Application.Run (new PopupsForm ());
        Console.WriteLine ("popups: done");
        return 0;
    }
}
