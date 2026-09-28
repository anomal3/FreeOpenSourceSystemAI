// Драйвер окон WinForms Mono для FreeOS (фаза 62) — третий рядом с X11 и
// Win32 (`XplatUIX11.cs`, `XplatUIWin32.cs`).
//
// # Модель
//
// У X11 каждый элемент управления — своё окно сервера, и отсечение детей,
// перерисовку и попадание мыши делает сервер. У FreeOS окно — только у формы
// верхнего уровня: поверхность в памяти программы, которую стол показывает под
// своим заголовком (`<freeos/window.h>`). Всё, что внутри формы, — «лёгкие» окна
// этого драйвера: `Hwnd` с прямоугольником в клиентской области родителя. Что
// делал бы сервер, делает драйвер:
//
// - **рисование** — в задний буфер формы (`TopLevel.Back`), с отсечением по
//   родителям, по видимым детям и по соседям выше в порядке наложения — ровно
//   так, как X11 отсекает дочерние окна: родитель никогда не закрашивает детей;
// - **поверхность** получает только изменившееся (`TopLevel.Dirty`), когда
//   очередь опустела: копия прямоугольника и `freeos_window_commit`. Копия, а не
//   рисование прямо в поверхность, по двум причинам: порядок байтов точки у
//   машины бывает RGB (у GDI+ — всегда BGR), и каретку ввода драйвер рисует
//   поверх копии, не трогая картинку формы;
// - **мышь** — попадание в самое глубокое видимое окно под точкой;
// - **клавиатура** — окну с фокусом (`focus_window`).
//
// # Чего договор окна не даёт, и что из этого следует
//
// - Отпускания кнопки мыши нет: стол сообщает щелчок. Драйвер отдаёт форме
//   нажатие и отпускание подряд — кнопки, флажки и поля работают, а
//   перетаскивания мышью (выделение текста, ползунок) нет.
// - Отпускания клавиши тоже нет: `WM_KEYUP` идёт сразу за `WM_KEYDOWN`.
// - Заголовок окна задаётся при открытии и потом не меняется.
// - Всплывающие окна без заголовка (подсказки, выпадающие списки, меню) на стол
//   не выходят: у каждого окна FreeOS своя полоса заголовка, а окон у программы
//   не больше восьми. Это отдельный шаг.
//
// Отладка: `FREEOS_MWF_DEBUG=1` печатает в stderr, что драйвер делает с окнами.

using System;
using System.Collections;
using System.Collections.Generic;
using System.ComponentModel;
using System.Drawing;
using System.Drawing.Imaging;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;

namespace System.Windows.Forms {

	internal class XplatUIFreeOS : XplatUIDriver {

		#region Договор окна: <freeos/window.h>

		static class Native {
			const string Lib = "freeos";

			[StructLayout (LayoutKind.Sequential)]
			internal struct WinEvent {
				internal uint Kind;
				internal uint Code;
				internal int X;
				internal int Y;
			}

			[DllImport (Lib)]
			internal static extern long freeos_window_open (byte[] title, uint width, uint height, out IntPtr surface);
			[DllImport (Lib)]
			internal static extern int freeos_window_commit (long id, int x, int y, int width, int height);
			[DllImport (Lib)]
			internal static extern int freeos_window_event (long id, out WinEvent ev);
			[DllImport (Lib)]
			internal static extern IntPtr freeos_window_resize (long id, uint width, uint height);
			[DllImport (Lib)]
			internal static extern int freeos_window_close (long id);
			[DllImport (Lib)]
			internal static extern int freeos_screen (out uint width, out uint height, out uint pixel_format);
		}

		const uint WIN_KEY = 1;
		const uint WIN_POINTER = 2;
		const uint WIN_CLOSE = 3;
		const uint WIN_MOVE = 4;
		const uint WIN_LEAVE = 5;
		const int WIN_MOD_SHIFT = 1;
		const int WIN_MOD_CTRL = 2;
		const int WIN_MOD_ALT = 4;
		const uint WIN_KEY_NAMED = 0x01000000;
		const uint PIXEL_RGB = 1;

		#endregion

		#region Состояние

		// Окно верхнего уровня — то, у которого есть окно FreeOS.
		sealed class TopLevel {
			internal Hwnd Hwnd;
			internal long Id = -1;
			internal IntPtr Surface;
			internal int Width;
			internal int Height;
			// Задний буфер: те же точки, что поверхность, в виде GDI+ (синий в
			// младшем байте). Память своя, а не libgdiplus: копировать на
			// поверхность её читают напрямую.
			internal IntPtr BackBits;
			internal Bitmap Back;
			internal Rectangle Dirty;
			internal string Title = "";
			internal bool IsOpen { get { return Id >= 0; } }
		}

		// Очередь сообщений одного потока: отложенные сообщения, асинхронные
		// вызовы, окна, ждущие перерисовки, и таймеры.
		sealed class FQueue {
			internal readonly Thread Thread;
			internal readonly LinkedList<object> Posted = new LinkedList<object> ();
			internal readonly List<Hwnd> Paint = new List<Hwnd> ();
			internal readonly ArrayList Timers = new ArrayList ();
			internal bool Quit;
			internal int ExitCode;
			internal bool IdleRaised;
			internal FQueue (Thread thread) { Thread = thread; }
		}

		// Что клавиша принесла, кроме кода: символ для WM_CHAR и модификаторы.
		sealed class KeyInfo {
			internal char Char;
			internal Keys Modifiers;
		}

		sealed class CaretState {
			internal IntPtr Hwnd;
			internal int X;
			internal int Y;
			internal int Width;
			internal int Height;
			internal bool Visible;
			internal bool On;
			internal Timer Timer;
		}

		static readonly object instance_lock = new object ();
		static XplatUIFreeOS instance;
		static int ref_count;
		static int next_handle = 0x10000;

		readonly bool debug;
		int screen_width = 1024;
		int screen_height = 768;
		bool pixel_rgb;

		readonly Dictionary<Hwnd, TopLevel> tops = new Dictionary<Hwnd, TopLevel> ();
		readonly Dictionary<Thread, FQueue> queues = new Dictionary<Thread, FQueue> ();
		readonly Dictionary<Hwnd, FQueue> hwnd_queue = new Dictionary<Hwnd, FQueue> ();
		readonly ArrayList unattached_timers = new ArrayList ();
		readonly Stack<IntPtr> modal_windows = new Stack<IntPtr> ();

		Hwnd foster;
		IntPtr focus_window;
		IntPtr active_window;
		IntPtr grab_window;
		Hwnd mouse_hwnd;
		MouseButtons mouse_state;
		Keys key_modifiers;
		Point mouse_position;
		bool themes_enabled;
		bool in_doevents;

		// Для двойного щелчка.
		long last_click_time;
		Point last_click_point;
		IntPtr last_click_hwnd;
		Msg last_click_message;

		readonly CaretState caret = new CaretState ();
		IDataObject clipboard;
		Bitmap scratch_bitmap;

		internal override event EventHandler Idle;

		#endregion

		#region Создание

		XplatUIFreeOS ()
		{
			debug = Environment.GetEnvironmentVariable ("FREEOS_MWF_DEBUG") != null;
		}

		internal static XplatUIFreeOS GetInstance ()
		{
			lock (instance_lock) {
				if (instance == null)
					instance = new XplatUIFreeOS ();
				ref_count++;
			}
			return instance;
		}

		public int Reference {
			get { return ref_count; }
		}

		internal override IntPtr InitializeDriver ()
		{
			uint width, height, format;
			if (Native.freeos_screen (out width, out height, out format) == 0 && width > 0 && height > 0) {
				screen_width = (int) width;
				screen_height = (int) height;
				pixel_rgb = format == PIXEL_RGB;
			}
			Trace ("screen {0}x{1}, pixels {2}", screen_width, screen_height, pixel_rgb ? "RGB" : "BGR");

			// Приёмная семья окон, у которых ещё нет родителя (как FosterParent у
			// X11): на стол она не выходит никогда.
			foster = new Hwnd ();
			IntPtr handle = NewHandle ();
			foster.WholeWindow = handle;
			foster.ClientWindow = handle;

			// Control.CreateGraphics () и Graphics.FromHwnd () приходят сюда
			// (System.Drawing, ветка FreeOS).
			GDIPlus.FreeOSGraphicsFromHwnd = GraphicsFromHwnd;

			// Буфер обмена — внутри программы: общего у стола пока нет.
			XplatUI.ClipboardGetContent = delegate (bool primary) { return clipboard; };
			XplatUI.ClipboardGetFormats = delegate (bool primary) {
				return clipboard != null ? clipboard.GetFormats () : new string[0];
			};
			XplatUI.ClipboardSetContent = delegate (bool primary, object data, bool copy) {
				clipboard = data as IDataObject ?? new DataObject (data);
			};
			XplatUI.ClipboardClear = delegate (bool primary) { clipboard = null; };
			return IntPtr.Zero;
		}

		internal override void ShutdownDriver (IntPtr token)
		{
		}

		static IntPtr NewHandle ()
		{
			return (IntPtr) Interlocked.Add (ref next_handle, 4);
		}

		void Trace (string format, params object[] args)
		{
			if (debug)
				Console.Error.WriteLine ("mwf-freeos: " + format, args);
		}

		#endregion

		#region Дерево окон и координаты

		static bool StyleSet (int s, WindowStyles ws)
		{
			return (s & (int) ws) == (int) ws;
		}

		static bool ExStyleSet (int ex, WindowExStyles exws)
		{
			return (ex & (int) exws) == (int) exws;
		}

		static Hwnd RootOf (Hwnd hwnd)
		{
			while (hwnd.parent != null)
				hwnd = hwnd.parent;
			return hwnd;
		}

		TopLevel TopOf (Hwnd hwnd)
		{
			if (hwnd == null)
				return null;
			TopLevel top;
			tops.TryGetValue (RootOf (hwnd), out top);
			return top;
		}

		// Виден ли на столе: сам видим и все предки видимы, а у корня открыто
		// окно FreeOS.
		bool IsShown (Hwnd hwnd)
		{
			for (Hwnd h = hwnd; h != null; h = h.parent) {
				if (!h.visible || h.zombie || h.zero_sized)
					return false;
				if (h.parent == null) {
					TopLevel top;
					return tops.TryGetValue (h, out top) && top.IsOpen;
				}
			}
			return false;
		}

		// Левый верхний угол окна целиком (с рамкой) в точках поверхности. У
		// окна верхнего уровня рамку и заголовок рисует стол, и поверхность
		// начинается с клиентской области — угол окна поэтому левее и выше нуля.
		static Point WholeOrigin (Hwnd hwnd)
		{
			if (hwnd.parent == null) {
				Rectangle client = hwnd.ClientRect;
				return new Point (-client.X, -client.Y);
			}
			Point p = ClientOrigin (hwnd.parent);
			return new Point (p.X + hwnd.x, p.Y + hwnd.y);
		}

