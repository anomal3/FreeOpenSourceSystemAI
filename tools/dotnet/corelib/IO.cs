// Файлы и каталоги (фаза N5a): System.IO поверх файловых членов хоста.
//
// Правила путей — как у .NET на Unix: разделитель `/`, путь от `/` полный,
// остальные считаются от текущего каталога. Хост получает только полные и
// разобранные пути; относительность, `.` и `..`, шаблоны `*.txt`, обход
// вложенных каталогов, копирование и тексты исключений — здесь.
//
// Файл читается и пишется целиком: `StreamWriter` копит текст и отдаёт его
// хосту на `Flush`, `StreamReader` читает файл при открытии. Для текстовых
// программ это не видно; поток байтов с позиционированием (`FileStream`) — не
// в этой фазе.

using System.Collections;
using System.Collections.Generic;
using System.Runtime.CompilerServices;
using System.Text;

namespace System
{
    public class UnauthorizedAccessException : SystemException
    {
        public UnauthorizedAccessException()
            : base("Attempted to perform an unauthorized operation.")
        {
        }

        public UnauthorizedAccessException(string message)
            : base(message)
        {
        }
    }

    public class ObjectDisposedException : InvalidOperationException
    {
        public ObjectDisposedException(string objectName)
            : this(objectName, "Cannot access a disposed object.")
        {
        }

        public ObjectDisposedException(string objectName, string message)
            : base(message)
        {
            ObjectName = objectName;
        }

        public string ObjectName { get; }
    }
}

namespace System.IO
{
    public class IOException : SystemException
    {
        public IOException()
            : base("I/O error occurred.")
        {
        }

        public IOException(string message)
            : base(message)
        {
        }

        public IOException(string message, Exception innerException)
            : base(message, innerException)
        {
        }
    }

    public class FileNotFoundException : IOException
    {
        public FileNotFoundException()
            : base("Unable to find the specified file.")
        {
        }

        public FileNotFoundException(string message)
            : base(message)
        {
        }

        public FileNotFoundException(string message, string fileName)
            : base(message)
        {
            FileName = fileName;
        }

        public string FileName { get; }
    }

    public class DirectoryNotFoundException : IOException
    {
        public DirectoryNotFoundException()
            : base("Attempted to access a path that is not on the disk.")
        {
        }

        public DirectoryNotFoundException(string message)
            : base(message)
        {
        }
    }

    public enum SearchOption
    {
        TopDirectoryOnly = 0,
        AllDirectories = 1,
    }

    /// Файловые члены хоста. Коды отказов — `clr_vm::IoError::code`.
    internal static class FileSystem
    {
        internal const int NotFound = 1;
        internal const int Exists = 2;
        internal const int NotEmpty = 3;
        internal const int Denied = 4;
        internal const int NoSpace = 5;
        internal const int WrongKind = 6;
        internal const int Unsupported = 7;

        internal const int KindFile = 1;
        internal const int KindDirectory = 2;

        private static string currentDirectory;

        internal static string CurrentDirectory
        {
            get => currentDirectory ??= GetCurrentDirectoryNative();
            set => currentDirectory = value;
        }

        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern string GetCurrentDirectoryNative();

        [MethodImpl(MethodImplOptions.InternalCall)]
        internal static extern int ReadFile(string path, out byte[] data);

        [MethodImpl(MethodImplOptions.InternalCall)]
        internal static extern int WriteFile(string path, byte[] data, bool append);

        [MethodImpl(MethodImplOptions.InternalCall)]
        internal static extern int RemoveFile(string path);

        [MethodImpl(MethodImplOptions.InternalCall)]
        internal static extern int CreateDirectory(string path);

        [MethodImpl(MethodImplOptions.InternalCall)]
        internal static extern int RemoveDirectory(string path);

        [MethodImpl(MethodImplOptions.InternalCall)]
        internal static extern int Rename(string from, string to);

        /// `KindFile`, `KindDirectory` или минус код отказа.
        [MethodImpl(MethodImplOptions.InternalCall)]
        internal static extern int Stat(string path, out long size);

        [MethodImpl(MethodImplOptions.InternalCall)]
        internal static extern int ListDirectory(string path, out string[] names);

        internal static int Kind(string fullPath) => Stat(fullPath, out long _);

