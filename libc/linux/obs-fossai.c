// OBS Studio module «Захват экрана FossAi» (23.11.2026): screen and window
// capture through the kernel capture of FossAi (`/dev/fossai-capture`,
// crates/kernel/src/linux/fossaicap.rs), instead of PipeWire and the
// xdg-desktop-portal OBS uses on Linux/Wayland.
//
// The kernel keeps a mirror of the captured rectangle filled by the
// compositor. One thread of the module runs a kernel capture per distinct
// rectangle the sources want (26.11: a monitor at its own size, the whole
// desktop, a window); sources of the same rectangle share it.
//
// The pointer (26.11): the capture runs without the kernel patching the
// pointer into frames; FOSSAI_CAP_CURSOR hands its picture, hotspot and
// place every frame and the source draws it as its own quad («Показывать
// курсор», on by default) — also over a fullscreen game's own buffer, which
// never has the pointer, and a pointer move costs no frame copy.
//
// How frames reach the GPU (25.11, hw-log19: three full copies of 5120x1440
// per frame on Roman's PC):
//  * dma-buf (default when OBS's OpenGL can import dma-bufs and there is a
//    render node - NVIDIA, or `nvk soft on`): the kernel answers a ring of
//    three linear buffers (`FOSSAI_CAP_DMABUF`), imported once as textures
//    (`gs_texture_create_from_dmabuf`: EGL_EXT_image_dma_buf_import - zink
//    on NVK). Each frame `FOSSAI_CAP_FRAME` names the slot with the newest
//    frame (the kernel copies the mirror into it - the only CPU copy left)
//    or, when the captured monitor shows a fullscreen game straight from its
//    swapchain buffer, that buffer itself - no copy at all;
//  * CPU copy (fallback: no render node, no import, `FOSSAI_OBS_DMABUF=0`):
//    `FOSSAI_CAP_GRAB` into one of two buffers, uploaded into a texture by
//    the render thread (no OBS frame queue copy any more).
// The path is chosen at run time and logged: `[fossai-capture] frames: ...`.
//
// Two sources:
//  * fossai_screen_capture — one monitor (by its connector name, `DP-1`; by
//    default the primary one) or the whole desktop. The monitors come from
//    `read` of the device (24.11): `x\ty\tw\th\tprimary\tname\tdescription`
//    lines, the primary first, in capture coordinates;
//  * fossai_window_capture — the rectangle of a window by its title
//    (CAP_WINDOWS), followed when the window moves or resizes. It shows what
//    is on the screen there (windows above it too), like a region capture.
//
// Built by xtask (obs.rs) with clang against the Debian 12 sysroot and the
// libobs-dev headers of Debian 13; linked against libobs.so.0.

#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>

#include <obs-module.h>
#include <graphics/graphics.h>
#include <util/platform.h>
#include <util/threading.h>

OBS_DECLARE_MODULE()

MODULE_EXPORT const char *obs_module_description(void)
{
	return "FossAi screen and window capture (/dev/fossai-capture)";
}

// ---- kernel interface (user_abi CapInfo/CapSpec/CapStatus) -----------------

#define CAP_DEVICE "/dev/fossai-capture"
#define CAP_IOC(nr) _IOC(_IOC_READ | _IOC_WRITE, 'F', (nr), 0)
#define CAP_INFO CAP_IOC(1)
#define CAP_START CAP_IOC(2)
#define CAP_GRAB CAP_IOC(3)
#define CAP_STOP CAP_IOC(4)
#define CAP_WINDOWS CAP_IOC(6)
// 24.11: «Определить» — the number of every monitor on it (the argument is
// the time in ms, 0 — 3 s), the same numbers as «Параметры → Дисплей».
#define CAP_IDENTIFY CAP_IOC(7)
// 25.11: the dma-buf ring and the newest frame.
#define CAP_DMABUF CAP_IOC(8)
#define CAP_FRAME CAP_IOC(9)
// 26.11: the pointer over the capture of the descriptor.
#define CAP_CURSOR CAP_IOC(10)

#define CAP_POINTER 1u
#define CAP_CURSOR_VISIBLE 1u
#define CAP_CURSOR_IN_FRAME 2u
#define CAP_CURSOR_IMAGE 4u
#define PIXEL_RGB 1u
#define PIXEL_BGR 2u

#define DRM_FORMAT_MOD_LINEAR 0ULL
#define DRM_FORMAT_MOD_INVALID 0x00ffffffffffffffULL

struct cap_info {
	uint32_t count;
	uint32_t format;
	uint32_t desk_w;
	uint32_t desk_h;
	int32_t outputs[8][4];
};

struct cap_spec {
	int32_t x, y;
	uint32_t w, h;
	uint32_t flags;
	uint32_t fps;
};

struct cap_pair {
	uint64_t a, b;
};

#define RING_MAX 4
#define GAME_MAX 4

// FOSSAI_CAP_DMABUF answer.
struct cap_dmabuf {
	uint32_t count;
	uint32_t width, height;
	uint32_t stride;
	uint32_t fourcc;
	uint32_t flags;
	uint64_t modifier;
	int32_t fds[RING_MAX];
};

// FOSSAI_CAP_FRAME: in `have`, `known`; the rest out.
struct cap_frame {
	uint64_t have;
	uint64_t known[GAME_MAX];
	uint64_t seq;
	uint64_t buffer; // a game's buffer, 0 — a ring slot
	int32_t slot;
	int32_t fd; // a new descriptor of `buffer`, -1 — none
	uint32_t width, height, stride, fourcc;
	uint64_t modifier;
	int32_t fence_fd; // -1: complete on return
	uint32_t flags;
};

// FOSSAI_CAP_CURSOR (user_abi::CapCursor): in `image`, `image_len`,
// `have_serial`; the rest out. The picture is 0xAARRGGBB, straight alpha,
// its top-left corner at `x, y` of the captured rectangle.
struct cap_cursor {
	uint64_t image;
	uint32_t image_len;
	uint32_t have_serial;
	uint32_t serial;
	uint32_t flags;
	int32_t x, y;
	uint32_t hot_x, hot_y;
	uint32_t width, height;
};

static bool read_info(struct cap_info *info)
{
	int fd = open(CAP_DEVICE, O_RDWR | O_CLOEXEC);
	if (fd < 0)
		return false;
	memset(info, 0, sizeof(*info));
	bool ok = ioctl(fd, CAP_INFO, info) == 0;
	close(fd);
	return ok;
}

// ---- monitors ------------------------------------------------------------------

#define MAX_MONITORS 8
#define MONITOR_PRIMARY "primary"
#define MONITOR_DESKTOP "desktop"

struct monitor {
	int x, y, w, h;
	bool primary;
	// The number of «Параметры → Дисплей» and of the «Определить» plates
	// (24.11; an older kernel — the place in the list).
	int number;
	char name[32];
	char description[96];
};

// Monitors from the device text, the primary first; their number.
static int read_monitors(struct monitor *list)
{
	int fd = open(CAP_DEVICE, O_RDONLY | O_CLOEXEC);
	if (fd < 0)
		return 0;
	char text[4096];
	size_t len = 0;
	for (;;) {
		ssize_t n = read(fd, text + len, sizeof(text) - 1 - len);
		if (n <= 0)
			break;
		len += (size_t)n;
		if (len == sizeof(text) - 1)
			break;
	}
	close(fd);
	text[len] = 0;
	int count = 0;
	for (char *line = text; line && *line && count < MAX_MONITORS;) {
		char *end = strchr(line, '\n');
		if (end)
			*end = 0;
		struct monitor *m = &list[count];
		int primary = 0, used = 0;
		memset(m, 0, sizeof(*m));
		if (sscanf(line, "%d\t%d\t%d\t%d\t%d\t%31[^\t]\t%n", &m->x, &m->y, &m->w, &m->h, &primary, m->name,
			   &used) == 6 &&
		    m->w > 0 && m->h > 0) {
			m->primary = primary != 0;
			m->number = count + 1;
			if (used > 0)
				snprintf(m->description, sizeof(m->description), "%s", line + used);
			// `description\tnumber` (24.11).
			char *tab = strrchr(m->description, '\t');
			if (tab) {
				int number = atoi(tab + 1);
				if (number > 0)
					m->number = number;
				*tab = 0;
			}
			// «RHT QEMU Monitor (Virtual-1)»: the name is in the list line already.
			char suffix[40];
			snprintf(suffix, sizeof(suffix), " (%s)", m->name);
			size_t dl = strlen(m->description), sl = strlen(suffix);
			if (dl > sl && strcmp(m->description + dl - sl, suffix) == 0)
				m->description[dl - sl] = 0;
			count++;
		}
		line = end ? end + 1 : NULL;
	}
	return count;
}