		static Point ClientOrigin (Hwnd hwnd)
		{
			Point o = WholeOrigin (hwnd);
			Rectangle client = hwnd.ClientRect;
			return new Point (o.X + client.X, o.Y + client.Y);
		}

		static Rectangle WholeRect (Hwnd hwnd)
		{
			return new Rectangle (WholeOrigin (hwnd), new Size (hwnd.width, hwnd.height));
		}

		static Rectangle ClientRectOnSurface (Hwnd hwnd)
		{
			return new Rectangle (ClientOrigin (hwnd), hwnd.ClientRect.Size);
		}

		// Что от окна видно на поверхности: пересечение с клиентскими областями
		// предков, минус видимые дети и соседи выше в порядке наложения — у себя
		// и у каждого предка. Порядок наложения — список детей родителя, от
		// нижнего к верхнему.
		Region VisibleRegion (Hwnd hwnd, bool client)
		{
			Region region = new Region ();
			region.MakeEmpty ();
			if (!IsShown (hwnd))
				return region;
			TopLevel top = TopOf (hwnd);
			Rectangle rect = client ? ClientRectOnSurface (hwnd) : WholeRect (hwnd);
			for (Hwnd p = hwnd.parent; p != null; p = p.parent)
				rect.Intersect (ClientRectOnSurface (p));
			rect.Intersect (new Rectangle (0, 0, top.Width, top.Height));
			if (rect.IsEmpty)
				return region;
			region.Union (rect);
			foreach (Hwnd child in hwnd.children) {
				if (child.visible && !child.zero_sized)
					region.Exclude (WholeRect (child));
			}
			for (Hwnd c = hwnd; c.parent != null; c = c.parent) {
				ArrayList siblings = c.parent.children;
				for (int i = siblings.IndexOf (c) + 1; i < siblings.Count; i++) {
					Hwnd s = (Hwnd) siblings [i];
					if (s.visible && !s.zero_sized)
						region.Exclude (WholeRect (s));
				}
			}
			return region;
		}

		// Самое глубокое видимое окно под точкой поверхности.
		static Hwnd HitTest (Hwnd hwnd, Point p)
		{
			if (!ClientRectOnSurface (hwnd).Contains (p))
				return hwnd;
			for (int i = hwnd.children.Count - 1; i >= 0; i--) {
				Hwnd child = (Hwnd) hwnd.children [i];
				if (child.visible && !child.zero_sized && !child.zombie && WholeRect (child).Contains (p))
					return HitTest (child, p);
			}
			return hwnd;
		}

		// «Экранные» координаты. Где стол поставил окно, программа не знает, —
		// драйвер считает, что там, где просила форма (`Location`): так перевод
		// туда и обратно сходится, а больше экранные точки ни для чего не нужны.
		Point ScreenOrigin (Hwnd hwnd)
		{
			Hwnd root = RootOf (hwnd);
			Point o = ClientOrigin (hwnd);
			Rectangle client = root.ClientRect;
			return new Point (root.x + client.X + o.X, root.y + client.Y + o.Y);
		}

		#endregion

		#region Окна FreeOS

		void OpenTop (Hwnd hwnd)
		{
			TopLevel top;
			if (!tops.TryGetValue (hwnd, out top) || top.IsOpen)
				return;
			Rectangle client = hwnd.ClientRect;
			int width = Math.Max (1, client.Width);
			int height = Math.Max (1, client.Height);
			IntPtr surface;
			byte[] title = Encoding.UTF8.GetBytes ((top.Title ?? "") + "\0");
			long id = Native.freeos_window_open (title, (uint) width, (uint) height, out surface);
			if (id < 0) {
				Console.Error.WriteLine ("mwf-freeos: the desktop refused a window '{0}' ({1}x{2}): error {3}",
					top.Title, width, height, Marshal.GetLastWin32Error ());
				return;
			}
			top.Id = id;
			top.Surface = surface;
			SetBackBuffer (top, width, height);
			Trace ("open '{0}' {1}x{2} as window {3}", top.Title, width, height, id);
			InvalidateTree (hwnd);
		}

		void CloseTop (TopLevel top)
		{
			if (!top.IsOpen)
				return;
			Native.freeos_window_close (top.Id);
			Trace ("close window {0}", top.Id);
			top.Id = -1;
			top.Surface = IntPtr.Zero;
			FreeBackBuffer (top);
		}

		void SetBackBuffer (TopLevel top, int width, int height)
		{
			FreeBackBuffer (top);
			top.Width = width;
			top.Height = height;
			top.BackBits = Marshal.AllocHGlobal (width * height * 4);
			top.Back = new Bitmap (width, height, width * 4, PixelFormat.Format32bppRgb, top.BackBits);
			using (Graphics g = Graphics.FromImage (top.Back))
				g.Clear (SystemColors.Control);
			top.Dirty = new Rectangle (0, 0, width, height);
		}

		static void FreeBackBuffer (TopLevel top)
		{
			if (top.Back != null) {
				top.Back.Dispose ();
				top.Back = null;
			}
			if (top.BackBits != IntPtr.Zero) {
				Marshal.FreeHGlobal (top.BackBits);
				top.BackBits = IntPtr.Zero;
			}
			top.Dirty = Rectangle.Empty;
		}

		// Размер клиентской области формы сменился — поверхность тоже.
		void ResizeTop (TopLevel top)
		{
			if (!top.IsOpen)
				return;
			Rectangle client = top.Hwnd.ClientRect;
			int width = Math.Max (1, client.Width);
			int height = Math.Max (1, client.Height);
			if (width == top.Width && height == top.Height)
				return;
			IntPtr surface = Native.freeos_window_resize (top.Id, (uint) width, (uint) height);
			if (surface == IntPtr.Zero) {
				Console.Error.WriteLine ("mwf-freeos: the desktop refused to resize window {0} to {1}x{2}", top.Id, width, height);
				return;
			}
			top.Surface = surface;
			SetBackBuffer (top, width, height);
			Trace ("resize window {0} to {1}x{2}", top.Id, width, height);
			InvalidateTree (top.Hwnd);
		}

		// Перенести изменившееся на поверхности и сказать об этом столу.
		unsafe void Flush (FQueue queue)
		{
			foreach (TopLevel top in tops.Values) {
				if (!top.IsOpen || top.Dirty.IsEmpty || QueueOf (top.Hwnd) != queue)
					continue;
				Rectangle r = Rectangle.Intersect (top.Dirty, new Rectangle (0, 0, top.Width, top.Height));
				top.Dirty = Rectangle.Empty;
				if (r.IsEmpty)
					continue;
				uint* back = (uint*) top.BackBits;
				uint* surface = (uint*) top.Surface;
				for (int y = r.Top; y < r.Bottom; y++) {
					uint* from = back + y * top.Width + r.Left;
					uint* to = surface + y * top.Width + r.Left;
					if (pixel_rgb) {
						for (int x = 0; x < r.Width; x++) {
							uint v = from [x];
							to [x] = 0xff000000u | (v & 0x0000ff00u) | ((v >> 16) & 0xffu) | ((v & 0xffu) << 16);
						}
					} else {
						for (int x = 0; x < r.Width; x++)
							to [x] = from [x] | 0xff000000u;
					}
				}
				DrawCaret (top, r);
				Native.freeos_window_commit (top.Id, r.X, r.Y, r.Width, r.Height);
			}
		}

		#endregion

		#region Перерисовка

		void AddExpose (Hwnd hwnd, bool client, Rectangle rect)
		{
			if (hwnd == null || hwnd.zombie)
				return;
			Rectangle bounds = client ? new Rectangle (Point.Empty, hwnd.ClientRect.Size) : new Rectangle (0, 0, hwnd.width, hwnd.height);
			rect.Intersect (bounds);
			if (rect.IsEmpty)
				return;
			FQueue queue = QueueOf (hwnd);
			if (client) {
				hwnd.AddInvalidArea (rect);
				if (!hwnd.expose_pending) {
					hwnd.expose_pending = true;
					if (!hwnd.nc_expose_pending && queue != null)
						lock (queue) queue.Paint.Add (hwnd);
				}
			} else {
				hwnd.AddNcInvalidArea (rect);
				if (!hwnd.nc_expose_pending) {
					hwnd.nc_expose_pending = true;
					if (!hwnd.expose_pending && queue != null)
						lock (queue) queue.Paint.Add (hwnd);
				}
			}
		}

		static bool HasBorder (Hwnd hwnd)
		{
			return hwnd.parent != null && (hwnd.border_style == FormBorderStyle.Fixed3D || hwnd.border_style == FormBorderStyle.FixedSingle);
		}

		// Окно и всё, что в нём, рисуется заново — окно появилось на столе или
		// сменило поверхность.
		void InvalidateTree (Hwnd hwnd)
		{
			if (!hwnd.visible || hwnd.zombie)
				return;
			AddExpose (hwnd, true, new Rectangle (Point.Empty, hwnd.ClientRect.Size));
			if (HasBorder (hwnd))
				AddExpose (hwnd, false, new Rectangle (0, 0, hwnd.width, hwnd.height));
			foreach (Hwnd child in hwnd.children.ToArray ())
				InvalidateTree (child);
		}

		// Часть поверхности открылась (окно ушло, сдвинулось, спряталось) —
		// перерисовать всех, кому в ней что-то видно.
		void InvalidateArea (Hwnd hwnd, Rectangle surface_rect)
		{
			if (!hwnd.visible || hwnd.zombie || surface_rect.IsEmpty)
				return;
			Rectangle whole = WholeRect (hwnd);
			if (!whole.IntersectsWith (surface_rect))
				return;
			Rectangle client = ClientRectOnSurface (hwnd);
			Rectangle c = Rectangle.Intersect (client, surface_rect);
			if (!c.IsEmpty) {
				c.Offset (-client.X, -client.Y);
				AddExpose (hwnd, true, c);
			}
			if (HasBorder (hwnd))
				AddExpose (hwnd, false, new Rectangle (0, 0, hwnd.width, hwnd.height));
			foreach (Hwnd child in hwnd.children.ToArray ())
				InvalidateArea (child, surface_rect);
		}

		void InvalidateUnder (Hwnd hwnd, Rectangle surface_rect)
		{
			if (hwnd.parent != null && IsShown (hwnd.parent))
				InvalidateArea (RootOf (hwnd), surface_rect);
		}