        internal static bool IsDirectory(string fullPath) => Kind(fullPath) == KindDirectory;

        internal static bool IsFile(string fullPath) => Kind(fullPath) == KindFile;

        /// Исключение с текстом .NET на Unix. Ненайденное — «файл», если его
        /// каталог есть, и «часть пути», если нет.
        internal static Exception Error(int status, string fullPath, bool directory)
        {
            switch (status)
            {
                case NotFound:
                    if (directory || !IsDirectory(Path.GetDirectoryName(fullPath) ?? "/"))
                    {
                        return new DirectoryNotFoundException("Could not find a part of the path '" + fullPath + "'.");
                    }
                    return new FileNotFoundException("Could not find file '" + fullPath + "'.", fullPath);
                case Exists:
                    return new IOException("The file '" + fullPath + "' already exists.");
                case NotEmpty:
                    return new IOException("Directory not empty : '" + fullPath + "'");
                case Denied:
                case WrongKind:
                    return new UnauthorizedAccessException("Access to the path '" + fullPath + "' is denied.");
                case NoSpace:
                    return new IOException("No space left on device : '" + fullPath + "'");
                case Unsupported:
                    return new NotSupportedException("Files are not available to this runtime.");
                default:
                    return new IOException("I/O error : '" + fullPath + "'");
            }
        }

        internal static void Check(int status, string fullPath, bool directory)
        {
            if (status != 0)
            {
                throw Error(status, fullPath, directory);
            }
        }

        internal static string Full(string path, string name)
        {
            if (path == null)
            {
                throw new ArgumentNullException(name);
            }
            if (path.Length == 0)
            {
                throw new ArgumentException("The value cannot be an empty string.", name);
            }
            return Path.GetFullPath(path);
        }

        /// Текст файла: UTF-8, метка порядка байт в начале пропускается.
        internal static string Decode(byte[] data)
        {
            int start = data.Length >= 3 && data[0] == 0xEF && data[1] == 0xBB && data[2] == 0xBF ? 3 : 0;
            return Encoding.UTF8.GetString(data, start, data.Length - start);
        }

        /// Строки, как их делит `StreamReader.ReadLine`: `\n`, `\r` или `\r\n`.
        internal static List<string> SplitLines(string text)
        {
            var lines = new List<string>();
            int start = 0;
            int i = 0;
            while (i < text.Length)
            {
                char c = text[i];
                if (c == '\n' || c == '\r')
                {
                    lines.Add(text.Substring(start, i - start));
                    i++;
                    if (c == '\r' && i < text.Length && text[i] == '\n')
                    {
                        i++;
                    }
                    start = i;
                }
                else
                {
                    i++;
                }
            }
            if (start < text.Length)
            {
                lines.Add(text.Substring(start));
            }
            return lines;
        }

        /// Шаблон имени `*` и `?`, с учётом регистра, как на Unix.
        internal static bool Matches(string name, string pattern)
        {
            if (pattern == "*" || pattern == "*.*" && name.IndexOf('.') >= 0)
            {
                return true;
            }
            int n = 0;
            int p = 0;
            int starP = -1;
            int starN = 0;
            while (n < name.Length)
            {
                if (p < pattern.Length && (pattern[p] == '?' || pattern[p] == name[n]))
                {
                    n++;
                    p++;
                }
                else if (p < pattern.Length && pattern[p] == '*')
                {
                    starP = p++;
                    starN = n;
                }
                else if (starP >= 0)
                {
                    p = starP + 1;
                    n = ++starN;
                }
                else
                {
                    return false;
                }
            }
            while (p < pattern.Length && pattern[p] == '*')
            {
                p++;
            }
            return p == pattern.Length;
        }