// The monitor `id` (a connector name, MONITOR_PRIMARY) among `list`; an
// unknown name (the monitor is off now) falls back to the primary one.
static const struct monitor *pick_monitor(const struct monitor *list, int count, const char *id)
{
	if (count <= 0)
		return NULL;
	for (int i = 0; id && i < count; i++)
		if (strcmp(list[i].name, id) == 0)
			return &list[i];
	for (int i = 0; i < count; i++)
		if (list[i].primary)
			return &list[i];
	return &list[0];
}

// Window lines `x\ty\tw\th\tname\n`, top first; the caller frees.
static char *read_windows(void)
{
	int fd = open(CAP_DEVICE, O_RDWR | O_CLOEXEC);
	if (fd < 0)
		return NULL;
	size_t len = 64 * 1024;
	char *text = bzalloc(len + 1);
	struct cap_pair arg = {(uint64_t)(uintptr_t)text, len};
	long n = ioctl(fd, CAP_WINDOWS, &arg);
	close(fd);
	if (n < 0) {
		bfree(text);
		return NULL;
	}
	text[n] = 0;
	return text;
}

// Find the window `name` in the list; true and its rectangle if it is there.
static bool find_window(const char *name, struct cap_spec *spec)
{
	char *text = read_windows();
	if (!text)
		return false;
	bool found = false;
	for (char *line = text; line && *line;) {
		char *end = strchr(line, '\n');
		if (end)
			*end = 0;
		int x, y, w, h, used = 0;
		if (sscanf(line, "%d\t%d\t%d\t%d\t%n", &x, &y, &w, &h, &used) == 4 && used > 0 &&
		    strcmp(line + used, name) == 0 && w > 0 && h > 0) {
			spec->x = x;
			spec->y = y;
			spec->w = (uint32_t)w;
			spec->h = (uint32_t)h;
			found = true;
			break;
		}
		line = end ? end + 1 : NULL;
	}
	bfree(text);
	return found;
}

// ---- the source --------------------------------------------------------------

struct fossai_capture {
	obs_source_t *source;
	bool window_mode;

	pthread_mutex_t lock;
	// Settings (under `lock`).
	char *monitor; // a connector name, MONITOR_PRIMARY or MONITOR_DESKTOP
	char *window;
	bool cursor;
	int fps; // 0 — the rate of OBS video
	bool changed;

	// Under `g_lock` (the shared capture thread). `want.flags` has
	// CAP_POINTER when the source shows the pointer.
	struct cap_spec want;
	bool want_ok;
	uint64_t next_check;
	struct fossai_capture *next;
};

// One capture thread for all sources of the process. Since 26.11 it runs one
// kernel capture — a *stream* — per distinct rectangle the sources want: two
// sources on monitor 1 and monitor 2 are two streams of the monitors' own
// sizes (before: one box around both, 5120x1440 on Roman's PC, and each
// source cut its part out of it). Sources that want the same rectangle share
// a stream; the kernel shares its session with `/bin/screenrec` recording
// the same monitor.
static pthread_mutex_t g_lock = PTHREAD_MUTEX_INITIALIZER;
static struct fossai_capture *g_sources;
static pthread_t g_thread;
static bool g_thread_running;
// The running thread's own stop flag (a thread being joined keeps its flag;
// a new one started meanwhile gets a fresh one).
static volatile bool *g_stop;

// ---- streams: frames between the thread and the render thread ---------------

enum frame_mode { MODE_NONE, MODE_CPU, MODE_RING, MODE_GAME };

// dma-buf import by OBS's OpenGL: 0 — not known yet, 1 — works, -1 — not
// available (no extension, an import failed, FOSSAI_OBS_DMABUF=0).
static volatile int g_dmabuf;

// The kernel has no FOSSAI_CAP_CURSOR (older image): the pointer is patched
// into the frames by the kernel (CAP_POINTER) as before 26.11.
static volatile bool g_old_kernel_cursor;

struct game_buffer {
	uint64_t id;
	int fd; // pending import (the thread got it), -1 — none
	uint32_t width, height, stride, fourcc;
	uint64_t modifier;
};

#define MAX_STREAMS 8
#define CURSOR_MAX 64

struct stream {
	struct stream *next; // `g_streams`, under `g_frame_lock`
	struct cap_spec spec; // the rectangle (flags: what the kernel was asked)
	// Ended by the thread; freed by the render side (textures need the
	// graphics context) — the thread never touches a dead stream again.
	bool dead;

	// Capture thread only.
	int fd;
	uint64_t have;
	bool ring_on;
	bool cursor_wanted;
	bool busy_logged;
	uint64_t retry_at;
	uint32_t cursor_have;
	uint32_t *cursor_buf; // CURSOR_MAX², the kernel copies the picture here
	bool cursor_logged;

	// Under `g_frame_lock`.
	enum frame_mode mode;
	uint32_t format; // PIXEL_RGB/PIXEL_BGR
	// MODE_CPU: two buffers, the thread writes the one that is not `ready`.
	uint32_t *cpu[2];
	int ready; // -1 — nothing yet
	uint64_t ready_seq;
	uint32_t cpu_gen; // a new capture on the CPU path
	// MODE_RING: the ring (fds to import while `ring_gen` is new).
	uint32_t ring_gen;
	struct cap_dmabuf ring;
	int slot; // -1 — nothing yet
	// MODE_GAME: the buffer to show and buffers to import.
	uint64_t game;
	struct game_buffer pending[GAME_MAX];
	uint64_t known[GAME_MAX]; // imported by the render thread
	// The pointer (26.11): place in the rectangle, picture `cur_pixels`.
	struct {
		bool visible, in_frame;
		int32_t x, y;
		uint32_t w, h;
		uint32_t serial; // 0 — no picture yet
	} cur;
	uint32_t *cur_pixels; // CURSOR_MAX²

	// Render side (graphics context; under `g_frame_lock` too).
	gs_texture_t *cpu_tex;
	uint32_t cpu_w, cpu_h, cpu_format;
	uint64_t cpu_seq;
	uint32_t r_cpu_gen;
	gs_texture_t *ring_tex[RING_MAX];
	uint32_t r_ring_gen;
	struct {
		uint64_t id;
		gs_texture_t *tex;
		uint64_t used;
	} game_tex[GAME_MAX];
	enum frame_mode logged;
	/// How colours were drawn, as last logged (0 - not yet).
	int srgb_logged;
	/// 27.11: the first CHECK_SIDE rows of the ring buffers mapped for the
	/// readback check of the first import (NULL - none or done).
	void *check_map[RING_MAX];
	uint32_t check_stride;
	gs_texture_t *cursor_tex;
	uint32_t cursor_serial;
};

static pthread_mutex_t g_frame_lock = PTHREAD_MUTEX_INITIALIZER;
static struct stream *g_streams; // under `g_frame_lock`
static uint64_t g_render_frames;  // under `g_frame_lock`

static bool same_rect(const struct cap_spec *a, const struct cap_spec *b)
{
	return a->x == b->x && a->y == b->y && a->w == b->w && a->h == b->h;
}