		// Рамку дочернего окна рисует драйвер, как у X11: `Fixed3D` и
		// `FixedSingle` — неклиентская область, до которой рисование элемента не
		// дотягивается.
		void DrawBorder (Hwnd hwnd)
		{
			TopLevel top = TopOf (hwnd);
			if (top == null || top.Back == null || !HasBorder (hwnd))
				return;
			Point o = WholeOrigin (hwnd);
			Region clip = VisibleRegion (hwnd, false);
			using (Graphics g = Graphics.FromImage (top.Back)) {
				Rectangle bounds = Rectangle.Round (clip.GetBounds (g));
				top.Dirty = Union (top.Dirty, bounds);
				clip.Translate (-o.X, -o.Y);
				g.TranslateTransform (o.X, o.Y);
				g.Clip = clip;
				Rectangle rect = new Rectangle (0, 0, hwnd.width, hwnd.height);
				if (hwnd.border_style == FormBorderStyle.Fixed3D) {
					ControlPaint.DrawBorder3D (g, rect, hwnd.border_static ? Border3DStyle.SunkenOuter : Border3DStyle.Sunken);
				} else {
					ControlPaint.DrawBorder (g, rect, Color.Black, ButtonBorderStyle.Solid);
				}
			}
			clip.Dispose ();
		}

		static Rectangle Union (Rectangle a, Rectangle b)
		{
			if (a.IsEmpty)
				return b;
			if (b.IsEmpty)
				return a;
			return Rectangle.Union (a, b);
		}

		Graphics ScratchGraphics ()
		{
			if (scratch_bitmap == null)
				scratch_bitmap = new Bitmap (1, 1, PixelFormat.Format32bppArgb);
			return Graphics.FromImage (scratch_bitmap);
		}

		// Graphics поверх заднего буфера формы: начало — угол клиентской
		// области окна, отсечение — видимая его часть (и, если задано, область).
		Graphics WindowGraphics (Hwnd hwnd, bool client, Region limit, out Rectangle painted)
		{
			painted = Rectangle.Empty;
			TopLevel top = TopOf (hwnd);
			if (top == null || top.Back == null || !IsShown (hwnd))
				return ScratchGraphics ();
			Point o = client ? ClientOrigin (hwnd) : WholeOrigin (hwnd);
			Region clip = VisibleRegion (hwnd, client);
			if (limit != null) {
				limit.Translate (o.X, o.Y);
				clip.Intersect (limit);
			}
			Graphics g = Graphics.FromImage (top.Back);
			painted = Rectangle.Round (clip.GetBounds (g));
			top.Dirty = Union (top.Dirty, painted);
			clip.Translate (-o.X, -o.Y);
			g.TranslateTransform (o.X, o.Y);
			g.Clip = clip;
			return g;
		}

		// Control.CreateGraphics () и Graphics.FromHwnd (): рисунок попадёт на
		// стол при следующем опустении очереди.
		Graphics GraphicsFromHwnd (IntPtr handle)
		{
			Hwnd hwnd = handle != IntPtr.Zero ? Hwnd.ObjectFromHandle (handle) : null;
			if (hwnd == null)
				return ScratchGraphics ();
			Rectangle painted;
			return WindowGraphics (hwnd, true, null, out painted);
		}

		internal override PaintEventArgs PaintEventStart (ref Message msg, IntPtr handle, bool client)
		{
			Hwnd hwnd = Hwnd.ObjectFromHandle (msg.HWnd);
			Hwnd paint_hwnd = msg.HWnd == handle ? hwnd : Hwnd.ObjectFromHandle (handle);
			if (hwnd == null || paint_hwnd == null)
				return new PaintEventArgs (ScratchGraphics (), Rectangle.Empty);

			Rectangle painted;
			if (client) {
				Region limit = null;
				if (hwnd.invalid_list.Count > 0) {
					limit = new Region ();
					limit.MakeEmpty ();
					foreach (Rectangle r in hwnd.ClipRectangles)
						limit.Union (r);
				}
				if (hwnd.UserClip != null) {
					if (limit == null)
						limit = hwnd.UserClip.Clone ();
					else
						limit.Intersect (hwnd.UserClip);
				}
				Rectangle area = hwnd.invalid_list.Count > 0 ? hwnd.Invalid : new Rectangle (Point.Empty, hwnd.ClientRect.Size);
				Graphics dc = WindowGraphics (paint_hwnd, true, limit, out painted);
				hwnd.expose_pending = false;
				hwnd.ClearInvalidArea ();
				return new PaintEventArgs (dc, area);
			} else {
				Region limit = null;
				if (!hwnd.nc_invalid.IsEmpty)
					limit = new Region (hwnd.nc_invalid);
				Rectangle area = hwnd.nc_invalid.IsEmpty ? new Rectangle (0, 0, hwnd.width, hwnd.height) : hwnd.nc_invalid;
				Graphics dc = WindowGraphics (paint_hwnd, false, limit, out painted);
				hwnd.nc_expose_pending = false;
				hwnd.ClearNcInvalidArea ();
				return new PaintEventArgs (dc, area);
			}
		}

		internal override void PaintEventEnd (ref Message msg, IntPtr handle, bool client, PaintEventArgs pevent)
		{
			if (pevent.Graphics != null)
				pevent.Graphics.Dispose ();
			pevent.SetGraphics (null);
			pevent.Dispose ();
		}

		internal override void Invalidate (IntPtr handle, Rectangle rc, bool clear)
		{
			Hwnd hwnd = Hwnd.ObjectFromHandle (handle);
			if (hwnd == null)
				return;
			if (clear)
				rc = new Rectangle (Point.Empty, hwnd.ClientRect.Size);
			AddExpose (hwnd, true, rc);
		}

		internal override void InvalidateNC (IntPtr handle)
		{
			Hwnd hwnd = Hwnd.ObjectFromHandle (handle);
			if (hwnd != null)
				AddExpose (hwnd, false, new Rectangle (0, 0, hwnd.width, hwnd.height));
		}

		internal override void UpdateWindow (IntPtr handle)
		{
			Hwnd hwnd = Hwnd.ObjectFromHandle (handle);
			if (hwnd == null || !hwnd.expose_pending || !IsShown (hwnd))
				return;
			SendMessage (handle, Msg.WM_PAINT, IntPtr.Zero, IntPtr.Zero);
			hwnd.expose_pending = false;
		}

		internal override void ScrollWindow (IntPtr handle, Rectangle area, int XAmount, int YAmount, bool with_children)
		{
			// Честная прокрутка копией точек — потом; пока область просто
			// рисуется заново, и видно то же самое, только медленнее.
			Hwnd hwnd = Hwnd.ObjectFromHandle (handle);
			if (hwnd != null)
				AddExpose (hwnd, true, area);
		}

		internal override void ScrollWindow (IntPtr handle, int XAmount, int YAmount, bool with_children)
		{
			Hwnd hwnd = Hwnd.ObjectFromHandle (handle);
			if (hwnd != null)
				AddExpose (hwnd, true, new Rectangle (Point.Empty, hwnd.ClientRect.Size));
		}

		#endregion

		#region Каретка ввода

		Rectangle CaretRect ()
		{
			Hwnd hwnd = Hwnd.ObjectFromHandle (caret.Hwnd);
			if (hwnd == null)
				return Rectangle.Empty;
			Point o = ClientOrigin (hwnd);
			return new Rectangle (o.X + caret.X, o.Y + caret.Y, Math.Max (1, caret.Width), Math.Max (1, caret.Height));
		}

		void MarkCaret ()
		{
			TopLevel top = TopOf (Hwnd.ObjectFromHandle (caret.Hwnd));
			if (top != null && top.IsOpen)
				top.Dirty = Union (top.Dirty, CaretRect ());
		}

		// Каретка рисуется только на поверхности, поверх свежей копии: картинка
		// формы в заднем буфере её не видит, и стирать нечего.
		unsafe void DrawCaret (TopLevel top, Rectangle copied)
		{
			if (!caret.Visible || !caret.On || caret.Hwnd == IntPtr.Zero)
				return;
			Hwnd hwnd = Hwnd.ObjectFromHandle (caret.Hwnd);
			if (hwnd == null || TopOf (hwnd) != top || !IsShown (hwnd))
				return;
			Rectangle r = CaretRect ();
			r.Intersect (ClientRectOnSurface (hwnd));
			r.Intersect (copied);
			uint* surface = (uint*) top.Surface;
			for (int y = r.Top; y < r.Bottom; y++)
				for (int x = r.Left; x < r.Right; x++)
					surface [y * top.Width + x] ^= 0x00ffffffu;
		}

		void CaretTick (object sender, EventArgs e)
		{
			caret.On = !caret.On;
			MarkCaret ();
		}

		internal override void CreateCaret (IntPtr handle, int width, int height)
		{
			if (caret.Hwnd != IntPtr.Zero)
				DestroyCaret (caret.Hwnd);
			caret.Hwnd = handle;
			caret.Width = width;
			caret.Height = height;
			caret.Visible = false;
			caret.On = false;
			if (caret.Timer == null) {
				caret.Timer = new Timer ();
				caret.Timer.Interval = CaretBlinkTime;
				caret.Timer.Tick += CaretTick;
			}
		}

		internal override void DestroyCaret (IntPtr handle)
		{
			if (caret.Hwnd != handle)
				return;
			MarkCaret ();
			if (caret.Timer != null)
				caret.Timer.Stop ();
			caret.Hwnd = IntPtr.Zero;
			caret.Visible = false;
			caret.On = false;
		}

		internal override void SetCaretPos (IntPtr handle, int x, int y)
		{
			if (caret.Hwnd != handle)
				return;
			MarkCaret ();
			caret.X = x;
			caret.Y = y;
			if (caret.Visible) {
				caret.On = true;
				caret.Timer.Stop ();
				caret.Timer.Start ();
			}
			MarkCaret ();
		}

		internal override void CaretVisible (IntPtr handle, bool visible)
		{
			if (caret.Hwnd != handle)
				return;
			caret.Visible = visible;
			caret.On = visible;
			if (visible)
				caret.Timer.Start ();
			else
				caret.Timer.Stop ();
			MarkCaret ();
		}

		#endregion

		#region Окна: создание, место, видимость