        /// Содержимое каталога: пути в том виде, в каком каталог назвали, плюс имя.
        internal static List<string> Enumerate(string path, string pattern, SearchOption option, bool files, bool directories)
        {
            string full = Full(path, "path");
            if (pattern == null)
            {
                throw new ArgumentNullException("searchPattern");
            }
            var result = new List<string>();
            var pending = new Queue<string>();
            var pendingFull = new Queue<string>();
            pending.Enqueue(path);
            pendingFull.Enqueue(full);
            bool top = true;
            while (pending.Count > 0)
            {
                string shown = pending.Dequeue();
                string fullDirectory = pendingFull.Dequeue();
                int status = ListDirectory(fullDirectory, out string[] names);
                if (status != 0)
                {
                    if (top)
                    {
                        throw Error(status == WrongKind ? NotFound : status, fullDirectory, true);
                    }
                    continue;
                }
                top = false;
                foreach (string name in names)
                {
                    string child = Path.Combine(shown, name);
                    string childFull = Path.Combine(fullDirectory, name);
                    bool isDirectory = IsDirectory(childFull);
                    if ((isDirectory ? directories : files) && Matches(name, pattern))
                    {
                        result.Add(child);
                    }
                    if (isDirectory && option == SearchOption.AllDirectories)
                    {
                        pending.Enqueue(child);
                        pendingFull.Enqueue(childFull);
                    }
                }
            }
            return result;
        }
    }

    // Строка перебирается циклом по индексу, а не `foreach`: на `foreach` по
    // строке компилятор с этой библиотекой падает с CS7038 «не удалось выдать
    // модуль» без единого слова о причине (найдено делением файла пополам).
    public static class Path
    {
        public static readonly char DirectorySeparatorChar = '/';
        public static readonly char AltDirectorySeparatorChar = '/';
        public static readonly char VolumeSeparatorChar = '/';
        public static readonly char PathSeparator = ':';

        public static string Combine(string path1, string path2)
        {
            if (path1 == null || path2 == null)
            {
                throw new ArgumentNullException(path1 == null ? "path1" : "path2");
            }
            if (path2.Length == 0)
            {
                return path1;
            }
            if (path1.Length == 0 || IsPathRooted(path2))
            {
                return path2;
            }
            return path1[path1.Length - 1] == '/' ? path1 + path2 : string.Concat(path1, "/", path2);
        }

        public static string Combine(string path1, string path2, string path3) => Combine(Combine(path1, path2), path3);

        public static string Combine(string path1, string path2, string path3, string path4) => Combine(Combine(Combine(path1, path2), path3), path4);

        public static string Combine(params string[] paths)
        {
            if (paths == null)
            {
                throw new ArgumentNullException("paths");
            }
            string result = string.Empty;
            foreach (string path in paths)
            {
                result = Combine(result, path);
            }
            return result;
        }

        public static string Join(string path1, string path2)
        {
            if (string.IsNullOrEmpty(path1))
            {
                return path2 ?? string.Empty;
            }
            if (string.IsNullOrEmpty(path2))
            {
                return path1;
            }
            return path1[path1.Length - 1] == '/' || path2[0] == '/' ? path1 + path2 : string.Concat(path1, "/", path2);
        }

        public static bool IsPathRooted(string path) => path != null && path.Length > 0 && path[0] == '/';

        public static bool EndsInDirectorySeparator(string path) => path != null && path.Length > 0 && path[path.Length - 1] == '/';

        public static string GetPathRoot(string path) => path == null ? null : IsPathRooted(path) ? "/" : string.Empty;

        public static string GetFileName(string path)
        {
            if (path == null)
            {
                return null;
            }
            int separator = path.LastIndexOf('/');
            return separator < 0 ? path : path.Substring(separator + 1);
        }

        public static string GetFileNameWithoutExtension(string path)
        {
            string name = GetFileName(path);
            if (name == null)
            {
                return null;
            }
            int dot = name.LastIndexOf('.');
            return dot < 0 ? name : name.Substring(0, dot);
        }

        /// Расширение с точкой; точка последним символом — пустое.
        public static string GetExtension(string path)
        {
            if (path == null)
            {
                return null;
            }
            for (int i = path.Length - 1; i >= 0; i--)
            {
                char c = path[i];
                if (c == '.')
                {
                    return i == path.Length - 1 ? string.Empty : path.Substring(i);
                }
                if (c == '/')
                {
                    break;
                }
            }
            return string.Empty;
        }

        public static bool HasExtension(string path) => !string.IsNullOrEmpty(GetExtension(path));

