namespace LayoutApp;

partial class Form1
{
    /// <summary>
    ///  Required designer variable.
    /// </summary>
    private System.ComponentModel.IContainer components = null;

    /// <summary>
    ///  Clean up any resources being used.
    /// </summary>
    /// <param name="disposing">true if managed resources should be disposed; otherwise, false.</param>
    protected override void Dispose(bool disposing)
    {
        if (disposing && (components != null))
        {
            components.Dispose();
        }
        base.Dispose(disposing);
    }

    #region Windows Form Designer generated code

    /// <summary>
    ///  Required method for Designer support - do not modify
    ///  the contents of this method with the code editor.
    /// </summary>
    private void InitializeComponent()
    {
        menuStrip1 = new MenuStrip();
        fileToolStripMenuItem = new ToolStripMenuItem();
        openToolStripMenuItem = new ToolStripMenuItem();
        toolStripSeparator1 = new ToolStripSeparator();
        exitToolStripMenuItem = new ToolStripMenuItem();
        viewToolStripMenuItem = new ToolStripMenuItem();
        wrapToolStripMenuItem = new ToolStripMenuItem();
        panel1 = new Panel();
        panel2 = new Panel();
        button1 = new Button();
        textBox1 = new TextBox();
        label1 = new Label();
        menuStrip1.SuspendLayout();
        panel2.SuspendLayout();
        SuspendLayout();
        //
        // menuStrip1
        //
        menuStrip1.ImageScalingSize = new Size(20, 20);
        menuStrip1.Items.AddRange(new ToolStripItem[] { fileToolStripMenuItem, viewToolStripMenuItem });
        menuStrip1.Location = new Point(0, 0);
        menuStrip1.Name = "menuStrip1";
        menuStrip1.Size = new Size(400, 28);
        menuStrip1.TabIndex = 0;
        menuStrip1.Text = "menuStrip1";
        //
        // fileToolStripMenuItem
        //
        fileToolStripMenuItem.DropDownItems.AddRange(new ToolStripItem[] { openToolStripMenuItem, toolStripSeparator1, exitToolStripMenuItem });
        fileToolStripMenuItem.Name = "fileToolStripMenuItem";
        fileToolStripMenuItem.Size = new Size(46, 24);
        fileToolStripMenuItem.Text = "&File";
        //
        // openToolStripMenuItem
        //
        openToolStripMenuItem.Name = "openToolStripMenuItem";
        openToolStripMenuItem.ShortcutKeys = Keys.Control | Keys.O;
        openToolStripMenuItem.Size = new Size(181, 26);
        openToolStripMenuItem.Text = "&Open";
        openToolStripMenuItem.Click += openToolStripMenuItem_Click;
        //
        // toolStripSeparator1
        //
        toolStripSeparator1.Name = "toolStripSeparator1";
        toolStripSeparator1.Size = new Size(178, 6);
        //
        // exitToolStripMenuItem
        //
        exitToolStripMenuItem.Name = "exitToolStripMenuItem";
        exitToolStripMenuItem.Size = new Size(181, 26);
        exitToolStripMenuItem.Text = "E&xit";
        exitToolStripMenuItem.Click += exitToolStripMenuItem_Click;
        //
        // viewToolStripMenuItem
        //
        viewToolStripMenuItem.DropDownItems.AddRange(new ToolStripItem[] { wrapToolStripMenuItem });
        viewToolStripMenuItem.Name = "viewToolStripMenuItem";
        viewToolStripMenuItem.Size = new Size(55, 24);
        viewToolStripMenuItem.Text = "&View";
        //
        // wrapToolStripMenuItem
        //
        wrapToolStripMenuItem.CheckOnClick = true;
        wrapToolStripMenuItem.Name = "wrapToolStripMenuItem";
        wrapToolStripMenuItem.Size = new Size(224, 26);
        wrapToolStripMenuItem.Text = "&Word wrap";
        wrapToolStripMenuItem.CheckedChanged += wrapToolStripMenuItem_CheckedChanged;
        //
        // panel1
        //
        panel1.BackColor = SystemColors.ControlDark;
        panel1.Dock = DockStyle.Left;
        panel1.Location = new Point(0, 28);
        panel1.Name = "panel1";
        panel1.Size = new Size(100, 198);
        panel1.TabIndex = 1;
        //
        // panel2
        //
        panel2.Controls.Add(button1);
        panel2.Controls.Add(textBox1);
        panel2.Dock = DockStyle.Fill;
        panel2.Location = new Point(100, 28);
        panel2.Name = "panel2";
        panel2.Size = new Size(300, 198);
        panel2.TabIndex = 2;
        //
        // button1
        //
        button1.Anchor = AnchorStyles.Bottom | AnchorStyles.Right;
        button1.Location = new Point(188, 158);
        button1.Name = "button1";
        button1.Size = new Size(100, 28);
        button1.TabIndex = 1;
        button1.Text = "Grow";
        button1.UseVisualStyleBackColor = true;
        button1.Click += button1_Click;
        //
        // textBox1
        //
        textBox1.Anchor = AnchorStyles.Top | AnchorStyles.Left | AnchorStyles.Right;
        textBox1.Location = new Point(12, 12);
        textBox1.Name = "textBox1";
        textBox1.Size = new Size(276, 27);
        textBox1.TabIndex = 0;
        //
        // label1
        //
        label1.Dock = DockStyle.Bottom;
        label1.Location = new Point(0, 226);
        label1.Name = "label1";
        label1.Size = new Size(400, 24);
        label1.TabIndex = 3;
        label1.Text = "Ready";
        //
        // Form1
        //
        AutoScaleMode = AutoScaleMode.None;
        ClientSize = new Size(400, 250);
        Controls.Add(panel2);
        Controls.Add(panel1);
        Controls.Add(label1);
        Controls.Add(menuStrip1);
        MainMenuStrip = menuStrip1;
        Name = "Form1";
        Text = "Layout";
        menuStrip1.ResumeLayout(false);
        menuStrip1.PerformLayout();
        panel2.ResumeLayout(false);
        panel2.PerformLayout();
        ResumeLayout(false);
        PerformLayout();
    }

    #endregion

    private MenuStrip menuStrip1;
    private ToolStripMenuItem fileToolStripMenuItem;
    private ToolStripMenuItem openToolStripMenuItem;
    private ToolStripSeparator toolStripSeparator1;
    private ToolStripMenuItem exitToolStripMenuItem;
    private ToolStripMenuItem viewToolStripMenuItem;
    private ToolStripMenuItem wrapToolStripMenuItem;
    private Panel panel1;
    private Panel panel2;
    private Button button1;
    private TextBox textBox1;
    private Label label1;
}