		internal override IntPtr CreateWindow (CreateParams cp)
		{
			Hwnd hwnd = new Hwnd ();
			int x = cp.X;
			int y = cp.Y;
			int width = Math.Max (0, cp.Width);
			int height = Math.Max (0, cp.Height);

			Hwnd parent = null;
			if (cp.Parent != IntPtr.Zero)
				parent = Hwnd.ObjectFromHandle (cp.Parent);
			else if (StyleSet (cp.Style, WindowStyles.WS_CHILD))
				parent = foster;

			if (cp.control is Form && cp.X == int.MinValue && cp.Y == int.MinValue) {
				Point next = Hwnd.GetNextStackedFormLocation (cp);
				x = next.X;
				y = next.Y;
			}

			hwnd.x = x;
			hwnd.y = y;
			hwnd.width = width;
			hwnd.height = height;
			hwnd.zero_sized = width < 1 || height < 1;
			hwnd.initial_style = cp.WindowStyle;
			hwnd.initial_ex_style = cp.WindowExStyle;
			if (StyleSet (cp.Style, WindowStyles.WS_DISABLED))
				hwnd.enabled = false;

			IntPtr handle = NewHandle ();
			hwnd.WholeWindow = handle;
			hwnd.ClientWindow = handle;
			hwnd.Parent = parent;
			lock (hwnd_queue)
				hwnd_queue [hwnd] = ThreadQueue (Thread.CurrentThread);
			SetHwndStyles (hwnd, cp);

			if (parent == null)
				tops [hwnd] = new TopLevel { Hwnd = hwnd, Title = cp.Caption ?? "" };

			Trace ("create {0:X} parent {1:X} {2}x{3} at {4},{5} '{6}' style {7}",
				handle.ToInt64 (), parent != null ? parent.Handle.ToInt64 () : 0, width, height, x, y, cp.Caption,
				(WindowStyles) cp.Style);

			SendMessage (handle, Msg.WM_CREATE, (IntPtr) 1, IntPtr.Zero);

			if (StyleSet (cp.Style, WindowStyles.WS_VISIBLE)) {
				hwnd.visible = true;
				Map (hwnd);
				if (!(Control.FromHandle (handle) is Form))
					SendMessage (handle, Msg.WM_SHOWWINDOW, (IntPtr) 1, IntPtr.Zero);
			}
			return hwnd.zombie ? IntPtr.Zero : handle;
		}

		internal override IntPtr CreateWindow (IntPtr Parent, int X, int Y, int Width, int Height)
		{
			CreateParams cp = new CreateParams ();
			cp.Caption = "";
			cp.X = X;
			cp.Y = Y;
			cp.Width = Width;
			cp.Height = Height;
			cp.ClassName = XplatUI.GetDefaultClassName (GetType ());
			cp.ClassStyle = 0;
			cp.ExStyle = 0;
			cp.Parent = IntPtr.Zero;
			cp.Param = 0;
			return CreateWindow (cp);
		}

		// Окно верхнего уровня без заголовка — подсказка, выпадающий список, меню.
		// На стол такие пока не выходят (см. шапку).
		static bool IsPopup (Hwnd hwnd)
		{
			int style = (int) hwnd.initial_style;
			return StyleSet (style, WindowStyles.WS_POPUP) && !StyleSet (style, WindowStyles.WS_CAPTION);
		}

		void Map (Hwnd hwnd)
		{
			if (hwnd.parent == null) {
				if (!IsPopup (hwnd))
					OpenTop (hwnd);
			} else if (IsShown (hwnd)) {
				InvalidateTree (hwnd);
			}
		}

		internal override void DestroyWindow (IntPtr handle)
		{
			Hwnd hwnd = Hwnd.ObjectFromHandle (handle);
			if (hwnd == null || hwnd.zombie)
				return;
			Trace ("destroy {0:X}", handle.ToInt64 ());

			if (hwnd.parent != null && IsShown (hwnd))
				InvalidateUnder (hwnd, WholeRect (hwnd));

			List<Hwnd> doomed = new List<Hwnd> ();
			Collect (hwnd, doomed);
			foreach (Hwnd h in doomed) {
				if (!h.zombie)
					SendMessage (h.Handle, Msg.WM_DESTROY, IntPtr.Zero, IntPtr.Zero);
			}

			TopLevel top;
			if (tops.TryGetValue (hwnd, out top)) {
				CloseTop (top);
				tops.Remove (hwnd);
			}
			foreach (Hwnd h in doomed) {
				IntPtr hh = h.client_window;
				if (focus_window == hh)
					focus_window = IntPtr.Zero;
				if (active_window == hh)
					active_window = IntPtr.Zero;
				if (grab_window == hh)
					grab_window = IntPtr.Zero;
				if (caret.Hwnd == hh)
					DestroyCaret (hh);
				if (mouse_hwnd == h)
					mouse_hwnd = null;
				FQueue queue = QueueOf (h);
				if (queue != null)
					lock (queue) queue.Paint.Remove (h);
				lock (hwnd_queue)
					hwnd_queue.Remove (h);
				h.expose_pending = h.nc_expose_pending = false;
			}
			hwnd.Parent = null;
			foreach (Hwnd h in doomed)
				h.Dispose ();
		}

		static void Collect (Hwnd hwnd, List<Hwnd> list)
		{
			list.Add (hwnd);
			foreach (Hwnd child in hwnd.children.ToArray ())
				Collect (child, list);
		}

		internal override bool SetVisible (IntPtr handle, bool visible, bool activate)
		{
			Hwnd hwnd = Hwnd.ObjectFromHandle (handle);
			if (hwnd == null)
				return true;
			bool was_shown = IsShown (hwnd);
			Rectangle old = hwnd.parent != null ? WholeRect (hwnd) : Rectangle.Empty;
			hwnd.visible = visible;
			Trace ("{0} {1:X}", visible ? "show" : "hide", handle.ToInt64 ());

			if (visible) {
				Map (hwnd);
				if (hwnd.parent == null && !IsPopup (hwnd) && activate)
					Activate (handle);
				SendMessage (handle, Msg.WM_WINDOWPOSCHANGED, IntPtr.Zero, IntPtr.Zero);
			} else {
				TopLevel top;
				if (tops.TryGetValue (hwnd, out top))
					CloseTop (top);
				else if (was_shown)
					InvalidateUnder (hwnd, old);
				if (active_window == handle)
					active_window = IntPtr.Zero;
			}
			return true;
		}

		internal override bool IsVisible (IntPtr handle)
		{
			Hwnd hwnd = Hwnd.ObjectFromHandle (handle);
			return hwnd != null && hwnd.visible;
		}

		internal override bool IsEnabled (IntPtr handle)
		{
			Hwnd hwnd = Hwnd.ObjectFromHandle (handle);
			return hwnd != null && hwnd.Enabled;
		}

		internal override void EnableWindow (IntPtr handle, bool Enable)
		{
			Hwnd hwnd = Hwnd.ObjectFromHandle (handle);
			if (hwnd != null)
				hwnd.Enabled = Enable;
		}

		// Клиентская область по WM_NCCALCSIZE — как у X11, но перерисовка — только
		// если она правда изменилась: GetWindowPos зовут часто.
		void PerformNCCalc (Hwnd hwnd)
		{
			XplatUIWin32.NCCALCSIZE_PARAMS ncp = new XplatUIWin32.NCCALCSIZE_PARAMS ();
			IntPtr ptr = Marshal.AllocHGlobal (Marshal.SizeOf (ncp));
			ncp.rgrc1.left = 0;
			ncp.rgrc1.top = 0;
			ncp.rgrc1.right = hwnd.width;
			ncp.rgrc1.bottom = hwnd.height;
			Marshal.StructureToPtr (ncp, ptr, true);
			NativeWindow.WndProc (hwnd.client_window, Msg.WM_NCCALCSIZE, (IntPtr) 1, ptr);
			ncp = (XplatUIWin32.NCCALCSIZE_PARAMS) Marshal.PtrToStructure (ptr, typeof (XplatUIWin32.NCCALCSIZE_PARAMS));
			Marshal.FreeHGlobal (ptr);

			Rectangle rect = new Rectangle (ncp.rgrc1.left, ncp.rgrc1.top,
				ncp.rgrc1.right - ncp.rgrc1.left, ncp.rgrc1.bottom - ncp.rgrc1.top);
			if (rect == hwnd.ClientRect && hwnd.client_rectangle != Rectangle.Empty)
				return;
			hwnd.ClientRect = rect;
			TopLevel top;
			if (tops.TryGetValue (hwnd, out top))
				ResizeTop (top);
			if (IsShown (hwnd)) {
				AddExpose (hwnd, true, new Rectangle (Point.Empty, rect.Size));
				if (HasBorder (hwnd))
					AddExpose (hwnd, false, new Rectangle (0, 0, hwnd.width, hwnd.height));
			}
		}

		internal override void SetWindowPos (IntPtr handle, int x, int y, int width, int height)
		{
			Hwnd hwnd = Hwnd.ObjectFromHandle (handle);
			if (hwnd == null)
				return;
			if (width < 0)
				width = 0;
			if (height < 0)
				height = 0;
			if (hwnd.x == x && hwnd.y == y && hwnd.width == width && hwnd.height == height)
				return;

			bool was_shown = IsShown (hwnd);
			Rectangle old = WholeRect (hwnd);
			hwnd.x = x;
			hwnd.y = y;
			hwnd.width = width;
			hwnd.height = height;
			hwnd.zero_sized = width < 1 || height < 1;
			if (!hwnd.zero_sized)
				PerformNCCalc (hwnd);

			if (hwnd.parent != null) {
				if (was_shown)
					InvalidateUnder (hwnd, old);
				if (IsShown (hwnd))
					InvalidateTree (hwnd);
			}
			SendMessage (hwnd.client_window, Msg.WM_WINDOWPOSCHANGED, IntPtr.Zero, IntPtr.Zero);
		}

		internal override void GetWindowPos (IntPtr handle, bool is_toplevel, out int x, out int y, out int width, out int height, out int client_width, out int client_height)
		{
			Hwnd hwnd = Hwnd.ObjectFromHandle (handle);
			if (hwnd == null) {
				x = y = width = height = client_width = client_height = 0;
				return;
			}
			x = hwnd.x;
			y = hwnd.y;
			width = hwnd.width;
			height = hwnd.height;
			PerformNCCalc (hwnd);
			client_width = hwnd.ClientRect.Width;
			client_height = hwnd.ClientRect.Height;
		}

		internal override bool CalculateWindowRect (ref Rectangle ClientRect, CreateParams cp, Menu menu, out Rectangle WindowRect)
		{
			WindowRect = Hwnd.GetWindowRectangle (cp, menu, ClientRect);
			return true;
		}

		internal override void RequestNCRecalc (IntPtr handle)
		{
			Hwnd hwnd = Hwnd.ObjectFromHandle (handle);
			if (hwnd == null)
				return;
			PerformNCCalc (hwnd);
			SendMessage (handle, Msg.WM_WINDOWPOSCHANGED, IntPtr.Zero, IntPtr.Zero);
			InvalidateNC (handle);
		}

		internal override IntPtr SetParent (IntPtr handle, IntPtr parent)
		{
			Hwnd hwnd = Hwnd.ObjectFromHandle (handle);
			if (hwnd == null)
				return IntPtr.Zero;
			if (hwnd.parent != null && IsShown (hwnd))
				InvalidateUnder (hwnd, WholeRect (hwnd));
			Hwnd new_parent = parent != IntPtr.Zero ? Hwnd.ObjectFromHandle (parent) : null;
			if (new_parent == null && StyleSet ((int) hwnd.initial_style, WindowStyles.WS_CHILD))
				new_parent = foster;
			hwnd.Parent = new_parent;
			if (new_parent == null && !tops.ContainsKey (hwnd))
				tops [hwnd] = new TopLevel { Hwnd = hwnd };
			if (IsShown (hwnd))
				InvalidateTree (hwnd);
			return IntPtr.Zero;
		}