        public static string ChangeExtension(string path, string extension)
        {
            if (path == null)
            {
                return null;
            }
            string subpath = path;
            for (int i = path.Length - 1; i >= 0; i--)
            {
                char c = path[i];
                if (c == '.')
                {
                    subpath = path.Substring(0, i);
                    break;
                }
                if (c == '/')
                {
                    break;
                }
            }
            if (extension == null)
            {
                return subpath;
            }
            if (extension.Length == 0 || extension[0] != '.')
            {
                subpath = string.Concat(subpath, ".");
            }
            return subpath + extension;
        }

        public static string GetDirectoryName(string path)
        {
            if (string.IsNullOrEmpty(path))
            {
                return null;
            }
            int root = path[0] == '/' ? 1 : 0;
            int end = path.Length;
            if (end <= root)
            {
                return null;
            }
            while (end > root && path[--end] != '/')
            {
            }
            while (end > root && path[end - 1] == '/')
            {
                end--;
            }
            return Collapse(path.Substring(0, end));
        }

        public static string GetFullPath(string path)
        {
            if (path == null)
            {
                throw new ArgumentNullException("path");
            }
            if (path.Length == 0)
            {
                throw new ArgumentException("The value cannot be an empty string.", "path");
            }
            string combined = IsPathRooted(path) ? path : Combine(FileSystem.CurrentDirectory, path);
            var parts = new List<string>();
            foreach (string part in combined.Split('/'))
            {
                if (part.Length == 0 || part == ".")
                {
                    continue;
                }
                if (part == "..")
                {
                    if (parts.Count > 0)
                    {
                        parts.RemoveAt(parts.Count - 1);
                    }
                    continue;
                }
                parts.Add(part);
            }
            string full = string.Concat("/", string.Join("/", parts));
            return EndsInDirectorySeparator(combined) && full.Length > 1 ? string.Concat(full, "/") : full;
        }

        public static string GetTempPath() => "/tmp/";

        private static string Collapse(string path)
        {
            if (path.IndexOf("//") < 0)
            {
                return path;
            }
            var builder = new StringBuilder();
            for (int i = 0; i < path.Length; i++)
            {
                char c = path[i];
                if (c != '/' || builder.Length == 0 || builder[builder.Length - 1] != '/')
                {
                    builder.Append(c);
                }
            }
            return builder.ToString();
        }
    }

    public static class File
    {
        public static bool Exists(string path) =>
            !string.IsNullOrEmpty(path) && !Path.EndsInDirectorySeparator(path) && FileSystem.IsFile(Path.GetFullPath(path));

        public static byte[] ReadAllBytes(string path)
        {
            string full = FileSystem.Full(path, "path");
            int status = FileSystem.ReadFile(full, out byte[] data);
            FileSystem.Check(status, full, false);
            return data;
        }

        public static string ReadAllText(string path) => FileSystem.Decode(ReadAllBytes(path));

        public static string ReadAllText(string path, Encoding encoding) => ReadAllText(path);

        public static string[] ReadAllLines(string path) => FileSystem.SplitLines(ReadAllText(path)).ToArray();

        public static string[] ReadAllLines(string path, Encoding encoding) => ReadAllLines(path);

        public static IEnumerable<string> ReadLines(string path) => FileSystem.SplitLines(ReadAllText(path));

        public static void WriteAllBytes(string path, byte[] bytes)
        {
            if (bytes == null)
            {
                throw new ArgumentNullException("bytes");
            }
            Write(path, bytes, false);
        }

        public static void WriteAllText(string path, string contents) => Write(path, Encoding.UTF8.GetBytes(contents ?? string.Empty), false);

        public static void WriteAllText(string path, string contents, Encoding encoding) => WriteAllText(path, contents);

        public static void AppendAllText(string path, string contents) => Write(path, Encoding.UTF8.GetBytes(contents ?? string.Empty), true);

        public static void WriteAllLines(string path, IEnumerable<string> contents) => Write(path, Encoding.UTF8.GetBytes(Lines(contents)), false);

        public static void WriteAllLines(string path, string[] contents) => WriteAllLines(path, (IEnumerable<string>)contents);

        public static void AppendAllLines(string path, IEnumerable<string> contents) => Write(path, Encoding.UTF8.GetBytes(Lines(contents)), true);

        private static string Lines(IEnumerable<string> contents)
        {
            if (contents == null)
            {
                throw new ArgumentNullException("contents");
            }
            var builder = new StringBuilder();
            foreach (string line in contents)
            {
                builder.Append(line).Append(Environment.NewLine);
            }
            return builder.ToString();
        }

