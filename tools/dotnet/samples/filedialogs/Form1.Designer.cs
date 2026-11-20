namespace FileDialogsApp;

partial class Form1
{
    private System.ComponentModel.IContainer components = null;

    protected override void Dispose(bool disposing)
    {
        if (disposing && (components != null))
        {
            components.Dispose();
        }
        base.Dispose(disposing);
    }

    #region Windows Form Designer generated code

    private void InitializeComponent()
    {
        openButton = new Button();
        saveButton = new Button();
        folderButton = new Button();
        label1 = new Label();
        SuspendLayout();
        //
        // openButton
        //
        openButton.Location = new Point(12, 12);
        openButton.Name = "openButton";
        openButton.Size = new Size(110, 29);
        openButton.TabIndex = 0;
        openButton.Text = "Open...";
        openButton.UseVisualStyleBackColor = true;
        openButton.Click += openButton_Click;
        //
        // saveButton
        //
        saveButton.Location = new Point(132, 12);
        saveButton.Name = "saveButton";
        saveButton.Size = new Size(110, 29);
        saveButton.TabIndex = 1;
        saveButton.Text = "Save...";
        saveButton.UseVisualStyleBackColor = true;
        saveButton.Click += saveButton_Click;
        //
        // folderButton
        //
        folderButton.Location = new Point(252, 12);
        folderButton.Name = "folderButton";
        folderButton.Size = new Size(110, 29);
        folderButton.TabIndex = 2;
        folderButton.Text = "Folder...";
        folderButton.UseVisualStyleBackColor = true;
        folderButton.Click += folderButton_Click;
        //
        // label1
        //
        label1.AutoSize = true;
        label1.Location = new Point(12, 56);
        label1.Name = "label1";
        label1.Size = new Size(0, 20);
        label1.TabIndex = 3;
        //
        // Form1
        //
        AutoScaleDimensions = new SizeF(8F, 20F);
        AutoScaleMode = AutoScaleMode.Font;
        ClientSize = new Size(480, 110);
        Controls.Add(label1);
        Controls.Add(folderButton);
        Controls.Add(saveButton);
        Controls.Add(openButton);
        Name = "Form1";
        Text = "File Dialogs";
        ResumeLayout(false);
        PerformLayout();
    }

    #endregion

    private Button openButton;
    private Button saveButton;
    private Button folderButton;
    private Label label1;
}