		internal override IntPtr GetParent (IntPtr handle, bool with_owner)
		{
			Hwnd hwnd = Hwnd.ObjectFromHandle (handle);
			if (hwnd == null)
				return IntPtr.Zero;
			if (hwnd.parent != null && hwnd.parent != foster)
				return hwnd.parent.Handle;
			if (with_owner && hwnd.owner != null)
				return hwnd.owner.Handle;
			return IntPtr.Zero;
		}

		internal override IntPtr GetPreviousWindow (IntPtr handle)
		{
			Hwnd hwnd = Hwnd.ObjectFromHandle (handle);
			if (hwnd == null || hwnd.parent == null)
				return IntPtr.Zero;
			int index = hwnd.parent.children.IndexOf (hwnd);
			return index > 0 ? ((Hwnd) hwnd.parent.children [index - 1]).Handle : IntPtr.Zero;
		}

		// Порядок наложения: список детей родителя, от нижнего к верхнему.
		internal override bool SetZOrder (IntPtr handle, IntPtr after_handle, bool top, bool bottom)
		{
			Hwnd hwnd = Hwnd.ObjectFromHandle (handle);
			if (hwnd == null || hwnd.parent == null)
				return false;
			ArrayList siblings = hwnd.parent.children;
			siblings.Remove (hwnd);
			if (top) {
				siblings.Add (hwnd);
			} else if (bottom) {
				siblings.Insert (0, hwnd);
			} else {
				Hwnd after = after_handle != IntPtr.Zero ? Hwnd.ObjectFromHandle (after_handle) : null;
				int index = after != null ? siblings.IndexOf (after) : -1;
				// «После» у Win32 — ниже: окно встаёт сразу под `after`.
				if (index < 0)
					siblings.Add (hwnd);
				else
					siblings.Insert (index, hwnd);
			}
			if (IsShown (hwnd))
				InvalidateArea (RootOf (hwnd), WholeRect (hwnd));
			return true;
		}

		internal override bool SetTopmost (IntPtr hWnd, bool Enabled)
		{
			return true;
		}

		internal override bool SetOwner (IntPtr hWnd, IntPtr hWndOwner)
		{
			Hwnd hwnd = Hwnd.ObjectFromHandle (hWnd);
			if (hwnd == null)
				return false;
			hwnd.owner = hWndOwner != IntPtr.Zero ? Hwnd.ObjectFromHandle (hWndOwner) : null;
			return true;
		}

		internal override FormWindowState GetWindowState (IntPtr handle)
		{
			return FormWindowState.Normal;
		}

		internal override void SetWindowState (IntPtr handle, FormWindowState state)
		{
			// Свернуть и развернуть окно умеет только стол, и программе он об
			// этом не сообщает.
		}

		internal override void SetWindowMinMax (IntPtr handle, Rectangle maximized, Size min, Size max)
		{
		}

		internal override void SetWindowStyle (IntPtr handle, CreateParams cp)
		{
			Hwnd hwnd = Hwnd.ObjectFromHandle (handle);
			if (hwnd != null)
				SetHwndStyles (hwnd, cp);
		}

		internal override double GetWindowTransparency (IntPtr handle)
		{
			return 1.0;
		}

		internal override void SetWindowTransparency (IntPtr handle, double transparency, Color key)
		{
		}

		internal override TransparencySupport SupportsTransparency ()
		{
			return TransparencySupport.None;
		}

		internal override void SetBorderStyle (IntPtr handle, FormBorderStyle border_style)
		{
			RequestNCRecalc (handle);
		}

		internal override void SetMenu (IntPtr handle, Menu menu)
		{
			Hwnd hwnd = Hwnd.ObjectFromHandle (handle);
			if (hwnd != null)
				hwnd.menu = menu;
			RequestNCRecalc (handle);
		}

		internal override bool GetText (IntPtr handle, out string text)
		{
			Hwnd hwnd = Hwnd.ObjectFromHandle (handle);
			TopLevel top;
			if (hwnd != null && tops.TryGetValue (hwnd, out top)) {
				text = top.Title;
				return true;
			}
			text = "";
			return false;
		}

		internal override bool Text (IntPtr handle, string text)
		{
			Hwnd hwnd = Hwnd.ObjectFromHandle (handle);
			TopLevel top;
			if (hwnd != null && tops.TryGetValue (hwnd, out top))
				top.Title = text ?? "";
			return true;
		}

		internal override void SetIcon (IntPtr handle, Icon icon)
		{
		}

		internal override Region GetClipRegion (IntPtr hwnd)
		{
			Hwnd h = Hwnd.ObjectFromHandle (hwnd);
			return h != null ? h.UserClip : null;
		}

		internal override void SetClipRegion (IntPtr hwnd, Region region)
		{
			Hwnd h = Hwnd.ObjectFromHandle (hwnd);
			if (h == null)
				return;
			h.UserClip = region;
			AddExpose (h, true, new Rectangle (Point.Empty, h.ClientRect.Size));
		}

		// Стили рамки и заголовка — как у X11 (DeriveStyles): по ним драйвер
		// рисует рамку дочернего окна.
		static void SetHwndStyles (Hwnd hwnd, CreateParams cp)
		{
			int style = cp.Style;
			int ex = cp.ExStyle;
			hwnd.border_static = false;
			hwnd.title_style = TitleStyle.None;
			if (StyleSet (style, WindowStyles.WS_CHILD)) {
				if (ExStyleSet (ex, WindowExStyles.WS_EX_CLIENTEDGE)) {
					hwnd.border_style = FormBorderStyle.Fixed3D;
				} else if (ExStyleSet (ex, WindowExStyles.WS_EX_STATICEDGE)) {
					hwnd.border_style = FormBorderStyle.Fixed3D;
					hwnd.border_static = true;
				} else if (StyleSet (style, WindowStyles.WS_BORDER)) {
					hwnd.border_style = FormBorderStyle.FixedSingle;
				} else {
					hwnd.border_style = FormBorderStyle.None;
				}
			} else {
				hwnd.border_style = FormBorderStyle.None;
			}
		}

		#endregion

		#region Фокус, активность, захват мыши

		internal override void Activate (IntPtr handle)
		{
			Hwnd hwnd = Hwnd.ObjectFromHandle (handle);
			if (hwnd == null)
				return;
			IntPtr root = RootOf (hwnd).Handle;
			if (active_window == root)
				return;
			IntPtr previous = active_window;
			active_window = root;
			Trace ("activate {0:X}", root.ToInt64 ());
			if (previous != IntPtr.Zero && Hwnd.ObjectFromHandle (previous) != null)
				SendMessage (previous, Msg.WM_ACTIVATE, (IntPtr) WindowActiveFlags.WA_INACTIVE, root);
			SendMessage (root, Msg.WM_ACTIVATE, (IntPtr) WindowActiveFlags.WA_ACTIVE, previous);
		}

		internal override IntPtr GetActive ()
		{
			return active_window;
		}

		internal override void SetForegroundWindow (IntPtr handle)
		{
			Activate (handle);
		}

		internal override IntPtr GetFocus ()
		{
			return focus_window;
		}

		internal override void SetFocus (IntPtr handle)
		{
			Hwnd hwnd = Hwnd.ObjectFromHandle (handle);
			if (hwnd == null || hwnd.client_window == focus_window || !hwnd.enabled)
				return;
			IntPtr previous = focus_window;
			focus_window = hwnd.client_window;
			Trace ("focus {0:X}", focus_window.ToInt64 ());
			if (previous != IntPtr.Zero && Hwnd.ObjectFromHandle (previous) != null)
				SendMessage (previous, Msg.WM_KILLFOCUS, focus_window, IntPtr.Zero);
			SendMessage (focus_window, Msg.WM_SETFOCUS, previous, IntPtr.Zero);
		}

		internal override void SetModal (IntPtr handle, bool Modal)
		{
			if (Modal) {
				modal_windows.Push (handle);
			} else {
				if (modal_windows.Count > 0 && modal_windows.Peek () == handle)
					modal_windows.Pop ();
				if (modal_windows.Count > 0)
					Activate (modal_windows.Peek ());
			}
		}

		internal override void GrabWindow (IntPtr hwnd, IntPtr ConfineToHwnd)
		{
			grab_window = hwnd;
		}

		internal override void GrabInfo (out IntPtr hwnd, out bool GrabConfined, out Rectangle GrabArea)
		{
			hwnd = grab_window;
			GrabConfined = false;
			GrabArea = Rectangle.Empty;
		}

		internal override void UngrabWindow (IntPtr hwnd)
		{
			grab_window = IntPtr.Zero;
		}

		#endregion

		#region Очередь сообщений

		FQueue ThreadQueue (Thread thread)
		{
			lock (queues) {
				FQueue queue;
				if (!queues.TryGetValue (thread, out queue)) {
					queue = new FQueue (thread);
					queues [thread] = queue;
					// Таймеры, заведённые до первой очереди потока.
					lock (unattached_timers) {
						foreach (Timer timer in unattached_timers.ToArray ()) {
							if (timer.thread == thread) {
								queue.Timers.Add (timer);
								unattached_timers.Remove (timer);
							}
						}
					}
				}
				return queue;
			}
		}

		FQueue QueueOf (Hwnd hwnd)
		{
			lock (hwnd_queue) {
				FQueue queue;
				return hwnd != null && hwnd_queue.TryGetValue (hwnd, out queue) ? queue : null;
			}
		}

		FQueue QueueFor (object queue_id)
		{
			return queue_id as FQueue ?? ThreadQueue (Thread.CurrentThread);
		}

		void Post (FQueue queue, MSG msg)
		{
			lock (queue)
				queue.Posted.AddLast (msg);
		}

		internal override object StartLoop (Thread thread)
		{
			return ThreadQueue (thread);
		}

		internal override void EndLoop (Thread thread)
		{
		}

		internal override bool PostMessage (IntPtr handle, Msg message, IntPtr wparam, IntPtr lparam)
		{
			Hwnd hwnd = Hwnd.ObjectFromHandle (handle);
			FQueue queue = QueueOf (hwnd) ?? ThreadQueue (Thread.CurrentThread);
			MSG msg = new MSG ();
			msg.hwnd = handle;
			msg.message = message;
			msg.wParam = wparam;
			msg.lParam = lparam;
			Post (queue, msg);
			return true;
		}

