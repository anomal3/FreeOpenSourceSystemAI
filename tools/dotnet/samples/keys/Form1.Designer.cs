namespace KeysApp;

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
        components = new System.ComponentModel.Container();
        menuStrip1 = new MenuStrip();
        fileToolStripMenuItem = new ToolStripMenuItem();
        openToolStripMenuItem = new ToolStripMenuItem();
        recentToolStripMenuItem = new ToolStripMenuItem();
        firstToolStripMenuItem = new ToolStripMenuItem();
        secondToolStripMenuItem = new ToolStripMenuItem();
        toolStripSeparator1 = new ToolStripSeparator();
        exitToolStripMenuItem = new ToolStripMenuItem();
        numericUpDown1 = new NumericUpDown();
        button1 = new Button();
        groupBox1 = new GroupBox();
        radioButton3 = new RadioButton();
        radioButton2 = new RadioButton();
        radioButton1 = new RadioButton();
        toolTip1 = new ToolTip(components);
        menuStrip1.SuspendLayout();
        ((System.ComponentModel.ISupportInitialize)numericUpDown1).BeginInit();
        groupBox1.SuspendLayout();
        SuspendLayout();
        //
        // menuStrip1
        //
        menuStrip1.Items.AddRange(new ToolStripItem[] { fileToolStripMenuItem });
        menuStrip1.Location = new Point(0, 0);
        menuStrip1.Name = "menuStrip1";
        menuStrip1.Size = new Size(320, 28);
        menuStrip1.TabIndex = 0;
        menuStrip1.Text = "menuStrip1";
        //
        // fileToolStripMenuItem
        //
        fileToolStripMenuItem.DropDownItems.AddRange(new ToolStripItem[] { openToolStripMenuItem, recentToolStripMenuItem, toolStripSeparator1, exitToolStripMenuItem });
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
        openToolStripMenuItem.Click += open_Click;
        //
        // recentToolStripMenuItem
        //
        recentToolStripMenuItem.DropDownItems.AddRange(new ToolStripItem[] { firstToolStripMenuItem, secondToolStripMenuItem });
        recentToolStripMenuItem.Name = "recentToolStripMenuItem";
        recentToolStripMenuItem.Size = new Size(181, 26);
        recentToolStripMenuItem.Text = "&Recent";
        //
        // firstToolStripMenuItem
        //
        firstToolStripMenuItem.Name = "firstToolStripMenuItem";
        firstToolStripMenuItem.Size = new Size(224, 26);
        firstToolStripMenuItem.Text = "first.txt";
        firstToolStripMenuItem.Click += recent_Click;
        //
        // secondToolStripMenuItem
        //
        secondToolStripMenuItem.Name = "secondToolStripMenuItem";
        secondToolStripMenuItem.ShortcutKeys = Keys.Control | Keys.Shift | Keys.S;
        secondToolStripMenuItem.Size = new Size(224, 26);
        secondToolStripMenuItem.Text = "second.txt";
        secondToolStripMenuItem.Click += recent_Click;
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
        exitToolStripMenuItem.Click += exit_Click;
        //
        // numericUpDown1
        //
        numericUpDown1.Location = new Point(12, 40);
        numericUpDown1.Name = "numericUpDown1";
        numericUpDown1.Size = new Size(120, 27);
        numericUpDown1.TabIndex = 1;
        numericUpDown1.Value = new decimal(new int[] { 5, 0, 0, 0 });
        numericUpDown1.ValueChanged += numericUpDown1_ValueChanged;
        numericUpDown1.Enter += control_Enter;
        //
        // button1
        //
        button1.Location = new Point(160, 38);
        button1.Name = "button1";
        button1.Size = new Size(94, 29);
        button1.TabIndex = 2;
        button1.Text = "Hello";
        toolTip1.SetToolTip(button1, "Says hello");
        button1.UseVisualStyleBackColor = true;
        button1.Click += button1_Click;
        button1.Enter += control_Enter;
        //
        // groupBox1
        //
        groupBox1.Controls.Add(radioButton3);
        groupBox1.Controls.Add(radioButton2);
        groupBox1.Controls.Add(radioButton1);
        groupBox1.Location = new Point(12, 76);
        groupBox1.Name = "groupBox1";
        groupBox1.Size = new Size(150, 112);
        groupBox1.TabIndex = 3;
        groupBox1.TabStop = false;
        groupBox1.Text = "Color";
        //
        // radioButton3
        //
        radioButton3.AutoSize = true;
        radioButton3.Location = new Point(12, 80);
        radioButton3.Name = "radioButton3";
        radioButton3.Size = new Size(58, 24);
        radioButton3.TabIndex = 2;
        radioButton3.Text = "Blue";
        radioButton3.UseVisualStyleBackColor = true;
        radioButton3.CheckedChanged += radio_CheckedChanged;
        radioButton3.Enter += control_Enter;
        //
        // radioButton2
        //
        radioButton2.AutoSize = true;
        radioButton2.Location = new Point(12, 52);
        radioButton2.Name = "radioButton2";
        radioButton2.Size = new Size(69, 24);
        radioButton2.TabIndex = 1;
        radioButton2.Text = "Green";
        radioButton2.UseVisualStyleBackColor = true;
        radioButton2.CheckedChanged += radio_CheckedChanged;
        radioButton2.Enter += control_Enter;
        //
        // radioButton1
        //
        radioButton1.AutoSize = true;
        radioButton1.Checked = true;
        radioButton1.Location = new Point(12, 24);
        radioButton1.Name = "radioButton1";
        radioButton1.Size = new Size(53, 24);
        radioButton1.TabIndex = 0;
        radioButton1.TabStop = true;
        radioButton1.Text = "Red";
        radioButton1.UseVisualStyleBackColor = true;
        radioButton1.CheckedChanged += radio_CheckedChanged;
        radioButton1.Enter += control_Enter;
        //
        // toolTip1
        //
        toolTip1.AutoPopDelay = 30000;
        toolTip1.Popup += toolTip1_Popup;
        //
        // Form1
        //
        AutoScaleDimensions = new SizeF(8F, 20F);
        AutoScaleMode = AutoScaleMode.Font;
        ClientSize = new Size(320, 200);
        Controls.Add(groupBox1);
        Controls.Add(button1);
        Controls.Add(numericUpDown1);
        Controls.Add(menuStrip1);
        MainMenuStrip = menuStrip1;
        Name = "Form1";
        Text = "Keys";
        menuStrip1.ResumeLayout(false);
        menuStrip1.PerformLayout();
        ((System.ComponentModel.ISupportInitialize)numericUpDown1).EndInit();
        groupBox1.ResumeLayout(false);
        groupBox1.PerformLayout();
        ResumeLayout(false);
        PerformLayout();
    }

    #endregion

    private MenuStrip menuStrip1;
    private ToolStripMenuItem fileToolStripMenuItem;
    private ToolStripMenuItem openToolStripMenuItem;
    private ToolStripMenuItem recentToolStripMenuItem;
    private ToolStripMenuItem firstToolStripMenuItem;
    private ToolStripMenuItem secondToolStripMenuItem;
    private ToolStripSeparator toolStripSeparator1;
    private ToolStripMenuItem exitToolStripMenuItem;
    private NumericUpDown numericUpDown1;
    private Button button1;
    private GroupBox groupBox1;
    private RadioButton radioButton3;
    private RadioButton radioButton2;
    private RadioButton radioButton1;
    private ToolTip toolTip1;
}
