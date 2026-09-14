namespace ControlsApp;

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
        textBox1 = new TextBox();
        checkBox1 = new CheckBox();
        label1 = new Label();
        SuspendLayout();
        // 
        // textBox1
        // 
        textBox1.Location = new Point(12, 12);
        textBox1.Name = "textBox1";
        textBox1.Size = new Size(250, 27);
        textBox1.TabIndex = 0;
        textBox1.TextChanged += textBox1_TextChanged;
        // 
        // checkBox1
        // 
        checkBox1.AutoSize = true;
        checkBox1.Location = new Point(12, 52);
        checkBox1.Name = "checkBox1";
        checkBox1.Size = new Size(129, 24);
        checkBox1.TabIndex = 1;
        checkBox1.Text = "Remember me";
        checkBox1.UseVisualStyleBackColor = true;
        checkBox1.CheckedChanged += checkBox1_CheckedChanged;
        // 
        // label1
        // 
        label1.AutoSize = true;
        label1.Location = new Point(12, 88);
        label1.Name = "label1";
        label1.Size = new Size(0, 20);
        label1.TabIndex = 2;
        // 
        // Form1
        // 
        AutoScaleDimensions = new SizeF(8F, 20F);
        AutoScaleMode = AutoScaleMode.Font;
        ClientSize = new Size(400, 140);
        Controls.Add(label1);
        Controls.Add(checkBox1);
        Controls.Add(textBox1);
        Name = "Form1";
        Text = "Controls";
        ResumeLayout(false);
        PerformLayout();
    }

    #endregion

    private TextBox textBox1;
    private CheckBox checkBox1;
    private Label label1;
}