        internal static void Write(string path, byte[] bytes, bool append)
        {
            string full = FileSystem.Full(path, "path");
            FileSystem.Check(FileSystem.WriteFile(full, bytes, append), full, false);
        }

        public static void Delete(string path)
        {
            string full = FileSystem.Full(path, "path");
            int status = FileSystem.RemoveFile(full);
            // Удалить то, чего нет, — не ошибка; ошибка — нет каталога.
            if (status == FileSystem.NotFound && FileSystem.IsDirectory(Path.GetDirectoryName(full) ?? "/"))
            {
                return;
            }
            FileSystem.Check(status, full, false);
        }

        public static void Copy(string sourceFileName, string destFileName) => Copy(sourceFileName, destFileName, false);

        public static void Copy(string sourceFileName, string destFileName, bool overwrite)
        {
            string source = FileSystem.Full(sourceFileName, "sourceFileName");
            string destination = FileSystem.Full(destFileName, "destFileName");
            int status = FileSystem.ReadFile(source, out byte[] data);
            FileSystem.Check(status, source, false);
            int kind = FileSystem.Kind(destination);
            if (kind == FileSystem.KindDirectory || (kind == FileSystem.KindFile && !overwrite))
            {
                throw FileSystem.Error(FileSystem.Exists, destination, false);
            }
            FileSystem.Check(FileSystem.WriteFile(destination, data, false), destination, false);
        }

        public static void Move(string sourceFileName, string destFileName) => Move(sourceFileName, destFileName, false);

        public static void Move(string sourceFileName, string destFileName, bool overwrite)
        {
            string source = FileSystem.Full(sourceFileName, "sourceFileName");
            string destination = FileSystem.Full(destFileName, "destFileName");
            if (!FileSystem.IsFile(source))
            {
                throw new FileNotFoundException("Could not find file '" + source + "'.", source);
            }
            int kind = FileSystem.Kind(destination);
            if (kind == FileSystem.KindDirectory || (kind == FileSystem.KindFile && !overwrite))
            {
                throw FileSystem.Error(FileSystem.Exists, destination, false);
            }
            if (kind == FileSystem.KindFile)
            {
                FileSystem.Check(FileSystem.RemoveFile(destination), destination, false);
            }
            FileSystem.Check(FileSystem.Rename(source, destination), destination, false);
        }

        public static StreamReader OpenText(string path) => new StreamReader(path);

        public static StreamWriter CreateText(string path) => new StreamWriter(path);

        public static StreamWriter AppendText(string path) => new StreamWriter(path, true);
    }

    public static class Directory
    {
        public static bool Exists(string path) => !string.IsNullOrEmpty(path) && FileSystem.IsDirectory(Path.GetFullPath(path));

        public static DirectoryInfo CreateDirectory(string path)
        {
            string full = FileSystem.Full(path, "path");
            // Сначала недостающие предки — от корня вниз.
            var missing = new Stack<string>();
            string current = full;
            while (current != null && current != "/")
            {
                int kind = FileSystem.Kind(current);
                if (kind == FileSystem.KindDirectory)
                {
                    break;
                }
                if (kind == FileSystem.KindFile)
                {
                    throw FileSystem.Error(FileSystem.Exists, current, false);
                }
                missing.Push(current);
                current = Path.GetDirectoryName(current);
            }
            while (missing.Count > 0)
            {
                string next = missing.Pop();
                int status = FileSystem.CreateDirectory(next);
                if (status != 0 && status != FileSystem.Exists)
                {
                    throw FileSystem.Error(status, next, true);
                }
            }
            return new DirectoryInfo(path);
        }

        public static void Delete(string path) => Delete(path, false);

        public static void Delete(string path, bool recursive)
        {
            string full = FileSystem.Full(path, "path");
            if (!FileSystem.IsDirectory(full))
            {
                throw FileSystem.Error(FileSystem.NotFound, full, true);
            }
            if (recursive)
            {
                DeleteTree(full);
                return;
            }
            FileSystem.Check(FileSystem.RemoveDirectory(full), full, true);
        }