static void close_pending_locked(struct stream *s)
{
	for (int i = 0; i < GAME_MAX; i++) {
		if (s->pending[i].id && s->pending[i].fd >= 0)
			close(s->pending[i].fd);
		s->pending[i].fd = -1;
		s->pending[i].id = 0;
	}
}

static void close_ring_fds_locked(struct stream *s)
{
	for (int i = 0; i < RING_MAX; i++) {
		if (s->ring.fds[i] >= 0)
			close(s->ring.fds[i]);
		s->ring.fds[i] = -1;
	}
}

static const char *screen_name(void *unused)
{
	UNUSED_PARAMETER(unused);
	return "Захват экрана FossAi";
}

static const char *window_name(void *unused)
{
	UNUSED_PARAMETER(unused);
	return "Захват окна FossAi";
}

// The rectangle to capture now; false — nothing to capture (no window).
static bool wanted_rect(struct fossai_capture *cap, struct cap_spec *spec)
{
	pthread_mutex_lock(&cap->lock);
	char *monitor = bstrdup(cap->monitor);
	char *window = cap->window ? bstrdup(cap->window) : NULL;
	spec->flags = cap->cursor ? CAP_POINTER : 0;
	spec->fps = (uint32_t)(cap->fps > 0 ? cap->fps : 30);
	pthread_mutex_unlock(&cap->lock);

	bool ok = false;
	if (cap->window_mode) {
		ok = window && *window && find_window(window, spec);
	} else {
		struct monitor list[MAX_MONITORS];
		int count = strcmp(monitor, MONITOR_DESKTOP) == 0 ? 0 : read_monitors(list);
		const struct monitor *m = pick_monitor(list, count, monitor);
		struct cap_info info;
		if (m) {
			spec->x = m->x;
			spec->y = m->y;
			spec->w = (uint32_t)m->w;
			spec->h = (uint32_t)m->h;
			ok = true;
		} else if (read_info(&info) && info.desk_w && info.desk_h) {
			spec->x = 0;
			spec->y = 0;
			spec->w = info.desk_w;
			spec->h = info.desk_h;
			ok = true;
		}
	}
	bfree(monitor);
	bfree(window);
	return ok;
}

static uint32_t pixel_format(void)
{
	struct cap_info info;
	return read_info(&info) ? info.format : PIXEL_BGR;
}

static const char *fourcc_text(uint32_t fourcc, char *text)
{
	for (int i = 0; i < 4; i++)
		text[i] = (char)((fourcc >> (8 * i)) & 0xff);
	text[4] = 0;
	return text;
}

// Ask the kernel for a dma-buf ring for the stream's capture; true — the
// frames go that way (published for the render thread).
static bool start_ring(struct stream *s, uint32_t format)
{
	struct cap_dmabuf ring;
	memset(&ring, 0, sizeof(ring));
	for (int i = 0; i < RING_MAX; i++)
		ring.fds[i] = -1;
	if (ioctl(s->fd, CAP_DMABUF, &ring) != 0) {
		blog(LOG_INFO, "[fossai-capture] frames: CPU copy (no dma-buf ring from the kernel: %s)",
		     errno == ENODEV ? "no GPU render node" : strerror(errno));
		return false;
	}
	if (ring.count == 0 || ring.count > RING_MAX) {
		for (int i = 0; i < RING_MAX; i++)
			if (ring.fds[i] >= 0)
				close(ring.fds[i]);
		return false;
	}
	char text[8];
	blog(LOG_INFO, "[fossai-capture] dma-buf ring: %u buffers %ux%u, stride %u, %s, modifier 0x%llx",
	     ring.count, ring.width, ring.height, ring.stride, fourcc_text(ring.fourcc, text),
	     (unsigned long long)ring.modifier);
	pthread_mutex_lock(&g_frame_lock);
	close_ring_fds_locked(s);
	close_pending_locked(s);
	s->ring = ring;
	s->ring_gen++;
	s->slot = -1;
	s->game = 0;
	s->mode = MODE_RING;
	s->format = format;
	pthread_mutex_unlock(&g_frame_lock);
	return true;
}

// The CPU path for the stream's capture: two buffers of its size.
static void start_cpu(struct stream *s, uint32_t format)
{
	pthread_mutex_lock(&g_frame_lock);
	close_ring_fds_locked(s);
	close_pending_locked(s);
	for (int i = 0; i < 2; i++) {
		bfree(s->cpu[i]);
		s->cpu[i] = bmalloc((size_t)s->spec.w * s->spec.h * 4);
	}
	s->ready = -1;
	s->ready_seq = 0;
	s->cpu_gen++;
	s->mode = MODE_CPU;
	s->format = format;
	pthread_mutex_unlock(&g_frame_lock);
}

// One frame through the ring: the newest slot or a game buffer. False — the
// ring is gone (the kernel restarted the capture): ask again.
static bool ring_frame(struct stream *s)
{
	struct cap_frame frame;
	memset(&frame, 0, sizeof(frame));
	frame.have = s->have;
	pthread_mutex_lock(&g_frame_lock);
	memcpy(frame.known, s->known, sizeof(frame.known));
	// Buffers handed over and not imported yet are known too.
	for (int i = 0; i < GAME_MAX; i++) {
		if (!s->pending[i].id)
			continue;
		for (int k = 0; k < GAME_MAX; k++) {
			if (!frame.known[k]) {
				frame.known[k] = s->pending[i].id;
				break;
			}
		}
	}
	pthread_mutex_unlock(&g_frame_lock);
	if (ioctl(s->fd, CAP_FRAME, &frame) != 0)
		return errno != ENODATA;
	s->have = frame.seq;
	pthread_mutex_lock(&g_frame_lock);
	if (frame.buffer) {
		if (frame.fd >= 0) {
			int free_at = -1;
			for (int i = 0; i < GAME_MAX && free_at < 0; i++)
				if (!s->pending[i].id)
					free_at = i;
			if (free_at < 0) {
				close(frame.fd); // asked again next frame
			} else {
				struct game_buffer *b = &s->pending[free_at];
				b->id = frame.buffer;
				b->fd = frame.fd;
				b->width = frame.width;
				b->height = frame.height;
				b->stride = frame.stride;
				b->fourcc = frame.fourcc;
				b->modifier = frame.modifier;
			}
		}
		s->game = frame.buffer;
		s->mode = MODE_GAME;
	} else if (frame.slot >= 0) {
		s->slot = frame.slot;
		s->game = 0;
		s->mode = MODE_RING;
	}
	pthread_mutex_unlock(&g_frame_lock);
	return true;
}

// One frame through the CPU: grab into the buffer that is not `ready`.
// False — the capture was ended from outside.
static bool cpu_frame(struct stream *s)
{
	pthread_mutex_lock(&g_frame_lock);
	int write = s->ready == 0 ? 1 : 0;
	uint32_t *buffer = s->cpu[write];
	pthread_mutex_unlock(&g_frame_lock);
	if (!buffer)
		return true;
	struct cap_pair arg = {(uint64_t)(uintptr_t)buffer, s->have};
	long seq = ioctl(s->fd, CAP_GRAB, &arg);
	if (seq < 0)
		return false;
	if ((uint64_t)seq != s->have) {
		s->have = (uint64_t)seq;
		pthread_mutex_lock(&g_frame_lock);
		s->ready = write;
		s->ready_seq = (uint64_t)seq;
		pthread_mutex_unlock(&g_frame_lock);
	}
	return true;
}

