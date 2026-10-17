// Сеть из C# на Mono во FreeOS (фаза 66a).
//
// До этой фазы Mono собиралась с `--enable-minimal=sockets`: libc отвечала на
// сокеты ENOSYS, и `System.Net.Sockets` не было вовсе. С фазы 65b сокеты BSD
// настоящие, и эта программа проверяет, что поверх них работает обычный код
// .NET — тот же, что под Windows:
//   - `TcpClient`: соединение, запись, чтение эха, адреса концов;
//   - `ConnectAsync`/`ReadAsync`: асинхронный ввод-вывод — его ждёт поток
//     опроса пула Mono;
//   - `TcpListener`: принять клиента с хоста;
//   - `HttpWebRequest` и `WebClient`: HTTP к сайту стенда, в том числе ответ
//     кусками (`Transfer-Encoding: chunked`);
//   - `Dns`: адрес по имени-числу.
// Каждая проверка устроена так, чтобы заглушка её не прошла: эхо обязано
// вернуться тем же текстом, тело — тем, что отдаёт хост.
//
// net.exe <эхо-порт> <порт сайта> <порт для приёма>
using System;
using System.IO;
using System.Net;
using System.Net.Sockets;
using System.Text;
using System.Threading.Tasks;

static class Net
{
    static int failures;

    static void Check(bool ok, string what)
    {
        if (ok)
        {
            Console.WriteLine("net: ok " + what);
        }
        else
        {
            failures++;
            Console.WriteLine("net: FAILED " + what);
        }
    }

    static string Echo(NetworkStream stream, string text)
    {
        byte[] data = Encoding.ASCII.GetBytes(text);
        stream.Write(data, 0, data.Length);
        byte[] answer = new byte[data.Length];
        int got = 0;
        while (got < answer.Length)
        {
            int read = stream.Read(answer, got, answer.Length - got);
            if (read <= 0)
            {
                break;
            }
            got += read;
        }
        return Encoding.ASCII.GetString(answer, 0, got);
    }

    static void Client(IPAddress host, int port)
    {
        using (TcpClient client = new TcpClient())
        {
            client.NoDelay = true;
            client.Connect(host, port);
            Check(client.Connected, "TcpClient connects");
            string echoed = Echo(client.GetStream(), "mono-sockets-hello");
            Console.WriteLine("net: echoed '" + echoed + "'");
            Check(echoed == "mono-sockets-hello", "the echo comes back");
            IPEndPoint mine = (IPEndPoint)client.Client.LocalEndPoint;
            IPEndPoint theirs = (IPEndPoint)client.Client.RemoteEndPoint;
            Console.WriteLine("net: local " + mine + " peer " + theirs);
            Check(mine.Port != 0 && !mine.Address.Equals(IPAddress.Any), "LocalEndPoint names the address");
            Check(theirs.Address.Equals(host) && theirs.Port == port, "RemoteEndPoint names the server");
            client.Client.Shutdown(SocketShutdown.Send);
            byte[] rest = new byte[16];
            Check(client.GetStream().Read(rest, 0, rest.Length) == 0, "the end of stream reads as zero");
        }
    }

    static async Task ClientAsync(IPAddress host, int port)
    {
        using (TcpClient client = new TcpClient())
        {
            await client.ConnectAsync(host, port);
            NetworkStream stream = client.GetStream();
            byte[] data = Encoding.ASCII.GetBytes("async-hello");
            await stream.WriteAsync(data, 0, data.Length);
            byte[] answer = new byte[data.Length];
            int got = 0;
            while (got < answer.Length)
            {
                int read = await stream.ReadAsync(answer, got, answer.Length - got);
                if (read <= 0)
                {
                    break;
                }
                got += read;
            }
            Check(Encoding.ASCII.GetString(answer, 0, got) == "async-hello", "ConnectAsync and ReadAsync get the echo");
        }
    }

    static void Web(IPAddress host, int port)
    {
        string root = "http://" + host + ":" + port;
        HttpWebRequest request = (HttpWebRequest)WebRequest.Create(root + "/hello.txt");
        using (HttpWebResponse response = (HttpWebResponse)request.GetResponse())
        using (StreamReader reader = new StreamReader(response.GetResponseStream()))
        {
            string body = reader.ReadToEnd();
            Console.WriteLine("net: http " + (int)response.StatusCode + " '" + body.Trim() + "'");
            Check(response.StatusCode == HttpStatusCode.OK && body == "hello from the host\n", "HttpWebRequest reads the page");
        }
        using (WebClient web = new WebClient())
        {
            string chunked = web.DownloadString(root + "/chunked.txt");
            Check(chunked == "hello", "WebClient reads a chunked answer");
        }
        try
        {
            WebRequest.Create(root + "/missing").GetResponse().Close();
            Check(false, "a missing page is an error");
        }
        catch (WebException error)
        {
            HttpWebResponse response = error.Response as HttpWebResponse;
            Check(response != null && response.StatusCode == HttpStatusCode.NotFound, "a missing page is 404");
        }
    }

    static void Serve(int port)
    {
        TcpListener listener = new TcpListener(IPAddress.Any, port);
        listener.Start();
        Console.WriteLine("net: listening on port " + port);
        using (TcpClient client = listener.AcceptTcpClient())
        {
            Console.WriteLine("net: accepted a client from " + ((IPEndPoint)client.Client.RemoteEndPoint).Address);
            NetworkStream stream = client.GetStream();
            byte[] buffer = new byte[512];
            long total = 0;
            int read;
            while ((read = stream.Read(buffer, 0, buffer.Length)) > 0)
            {
                stream.Write(buffer, 0, read);
                total += read;
            }
            Console.WriteLine("net: served " + total + " bytes");
        }
        listener.Stop();
    }

    static int Main(string[] args)
    {
        if (args.Length != 3)
        {
            Console.WriteLine("usage: net.exe <echo port> <site port> <listen port>");
            return 2;
        }
        IPAddress host = Dns.GetHostAddresses("10.0.2.2")[0];
        Check(host.Equals(IPAddress.Parse("10.0.2.2")), "Dns gives the address");
        Client(host, int.Parse(args[0]));
        ClientAsync(host, int.Parse(args[0])).Wait();
        Web(host, int.Parse(args[1]));
        Console.WriteLine("net: done, " + failures + " check(s) failed");
        Serve(int.Parse(args[2]));
        return failures == 0 ? 0 : 1;
    }
}