        private static void DeleteTree(string full)
        {
            FileSystem.Check(FileSystem.ListDirectory(full, out string[] names), full, true);
            foreach (string name in names)
            {
                string child = Path.Combine(full, name);
                if (FileSystem.IsDirectory(child))
                {
                    DeleteTree(child);
                }
                else
                {
                    FileSystem.Check(FileSystem.RemoveFile(child), child, false);
                }
            }
            FileSystem.Check(FileSystem.RemoveDirectory(full), full, true);
        }

        public static void Move(string sourceDirName, string destDirName)
        {
            string source = FileSystem.Full(sourceDirName, "sourceDirName");
            string destination = FileSystem.Full(destDirName, "destDirName");
            if (!FileSystem.IsDirectory(source))
            {
                throw FileSystem.Error(FileSystem.NotFound, source, true);
            }
            if (FileSystem.Kind(destination) > 0)
            {
                throw new IOException("Cannot create '" + destination + "' because a file or directory with the same name already exists.");
            }
            FileSystem.Check(FileSystem.Rename(source, destination), destination, true);
        }

        public static string GetCurrentDirectory() => FileSystem.CurrentDirectory;

        public static void SetCurrentDirectory(string path)
        {
            string full = FileSystem.Full(path, "path");
            if (!FileSystem.IsDirectory(full))
            {
                throw FileSystem.Error(FileSystem.NotFound, full, true);
            }
            FileSystem.CurrentDirectory = full;
        }

        public static string[] GetFiles(string path) => GetFiles(path, "*", SearchOption.TopDirectoryOnly);

        public static string[] GetFiles(string path, string searchPattern) => GetFiles(path, searchPattern, SearchOption.TopDirectoryOnly);

        public static string[] GetFiles(string path, string searchPattern, SearchOption searchOption) =>
            FileSystem.Enumerate(path, searchPattern, searchOption, true, false).ToArray();

        public static string[] GetDirectories(string path) => GetDirectories(path, "*", SearchOption.TopDirectoryOnly);

        public static string[] GetDirectories(string path, string searchPattern) => GetDirectories(path, searchPattern, SearchOption.TopDirectoryOnly);

        public static string[] GetDirectories(string path, string searchPattern, SearchOption searchOption) =>
            FileSystem.Enumerate(path, searchPattern, searchOption, false, true).ToArray();

        public static string[] GetFileSystemEntries(string path) => FileSystem.Enumerate(path, "*", SearchOption.TopDirectoryOnly, true, true).ToArray();

        public static IEnumerable<string> EnumerateFiles(string path) => FileSystem.Enumerate(path, "*", SearchOption.TopDirectoryOnly, true, false);

        public static IEnumerable<string> EnumerateFiles(string path, string searchPattern) =>
            FileSystem.Enumerate(path, searchPattern, SearchOption.TopDirectoryOnly, true, false);

        public static IEnumerable<string> EnumerateFiles(string path, string searchPattern, SearchOption searchOption) =>
            FileSystem.Enumerate(path, searchPattern, searchOption, true, false);

        public static IEnumerable<string> EnumerateDirectories(string path) => FileSystem.Enumerate(path, "*", SearchOption.TopDirectoryOnly, false, true);

        public static IEnumerable<string> EnumerateFileSystemEntries(string path) => FileSystem.Enumerate(path, "*", SearchOption.TopDirectoryOnly, true, true);
    }

    public abstract class FileSystemInfo
    {
        protected string FullPath;
        protected string OriginalPath;

        public string FullName => FullPath;

        public virtual string Name => Path.GetFileName(FullPath);

        public string Extension => Path.GetExtension(FullPath);

        public abstract bool Exists { get; }

        public abstract void Delete();

        public override string ToString() => OriginalPath;
    }

    public sealed class FileInfo : FileSystemInfo
    {
        public FileInfo(string fileName)
        {
            OriginalPath = fileName;
            FullPath = FileSystem.Full(fileName, "fileName");
        }

        public override bool Exists => FileSystem.IsFile(FullPath);

        public long Length
        {
            get
            {
                if (FileSystem.Stat(FullPath, out long size) != FileSystem.KindFile)
                {
                    throw new FileNotFoundException("Could not find file '" + FullPath + "'.", FullPath);
                }
                return size;
            }
        }

        public string DirectoryName => Path.GetDirectoryName(FullPath);

        public DirectoryInfo Directory => DirectoryName == null ? null : new DirectoryInfo(DirectoryName);