		internal override void PostQuitMessage (int exitCode)
		{
			FQueue queue = ThreadQueue (Thread.CurrentThread);
			lock (queue) {
				queue.Quit = true;
				queue.ExitCode = exitCode;
			}
		}

		internal override IntPtr SendMessage (IntPtr hwnd, Msg message, IntPtr wParam, IntPtr lParam)
		{
			Hwnd h = Hwnd.ObjectFromHandle (hwnd);
			FQueue queue = QueueOf (h);
			if (queue != null && queue.Thread != Thread.CurrentThread) {
				// Чужой поток: вызов уходит в очередь потока окна, как у X11.
				AsyncMethodData data = new AsyncMethodData ();
				data.Handle = hwnd;
				data.Method = new WndProcDelegate (NativeWindow.WndProc);
				data.Args = new object[] { hwnd, message, wParam, lParam };
				data.Result = new AsyncMethodResult ();
				SendAsyncMethod (data);
				return IntPtr.Zero;
			}
			return NativeWindow.WndProc (hwnd, message, wParam, lParam);
		}

		delegate IntPtr WndProcDelegate (IntPtr hwnd, Msg message, IntPtr wParam, IntPtr lParam);

		internal override void SendAsyncMethod (AsyncMethodData method)
		{
			Hwnd hwnd = Hwnd.ObjectFromHandle (method.Handle);
			FQueue queue = QueueOf (hwnd);
			if (queue == null) {
				lock (queues) {
					foreach (FQueue q in queues.Values) {
						queue = q;
						break;
					}
				}
			}
			if (queue == null)
				queue = ThreadQueue (Thread.CurrentThread);
			lock (queue)
				queue.Posted.AddLast (method);
		}

		internal override int SendInput (IntPtr handle, Queue keys)
		{
			Hwnd hwnd = Hwnd.ObjectFromHandle (handle);
			FQueue queue = QueueOf (hwnd) ?? ThreadQueue (Thread.CurrentThread);
			int count = keys.Count;
			while (keys.Count > 0) {
				MSG msg = (MSG) keys.Dequeue ();
				msg.hwnd = handle;
				Post (queue, msg);
			}
			return count;
		}

		internal override bool TranslateMessage (ref MSG msg)
		{
			KeyInfo info = msg.refobject as KeyInfo;
			if (info == null || info.Char == '\0')
				return false;
			if (msg.message != Msg.WM_KEYDOWN && msg.message != Msg.WM_SYSKEYDOWN)
				return false;
			// WM_CHAR — сразу за WM_KEYDOWN, впереди всего остального: если
			// клавишу съела форма (Tab, Enter у кнопки по умолчанию),
			// TranslateMessage не зовут, и символа не будет, как у Win32.
			MSG ch = new MSG ();
			ch.hwnd = msg.hwnd;
			ch.message = msg.message == Msg.WM_SYSKEYDOWN ? Msg.WM_SYSCHAR : Msg.WM_CHAR;
			ch.wParam = (IntPtr) info.Char;
			ch.lParam = msg.lParam;
			ch.refobject = new KeyInfo { Modifiers = info.Modifiers };
			FQueue queue = QueueOf (Hwnd.ObjectFromHandle (msg.hwnd)) ?? ThreadQueue (Thread.CurrentThread);
			lock (queue)
				queue.Posted.AddFirst (ch);
			return true;
		}

		internal override IntPtr DispatchMessage (ref MSG msg)
		{
			return NativeWindow.WndProc (msg.hwnd, msg.message, msg.wParam, msg.lParam);
		}

		internal override void DoEvents ()
		{
			MSG msg = new MSG ();
			in_doevents = true;
			while (PeekMessage (null, ref msg, IntPtr.Zero, 0, 0, (uint) PeekMessageFlags.PM_REMOVE)) {
				TranslateMessage (ref msg);
				DispatchMessage (ref msg);
			}
			in_doevents = false;
		}

		internal override bool PeekMessage (object queue_id, ref MSG msg, IntPtr hWnd, int wFilterMin, int wFilterMax, uint flags)
		{
			FQueue queue = QueueFor (queue_id);
			PollEvents (queue);
			CheckTimers (queue, Timer.StopWatchNowMilliseconds);
			bool pending;
			lock (queue)
				pending = queue.Posted.Count > 0 || queue.Paint.Count > 0 || queue.Quit;
			if (!pending) {
				Flush (queue);
				return false;
			}
			return NextMessage (queue, ref msg, false);
		}

		internal override bool GetMessage (object queue_id, ref MSG msg, IntPtr hWnd, int wFilterMin, int wFilterMax)
		{
			return NextMessage (QueueFor (queue_id), ref msg, true);
		}

		// Следующее сообщение: отложенные и асинхронные вызовы, события окон,
		// таймеры, перерисовка. Пусто — изменившееся уходит на стол, раз за
		// опустение поднимается Application.Idle, и поток спит до ближайшего
		// таймера, но не дольше 10 мс: события стол не будит.
		bool NextMessage (FQueue queue, ref MSG msg, bool wait)
		{
			for (;;) {
				object item = null;
				lock (queue) {
					if (queue.Posted.Count > 0) {
						item = queue.Posted.First.Value;
						queue.Posted.RemoveFirst ();
					}
				}
				if (item is AsyncMethodData) {
					XplatUIDriverSupport.ExecuteClientMessage (GCHandle.Alloc (item));
					continue;
				}
				if (item != null) {
					msg = (MSG) item;
					if (msg.hwnd != IntPtr.Zero) {
						Hwnd target = Hwnd.ObjectFromHandle (msg.hwnd);
						if (target == null || target.zombie)
							continue;
					}
					Delivered (queue, ref msg);
					return true;
				}

				if (PollEvents (queue))
					continue;

				bool quit;
				int exit_code;
				lock (queue) {
					quit = queue.Quit;
					exit_code = queue.ExitCode;
					if (quit)
						queue.Quit = false;
				}
				if (quit) {
					msg.hwnd = IntPtr.Zero;
					msg.message = Msg.WM_QUIT;
					msg.wParam = (IntPtr) exit_code;
					msg.lParam = IntPtr.Zero;
					return false;
				}

				CheckTimers (queue, Timer.StopWatchNowMilliseconds);
				lock (queue) {
					if (queue.Posted.Count > 0)
						continue;
				}

				Hwnd paint = null;
				lock (queue) {
					if (queue.Paint.Count > 0) {
						paint = queue.Paint [0];
						queue.Paint.RemoveAt (0);
					}
				}
				if (paint != null) {
					if (PaintMessage (queue, paint, ref msg))
						return true;
					continue;
				}

				Flush (queue);
				if (!queue.IdleRaised) {
					queue.IdleRaised = true;
					RaiseIdle (EventArgs.Empty);
					continue;
				}
				if (!wait)
					return false;
				int timeout = NextTimeout (queue, Timer.StopWatchNowMilliseconds);
				Thread.Sleep (Math.Max (1, Math.Min (10, timeout)));
			}
		}

		void Delivered (FQueue queue, ref MSG msg)
		{
			queue.IdleRaised = false;
			KeyInfo info = msg.refobject as KeyInfo;
			switch (msg.message) {
			case Msg.WM_KEYDOWN:
			case Msg.WM_SYSKEYDOWN:
			case Msg.WM_KEYUP:
			case Msg.WM_SYSKEYUP:
			case Msg.WM_CHAR:
			case Msg.WM_SYSCHAR:
				key_modifiers = info != null ? info.Modifiers : Keys.None;
				break;
			case Msg.WM_LBUTTONDOWN:
			case Msg.WM_LBUTTONDBLCLK:
				mouse_state = MouseButtons.Left;
				key_modifiers = Keys.None;
				break;
			case Msg.WM_RBUTTONDOWN:
			case Msg.WM_RBUTTONDBLCLK:
				mouse_state = MouseButtons.Right;
				key_modifiers = Keys.None;
				break;
			case Msg.WM_LBUTTONUP:
			case Msg.WM_RBUTTONUP:
				mouse_state = MouseButtons.None;
				key_modifiers = Keys.None;
				break;
			default:
				key_modifiers = Keys.None;
				break;
			}
		}

		bool PaintMessage (FQueue queue, Hwnd hwnd, ref MSG msg)
		{
			if (hwnd.zombie)
				return false;
			if (!IsShown (hwnd)) {
				hwnd.expose_pending = false;
				hwnd.nc_expose_pending = false;
				hwnd.ClearInvalidArea ();
				hwnd.ClearNcInvalidArea ();
				return false;
			}
			queue.IdleRaised = false;
			msg.hwnd = hwnd.Handle;
			msg.wParam = IntPtr.Zero;
			msg.lParam = IntPtr.Zero;
			msg.refobject = null;
			if (hwnd.nc_expose_pending) {
				DrawBorder (hwnd);
				if (hwnd.expose_pending)
					lock (queue) queue.Paint.Add (hwnd);
				msg.message = Msg.WM_NCPAINT;
				msg.wParam = (IntPtr) 1;
				return true;
			}
			if (!hwnd.expose_pending)
				return false;
			msg.message = Msg.WM_PAINT;
			return true;
		}

		#endregion

		#region События окон FreeOS

		// Забрать события всех окон этого потока. `true` — что-то пришло.
		bool PollEvents (FQueue queue)
		{
			bool any = false;
			foreach (TopLevel top in new List<TopLevel> (tops.Values)) {
				if (!top.IsOpen || QueueOf (top.Hwnd) != queue)
					continue;
				Native.WinEvent ev;
				while (top.IsOpen && Native.freeos_window_event (top.Id, out ev) == 1) {
					any = true;
					Translate (queue, top, ev);
				}
			}
			return any;
		}

		void Translate (FQueue queue, TopLevel top, Native.WinEvent ev)
		{
			switch (ev.Kind) {
			case WIN_KEY:
				if (active_window != top.Hwnd.Handle)
					Activate (top.Hwnd.Handle);
				KeyEvent (queue, top, ev);
				break;
			case WIN_POINTER:
				if (active_window != top.Hwnd.Handle)
					Activate (top.Hwnd.Handle);
				Click (queue, top, ev);
				break;
			case WIN_MOVE:
				Move (queue, top, new Point (ev.X, ev.Y));
				break;
			case WIN_LEAVE:
				if (mouse_hwnd != null && !mouse_hwnd.zombie)
					Post (queue, MakeMsg (mouse_hwnd.Handle, Msg.WM_MOUSELEAVE, IntPtr.Zero, IntPtr.Zero));
				mouse_hwnd = null;
				break;
			case WIN_CLOSE:
				Trace ("close asked for window {0}", top.Id);
				Post (queue, MakeMsg (top.Hwnd.Handle, Msg.WM_CLOSE, IntPtr.Zero, IntPtr.Zero));
				break;
			}
		}