// The pointer over the stream (26.11): place, flags and — when it changed —
// the picture.
static void cursor_frame(struct stream *s)
{
	struct cap_cursor c;
	memset(&c, 0, sizeof(c));
	c.image = (uint64_t)(uintptr_t)s->cursor_buf;
	c.image_len = CURSOR_MAX * CURSOR_MAX * 4;
	c.have_serial = s->cursor_have;
	if (ioctl(s->fd, CAP_CURSOR, &c) != 0) {
		if (errno == ENOTTY || errno == EINVAL) {
			blog(LOG_INFO, "[fossai-capture] the kernel has no pointer request - the pointer is drawn into the frames");
			g_old_kernel_cursor = true;
		}
		return;
	}
	bool fits = c.width > 0 && c.height > 0 && c.width <= CURSOR_MAX && c.height <= CURSOR_MAX;
	pthread_mutex_lock(&g_frame_lock);
	s->cur.visible = (c.flags & CAP_CURSOR_VISIBLE) && fits;
	s->cur.in_frame = (c.flags & CAP_CURSOR_IN_FRAME) != 0;
	s->cur.x = c.x;
	s->cur.y = c.y;
	if ((c.flags & CAP_CURSOR_IMAGE) && fits) {
		memcpy(s->cur_pixels, s->cursor_buf, (size_t)c.width * c.height * 4);
		s->cur.w = c.width;
		s->cur.h = c.height;
		s->cur.serial = c.serial;
		s->cursor_have = c.serial;
	}
	pthread_mutex_unlock(&g_frame_lock);
	if ((c.flags & CAP_CURSOR_IMAGE) && fits && !s->cursor_logged) {
		s->cursor_logged = true;
		blog(LOG_INFO, "[fossai-capture] pointer %ux%u (hotspot %u,%u) over %ux%u at %d,%d - %s",
		     c.width, c.height, c.hot_x, c.hot_y, s->spec.w, s->spec.h, s->spec.x, s->spec.y,
		     (c.flags & CAP_CURSOR_IN_FRAME) ? "in the composed frame already (software pointer)"
						     : "drawn by the source");
	}
}

// Start the kernel capture of a stream; false — not now (retried later).
static bool stream_start(struct stream *s, int fps, uint32_t format)
{
	s->fd = open(CAP_DEVICE, O_RDWR | O_CLOEXEC);
	if (s->fd < 0) {
		blog(LOG_WARNING, "[fossai-capture] %s: %s", CAP_DEVICE, strerror(errno));
		return false;
	}
	struct cap_spec spec = s->spec;
	// The source draws the pointer itself; an old kernel patches it in.
	spec.flags = g_old_kernel_cursor && s->cursor_wanted ? CAP_POINTER : 0;
	spec.fps = (uint32_t)fps;
	if (ioctl(s->fd, CAP_START, &spec) != 0) {
		if (errno == EBUSY) {
			if (!s->busy_logged)
				blog(LOG_INFO, "[fossai-capture] another capture runs, waiting");
			s->busy_logged = true;
		} else {
			blog(LOG_WARNING, "[fossai-capture] start %ux%u at %d,%d: %s", spec.w, spec.h, spec.x, spec.y,
			     strerror(errno));
		}
		close(s->fd);
		s->fd = -1;
		return false;
	}
	s->busy_logged = false;
	s->spec.flags = spec.flags;
	s->have = 0;
	blog(LOG_INFO, "[fossai-capture] capturing %ux%u at %d,%d, %d fps, pointer %s", spec.w, spec.h, spec.x, spec.y,
	     fps, !s->cursor_wanted ? "off" : (spec.flags & CAP_POINTER) ? "on" : "drawn by the source");
	s->ring_on = g_dmabuf >= 0 && start_ring(s, format);
	if (!s->ring_on)
		start_cpu(s, format);
	return true;
}

static struct stream *stream_new(const struct cap_spec *rect)
{
	struct stream *s = bzalloc(sizeof(*s));
	s->spec = *rect;
	s->spec.flags = 0;
	s->fd = -1;
	s->ready = -1;
	s->slot = -1;
	for (int i = 0; i < RING_MAX; i++)
		s->ring.fds[i] = -1;
	for (int i = 0; i < GAME_MAX; i++)
		s->pending[i].fd = -1;
	s->cursor_buf = bzalloc(CURSOR_MAX * CURSOR_MAX * 4);
	s->cur_pixels = bzalloc(CURSOR_MAX * CURSOR_MAX * 4);
	// Touch the pages: the kernel copies into them.
	memset(s->cursor_buf, 0, CURSOR_MAX * CURSOR_MAX * 4);
	pthread_mutex_lock(&g_frame_lock);
	s->next = g_streams;
	g_streams = s;
	pthread_mutex_unlock(&g_frame_lock);
	return s;
}

// The thread is done with the stream: the kernel capture ends now, the
// render side frees the rest.
static void stream_end(struct stream *s)
{
	if (s->fd >= 0) {
		ioctl(s->fd, CAP_STOP, 0);
		close(s->fd);
		s->fd = -1;
	}
	pthread_mutex_lock(&g_frame_lock);
	s->dead = true;
	s->mode = MODE_NONE;
	pthread_mutex_unlock(&g_frame_lock);
}

static void *capture_thread(void *data)
{
	volatile bool *stop = data;
	os_set_thread_name("fossai-capture");
	uint32_t format = pixel_format();
	struct stream *live[MAX_STREAMS];
	int nlive = 0;

	while (!*stop) {
		uint64_t now = os_gettime_ns();
		// The rate of OBS video (the canvas): 30 or 60 without stutter. Not
		// under `g_lock` - the render thread takes it in get_width.
		struct obs_video_info ovi;
		int video_fps = obs_get_video_info(&ovi) && ovi.fps_den
					? (int)((ovi.fps_num + ovi.fps_den - 1) / ovi.fps_den)
					: 30;
		// What every source wants now (windows are looked up twice a second):
		// the distinct rectangles, each with the pointer if a source wants it.
		struct cap_spec wants[MAX_STREAMS];
		int nwant = 0;
		int fps = 0;
		pthread_mutex_lock(&g_lock);
		for (struct fossai_capture *cap = g_sources; cap; cap = cap->next) {
			pthread_mutex_lock(&cap->lock);
			bool redo = cap->changed;
			cap->changed = false;
			int want_fps = cap->fps;
			pthread_mutex_unlock(&cap->lock);
			if (redo || (cap->window_mode && now >= cap->next_check) || (!cap->want_ok && now >= cap->next_check)) {
				struct cap_spec want = {0};
				bool ok = wanted_rect(cap, &want);
				cap->want = want;
				cap->want_ok = ok;
				cap->next_check = now + 500000000ULL;
			}
			if (want_fps <= 0)
				want_fps = video_fps;
			if (want_fps > fps)
				fps = want_fps;
			if (!cap->want_ok)
				continue;
			int at = 0;
			while (at < nwant && !same_rect(&wants[at], &cap->want))
				at++;
			if (at == nwant) {
				if (nwant == MAX_STREAMS)
					continue;
				wants[nwant] = cap->want;
				wants[nwant].flags = 0;
				nwant++;
			}
			wants[at].flags |= cap->want.flags & CAP_POINTER;
		}
		pthread_mutex_unlock(&g_lock);
		if (fps <= 0 || fps > 240)
			fps = 30;

		// Streams nobody wants any more end; new rectangles get streams.
		for (int i = 0; i < nlive;) {
			bool kept = false;
			for (int k = 0; k < nwant && !kept; k++)
				kept = same_rect(&live[i]->spec, &wants[k]);
			if (kept) {
				i++;
				continue;
			}
			stream_end(live[i]);
			live[i] = live[--nlive];
		}
		for (int k = 0; k < nwant; k++) {
			struct stream *s = NULL;
			for (int i = 0; i < nlive && !s; i++)
				if (same_rect(&live[i]->spec, &wants[k]))
					s = live[i];
			if (!s && nlive < MAX_STREAMS) {
				s = stream_new(&wants[k]);
				live[nlive++] = s;
			}
			if (s)
				s->cursor_wanted = (wants[k].flags & CAP_POINTER) != 0;
		}

		bool waiting = nlive == 0;
		for (int i = 0; i < nlive; i++) {
			struct stream *s = live[i];
			if (s->fd < 0) {
				if (now < s->retry_at || !stream_start(s, fps, format)) {
					if (now >= s->retry_at)
						s->retry_at = now + 250000000ULL;
					waiting = true;
					continue;
				}
			}
			// OBS's OpenGL could not import the ring: the CPU from now on.
			if (s->ring_on && g_dmabuf < 0) {
				blog(LOG_INFO, "[fossai-capture] frames: CPU copy (OpenGL did not take the dma-buf)");
				s->ring_on = false;
				s->have = 0;
				start_cpu(s, format);
			}
			bool alive = s->ring_on ? ring_frame(s) : cpu_frame(s);
			if (!alive && s->ring_on) {
				// The ring is gone with a restarted capture: a new one.
				s->ring_on = start_ring(s, format);
				s->have = 0;
				if (!s->ring_on)
					start_cpu(s, format);
			} else if (!alive) {
				// The capture was ended from outside: start again.
				close(s->fd);
				s->fd = -1;
				pthread_mutex_lock(&g_frame_lock);
				s->mode = MODE_NONE;
				pthread_mutex_unlock(&g_frame_lock);
				continue;
			}
			if (s->cursor_wanted && !g_old_kernel_cursor)
				cursor_frame(s);
		}

		// Next frame time.
		uint64_t period = 1000000000ULL / (uint64_t)fps;
		uint64_t spent = os_gettime_ns() - now;
		if (waiting && nlive == 0)
			os_sleep_ms(250);
		else if (spent < period)
			os_sleepto_ns(now + period);
	}

	for (int i = 0; i < nlive; i++)
		stream_end(live[i]);
	return NULL;
}

