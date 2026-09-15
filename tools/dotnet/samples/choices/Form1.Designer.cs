namespace ChoicesApp;

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
        groupBox1 = new GroupBox();
        radioButton3 = new RadioButton();
        radioButton2 = new RadioButton();
        radioButton1 = new RadioButton();
        progressBar1 = new ProgressBar();
        trackBar1 = new TrackBar();
        label1 = new Label();
        groupBox1.SuspendLayout();
        ((System.ComponentModel.ISupportInitialize)trackBar1).BeginInit();
        SuspendLayout();
        //
        // groupBox1
        //
        groupBox1.Controls.Add(radioButton3);
        groupBox1.Controls.Add(radioButton2);
        groupBox1.Controls.Add(radioButton1);
        groupBox1.Location = new Point(12, 12);
        groupBox1.Name = "groupBox1";
        groupBox1.Size = new Size(150, 120);
        groupBox1.TabIndex = 0;
        groupBox1.TabStop = false;
        groupBox1.Text = "Size";
        //
        // radioButton3
        //
        radioButton3.AutoSize = true;
        radioButton3.Location = new Point(12, 84);
        radioButton3.Name = "radioButton3";
        radioButton3.Size = new Size(63, 24);
        radioButton3.TabIndex = 2;
        radioButton3.Text = "Large";
        radioButton3.UseVisualStyleBackColor = true;
        radioButton3.CheckedChanged += radio_CheckedChanged;
        //
        // radioButton2
        //
        radioButton2.AutoSize = true;
        radioButton2.Location = new Point(12, 56);
        radioButton2.Name = "radioButton2";
        radioButton2.Size = new Size(83, 24);
        radioButton2.TabIndex = 1;
        radioButton2.Text = "Medium";
        radioButton2.UseVisualStyleBackColor = true;
        radioButton2.CheckedChanged += radio_CheckedChanged;
        //
        // radioButton1
        //
        radioButton1.AutoSize = true;
        radioButton1.Checked = true;
        radioButton1.Location = new Point(12, 28);
        radioButton1.Name = "radioButton1";
        radioButton1.Size = new Size(64, 24);
        radioButton1.TabIndex = 0;
        radioButton1.TabStop = true;
        radioButton1.Text = "Small";
        radioButton1.UseVisualStyleBackColor = true;
        radioButton1.CheckedChanged += radio_CheckedChanged;
        //
        // progressBar1
        //
        progressBar1.Location = new Point(180, 20);
        progressBar1.Name = "progressBar1";
        progressBar1.Size = new Size(180, 24);
        progressBar1.Step = 25;
        progressBar1.TabIndex = 1;
        progressBar1.Value = 30;
        //
        // trackBar1
        //
        trackBar1.Location = new Point(180, 60);
        trackBar1.Maximum = 10;
        trackBar1.Name = "trackBar1";
        trackBar1.Size = new Size(180, 56);
        trackBar1.TabIndex = 2;
        trackBar1.Value = 3;
        trackBar1.ValueChanged += trackBar1_ValueChanged;
        //
        // label1
        //
        label1.AutoSize = true;
        label1.Location = new Point(180, 120);
        label1.Name = "label1";
        label1.Size = new Size(42, 20);
        label1.TabIndex = 3;
        label1.Text = "Small";
        //
        // Form1
        //
        AutoScaleMode = AutoScaleMode.None;
        ClientSize = new Size(380, 150);
        Controls.Add(label1);
        Controls.Add(trackBar1);
        Controls.Add(progressBar1);
        Controls.Add(groupBox1);
        Name = "Form1";
        Text = "Choices";
        groupBox1.ResumeLayout(false);
        groupBox1.PerformLayout();
        ((System.ComponentModel.ISupportInitialize)trackBar1).EndInit();
        ResumeLayout(false);
        PerformLayout();
    }

    #endregion

    private GroupBox groupBox1;
    private RadioButton radioButton3;
    private RadioButton radioButton2;
    private RadioButton radioButton1;
    private ProgressBar progressBar1;
    private TrackBar trackBar1;
    private Label label1;
}