		static MSG MakeMsg (IntPtr hwnd, Msg message, IntPtr wparam, IntPtr lparam)
		{
			MSG msg = new MSG ();
			msg.hwnd = hwnd;
			msg.message = message;
			msg.wParam = wparam;
			msg.lParam = lparam;
			return msg;
		}

		static IntPtr MakeLParam (int x, int y)
		{
			return (IntPtr) ((y << 16) | (x & 0xffff));
		}

		// Окно под указателем (или захватившее мышь) и точка в его клиентских
		// координатах. Выключенное окно отдаёт мышь ближайшему включённому
		// предку — как у X11.
		Hwnd PointerTarget (TopLevel top, Point surface, out Point client)
		{
			Hwnd target = null;
			if (grab_window != IntPtr.Zero) {
				target = Hwnd.ObjectFromHandle (grab_window);
				if (target != null && TopOf (target) != top)
					target = null;
			}
			if (target == null) {
				target = HitTest (top.Hwnd, surface);
				if (!target.Enabled) {
					Hwnd enabled = Hwnd.ObjectFromHandle (target.EnabledHwnd);
					if (enabled != null)
						target = enabled;
				}
			}
			Point o = ClientOrigin (target);
			client = new Point (surface.X - o.X, surface.Y - o.Y);
			Point screen = ScreenOrigin (top.Hwnd);
			mouse_position = new Point (screen.X + surface.X, screen.Y + surface.Y);
			return target;
		}

		void EnterLeave (FQueue queue, Hwnd target)
		{
			if (mouse_hwnd == target)
				return;
			if (mouse_hwnd != null && !mouse_hwnd.zombie)
				Post (queue, MakeMsg (mouse_hwnd.Handle, Msg.WM_MOUSELEAVE, IntPtr.Zero, IntPtr.Zero));
			mouse_hwnd = target;
			Post (queue, MakeMsg (target.Handle, Msg.WM_MOUSE_ENTER, IntPtr.Zero, IntPtr.Zero));
		}

		void Move (FQueue queue, TopLevel top, Point surface)
		{
			Point client;
			Hwnd target = PointerTarget (top, surface, out client);
			EnterLeave (queue, target);
			// Кнопки при движении не сообщаются: нажатие и отпускание уже
			// отданы щелчком (см. шапку), и нажатая кнопка при движении начала
			// бы выделение, которого человек не делал.
			MSG msg = MakeMsg (target.Handle, Msg.WM_MOUSEMOVE, IntPtr.Zero, MakeLParam (client.X, client.Y));
			lock (queue) {
				LinkedListNode<object> last = queue.Posted.Last;
				if (last != null && last.Value is MSG && ((MSG) last.Value).message == Msg.WM_MOUSEMOVE
					&& ((MSG) last.Value).hwnd == msg.hwnd) {
					last.Value = msg;
					return;
				}
				queue.Posted.AddLast (msg);
			}
		}

		void Click (FQueue queue, TopLevel top, Native.WinEvent ev)
		{
			Point client;
			Point surface = new Point (ev.X, ev.Y);
			Hwnd target = PointerTarget (top, surface, out client);
			EnterLeave (queue, target);
			bool right = ev.Code == 2;
			Msg down = right ? Msg.WM_RBUTTONDOWN : Msg.WM_LBUTTONDOWN;
			Msg up = right ? Msg.WM_RBUTTONUP : Msg.WM_LBUTTONUP;
			IntPtr button = (IntPtr) (right ? 2 : 1);   // MK_RBUTTON : MK_LBUTTON

			long now = Timer.StopWatchNowMilliseconds;
			Size slack = DoubleClickSize;
			if (last_click_hwnd == target.Handle && last_click_message == down
				&& now - last_click_time <= DoubleClickTime
				&& Math.Abs (surface.X - last_click_point.X) <= slack.Width / 2
				&& Math.Abs (surface.Y - last_click_point.Y) <= slack.Height / 2) {
				down = right ? Msg.WM_RBUTTONDBLCLK : Msg.WM_LBUTTONDBLCLK;
				last_click_hwnd = IntPtr.Zero;
			} else {
				last_click_hwnd = target.Handle;
				last_click_message = down;
				last_click_time = now;
				last_click_point = surface;
			}

			IntPtr lparam = MakeLParam (client.X, client.Y);
			Trace ("click {0} at {1},{2} -> {3:X} at {4},{5}", right ? "right" : "left", surface.X, surface.Y,
				target.Handle.ToInt64 (), client.X, client.Y);
			Post (queue, MakeMsg (target.Handle, Msg.WM_MOUSEMOVE, IntPtr.Zero, lparam));
			Post (queue, MakeMsg (target.Handle, down, button, lparam));
			Post (queue, MakeMsg (target.Handle, up, IntPtr.Zero, lparam));
		}

		void KeyEvent (FQueue queue, TopLevel top, Native.WinEvent ev)
		{
			Keys modifiers = Keys.None;
			if ((ev.X & WIN_MOD_SHIFT) != 0)
				modifiers |= Keys.Shift;
			if ((ev.X & WIN_MOD_CTRL) != 0)
				modifiers |= Keys.Control;
			if ((ev.X & WIN_MOD_ALT) != 0)
				modifiers |= Keys.Alt;

			int vk = VirtualKey (ev.Code, ev.Y);
			char ch = '\0';
			if (ev.Code != 0 && ev.Code < 0x10000 && ev.Code < WIN_KEY_NAMED)
				ch = (char) ev.Code;

			IntPtr target = focus_window;
			Hwnd focused = Hwnd.ObjectFromHandle (target);
			if (focused == null || TopOf (focused) != top)
				target = top.Hwnd.Handle;

			bool alt = (modifiers & Keys.Alt) != 0;
			IntPtr down_lparam = (IntPtr) (1 | (alt ? 1 << 29 : 0));
			IntPtr up_lparam = (IntPtr) (unchecked ((int) 0xC0000001) | (alt ? 1 << 29 : 0));

			MSG down = MakeMsg (target, alt ? Msg.WM_SYSKEYDOWN : Msg.WM_KEYDOWN, (IntPtr) vk, down_lparam);
			down.refobject = new KeyInfo { Char = ch, Modifiers = modifiers };
			MSG up = MakeMsg (target, alt ? Msg.WM_SYSKEYUP : Msg.WM_KEYUP, (IntPtr) vk, up_lparam);
			up.refobject = new KeyInfo { Modifiers = modifiers };
			Trace ("key 0x{0:X} '{1}' vk 0x{2:X} mods {3} -> {4:X}", ev.Code, ch == '\0' ? ' ' : ch, vk, modifiers, target.ToInt64 ());
			Post (queue, down);
			Post (queue, up);
		}

		// Код клавиши Win32 по событию стола: имя клавиши, управляющий символ или
		// буква раскладки US (`y`), — не по символу, иначе `Ctrl+O` в русской
		// раскладке был бы `Ctrl+Щ`.
		static int VirtualKey (uint code, int us)
		{
			if (code >= WIN_KEY_NAMED) {
				switch (code - WIN_KEY_NAMED) {
				case 1: return (int) VirtualKeys.VK_LEFT;
				case 2: return (int) VirtualKeys.VK_RIGHT;
				case 3: return (int) VirtualKeys.VK_UP;
				case 4: return (int) VirtualKeys.VK_DOWN;
				case 5: return (int) VirtualKeys.VK_HOME;
				case 6: return (int) VirtualKeys.VK_END;
				case 7: return (int) VirtualKeys.VK_PRIOR;
				case 8: return (int) VirtualKeys.VK_NEXT;
				case 9: return (int) VirtualKeys.VK_DELETE;
				case 10: return (int) VirtualKeys.VK_APPS;
				}
				return 0;
			}
			switch (code) {
			case 0x08: return (int) VirtualKeys.VK_BACK;
			case 0x09: return (int) VirtualKeys.VK_TAB;
			case 0x0A:
			case 0x0D: return (int) VirtualKeys.VK_RETURN;
			case 0x1B: return (int) VirtualKeys.VK_ESCAPE;
			case 0x7F: return (int) VirtualKeys.VK_DELETE;
			}
			int c = us != 0 ? us : (code < 0x80 ? (int) code : 0);
			if (c >= 'a' && c <= 'z')
				return c - 'a' + 'A';
			if (c >= 'A' && c <= 'Z')
				return c;
			if (c >= '0' && c <= '9')
				return c;
			switch (c) {
			case ' ': return (int) VirtualKeys.VK_SPACE;
			case ';': return 0xBA;
			case '=': return 0xBB;
			case ',': return 0xBC;
			case '-': return 0xBD;
			case '.': return 0xBE;
			case '/': return 0xBF;
			case '`': return 0xC0;
			case '[': return 0xDB;
			case '\\': return 0xDC;
			case ']': return 0xDD;
			case '\'': return 0xDE;
			}
			return 0;
		}

		#endregion

		#region Таймеры

		internal override void SetTimer (Timer timer)
		{
			FQueue queue;
			lock (queues)
				queues.TryGetValue (timer.thread, out queue);
			if (queue == null) {
				lock (unattached_timers) {
					if (!unattached_timers.Contains (timer))
						unattached_timers.Add (timer);
				}
				return;
			}
			lock (queue) {
				if (!queue.Timers.Contains (timer))
					queue.Timers.Add (timer);
			}
		}

		internal override void KillTimer (Timer timer)
		{
			FQueue queue;
			lock (queues)
				queues.TryGetValue (timer.thread, out queue);
			if (queue == null) {
				lock (unattached_timers)
					unattached_timers.Remove (timer);
				return;
			}
			lock (queue)
				queue.Timers.Remove (timer);
		}

		// Как у X11: таймер тикает до OnLoad главной формы только внутри
		// DoEvents.
		void CheckTimers (FQueue queue, long now)
		{
			Timer[] timers;
			lock (queue)
				timers = (Timer[]) queue.Timers.ToArray (typeof (Timer));
			foreach (Timer timer in timers) {
				if (timer.Enabled && timer.Expires <= now && !timer.Busy) {
					if (in_doevents ||
					    (Application.MWFThread.Current.Context != null &&
					     (Application.MWFThread.Current.Context.MainForm == null ||
					      Application.MWFThread.Current.Context.MainForm.IsLoaded))) {
						timer.Busy = true;
						timer.Update (now);
						timer.FireTick ();
						timer.Busy = false;
					}
				}
			}
		}

		static int NextTimeout (FQueue queue, long now)
		{
			int timeout = int.MaxValue;
			lock (queue) {
				foreach (Timer timer in queue.Timers) {
					if (!timer.Enabled)
						continue;
					long left = timer.Expires - now;
					timeout = (int) Math.Max (0, Math.Min (timeout, left));
				}
			}
			return timeout;
		}

		internal override void RaiseIdle (EventArgs e)
		{
			if (Idle != null)
				Idle (this, e);
		}

		#endregion