// ---- rendering (graphics thread) ----------------------------------------------

static enum gs_color_format gs_format_of(uint32_t format)
{
	return format == PIXEL_RGB ? GS_RGBA : GS_BGRX;
}

// Is LINEAR among the modifiers OpenGL takes for `fourcc` (else the import
// goes without a modifier - implicit, linear for our buffers).
static bool linear_modifier_ok(uint32_t fourcc)
{
	uint64_t *mods = NULL;
	size_t n = 0;
	bool found = false;
	if (gs_query_dmabuf_modifiers_for_format(fourcc, &mods, &n)) {
		for (size_t i = 0; i < n; i++)
			found = found || mods[i] == DRM_FORMAT_MOD_LINEAR;
	}
	bfree(mods);
	return found;
}

static gs_texture_t *import_dmabuf(int fd, uint32_t width, uint32_t height, uint32_t stride, uint32_t fourcc,
				   uint64_t modifier)
{
	uint32_t strides[1] = {stride};
	uint32_t offsets[1] = {0};
	uint64_t modifiers[1] = {modifier};
	int fds[1] = {fd};
	bool with_modifier = modifier != DRM_FORMAT_MOD_INVALID && linear_modifier_ok(fourcc);
	enum gs_color_format color = fourcc == 0x34324258 /* XB24 */ ? GS_RGBA : GS_BGRX;
	gs_texture_t *tex = gs_texture_create_from_dmabuf(width, height, fourcc, color, 1, fds, strides, offsets,
							  with_modifier ? modifiers : NULL);
	if (!tex && with_modifier)
		tex = gs_texture_create_from_dmabuf(width, height, fourcc, color, 1, fds, strides, offsets, NULL);
	return tex;
}

// Side of the square the readback check compares (top left of the frame).
#define CHECK_SIDE 16

static void drop_check_map(struct stream *s, int i)
{
	if (s->check_map[i])
		munmap(s->check_map[i], (size_t)s->check_stride * CHECK_SIDE);
	s->check_map[i] = NULL;
}

// Draw the top left CHECK_SIDE square of `tex` into a texture of ours, read it
// back and compare it with the same pixels through the CPU (`map`, rows at
// `stride`). Colour channels only, off by at most 2. Unable to tell (no
// render target, no staging) counts as the same.
static bool readback_matches(gs_texture_t *tex, const uint8_t *map, uint32_t stride)
{
	const uint32_t n = CHECK_SIDE;
	gs_texrender_t *tr = gs_texrender_create(GS_BGRA, GS_ZS_NONE);
	gs_stagesurf_t *stage = gs_stagesurface_create(n, n, GS_BGRA);
	bool same = true, told = false;
	int bad = 0;
	uint32_t got = 0, want = 0;
	if (tr && stage && gs_texrender_begin(tr, n, n)) {
		gs_ortho(0.0f, (float)n, 0.0f, (float)n, -100.0f, 100.0f);
		const bool previous = gs_framebuffer_srgb_enabled();
		gs_enable_framebuffer_srgb(false);
		gs_blend_state_push();
		gs_enable_blending(false);
		gs_effect_t *effect = obs_get_base_effect(OBS_EFFECT_OPAQUE);
		gs_effect_set_texture(gs_effect_get_param_by_name(effect, "image"), tex);
		while (gs_effect_loop(effect, "Draw"))
			gs_draw_sprite_subregion(tex, 0, 0, 0, n, n);
		gs_blend_state_pop();
		gs_enable_framebuffer_srgb(previous);
		gs_texrender_end(tr);
		gs_stage_texture(stage, gs_texrender_get_texture(tr));
		uint8_t *data = NULL;
		uint32_t line = 0;
		if (gs_stagesurface_map(stage, &data, &line)) {
			told = true;
			for (uint32_t y = 0; y < n; y++) {
				for (uint32_t x = 0; x < n; x++) {
					const uint8_t *a = data + (size_t)y * line + x * 4;
					const uint8_t *b = map + (size_t)y * stride + x * 4;
					for (int c = 0; c < 3; c++) {
						if (abs((int)a[c] - (int)b[c]) > 2) {
							if (!bad) {
								memcpy(&got, a, 4);
								memcpy(&want, b, 4);
							}
							bad++;
							break;
						}
					}
				}
			}
			gs_stagesurface_unmap(stage);
		}
	}
	if (stage)
		gs_stagesurface_destroy(stage);
	if (tr)
		gs_texrender_destroy(tr);
	if (bad) {
		same = false;
		blog(LOG_WARNING, "[fossai-capture] dma-buf readback differs: %d of %u points (first: GPU 0x%08x, memory 0x%08x)",
		     bad, n * n, got, want);
	} else {
		blog(LOG_INFO, "[fossai-capture] dma-buf readback %s",
		     told ? "matches the memory (16x16 points)" : "not possible - trusted");
	}
	return same;
}

static void drop_ring_textures(struct stream *s)
{
	for (int i = 0; i < RING_MAX; i++) {
		if (s->ring_tex[i])
			gs_texture_destroy(s->ring_tex[i]);
		s->ring_tex[i] = NULL;
	}
}

static void drop_game_textures(struct stream *s)
{
	for (int i = 0; i < GAME_MAX; i++) {
		if (s->game_tex[i].tex)
			gs_texture_destroy(s->game_tex[i].tex);
		s->game_tex[i].tex = NULL;
		s->game_tex[i].id = 0;
		s->known[i] = 0;
	}
}

static void dmabuf_failed(const char *what)
{
	if (g_dmabuf >= 0)
		blog(LOG_WARNING, "[fossai-capture] dma-buf import failed (%s) - CPU copy from now on", what);
	g_dmabuf = -1;
}

// Free the streams the thread ended (graphics context, under `g_frame_lock`).
static void sweep_streams_locked(void)
{
	for (struct stream **at = &g_streams; *at;) {
		struct stream *s = *at;
		if (!s->dead) {
			at = &s->next;
			continue;
		}
		*at = s->next;
		drop_ring_textures(s);
		drop_game_textures(s);
		for (int i = 0; i < RING_MAX; i++)
			drop_check_map(s, i);
		if (s->cpu_tex)
			gs_texture_destroy(s->cpu_tex);
		if (s->cursor_tex)
			gs_texture_destroy(s->cursor_tex);
		close_ring_fds_locked(s);
		close_pending_locked(s);
		for (int i = 0; i < 2; i++)
			bfree(s->cpu[i]);
		bfree(s->cursor_buf);
		bfree(s->cur_pixels);
		bfree(s);
	}
}

