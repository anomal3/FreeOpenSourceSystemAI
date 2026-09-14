namespace ListsApp;

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
        listBox1 = new ListBox();
        comboBox1 = new ComboBox();
        label1 = new Label();
        SuspendLayout();
        // 
        // listBox1
        // 
        listBox1.FormattingEnabled = true;
        listBox1.Items.AddRange(new object[] { "Moscow", "Oslo", "Paris" });
        listBox1.Location = new Point(12, 12);
        listBox1.Name = "listBox1";
        listBox1.Size = new Size(150, 104);
        listBox1.TabIndex = 0;
        listBox1.SelectedIndexChanged += listBox1_SelectedIndexChanged;
        // 
        // comboBox1
        // 
        comboBox1.DropDownStyle = ComboBoxStyle.DropDownList;
        comboBox1.FormattingEnabled = true;
        comboBox1.Items.AddRange(new object[] { "Small", "Medium", "Large" });
        comboBox1.Location = new Point(180, 12);
        comboBox1.Name = "comboBox1";
        comboBox1.Size = new Size(151, 28);
        comboBox1.TabIndex = 1;
        comboBox1.SelectedIndexChanged += comboBox1_SelectedIndexChanged;
        // 
        // label1
        // 
        label1.AutoSize = true;
        label1.Location = new Point(180, 60);
        label1.Name = "label1";
        label1.Size = new Size(0, 20);
        label1.TabIndex = 2;
        // 
        // Form1
        // 
        AutoScaleDimensions = new SizeF(8F, 20F);
        AutoScaleMode = AutoScaleMode.Font;
        ClientSize = new Size(360, 140);
        Controls.Add(label1);
        Controls.Add(comboBox1);
        Controls.Add(listBox1);
        Name = "Form1";
        Text = "Lists";
        ResumeLayout(false);
        PerformLayout();
    }

    #endregion

    private ListBox listBox1;
    private ComboBox comboBox1;
    private Label label1;
}