		#region Поведение по умолчанию

		internal override IntPtr DefWndProc (ref Message msg)
		{
			switch ((Msg) msg.Msg) {
			case Msg.WM_PAINT: {
				Hwnd hwnd = Hwnd.GetObjectFromWindow (msg.HWnd);
				if (hwnd != null)
					hwnd.expose_pending = false;
				return IntPtr.Zero;
			}
			case Msg.WM_NCPAINT: {
				Hwnd hwnd = Hwnd.GetObjectFromWindow (msg.HWnd);
				if (hwnd != null)
					hwnd.nc_expose_pending = false;
				return IntPtr.Zero;
			}
			case Msg.WM_NCCALCSIZE: {
				// Рамка и заголовок по стилям окна, как у X11. У окна верхнего
				// уровня их рисует стол, и поверхность — ровно то, что осталось.
				if (msg.WParam == (IntPtr) 1) {
					Hwnd hwnd = Hwnd.GetObjectFromWindow (msg.HWnd);
					Control ctrl = hwnd != null ? Control.FromHandle (hwnd.Handle) : null;
					if (ctrl != null) {
						XplatUIWin32.NCCALCSIZE_PARAMS ncp = (XplatUIWin32.NCCALCSIZE_PARAMS) Marshal.PtrToStructure (msg.LParam, typeof (XplatUIWin32.NCCALCSIZE_PARAMS));
						Hwnd.Borders rect = Hwnd.GetBorders (ctrl.GetCreateParams (), null);
						ncp.rgrc1.top += rect.top;
						ncp.rgrc1.bottom -= rect.bottom;
						ncp.rgrc1.left += rect.left;
						ncp.rgrc1.right -= rect.right;
						Marshal.StructureToPtr (ncp, msg.LParam, true);
					}
				}
				return IntPtr.Zero;
			}
			case Msg.WM_CONTEXTMENU:
			case Msg.WM_MOUSEWHEEL: {
				Hwnd hwnd = Hwnd.GetObjectFromWindow (msg.HWnd);
				if (hwnd != null && hwnd.parent != null && hwnd.parent != foster)
					SendMessage (hwnd.parent.client_window, (Msg) msg.Msg, msg.WParam, msg.LParam);
				return IntPtr.Zero;
			}
			case Msg.WM_SETCURSOR:
				// Указатель у стола один на всех, и программе его не сменить.
				return (IntPtr) 1;
			}
			return IntPtr.Zero;
		}

		internal override void HandleException (Exception e)
		{
			Console.Error.WriteLine ("mwf-freeos: {0}", e);
		}

		#endregion

		#region Мышь, клавиатура, экран

		internal override Keys ModifierKeys {
			get { return key_modifiers; }
		}

		internal override MouseButtons MouseButtons {
			get { return mouse_state; }
		}

		internal override Point MousePosition {
			get { return mouse_position; }
		}

		internal override void GetCursorPos (IntPtr handle, out int x, out int y)
		{
			x = mouse_position.X;
			y = mouse_position.Y;
			if (handle != IntPtr.Zero)
				ScreenToClient (handle, ref x, ref y);
		}

		internal override void SetCursorPos (IntPtr handle, int x, int y)
		{
			// Указатель двигает человек, а не программа.
		}

		internal override void ClientToScreen (IntPtr handle, ref int x, ref int y)
		{
			Hwnd hwnd = Hwnd.ObjectFromHandle (handle);
			if (hwnd == null)
				return;
			Point o = ScreenOrigin (hwnd);
			x += o.X;
			y += o.Y;
		}

		internal override void ScreenToClient (IntPtr handle, ref int x, ref int y)
		{
			Hwnd hwnd = Hwnd.ObjectFromHandle (handle);
			if (hwnd == null)
				return;
			Point o = ScreenOrigin (hwnd);
			x -= o.X;
			y -= o.Y;
		}

		internal override Point GetMenuOrigin (IntPtr handle)
		{
			Hwnd hwnd = Hwnd.ObjectFromHandle (handle);
			return hwnd != null ? hwnd.MenuOrigin : Point.Empty;
		}

		internal override void MenuToScreen (IntPtr handle, ref int x, ref int y)
		{
			Hwnd hwnd = Hwnd.ObjectFromHandle (handle);
			if (hwnd == null)
				return;
			Point o = ScreenOrigin (hwnd);
			Rectangle client = hwnd.ClientRect;
			x += o.X - client.X;
			y += o.Y - client.Y;
		}

		internal override void ScreenToMenu (IntPtr handle, ref int x, ref int y)
		{
			Hwnd hwnd = Hwnd.ObjectFromHandle (handle);
			if (hwnd == null)
				return;
			Point o = ScreenOrigin (hwnd);
			Rectangle client = hwnd.ClientRect;
			x -= o.X - client.X;
			y -= o.Y - client.Y;
		}

		internal override void GetDisplaySize (out Size size)
		{
			size = new Size (screen_width, screen_height);
		}

		internal override Rectangle VirtualScreen {
			get { return new Rectangle (0, 0, screen_width, screen_height); }
		}

		internal override Rectangle WorkingArea {
			get { return new Rectangle (0, 0, screen_width, screen_height); }
		}

		internal override Screen[] AllScreens {
			get { return new Screen[] { new Screen (true, "FreeOS", VirtualScreen, WorkingArea) }; }
		}

		internal override int CaptionHeight { get { return 19; } }
		internal override Size CursorSize { get { return new Size (32, 32); } }
		internal override bool DragFullWindows { get { return true; } }
		internal override Size DragSize { get { return new Size (4, 4); } }
		internal override Size FrameBorderSize { get { return new Size (4, 4); } }
		internal override Size IconSize { get { return new Size (32, 32); } }
		internal override Size MaxWindowTrackSize { get { return new Size (screen_width, screen_height); } }
		internal override bool MenuAccessKeysUnderlined { get { return false; } }
		internal override Size MinimizedWindowSpacingSize { get { return new Size (160, 24); } }
		internal override Size MinimumWindowSize { get { return new Size (112, 27); } }
		internal override Size SmallIconSize { get { return new Size (16, 16); } }
		internal override int MouseButtonCount { get { return 2; } }
		internal override bool MouseButtonsSwapped { get { return false; } }
		internal override bool MouseWheelPresent { get { return false; } }
		internal override bool ThemesEnabled { get { return themes_enabled; } }
		internal override int KeyboardSpeed { get { return 31; } }
		internal override int KeyboardDelay { get { return 1; } }

		internal override void EnableThemes ()
		{
			themes_enabled = true;
		}

		internal override void AudibleAlert (AlertType alert)
		{
		}

		internal override void BeginMoveResize (IntPtr handle)
		{
		}

		internal override void ResetMouseHover (IntPtr hwnd)
		{
		}

		internal override void RequestAdditionalWM_NCMessages (IntPtr hwnd, bool hover, bool leave)
		{
		}

		internal override bool GetFontMetrics (Graphics g, Font font, out int ascent, out int descent)
		{
			FontFamily family = font.FontFamily;
			float line = family.GetLineSpacing (font.Style);
			float height = font.GetHeight (g);
			ascent = (int) Math.Ceiling (family.GetCellAscent (font.Style) * height / line);
			descent = (int) Math.Ceiling (family.GetCellDescent (font.Style) * height / line);
			return true;
		}

		internal override SizeF GetAutoScaleSize (Font font)
		{
			// Та же «волшебная» строка и то же число, что у X11 и Win32.
			const string magic_string = "The quick brown fox jumped over the lazy dog.";
			const double magic_number = 44.549996948242189;
			using (Graphics g = ScratchGraphics ()) {
				float width = (float) (g.MeasureString (magic_string, font).Width / magic_number);
				return new SizeF (width, font.Height);
			}
		}

		#endregion

		#region Указатели, трей, буфер обмена по-старому, обратимое рисование

		internal override void SetCursor (IntPtr hwnd, IntPtr cursor)
		{
		}

		internal override void ShowCursor (bool show)
		{
		}

		internal override void OverrideCursor (IntPtr cursor)
		{
		}

		static int next_cursor = 0x100;

		internal override IntPtr DefineCursor (Bitmap bitmap, Bitmap mask, Color cursor_pixel, Color mask_pixel, int xHotSpot, int yHotSpot)
		{
			return (IntPtr) Interlocked.Increment (ref next_cursor);
		}

		internal override IntPtr DefineStdCursor (StdCursor id)
		{
			return (IntPtr) ((int) id + 1);
		}

		internal override Bitmap DefineStdCursorBitmap (StdCursor id)
		{
			return new Bitmap (16, 16, PixelFormat.Format32bppArgb);
		}

		internal override void DestroyCursor (IntPtr cursor)
		{
		}

		internal override void GetCursorInfo (IntPtr cursor, out int width, out int height, out int hotspot_x, out int hotspot_y)
		{
			width = 16;
			height = 16;
			hotspot_x = 0;
			hotspot_y = 0;
		}

		internal override bool SystrayAdd (IntPtr hwnd, string tip, Icon icon, out ToolTip tt)
		{
			tt = null;
			return false;
		}

		internal override bool SystrayChange (IntPtr hwnd, string tip, Icon icon, ref ToolTip tt)
		{
			return false;
		}

		internal override void SystrayRemove (IntPtr hwnd, ref ToolTip tt)
		{
		}

		internal override void SystrayBalloon (IntPtr hwnd, int timeout, string title, string text, ToolTipIcon icon)
		{
		}

		// Старый путь буфера обмена не используется: XplatUI.Clipboard* заданы в
		// InitializeDriver.
		internal override void ClipboardClose (IntPtr handle)
		{
		}

		internal override IntPtr ClipboardOpen (bool primary_selection)
		{
			return IntPtr.Zero;
		}

		internal override int ClipboardGetID (IntPtr handle, string format)
		{
			return format.GetHashCode ();
		}

		internal override void ClipboardStore (IntPtr handle, object obj, int id, XplatUI.ObjectToClipboard converter, bool copy)
		{
		}

		internal override int[] ClipboardAvailableFormats (IntPtr handle)
		{
			return new int[0];
		}

		internal override object ClipboardRetrieve (IntPtr handle, int id, XplatUI.ClipboardToObject converter)
		{
			return null;
		}

		// Обратимое рисование (рамка перетаскивания сплиттера) — поверх экрана,
		// которого у программы нет. Пока ничего.
		internal override void DrawReversibleLine (Point start, Point end, Color backColor)
		{
		}

		internal override void DrawReversibleRectangle (IntPtr handle, Rectangle rect, int line_width)
		{
		}

		internal override void FillReversibleRectangle (Rectangle rectangle, Color backColor)
		{
		}

		internal override void DrawReversibleFrame (Rectangle rectangle, Color backColor, FrameStyle style)
		{
		}

		#endregion
	}
}
