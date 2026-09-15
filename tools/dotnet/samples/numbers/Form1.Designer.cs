namespace NumbersApp;

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
        numericUpDown1 = new NumericUpDown();
        numericUpDown2 = new NumericUpDown();
        label1 = new Label();
        ((System.ComponentModel.ISupportInitialize)numericUpDown1).BeginInit();
        ((System.ComponentModel.ISupportInitialize)numericUpDown2).BeginInit();
        SuspendLayout();
        //
        // numericUpDown1
        //
        numericUpDown1.Location = new Point(12, 12);
        numericUpDown1.Maximum = new decimal(new int[] { 50, 0, 0, 0 });
        numericUpDown1.Minimum = new decimal(new int[] { 5, 0, 0, 0 });
        numericUpDown1.Name = "numericUpDown1";
        numericUpDown1.Size = new Size(120, 27);
        numericUpDown1.TabIndex = 0;
        numericUpDown1.Value = new decimal(new int[] { 10, 0, 0, 0 });
        numericUpDown1.ValueChanged += numericUpDown1_ValueChanged;
        //
        // numericUpDown2
        //
        numericUpDown2.DecimalPlaces = 2;
        numericUpDown2.Increment = new decimal(new int[] { 25, 0, 0, 131072 });
        numericUpDown2.Location = new Point(12, 48);
        numericUpDown2.Maximum = new decimal(new int[] { 10, 0, 0, 0 });
        numericUpDown2.Name = "numericUpDown2";
        numericUpDown2.Size = new Size(120, 27);
        numericUpDown2.TabIndex = 1;
        numericUpDown2.ThousandsSeparator = true;
        numericUpDown2.Value = new decimal(new int[] { 15, 0, 0, 65536 });
        //
        // label1
        //
        label1.AutoSize = true;
        label1.Location = new Point(150, 14);
        label1.Name = "label1";
        label1.Size = new Size(25, 20);
        label1.TabIndex = 2;
        label1.Text = "10";
        //
        // Form1
        //
        AutoScaleMode = AutoScaleMode.None;
        ClientSize = new Size(260, 90);
        Controls.Add(label1);
        Controls.Add(numericUpDown2);
        Controls.Add(numericUpDown1);
        Name = "Form1";
        Text = "Numbers";
        ((System.ComponentModel.ISupportInitialize)numericUpDown1).EndInit();
        ((System.ComponentModel.ISupportInitialize)numericUpDown2).EndInit();
        ResumeLayout(false);
        PerformLayout();
    }

    #endregion

    private NumericUpDown numericUpDown1;
    private NumericUpDown numericUpDown2;
    private Label label1;
}