// The texture with the newest frame of the stream (under `g_frame_lock`);
// NULL — nothing to show yet.
static gs_texture_t *frame_texture_locked(struct stream *s)
{
	switch (s->mode) {
	case MODE_RING: {
		if (s->r_ring_gen != s->ring_gen) {
			drop_ring_textures(s);
			s->r_ring_gen = s->ring_gen;
			const struct cap_dmabuf *r = &s->ring;
			bool ok = true;
			for (uint32_t i = 0; i < r->count && i < RING_MAX; i++) {
				s->ring_tex[i] = r->fds[i] >= 0 ? import_dmabuf(r->fds[i], r->width, r->height, r->stride,
										 r->fourcc, r->modifier)
								 : NULL;
				ok = ok && s->ring_tex[i];
				// 27.11: a CPU view of the same memory for the readback check.
				drop_check_map(s, i);
				if (ok && g_dmabuf == 0 && r->fds[i] >= 0) {
					void *map = mmap(NULL, (size_t)r->stride * CHECK_SIDE, PROT_READ, MAP_SHARED, r->fds[i], 0);
					s->check_map[i] = map == MAP_FAILED ? NULL : map;
				}
			}
			s->check_stride = r->stride;
			// The textures keep the buffers; the descriptors are not needed.
			close_ring_fds_locked(s);
			if (!ok) {
				drop_ring_textures(s);
				dmabuf_failed("ring");
				return NULL;
			}
			if (g_dmabuf == 0)
				blog(LOG_INFO, "[fossai-capture] dma-buf import works");
			g_dmabuf = 1;
		}
		// 27.11: the first frame shown is read back from the GPU and compared
		// with the memory the kernel wrote (zink/NVK importing system memory
		// can only be checked on real hardware).
		if (s->slot >= 0 && s->slot < RING_MAX && s->check_map[s->slot] && s->ring_tex[s->slot]) {
			bool same = readback_matches(s->ring_tex[s->slot], s->check_map[s->slot], s->check_stride);
			for (int i = 0; i < RING_MAX; i++)
				drop_check_map(s, i);
			if (!same) {
				drop_ring_textures(s);
				dmabuf_failed("the GPU reads other pixels than the kernel wrote");
				return NULL;
			}
		}
		if (!s->game)
			drop_game_textures(s);
		if (s->logged != MODE_RING) {
			blog(LOG_INFO, "[fossai-capture] frames: dma-buf ring (one kernel copy, no copy in OBS)");
			s->logged = MODE_RING;
		}
		return s->slot >= 0 && s->slot < RING_MAX ? s->ring_tex[s->slot] : NULL;
	}
	case MODE_GAME: {
		// Import what the thread handed over.
		for (int i = 0; i < GAME_MAX; i++) {
			struct game_buffer *b = &s->pending[i];
			if (!b->id)
				continue;
			int at = 0;
			for (int k = 0; k < GAME_MAX; k++) {
				if (s->game_tex[k].id == b->id || !s->game_tex[k].tex) {
					at = k;
					break;
				}
				if (s->game_tex[k].used < s->game_tex[at].used)
					at = k;
			}
			if (s->game_tex[at].tex)
				gs_texture_destroy(s->game_tex[at].tex);
			s->game_tex[at].tex = import_dmabuf(b->fd, b->width, b->height, b->stride, b->fourcc, b->modifier);
			s->game_tex[at].id = s->game_tex[at].tex ? b->id : 0;
			s->game_tex[at].used = g_render_frames;
			s->known[at] = s->game_tex[at].id;
			if (!s->game_tex[at].tex)
				dmabuf_failed("game buffer");
			else
				blog(LOG_INFO, "[fossai-capture] game buffer 0x%llx %ux%u imported",
				     (unsigned long long)b->id, b->width, b->height);
			close(b->fd);
			b->fd = -1;
			b->id = 0;
		}
		if (s->logged != MODE_GAME) {
			blog(LOG_INFO, "[fossai-capture] frames: the fullscreen game's own buffer (no copy at all)");
			s->logged = MODE_GAME;
		}
		for (int k = 0; k < GAME_MAX; k++) {
			if (s->game_tex[k].tex && s->game_tex[k].id == s->game) {
				s->game_tex[k].used = g_render_frames;
				return s->game_tex[k].tex;
			}
		}
		// Not imported yet: the last ring frame meanwhile.
		return s->slot >= 0 && s->slot < RING_MAX ? s->ring_tex[s->slot] : NULL;
	}
	case MODE_CPU: {
		if (s->logged != MODE_CPU) {
			blog(LOG_INFO, "[fossai-capture] frames: CPU copy (grab + texture upload)");
			s->logged = MODE_CPU;
		}
		uint32_t w = s->spec.w, h = s->spec.h;
		if (s->cpu_tex && (s->cpu_w != w || s->cpu_h != h || s->cpu_format != s->format)) {
			gs_texture_destroy(s->cpu_tex);
			s->cpu_tex = NULL;
		}
		if (s->r_cpu_gen != s->cpu_gen) {
			s->r_cpu_gen = s->cpu_gen;
			s->cpu_seq = 0;
		}
		if (!s->cpu_tex && w && h) {
			s->cpu_tex = gs_texture_create(w, h, gs_format_of(s->format), 1, NULL, GS_DYNAMIC);
			s->cpu_w = w;
			s->cpu_h = h;
			s->cpu_format = s->format;
			s->cpu_seq = 0;
		}
		if (s->cpu_tex && s->ready >= 0 && s->ready_seq != s->cpu_seq && s->cpu[s->ready]) {
			gs_texture_set_image(s->cpu_tex, (const uint8_t *)s->cpu[s->ready], w * 4, false);
			s->cpu_seq = s->ready_seq;
		}
		return s->cpu_seq ? s->cpu_tex : NULL;
	}
	default:
		return NULL;
	}
}

// The pointer as its own quad over the frame (26.11, like OBS's xshm and
// PipeWire captures): straight alpha, clipped to the source.
static void draw_cursor_locked(struct stream *s, const struct cap_spec *want)
{
	if (!s->cursor_tex || s->cursor_serial != s->cur.serial) {
		if (s->cursor_tex)
			gs_texture_destroy(s->cursor_tex);
		const uint8_t *data = (const uint8_t *)s->cur_pixels;
		s->cursor_tex = gs_texture_create(s->cur.w, s->cur.h, GS_BGRA, 1, &data, 0);
		s->cursor_serial = s->cur.serial;
	}
	if (!s->cursor_tex)
		return;
	int32_t x0 = s->cur.x < 0 ? 0 : s->cur.x;
	int32_t y0 = s->cur.y < 0 ? 0 : s->cur.y;
	int32_t x1 = s->cur.x + (int32_t)s->cur.w;
	int32_t y1 = s->cur.y + (int32_t)s->cur.h;
	if (x1 > (int32_t)want->w)
		x1 = (int32_t)want->w;
	if (y1 > (int32_t)want->h)
		y1 = (int32_t)want->h;
	if (x1 <= x0 || y1 <= y0)
		return;
	gs_effect_t *effect = obs_get_base_effect(OBS_EFFECT_DEFAULT);
	const bool linear_srgb = gs_get_linear_srgb();
	const bool previous = gs_framebuffer_srgb_enabled();
	gs_enable_framebuffer_srgb(linear_srgb);
	gs_blend_state_push();
	gs_blend_function(GS_BLEND_SRCALPHA, GS_BLEND_INVSRCALPHA);
	gs_enable_color(true, true, true, false);
	gs_eparam_t *image = gs_effect_get_param_by_name(effect, "image");
	if (linear_srgb)
		gs_effect_set_texture_srgb(image, s->cursor_tex);
	else
		gs_effect_set_texture(image, s->cursor_tex);
	gs_matrix_push();
	gs_matrix_translate3f((float)x0, (float)y0, 0.0f);
	while (gs_effect_loop(effect, "Draw"))
		gs_draw_sprite_subregion(s->cursor_tex, 0, (uint32_t)(x0 - s->cur.x), (uint32_t)(y0 - s->cur.y),
					 (uint32_t)(x1 - x0), (uint32_t)(y1 - y0));
	gs_matrix_pop();
	gs_enable_color(true, true, true, true);
	gs_blend_state_pop();
	gs_enable_framebuffer_srgb(previous);
}

