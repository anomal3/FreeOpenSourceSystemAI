// Образец для фазы N5a: файлы и каталоги — запись, чтение, дозапись, потоки
// строк, перечисление, копирование, перенос, удаление и исключения.
//
// Пути относительные: у `dotnet` на Windows — от текущего каталога (его
// выставляет `clr-check`), у `/bin/dotnet` на FreeOS — от домашнего каталога
// пользователя. Печатаются только имена, содержимое и числа: полный путь и
// разделитель каталогов у Windows другие, и сравнивать их было бы нечестно.
// Переводы строк записываются явно (`\n`), а не `Environment.NewLine`, — по той
// же причине.

using System.Text;

namespace FreeOs.Samples.Files;

public static class Program
{
    public static int Main()
    {
        Console.OutputEncoding = Encoding.UTF8;
        Console.WriteLine("files: start");

        const string root = "n5-files";
        if (Directory.Exists(root))
        {
            Directory.Delete(root, true);
        }
        Console.WriteLine(Directory.Exists(root) + " " + File.Exists(root));
        DirectoryInfo created = Directory.CreateDirectory(Path.Combine(root, "logs", "old"));
        Console.WriteLine(created.Name + " " + created.Exists + " " + Directory.Exists(Path.Combine(root, "logs")));

        // Целиком: текст, дозапись, строки, байты.
        string notes = Path.Combine(root, "notes.txt");
        File.WriteAllText(notes, "first line\nвторая строка\n");
        File.AppendAllText(notes, "third\n");
        string[] lines = File.ReadAllLines(notes);
        byte[] bytes = File.ReadAllBytes(notes);
        Console.WriteLine(File.ReadAllText(notes).Length + " " + lines.Length + " [" + string.Join("|", lines) + "] " + bytes.Length + " "
            + bytes[11] + " " + Encoding.UTF8.GetString(bytes, 11, 12) + " " + new FileInfo(notes).Length);
        File.WriteAllBytes(Path.Combine(root, "data.bin"), new byte[] { 0, 1, 2, 250, 255 });
        byte[] data = File.ReadAllBytes(Path.Combine(root, "data.bin"));
        Console.WriteLine(data.Length + " " + data[3] + " " + data[4] + " " + File.Exists(Path.Combine(root, "data.bin")));

        // Потоками строк.
        string log = Path.Combine(root, "logs", "app.log");
        using (var writer = new StreamWriter(log))
        {
            writer.Write("level=");
            writer.Write("info\n");
            writer.Write(42);
            writer.Write('\n');
        }
        using (var writer = new StreamWriter(log, true))
        {
            writer.Write("appended\n");
        }
        using (var reader = new StreamReader(log))
        {
            int number = 0;
            string? line;
            while ((line = reader.ReadLine()) != null)
            {
                Console.WriteLine("log " + ++number + ": " + line);
            }
            Console.WriteLine("end " + reader.EndOfStream);
        }
        Console.WriteLine(File.ReadLines(log).Count() + " " + File.ReadLines(log).Last());

        // Перечисление — имена, упорядоченные здесь: порядок каталога у разных
        // файловых систем разный, и .NET его не выравнивает.
        File.WriteAllText(Path.Combine(root, "a.md"), "# a");
        File.WriteAllText(Path.Combine(root, "logs", "b.txt"), "b");
        Console.WriteLine(Names(Directory.GetFiles(root)) + " | " + Names(Directory.GetFiles(root, "*.txt")) + " | "
            + Names(Directory.GetDirectories(root)) + " | " + Names(Directory.GetFiles(root, "*.txt", SearchOption.AllDirectories)) + " | "
            + Names(Directory.EnumerateFileSystemEntries(Path.Combine(root, "logs"))));

        // Копирование, перенос, удаление.
        string copy = Path.Combine(root, "copy.txt");
        string moved = Path.Combine(root, "logs", "moved.txt");
        File.Copy(notes, copy);
        File.Move(copy, moved);
        Console.WriteLine(File.Exists(copy) + " " + File.ReadAllText(moved).Length);
        try
        {
            File.Copy(notes, moved);
        }
        catch (IOException e)
        {
            Console.WriteLine("copy over: " + e.GetType().Name);
        }
        File.Copy(Path.Combine(root, "a.md"), moved, true);
        File.Delete(Path.Combine(root, "data.bin"));
        File.Delete(Path.Combine(root, "missing.bin"));
        Console.WriteLine(File.ReadAllText(moved) + " " + File.Exists(Path.Combine(root, "data.bin")));
        try
        {
            File.ReadAllText(Path.Combine(root, "missing.txt"));
        }
        catch (FileNotFoundException e)
        {
            Console.WriteLine("read: " + e.GetType().Name + " " + Path.GetFileName(e.FileName));
        }
        try
        {
            File.WriteAllText(Path.Combine(root, "no-such-dir", "x.txt"), "x");
        }
        catch (DirectoryNotFoundException e)
        {
            Console.WriteLine("write: " + e.GetType().Name);
        }
        try
        {
            Directory.Delete(Path.Combine(root, "logs"));
        }
        catch (IOException e)
        {
            Console.WriteLine("rmdir: " + e.GetType().Name);
        }
        Directory.Delete(Path.Combine(root, "logs", "old"));
        Directory.Move(Path.Combine(root, "logs"), Path.Combine(root, "archive"));
        Console.WriteLine(Directory.Exists(Path.Combine(root, "logs")) + " " + Names(Directory.GetFiles(Path.Combine(root, "archive"))));

        // Путь — только то, в чём Windows и Unix согласны.
        Console.WriteLine(Path.GetFileName("dir/sub/file.tar.gz") + " " + Path.GetFileNameWithoutExtension("dir/sub/file.tar.gz") + " "
            + Path.GetExtension("dir/sub/file.tar.gz") + " " + Path.ChangeExtension("report.txt", ".md") + " " + Path.HasExtension("README")
            + " [" + Path.GetExtension("dir.d/file") + "] " + Path.IsPathRooted("relative/path"));

        var info = new FileInfo(notes);
        Console.WriteLine(info.Name + " " + info.Extension + " " + info.Exists + " " + info.Length + " "
            + new FileInfo(Path.Combine(root, "nope")).Exists + " " + info.Directory!.Name + " " + new DirectoryInfo(root).GetFiles().Length);

        // Этот файл остаётся — его читает оболочка в сценарии стенда.
        File.WriteAllText(Path.Combine(root, "from-dotnet.txt"), "written by a C# program\n");
        Console.WriteLine("files: done");
        return lines.Length + 10;
    }

    private static string Names(IEnumerable<string> paths)
    {
        string[] names = paths.Select(path => Path.GetFileName(path)).ToArray();
        Array.Sort(names, string.CompareOrdinal);
        return string.Join(",", names);
    }
}
