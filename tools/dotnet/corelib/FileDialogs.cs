// Окна выбора файла и папки WinForms (20.11): OpenFileDialog, SaveFileDialog,
// FolderBrowserDialog и Microsoft.Win32.OpenFolderDialog (.NET 8).
//
// Своего окна здесь нет: его рисует системная программа /bin/filedlg — то же
// окно «Открыть» / «Сохранить как» / «Выбор папки», что у Блокнота, отдельным
// окном стола, модальным к форме. Среда передаёт ей запрос строками
// `ключ\tзначение` (договор user_progs::filedlg) и ждёт ответа, продолжая
// рисовать формы (Application.RunFileDialog).

using System.Collections.Generic;
using System.ComponentModel;
using System.Runtime.CompilerServices;
using System.Text;

namespace System.Windows.Forms
{
    internal static class FreeOsFileDialog
    {
        // Номер окна выбора или отрицательное — не открылось.
        [MethodImpl(MethodImplOptions.InternalCall)]
        internal static extern int Start(string request, int owner);

        // 0 — ещё открыто, 1 — выбран путь, 2 — отмена.
        [MethodImpl(MethodImplOptions.InternalCall)]
        internal static extern int Poll(int dialog, out string path);

        internal static string Clean(string text) =>
            (text ?? string.Empty).Replace('\n', ' ').Replace('\r', ' ').Replace('\t', ' ');

        // Строки запроса по свойствам окна.
        internal static string Request(string mode, string title, string directory, string name, string filter, int filterIndex, bool addExtension)
        {
            var text = new StringBuilder();
            text.Append("mode\t").Append(mode).Append('\n');
            if (!string.IsNullOrEmpty(title))
            {
                text.Append("title\t").Append(Clean(title)).Append('\n');
            }
            if (!string.IsNullOrEmpty(directory))
            {
                text.Append("dir\t").Append(Clean(directory)).Append('\n');
            }
            if (!string.IsNullOrEmpty(name))
            {
                text.Append("name\t").Append(Clean(name)).Append('\n');
            }
            // Фильтр WinForms: «Подпись|*.txt;*.log|Подпись|*.*».
            if (!string.IsNullOrEmpty(filter))
            {
                string[] parts = filter.Split('|');
                for (int i = 0; i + 1 < parts.Length; i += 2)
                {
                    string label = parts[i];
                    int open = label.IndexOf(" (");
                    if (open > 0)
                    {
                        label = label.Substring(0, open);
                    }
                    var exts = new StringBuilder();
                    foreach (string mask in parts[i + 1].Split(';'))
                    {
                        string ext = mask.Trim();
                        if (ext.StartsWith("*."))
                        {
                            ext = ext.Substring(2);
                        }
                        if (ext.Length == 0 || ext == "*" || ext == "*.*")
                        {
                            continue;
                        }
                        if (exts.Length > 0)
                        {
                            exts.Append(';');
                        }
                        exts.Append(Clean(ext));
                    }
                    text.Append("filter\t").Append(Clean(label)).Append('\t').Append(exts.ToString()).Append('\n');
                }
            }
            // FilterIndex у WinForms — с единицы.
            text.Append("index\t").Append((filterIndex > 0 ? filterIndex - 1 : 0).ToString()).Append('\n');
            text.Append("extension\t").Append(addExtension ? "1" : "0").Append('\n');
            return text.ToString();
        }

        internal static Form OwnerOf(IWin32Window owner) => owner as Form ?? Application.DialogOwner();
    }

    public abstract class CommonDialog : Component
    {
        public object Tag { get; set; }

        public event EventHandler HelpRequest;

        public DialogResult ShowDialog() => ShowDialog(null);

        public DialogResult ShowDialog(IWin32Window owner) =>
            RunDialog(FreeOsFileDialog.OwnerOf(owner)) ? DialogResult.OK : DialogResult.Cancel;

        public abstract void Reset();

        protected abstract bool RunDialog(IntPtr hwndOwner);

        internal virtual bool RunDialog(Form owner) => RunDialog(default(IntPtr));

        protected virtual void OnHelpRequest(EventArgs e) => HelpRequest?.Invoke(this, e);
    }