static void capture_render(void *data, gs_effect_t *unused)
{
	UNUSED_PARAMETER(unused);
	struct fossai_capture *cap = data;
	pthread_mutex_lock(&g_lock);
	struct cap_spec want = cap->want;
	bool want_ok = cap->want_ok;
	pthread_mutex_unlock(&g_lock);

	pthread_mutex_lock(&g_frame_lock);
	g_render_frames++;
	sweep_streams_locked();
	struct stream *s = NULL;
	for (struct stream *at = g_streams; want_ok && at && !s; at = at->next)
		if (!at->dead && same_rect(&at->spec, &want))
			s = at;
	gs_texture_t *tex = s ? frame_texture_locked(s) : NULL;
	if (tex) {
		const bool linear_srgb = gs_get_linear_srgb();
		const bool previous = gs_framebuffer_srgb_enabled();
		gs_enable_framebuffer_srgb(linear_srgb);
		// 27.11 (hw-log20, «цвет капец при записи»): a dma-buf texture
		// (ring, game buffer) is an EGLImage of a UNORM format (XR24/XB24):
		// `gs_effect_set_texture_srgb` only switches GL_TEXTURE_SRGB_DECODE,
		// which does nothing on a non-sRGB format - the sRGB values went in
		// as linear and the sRGB framebuffer encoded them once more (pale,
		// washed-out picture in the preview and in every recording). Such
		// textures are decoded in the shader (DrawSrgbDecompress); OBS's own
		// textures (CPU copy) keep the sRGB view.
		const bool dmabuf_tex = s->mode != MODE_CPU;
		gs_effect_t *effect = obs_get_base_effect(linear_srgb && dmabuf_tex ? OBS_EFFECT_DEFAULT : OBS_EFFECT_OPAQUE);
		const char *tech = linear_srgb && dmabuf_tex ? "DrawSrgbDecompress" : "Draw";
		gs_eparam_t *image = gs_effect_get_param_by_name(effect, "image");
		if (linear_srgb && !dmabuf_tex)
			gs_effect_set_texture_srgb(image, tex);
		else
			gs_effect_set_texture(image, tex);
		// The frame is opaque: no blending with what is under the source
		// (the X channel of XR24 may read as anything on some drivers).
		gs_blend_state_push();
		gs_enable_blending(false);
		while (gs_effect_loop(effect, tech))
			gs_draw_sprite_subregion(tex, 0, 0, 0, want.w, want.h);
		gs_blend_state_pop();
		gs_enable_framebuffer_srgb(previous);
		if (s->srgb_logged != (int)(linear_srgb && dmabuf_tex) + 1) {
			s->srgb_logged = (int)(linear_srgb && dmabuf_tex) + 1;
			blog(LOG_INFO, "[fossai-capture] colours: %s", linear_srgb && dmabuf_tex
				? "dma-buf frame decoded from sRGB in the shader (linear canvas)"
				: (linear_srgb ? "sRGB texture view (linear canvas)" : "frame drawn as is (sRGB canvas)"));
		}
		// The pointer: unless it is in the frame already (the compositor
		// drew it, or an old kernel patched it in).
		if ((want.flags & CAP_POINTER) && !g_old_kernel_cursor && s->cur.visible && s->cur.serial &&
		    !(s->cur.in_frame && s->mode != MODE_GAME))
			draw_cursor_locked(s, &want);
	}
	pthread_mutex_unlock(&g_frame_lock);
}

static uint32_t capture_width(void *data)
{
	struct fossai_capture *cap = data;
	pthread_mutex_lock(&g_lock);
	uint32_t w = cap->want_ok ? cap->want.w : 0;
	pthread_mutex_unlock(&g_lock);
	return w;
}

static uint32_t capture_height(void *data)
{
	struct fossai_capture *cap = data;
	pthread_mutex_lock(&g_lock);
	uint32_t h = cap->want_ok ? cap->want.h : 0;
	pthread_mutex_unlock(&g_lock);
	return h;
}

static void apply_settings(struct fossai_capture *cap, obs_data_t *settings)
{
	pthread_mutex_lock(&cap->lock);
	bfree(cap->monitor);
	const char *id = obs_data_get_string(settings, "monitor_id");
	// Scenes of 23.11 keep the index `monitor` (-1 — the whole desktop).
	if (!obs_data_has_user_value(settings, "monitor_id") && obs_data_has_user_value(settings, "monitor")) {
		long long index = obs_data_get_int(settings, "monitor");
		struct monitor list[MAX_MONITORS];
		int count = read_monitors(list);
		id = index < 0 ? MONITOR_DESKTOP : index < count ? list[index].name : MONITOR_PRIMARY;
		cap->monitor = bstrdup(id);
	} else {
		cap->monitor = bstrdup(id && *id ? id : MONITOR_PRIMARY);
	}
	bfree(cap->window);
	cap->window = bstrdup(obs_data_get_string(settings, "window"));
	cap->cursor = obs_data_get_bool(settings, "show_cursor");
	cap->fps = (int)obs_data_get_int(settings, "fps");
	if (cap->fps < 0 || cap->fps > 240)
		cap->fps = 0;
	cap->changed = true;
	pthread_mutex_unlock(&cap->lock);
}

static void *create_common(obs_data_t *settings, obs_source_t *source, bool window_mode)
{
	struct fossai_capture *cap = bzalloc(sizeof(*cap));
	cap->source = source;
	cap->window_mode = window_mode;
	pthread_mutex_init(&cap->lock, NULL);
	apply_settings(cap, settings);
	pthread_mutex_lock(&g_lock);
	cap->next = g_sources;
	g_sources = cap;
	if (!g_thread_running) {
		g_stop = bzalloc(sizeof(bool));
		g_thread_running = pthread_create(&g_thread, NULL, capture_thread, (void *)g_stop) == 0;
		if (!g_thread_running) {
			bfree((void *)g_stop);
			g_stop = NULL;
		}
	}
	pthread_mutex_unlock(&g_lock);
	return cap;
}

static void *screen_create(obs_data_t *settings, obs_source_t *source)
{
	return create_common(settings, source, false);
}

static void *window_create(obs_data_t *settings, obs_source_t *source)
{
	return create_common(settings, source, true);
}

static void capture_destroy(void *data)
{
	struct fossai_capture *cap = data;
	pthread_mutex_lock(&g_lock);
	for (struct fossai_capture **at = &g_sources; *at; at = &(*at)->next) {
		if (*at == cap) {
			*at = cap->next;
			break;
		}
	}
	bool last = g_sources == NULL && g_thread_running;
	pthread_t thread = g_thread;
	volatile bool *stop = g_stop;
	if (last) {
		*stop = true;
		g_stop = NULL;
		g_thread_running = false;
	}
	pthread_mutex_unlock(&g_lock);
	// The last source: the thread stops the kernel captures and ends; the
	// streams and their textures go with it.
	if (last) {
		pthread_join(thread, NULL);
		bfree((void *)stop);
		obs_enter_graphics();
		pthread_mutex_lock(&g_frame_lock);
		sweep_streams_locked();
		pthread_mutex_unlock(&g_frame_lock);
		obs_leave_graphics();
	}
	pthread_mutex_destroy(&cap->lock);
	bfree(cap->monitor);
	bfree(cap->window);
	bfree(cap);
}

static void capture_update(void *data, obs_data_t *settings)
{
	apply_settings(data, settings);
}

