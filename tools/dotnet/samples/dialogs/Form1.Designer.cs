namespace DialogsApp;

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
        timer1 = new System.Windows.Forms.Timer(components);
        button1 = new Button();
        label1 = new Label();
        SuspendLayout();
        // 
        // timer1
        // 
        timer1.Interval = 50;
        timer1.Tick += timer1_Tick;
        // 
        // button1
        // 
        button1.Location = new Point(12, 12);
        button1.Name = "button1";
        button1.Size = new Size(120, 29);
        button1.TabIndex = 0;
        button1.Text = "Ask";
        button1.UseVisualStyleBackColor = true;
        button1.Click += button1_Click;
        // 
        // label1
        // 
        label1.AutoSize = true;
        label1.Location = new Point(12, 56);
        label1.Name = "label1";
        label1.Size = new Size(0, 20);
        label1.TabIndex = 1;
        // 
        // Form1
        // 
        AutoScaleDimensions = new SizeF(8F, 20F);
        AutoScaleMode = AutoScaleMode.Font;
        ClientSize = new Size(320, 100);
        Controls.Add(label1);
        Controls.Add(button1);
        Name = "Form1";
        Text = "Dialogs";
        ResumeLayout(false);
        PerformLayout();
    }

    #endregion

    private System.Windows.Forms.Timer timer1;
    private Button button1;
    private Label label1;
}