        public override void Delete() => File.Delete(FullPath);

        public FileInfo CopyTo(string destFileName) => CopyTo(destFileName, false);

        public FileInfo CopyTo(string destFileName, bool overwrite)
        {
            File.Copy(FullPath, destFileName, overwrite);
            return new FileInfo(destFileName);
        }

        public void MoveTo(string destFileName)
        {
            File.Move(FullPath, destFileName);
            OriginalPath = destFileName;
            FullPath = Path.GetFullPath(destFileName);
        }

        public StreamReader OpenText() => new StreamReader(FullPath);

        public StreamWriter CreateText() => new StreamWriter(FullPath);

        public StreamWriter AppendText() => new StreamWriter(FullPath, true);
    }

    public sealed class DirectoryInfo : FileSystemInfo
    {
        public DirectoryInfo(string path)
        {
            OriginalPath = path;
            FullPath = FileSystem.Full(path, "path");
        }

        public override string Name
        {
            get
            {
                string trimmed = FullPath.Length > 1 && Path.EndsInDirectorySeparator(FullPath) ? FullPath.Substring(0, FullPath.Length - 1) : FullPath;
                return trimmed == "/" ? "/" : Path.GetFileName(trimmed);
            }
        }

        public override bool Exists => FileSystem.IsDirectory(FullPath);

        public DirectoryInfo Parent
        {
            get
            {
                string parent = Path.GetDirectoryName(FullPath);
                return parent == null ? null : new DirectoryInfo(parent);
            }
        }

        public void Create() => Directory.CreateDirectory(FullPath);

        public DirectoryInfo CreateSubdirectory(string path) => Directory.CreateDirectory(Path.Combine(FullPath, path));

        public override void Delete() => Directory.Delete(FullPath);

        public void Delete(bool recursive) => Directory.Delete(FullPath, recursive);

        public FileInfo[] GetFiles() => GetFiles("*", SearchOption.TopDirectoryOnly);

        public FileInfo[] GetFiles(string searchPattern) => GetFiles(searchPattern, SearchOption.TopDirectoryOnly);

        public FileInfo[] GetFiles(string searchPattern, SearchOption searchOption)
        {
            List<string> paths = FileSystem.Enumerate(FullPath, searchPattern, searchOption, true, false);
            var result = new FileInfo[paths.Count];
            for (int i = 0; i < result.Length; i++)
            {
                result[i] = new FileInfo(paths[i]);
            }
            return result;
        }

        public DirectoryInfo[] GetDirectories()
        {
            List<string> paths = FileSystem.Enumerate(FullPath, "*", SearchOption.TopDirectoryOnly, false, true);
            var result = new DirectoryInfo[paths.Count];
            for (int i = 0; i < result.Length; i++)
            {
                result[i] = new DirectoryInfo(paths[i]);
            }
            return result;
        }
    }

    public abstract class TextWriter : IDisposable
    {
        public virtual string NewLine => Environment.NewLine;

        public abstract void Write(char value);

        public virtual void Write(string value)
        {
            if (value == null)
            {
                return;
            }
            for (int i = 0; i < value.Length; i++)
            {
                Write(value[i]);
            }
        }

        public virtual void Write(char[] buffer) => Write(new string(buffer ?? new char[0]));

        public virtual void Write(bool value) => Write(value.ToString());

        public virtual void Write(int value) => Write(value.ToString());

        public virtual void Write(uint value) => Write(value.ToString());

        public virtual void Write(long value) => Write(value.ToString());

        public virtual void Write(ulong value) => Write(value.ToString());

        public virtual void Write(float value) => Write(value.ToString());

        public virtual void Write(double value) => Write(value.ToString());

        public virtual void Write(object value) => Write(value?.ToString());

        public virtual void Write(string format, object arg0) => Write(string.Format(format, arg0));

        public virtual void Write(string format, object arg0, object arg1) => Write(string.Format(format, arg0, arg1));

        public virtual void Write(string format, params object[] arg) => Write(string.Format(format, arg));

        public virtual void WriteLine() => Write(NewLine);

        public virtual void WriteLine(string value) => Write(value + NewLine);

        public virtual void WriteLine(char value) => Write(value.ToString() + NewLine);