static void capture_defaults(obs_data_t *settings)
{
	// The primary monitor by its name (the list shows names); no monitor
	// list (an old kernel) — the primary anyway.
	struct monitor list[MAX_MONITORS];
	const struct monitor *primary = pick_monitor(list, read_monitors(list), NULL);
	obs_data_set_default_string(settings, "monitor_id", primary ? primary->name : MONITOR_PRIMARY);
	obs_data_set_default_int(settings, "monitor", -1);
	obs_data_set_default_string(settings, "window", "");
	obs_data_set_default_bool(settings, "show_cursor", true);
	obs_data_set_default_int(settings, "fps", 0);
}

// «Определить» (24.11, like Windows): a big number on every monitor for 3 s
// — the numbers of the list above.
static bool identify_clicked(obs_properties_t *props, obs_property_t *property, void *data)
{
	UNUSED_PARAMETER(props);
	UNUSED_PARAMETER(property);
	UNUSED_PARAMETER(data);
	int fd = open(CAP_DEVICE, O_RDWR | O_CLOEXEC);
	if (fd < 0)
		return false;
	if (ioctl(fd, CAP_IDENTIFY, 0) != 0)
		blog(LOG_WARNING, "[fossai-capture] identify: %s", strerror(errno));
	else
		blog(LOG_INFO, "[fossai-capture] identify monitors");
	close(fd);
	return false;
}

static obs_properties_t *screen_properties(void *data)
{
	UNUSED_PARAMETER(data);
	obs_properties_t *props = obs_properties_create();
	obs_property_t *list =
		obs_properties_add_list(props, "monitor_id", "Монитор", OBS_COMBO_TYPE_LIST, OBS_COMBO_FORMAT_STRING);
	struct monitor monitors[MAX_MONITORS];
	int count = read_monitors(monitors);
	char text[192];
	for (int i = 0; i < count; i++) {
		const struct monitor *m = &monitors[i];
		snprintf(text, sizeof(text), "Монитор %d — %s %d×%d%s%s%s", m->number, m->name, m->w, m->h,
			 m->primary ? ", основной" : "", *m->description ? " — " : "", m->description);
		obs_property_list_add_string(list, text, m->name);
	}
	struct cap_info info;
	if (read_info(&info) && info.desk_w && info.desk_h) {
		snprintf(text, sizeof(text), "Весь рабочий стол — %u×%u", info.desk_w, info.desk_h);
		obs_property_list_add_string(list, text, MONITOR_DESKTOP);
	} else {
		obs_property_list_add_string(list, "Весь рабочий стол", MONITOR_DESKTOP);
	}
	obs_properties_add_button(props, "identify", "Определить", identify_clicked);
	obs_properties_add_bool(props, "show_cursor", "Показывать курсор");
	obs_property_t *fps = obs_properties_add_int(props, "fps", "Кадров в секунду (0 — как у видео OBS)", 0, 240, 1);
	obs_property_set_long_description(fps, "0 — столько же, сколько в Настройки → Видео (30 или 60)");
	return props;
}

static obs_properties_t *window_properties(void *data)
{
	UNUSED_PARAMETER(data);
	obs_properties_t *props = obs_properties_create();
	obs_property_t *list =
		obs_properties_add_list(props, "window", "Окно", OBS_COMBO_TYPE_EDITABLE, OBS_COMBO_FORMAT_STRING);
	char *text = read_windows();
	for (char *line = text; line && *line;) {
		char *end = strchr(line, '\n');
		if (end)
			*end = 0;
		int x, y, w, h, used = 0;
		if (sscanf(line, "%d\t%d\t%d\t%d\t%n", &x, &y, &w, &h, &used) == 4 && used > 0 && line[used])
			obs_property_list_add_string(list, line + used, line + used);
		line = end ? end + 1 : NULL;
	}
	bfree(text);
	obs_properties_add_bool(props, "show_cursor", "Показывать курсор");
	obs_properties_add_int(props, "fps", "Кадров в секунду (0 — как у видео OBS)", 0, 240, 1);
	return props;
}

// 25.11: synchronous video sources drawing a texture (were asynchronous
// sources fed with frames OBS copied once more).
static struct obs_source_info screen_info = {
	.id = "fossai_screen_capture",
	.type = OBS_SOURCE_TYPE_INPUT,
	.output_flags = OBS_SOURCE_VIDEO | OBS_SOURCE_CUSTOM_DRAW | OBS_SOURCE_SRGB | OBS_SOURCE_DO_NOT_DUPLICATE,
	.get_name = screen_name,
	.create = screen_create,
	.destroy = capture_destroy,
	.update = capture_update,
	.get_defaults = capture_defaults,
	.get_properties = screen_properties,
	.get_width = capture_width,
	.get_height = capture_height,
	.video_render = capture_render,
	.icon_type = OBS_ICON_TYPE_DESKTOP_CAPTURE,
};

static struct obs_source_info window_info = {
	.id = "fossai_window_capture",
	.type = OBS_SOURCE_TYPE_INPUT,
	.output_flags = OBS_SOURCE_VIDEO | OBS_SOURCE_CUSTOM_DRAW | OBS_SOURCE_SRGB | OBS_SOURCE_DO_NOT_DUPLICATE,
	.get_name = window_name,
	.create = window_create,
	.destroy = capture_destroy,
	.update = capture_update,
	.get_defaults = capture_defaults,
	.get_properties = window_properties,
	.get_width = capture_width,
	.get_height = capture_height,
	.video_render = capture_render,
	.icon_type = OBS_ICON_TYPE_WINDOW_CAPTURE,
};

// Can OBS's OpenGL import dma-bufs of our formats (EGL dma-buf import)?
// Logged; FOSSAI_OBS_DMABUF=0 turns the path off, =1 tries it even when the
// query says no.
static void probe_dmabuf(void)
{
	const char *env = getenv("FOSSAI_OBS_DMABUF");
	if (env && strcmp(env, "0") == 0) {
		g_dmabuf = -1;
		blog(LOG_INFO, "[fossai-capture] dma-buf off (FOSSAI_OBS_DMABUF=0)");
		return;
	}
	enum gs_dmabuf_flags flags = GS_DMABUF_FLAG_NONE;
	uint32_t *formats = NULL;
	size_t n = 0;
	obs_enter_graphics();
	bool ok = gs_query_dmabuf_capabilities(&flags, &formats, &n);
	obs_leave_graphics();
	bool xr24 = false, xb24 = false;
	for (size_t i = 0; ok && i < n; i++) {
		xr24 = xr24 || formats[i] == 0x34325258;
		xb24 = xb24 || formats[i] == 0x34324258;
	}
	bfree(formats);
	bool wanted = pixel_format() == PIXEL_RGB ? xb24 : xr24;
	if (env && strcmp(env, "1") == 0 && !wanted) {
		blog(LOG_INFO, "[fossai-capture] dma-buf: OpenGL does not list the format (%zu formats) - tried anyway (FOSSAI_OBS_DMABUF=1)", n);
		g_dmabuf = 0;
		return;
	}
	g_dmabuf = wanted ? 0 : -1;
	blog(LOG_INFO, "[fossai-capture] dma-buf import %s (%zu formats, XR24 %s, XB24 %s, implicit modifiers %s)",
	     wanted ? "available" : "not available - CPU copy", n, xr24 ? "yes" : "no", xb24 ? "yes" : "no",
	     (flags & GS_DMABUF_FLAG_IMPLICIT_MODIFIERS_SUPPORTED) ? "yes" : "no");
}

bool obs_module_load(void)
{
	obs_register_source(&screen_info);
	obs_register_source(&window_info);
	int fd = open(CAP_DEVICE, O_RDWR | O_CLOEXEC);
	blog(LOG_INFO, "[fossai-capture] loaded (%s)", fd >= 0 ? "device present" : "no device");
	if (fd >= 0)
		close(fd);
	probe_dmabuf();
	return true;
}