    public abstract class FileDialog : CommonDialog
    {
        private string fileName = string.Empty;

        public bool AddExtension { get; set; } = true;

        public bool CheckFileExists { get; set; }

        public bool CheckPathExists { get; set; } = true;

        public string DefaultExt { get; set; } = string.Empty;

        public bool DereferenceLinks { get; set; } = true;

        public string FileName
        {
            get => fileName;
            set => fileName = value ?? string.Empty;
        }

        public string[] FileNames => fileName.Length == 0 ? new string[0] : new[] { fileName };

        public string Filter { get; set; } = string.Empty;

        public int FilterIndex { get; set; } = 1;

        public string InitialDirectory { get; set; } = string.Empty;

        public bool RestoreDirectory { get; set; }

        public bool ShowHelp { get; set; }

        public bool SupportMultiDottedExtensions { get; set; }

        public string Title { get; set; } = string.Empty;

        public bool ValidateNames { get; set; } = true;

        public event CancelEventHandler FileOk;

        internal abstract string Mode { get; }

        public override void Reset()
        {
            AddExtension = true;
            DefaultExt = string.Empty;
            fileName = string.Empty;
            Filter = string.Empty;
            FilterIndex = 1;
            InitialDirectory = string.Empty;
            Title = string.Empty;
        }

        protected override bool RunDialog(IntPtr hwndOwner) => RunDialog((Form)null);

        internal override bool RunDialog(Form owner)
        {
            string directory = InitialDirectory;
            string name = fileName;
            // Полный путь в FileName — папка и имя, как у Windows.
            int slash = name.LastIndexOf('/');
            if (slash >= 0)
            {
                if (string.IsNullOrEmpty(directory))
                {
                    directory = slash == 0 ? "/" : name.Substring(0, slash);
                }
                name = name.Substring(slash + 1);
            }
            string filter = Filter;
            // DefaultExt без фильтра — расширение всё равно дописывается.
            if (string.IsNullOrEmpty(filter) && !string.IsNullOrEmpty(DefaultExt) && Mode == "save")
            {
                string ext = DefaultExt.StartsWith(".") ? DefaultExt.Substring(1) : DefaultExt;
                filter = ext + "|*." + ext + "|*.*|*.*";
            }
            string request = FreeOsFileDialog.Request(Mode, Title, directory, name, filter, FilterIndex, AddExtension);
            string path = Application.RunFileDialog(request, owner);
            if (path == null)
            {
                return false;
            }
            string previous = fileName;
            fileName = path;
            var ok = new CancelEventArgs(false);
            FileOk?.Invoke(this, ok);
            if (ok.Cancel)
            {
                fileName = previous;
                return false;
            }
            return true;
        }

        public override string ToString() => base.ToString() + ": Title: " + Title + ", FileName: " + fileName;
    }

    public sealed class OpenFileDialog : FileDialog
    {
        public OpenFileDialog()
        {
            CheckFileExists = true;
        }

        public bool Multiselect { get; set; }

        public bool ReadOnlyChecked { get; set; }

        public bool ShowReadOnly { get; set; }

        public string SafeFileName
        {
            get
            {
                int slash = FileName.LastIndexOf('/');
                return slash >= 0 ? FileName.Substring(slash + 1) : FileName;
            }
        }

        public string[] SafeFileNames => FileName.Length == 0 ? new string[0] : new[] { SafeFileName };

        // OpenFile() — когда у среды появится System.IO.Stream.
        internal override string Mode => "open";
    }

    public sealed class SaveFileDialog : FileDialog
    {
        // Перезапись окно подтверждает само, как и создание нового файла не
        // спрашивает: так ведёт себя «Сохранить как» FossAi.
        public bool CreatePrompt { get; set; }

        public bool OverwritePrompt { get; set; } = true;

        internal override string Mode => "save";
    }

    public sealed class FolderBrowserDialog : CommonDialog
    {
        private string selectedPath = string.Empty;

        public string Description { get; set; } = string.Empty;

        public string InitialDirectory { get; set; } = string.Empty;

        public string SelectedPath
        {
            get => selectedPath;
            set => selectedPath = value ?? string.Empty;
        }

        public bool ShowNewFolderButton { get; set; } = true;

