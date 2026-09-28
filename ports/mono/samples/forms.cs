// WinForms под Mono во FreeOS (фаза 62): форма с надписью, кнопкой, полем и
// флажком. Каждое действие печатается строкой — по ним стенд проверяет, что
// щелчок дошёл до кнопки, набор — до поля, а форма закрылась по крестику стола.
//
// Раскладка задана числами, а не вычисляется: стенд целится в точки
// содержимого окна (`Aim::Program`), и они обязаны быть теми же, что здесь.
// Тот же файл под .NET Framework на Windows открывает ту же форму.
//
// Язык — C# 5: другого у csc из .NET Framework нет.

using System;
using System.Drawing;
using System.Windows.Forms;

class MainForm : Form
{
    readonly Label label;
    readonly Button button;
    readonly TextBox box;
    readonly CheckBox check;
    int clicks;

    public MainForm ()
    {
        Text = "Mono Forms";
        ClientSize = new Size (360, 200);

        label = new Label ();
        label.Text = "Hello from WinForms on Mono";
        label.Location = new Point (12, 12);
        label.Size = new Size (330, 20);

        button = new Button ();
        button.Text = "Click me";
        button.Location = new Point (12, 44);
        button.Size = new Size (120, 32);
        button.Click += delegate {
            clicks++;
            label.Text = "Clicked " + clicks + " time(s)";
            Console.WriteLine ("forms: clicked " + clicks);
        };

        box = new TextBox ();
        box.Location = new Point (12, 90);
        box.Size = new Size (200, 24);
        box.TextChanged += delegate { Console.WriteLine ("forms: text '" + box.Text + "'"); };

        check = new CheckBox ();
        check.Text = "Checked";
        check.Location = new Point (12, 126);
        check.Size = new Size (120, 24);
        check.CheckedChanged += delegate { Console.WriteLine ("forms: checked " + check.Checked); };

        Controls.Add (label);
        Controls.Add (button);
        Controls.Add (box);
        Controls.Add (check);

        Shown += delegate { Console.WriteLine ("forms: shown " + ClientSize.Width + "x" + ClientSize.Height); };
        FormClosed += delegate { Console.WriteLine ("forms: closed"); };
    }

    [STAThread]
    static int Main ()
    {
        Application.Run (new MainForm ());
        Console.WriteLine ("forms: done");
        return 0;
    }
}
