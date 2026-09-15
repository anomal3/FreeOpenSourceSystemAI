namespace TabsApp;

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
        tabControl1 = new TabControl();
        tabPage1 = new TabPage();
        label1 = new Label();
        tabPage2 = new TabPage();
        checkBox1 = new CheckBox();
        statusStrip1 = new StatusStrip();
        toolStripStatusLabel1 = new ToolStripStatusLabel();
        toolTip1 = new ToolTip(components);
        tabControl1.SuspendLayout();
        tabPage1.SuspendLayout();
        tabPage2.SuspendLayout();
        statusStrip1.SuspendLayout();
        SuspendLayout();
        //
        // tabControl1
        //
        tabControl1.Controls.Add(tabPage1);
        tabControl1.Controls.Add(tabPage2);
        tabControl1.Dock = DockStyle.Fill;
        tabControl1.Location = new Point(0, 0);
        tabControl1.Name = "tabControl1";
        tabControl1.SelectedIndex = 0;
        tabControl1.Size = new Size(360, 176);
        tabControl1.TabIndex = 0;
        tabControl1.SelectedIndexChanged += tabControl1_SelectedIndexChanged;
        //
        // tabPage1
        //
        tabPage1.Controls.Add(label1);
        tabPage1.Location = new Point(4, 29);
        tabPage1.Name = "tabPage1";
        tabPage1.Padding = new Padding(3);
        tabPage1.Size = new Size(352, 143);
        tabPage1.TabIndex = 0;
        tabPage1.Text = "General";
        tabPage1.UseVisualStyleBackColor = true;
        //
        // label1
        //
        label1.AutoSize = true;
        label1.Location = new Point(12, 12);
        label1.Name = "label1";
        label1.Size = new Size(72, 20);
        label1.TabIndex = 0;
        label1.Text = "First page";
        toolTip1.SetToolTip(label1, "The first page");
        //
        // tabPage2
        //
        tabPage2.Controls.Add(checkBox1);
        tabPage2.Location = new Point(4, 29);
        tabPage2.Name = "tabPage2";
        tabPage2.Padding = new Padding(3);
        tabPage2.Size = new Size(352, 143);
        tabPage2.TabIndex = 1;
        tabPage2.Text = "Options";
        tabPage2.UseVisualStyleBackColor = true;
        //
        // checkBox1
        //
        checkBox1.AutoSize = true;
        checkBox1.Location = new Point(12, 12);
        checkBox1.Name = "checkBox1";
        checkBox1.Size = new Size(83, 24);
        checkBox1.TabIndex = 0;
        checkBox1.Text = "Enabled";
        toolTip1.SetToolTip(checkBox1, "Turns it on");
        checkBox1.UseVisualStyleBackColor = true;
        checkBox1.CheckedChanged += checkBox1_CheckedChanged;
        //
        // statusStrip1
        //
        statusStrip1.ImageScalingSize = new Size(20, 20);
        statusStrip1.Items.AddRange(new ToolStripItem[] { toolStripStatusLabel1 });
        statusStrip1.Location = new Point(0, 176);
        statusStrip1.Name = "statusStrip1";
        statusStrip1.Size = new Size(360, 26);
        statusStrip1.TabIndex = 1;
        statusStrip1.Text = "statusStrip1";
        //
        // toolStripStatusLabel1
        //
        toolStripStatusLabel1.Name = "toolStripStatusLabel1";
        toolStripStatusLabel1.Size = new Size(50, 20);
        toolStripStatusLabel1.Text = "Ready";
        //
        // Form1
        //
        AutoScaleMode = AutoScaleMode.None;
        ClientSize = new Size(360, 202);
        Controls.Add(tabControl1);
        Controls.Add(statusStrip1);
        Name = "Form1";
        Text = "Tabs";
        tabControl1.ResumeLayout(false);
        tabPage1.ResumeLayout(false);
        tabPage1.PerformLayout();
        tabPage2.ResumeLayout(false);
        tabPage2.PerformLayout();
        statusStrip1.ResumeLayout(false);
        statusStrip1.PerformLayout();
        ResumeLayout(false);
        PerformLayout();
    }

    #endregion

    private TabControl tabControl1;
    private TabPage tabPage1;
    private Label label1;
    private TabPage tabPage2;
    private CheckBox checkBox1;
    private StatusStrip statusStrip1;
    private ToolStripStatusLabel toolStripStatusLabel1;
    private ToolTip toolTip1;
}