        public bool UseDescriptionForTitle { get; set; }

        public bool Multiselect { get; set; }

        public string[] SelectedPaths => selectedPath.Length == 0 ? new string[0] : new[] { selectedPath };

        public override void Reset()
        {
            Description = string.Empty;
            InitialDirectory = string.Empty;
            selectedPath = string.Empty;
            ShowNewFolderButton = true;
            UseDescriptionForTitle = false;
        }

        protected override bool RunDialog(IntPtr hwndOwner) => RunDialog((Form)null);

        internal override bool RunDialog(Form owner)
        {
            string title = UseDescriptionForTitle && !string.IsNullOrEmpty(Description) ? Description : string.Empty;
            string directory = !string.IsNullOrEmpty(selectedPath) ? selectedPath : InitialDirectory;
            string request = FreeOsFileDialog.Request("folder", title, directory, string.Empty, string.Empty, 0, false);
            string path = Application.RunFileDialog(request, owner);
            if (path == null)
            {
                return false;
            }
            selectedPath = path;
            return true;
        }
    }
}

namespace Microsoft.Win32
{
    using System;
    using System.Windows.Forms;

    // Окна выбора файла WPF (Microsoft.Win32): то же системное окно.
    public abstract class CommonDialog
    {
        public object Tag { get; set; }

        public bool? ShowDialog() => ShowDialog(null);

        public bool? ShowDialog(object owner) => RunDialog(Application.DialogOwner());

        internal abstract bool RunDialog(Form owner);

        public virtual void Reset()
        {
        }
    }

    public abstract class FileDialog : CommonDialog
    {
        public bool AddExtension { get; set; } = true;

        public string DefaultExt { get; set; } = string.Empty;

        public string FileName { get; set; } = string.Empty;

        public string[] FileNames => string.IsNullOrEmpty(FileName) ? new string[0] : new[] { FileName };

        public string SafeFileName
        {
            get
            {
                int slash = FileName.LastIndexOf('/');
                return slash >= 0 ? FileName.Substring(slash + 1) : FileName;
            }
        }

        public string Filter { get; set; } = string.Empty;

        public int FilterIndex { get; set; } = 1;

        public string InitialDirectory { get; set; } = string.Empty;

        public string Title { get; set; } = string.Empty;

        internal abstract string Mode { get; }

        internal override bool RunDialog(Form owner)
        {
            string request = FreeOsFileDialog.Request(Mode, Title, InitialDirectory, FileName, Filter, FilterIndex, AddExtension);
            string path = Application.RunFileDialog(request, owner);
            if (path == null)
            {
                return false;
            }
            FileName = path;
            return true;
        }
    }

    public sealed class OpenFileDialog : FileDialog
    {
        public bool Multiselect { get; set; }

        internal override string Mode => "open";
    }

    public sealed class SaveFileDialog : FileDialog
    {
        public bool OverwritePrompt { get; set; } = true;

        internal override string Mode => "save";
    }

    // Выбор папки (.NET 8, Microsoft.Win32.OpenFolderDialog).
    public sealed class OpenFolderDialog : CommonDialog
    {
        public string FolderName { get; set; } = string.Empty;

        public string[] FolderNames => string.IsNullOrEmpty(FolderName) ? new string[0] : new[] { FolderName };

        public string SafeFolderName
        {
            get
            {
                string name = FolderName.Length > 1 && FolderName.EndsWith("/") ? FolderName.Substring(0, FolderName.Length - 1) : FolderName;
                int slash = name.LastIndexOf('/');
                return slash >= 0 ? name.Substring(slash + 1) : name;
            }
        }

        public string InitialDirectory { get; set; } = string.Empty;

        public string Title { get; set; } = string.Empty;

        public bool Multiselect { get; set; }

        internal override bool RunDialog(Form owner)
        {
            string directory = !string.IsNullOrEmpty(FolderName) ? FolderName : InitialDirectory;
            string request = FreeOsFileDialog.Request("folder", Title, directory, string.Empty, string.Empty, 0, false);
            string path = Application.RunFileDialog(request, owner);
            if (path == null)
            {
                return false;
            }
            FolderName = path;
            return true;
        }
    }
}