        public virtual void WriteLine(bool value) => WriteLine(value.ToString());

        public virtual void WriteLine(int value) => WriteLine(value.ToString());

        public virtual void WriteLine(uint value) => WriteLine(value.ToString());

        public virtual void WriteLine(long value) => WriteLine(value.ToString());

        public virtual void WriteLine(ulong value) => WriteLine(value.ToString());

        public virtual void WriteLine(float value) => WriteLine(value.ToString());

        public virtual void WriteLine(double value) => WriteLine(value.ToString());

        public virtual void WriteLine(object value) => WriteLine(value?.ToString());

        public virtual void WriteLine(string format, object arg0) => WriteLine(string.Format(format, arg0));

        public virtual void WriteLine(string format, object arg0, object arg1) => WriteLine(string.Format(format, arg0, arg1));

        public virtual void WriteLine(string format, params object[] arg) => WriteLine(string.Format(format, arg));

        public virtual void Flush()
        {
        }

        public virtual void Close() => Dispose();

        public void Dispose()
        {
            Dispose(true);
        }

        protected virtual void Dispose(bool disposing)
        {
        }
    }

    public class StreamWriter : TextWriter
    {
        private readonly string path;
        private readonly StringBuilder pending = new StringBuilder();
        private bool disposed;

        public StreamWriter(string path)
            : this(path, false)
        {
        }

        // Файл создаётся (или обрезается) сразу, как у .NET: ошибка пути видна
        // при открытии, а не при первой записи.
        public StreamWriter(string path, bool append)
        {
            this.path = FileSystem.Full(path, "path");
            FileSystem.Check(FileSystem.WriteFile(this.path, new byte[0], append), this.path, false);
        }

        public StreamWriter(string path, bool append, Encoding encoding)
            : this(path, append)
        {
        }

        public bool AutoFlush { get; set; }

        public override void Write(char value)
        {
            pending.Append(value);
            if (AutoFlush)
            {
                Flush();
            }
        }

        public override void Write(string value)
        {
            pending.Append(value);
            if (AutoFlush)
            {
                Flush();
            }
        }

        public override void Flush()
        {
            if (disposed)
            {
                throw new ObjectDisposedException(null, "Cannot write to a closed TextWriter.");
            }
            if (pending.Length == 0)
            {
                return;
            }
            byte[] bytes = Encoding.UTF8.GetBytes(pending.ToString());
            pending.Clear();
            FileSystem.Check(FileSystem.WriteFile(path, bytes, true), path, false);
        }

        protected override void Dispose(bool disposing)
        {
            if (!disposed)
            {
                Flush();
                disposed = true;
            }
        }
    }

    public abstract class TextReader : IDisposable
    {
        public virtual int Peek() => -1;

        public virtual int Read() => -1;

        public virtual string ReadLine()
        {
            if (Peek() < 0)
            {
                return null;
            }
            var builder = new StringBuilder();
            while (true)
            {
                int c = Read();
                if (c < 0 || c == '\n')
                {
                    break;
                }
                if (c == '\r')
                {
                    if (Peek() == '\n')
                    {
                        Read();
                    }
                    break;
                }
                builder.Append((char)c);
            }
            return builder.ToString();
        }

        public virtual string ReadToEnd()
        {
            var builder = new StringBuilder();
            int c;
            while ((c = Read()) >= 0)
            {
                builder.Append((char)c);
            }
            return builder.ToString();
        }

        public virtual void Close() => Dispose();

        public void Dispose()
        {
            Dispose(true);
        }

        protected virtual void Dispose(bool disposing)
        {
        }
    }

    public class StreamReader : TextReader
    {
        private readonly string text;
        private int position;

        public StreamReader(string path)
        {
            string full = FileSystem.Full(path, "path");
            int status = FileSystem.ReadFile(full, out byte[] data);
            FileSystem.Check(status, full, false);
            text = FileSystem.Decode(data);
        }

        public StreamReader(string path, Encoding encoding)
            : this(path)
        {
        }

        public bool EndOfStream => position >= text.Length;

        public override int Peek() => position < text.Length ? text[position] : -1;

        public override int Read() => position < text.Length ? text[position++] : -1;

        public override string ReadToEnd()
        {
            string rest = text.Substring(position);
            position = text.Length;
            return rest;
        }
    }
}
