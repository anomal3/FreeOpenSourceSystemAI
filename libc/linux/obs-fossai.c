// OBS Studio module «Захват экрана FossAi» (23.11.2026): screen and window
// capture through the kernel capture of FossAi (`/dev/fossai-capture`,
// crates/kernel/src/linux/fossaicap.rs), instead of PipeWire and the
// xdg-desktop-portal OBS uses on Linux/Wayland.
//
// The kernel keeps a mirror of the captured rectangle filled by the
// compositor. One thread of the module captures the box around what all
// sources want and each source draws its part of it.
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

#define CAP_POINTER 1u
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

	// Under `g_lock` (the shared capture thread).
	struct cap_spec want;
	bool want_ok;
	uint64_t next_check;
	struct fossai_capture *next;
};

// One kernel capture for all sources of the process (24.11): the kernel runs
// one capture at a time, so a second source (monitor 2 next to monitor 1, a
// window) used to wait forever. The thread captures the box around what all
// sources want and each source draws its part.
static pthread_mutex_t g_lock = PTHREAD_MUTEX_INITIALIZER;
static struct fossai_capture *g_sources;
static pthread_t g_thread;
static bool g_thread_running;
// The running thread's own stop flag (a thread being joined keeps its flag;
// a new one started meanwhile gets a fresh one).
static volatile bool *g_stop;

// ---- frames between the thread and the render thread (25.11) -----------------

enum frame_mode { MODE_NONE, MODE_CPU, MODE_RING, MODE_GAME };

// dma-buf import by OBS's OpenGL: 0 — not known yet, 1 — works, -1 — not
// available (no extension, an import failed, FOSSAI_OBS_DMABUF=0).
static volatile int g_dmabuf;

struct game_buffer {
	uint64_t id;
	int fd; // pending import (the thread got it), -1 — none
	uint32_t width, height, stride, fourcc;
	uint64_t modifier;
};

// Under `g_frame_lock`.
static pthread_mutex_t g_frame_lock = PTHREAD_MUTEX_INITIALIZER;
static struct {
	enum frame_mode mode;
	struct cap_spec box; // the captured box of the frame
	uint32_t format;     // PIXEL_RGB/PIXEL_BGR
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
} g_frame = {.ready = -1, .slot = -1};

// Render thread side (graphics context; under `g_frame_lock` too).
static struct {
	gs_texture_t *cpu_tex;
	uint32_t cpu_w, cpu_h, cpu_format;
	uint64_t cpu_seq;
	uint32_t cpu_gen;
	gs_texture_t *ring_tex[RING_MAX];
	uint32_t ring_gen;
	struct {
		uint64_t id;
		gs_texture_t *tex;
		uint64_t used;
	} game[GAME_MAX];
	uint64_t frames;
	enum frame_mode logged;
} g_render;

static void close_pending_locked(void)
{
	for (int i = 0; i < GAME_MAX; i++) {
		if (g_frame.pending[i].id && g_frame.pending[i].fd >= 0)
			close(g_frame.pending[i].fd);
		g_frame.pending[i].fd = -1;
		g_frame.pending[i].id = 0;
	}
}

static void close_ring_fds_locked(void)
{
	for (int i = 0; i < RING_MAX; i++) {
		if (g_frame.ring.fds[i] >= 0)
			close(g_frame.ring.fds[i]);
		g_frame.ring.fds[i] = -1;
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

// Ask the kernel for a dma-buf ring for the running capture `fd`; true — the
// frames go that way (published for the render thread).
static bool start_ring(int fd, const struct cap_spec *box, uint32_t format)
{
	struct cap_dmabuf ring;
	memset(&ring, 0, sizeof(ring));
	for (int i = 0; i < RING_MAX; i++)
		ring.fds[i] = -1;
	if (ioctl(fd, CAP_DMABUF, &ring) != 0) {
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
	close_ring_fds_locked();
	close_pending_locked();
	g_frame.ring = ring;
	g_frame.ring_gen++;
	g_frame.slot = -1;
	g_frame.game = 0;
	g_frame.mode = MODE_RING;
	g_frame.box = *box;
	g_frame.format = format;
	pthread_mutex_unlock(&g_frame_lock);
	return true;
}

// The CPU path for the running capture: two buffers of the box size.
static void start_cpu(const struct cap_spec *box, uint32_t format)
{
	pthread_mutex_lock(&g_frame_lock);
	close_ring_fds_locked();
	close_pending_locked();
	for (int i = 0; i < 2; i++) {
		bfree(g_frame.cpu[i]);
		g_frame.cpu[i] = bmalloc((size_t)box->w * box->h * 4);
	}
	g_frame.ready = -1;
	g_frame.ready_seq = 0;
	g_frame.cpu_gen++;
	g_frame.mode = MODE_CPU;
	g_frame.box = *box;
	g_frame.format = format;
	pthread_mutex_unlock(&g_frame_lock);
}

// One frame through the ring: the newest slot or a game buffer. False — the
// ring is gone (the kernel restarted the capture): ask again.
static bool ring_frame(int fd, uint64_t *have)
{
	struct cap_frame frame;
	memset(&frame, 0, sizeof(frame));
	frame.have = *have;
	pthread_mutex_lock(&g_frame_lock);
	memcpy(frame.known, g_frame.known, sizeof(frame.known));
	// Buffers handed over and not imported yet are known too.
	for (int i = 0; i < GAME_MAX; i++) {
		if (!g_frame.pending[i].id)
			continue;
		for (int k = 0; k < GAME_MAX; k++) {
			if (!frame.known[k]) {
				frame.known[k] = g_frame.pending[i].id;
				break;
			}
		}
	}
	pthread_mutex_unlock(&g_frame_lock);
	if (ioctl(fd, CAP_FRAME, &frame) != 0)
		return errno != ENODATA;
	*have = frame.seq;
	pthread_mutex_lock(&g_frame_lock);
	if (frame.buffer) {
		if (frame.fd >= 0) {
			int free_at = -1;
			for (int i = 0; i < GAME_MAX && free_at < 0; i++)
				if (!g_frame.pending[i].id)
					free_at = i;
			if (free_at < 0) {
				close(frame.fd); // asked again next frame
			} else {
				struct game_buffer *b = &g_frame.pending[free_at];
				b->id = frame.buffer;
				b->fd = frame.fd;
				b->width = frame.width;
				b->height = frame.height;
				b->stride = frame.stride;
				b->fourcc = frame.fourcc;
				b->modifier = frame.modifier;
			}
		}
		g_frame.game = frame.buffer;
		g_frame.mode = MODE_GAME;
	} else if (frame.slot >= 0) {
		g_frame.slot = frame.slot;
		g_frame.game = 0;
		g_frame.mode = MODE_RING;
	}
	pthread_mutex_unlock(&g_frame_lock);
	return true;
}

// One frame through the CPU: grab into the buffer that is not `ready`.
// False — the capture was ended from outside.
static bool cpu_frame(int fd, uint64_t *have)
{
	pthread_mutex_lock(&g_frame_lock);
	int write = g_frame.ready == 0 ? 1 : 0;
	uint32_t *buffer = g_frame.cpu[write];
	pthread_mutex_unlock(&g_frame_lock);
	if (!buffer)
		return true;
	struct cap_pair arg = {(uint64_t)(uintptr_t)buffer, *have};
	long seq = ioctl(fd, CAP_GRAB, &arg);
	if (seq < 0)
		return false;
	if ((uint64_t)seq != *have) {
		*have = (uint64_t)seq;
		pthread_mutex_lock(&g_frame_lock);
		g_frame.ready = write;
		g_frame.ready_seq = (uint64_t)seq;
		pthread_mutex_unlock(&g_frame_lock);
	}
	return true;
}

static void *capture_thread(void *data)
{
	volatile bool *stop = data;
	os_set_thread_name("fossai-capture");
	int fd = -1;
	struct cap_spec spec = {0};
	uint64_t have = 0;
	uint32_t format = pixel_format();
	bool busy_logged = false;
	bool ring = false;

	while (!*stop) {
		uint64_t now = os_gettime_ns();
		// The rate of OBS video (the canvas): 30 or 60 without stutter. Not
		// under `g_lock` - the render thread takes it in get_width.
		struct obs_video_info ovi;
		int video_fps = obs_get_video_info(&ovi) && ovi.fps_den
					? (int)((ovi.fps_num + ovi.fps_den - 1) / ovi.fps_den)
					: 30;
		pthread_mutex_lock(&g_lock);
		// What every source wants now (windows are looked up twice a second).
		int fps = 0;
		bool any = false;
		int32_t x0 = 0, y0 = 0, x1 = 0, y1 = 0;
		uint32_t flags = 0;
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
			const struct cap_spec *w = &cap->want;
			if (!any) {
				x0 = w->x;
				y0 = w->y;
				x1 = w->x + (int32_t)w->w;
				y1 = w->y + (int32_t)w->h;
			} else {
				x0 = w->x < x0 ? w->x : x0;
				y0 = w->y < y0 ? w->y : y0;
				x1 = w->x + (int32_t)w->w > x1 ? w->x + (int32_t)w->w : x1;
				y1 = w->y + (int32_t)w->h > y1 ? w->y + (int32_t)w->h : y1;
			}
			flags |= w->flags;
			any = true;
		}
		pthread_mutex_unlock(&g_lock);
		if (fps <= 0 || fps > 240)
			fps = 30;
		struct cap_spec box = {x0, y0, (uint32_t)(x1 - x0), (uint32_t)(y1 - y0), flags, (uint32_t)fps};
		bool same = fd >= 0 && any && box.x == spec.x && box.y == spec.y && box.w == spec.w && box.h == spec.h &&
			    box.flags == spec.flags;
		if (!same) {
			if (fd >= 0) {
				ioctl(fd, CAP_STOP, 0);
				close(fd);
				fd = -1;
			}
			pthread_mutex_lock(&g_frame_lock);
			g_frame.mode = MODE_NONE;
			pthread_mutex_unlock(&g_frame_lock);
			if (any) {
				fd = open(CAP_DEVICE, O_RDWR | O_CLOEXEC);
				if (fd < 0) {
					blog(LOG_WARNING, "[fossai-capture] %s: %s", CAP_DEVICE, strerror(errno));
				} else if (ioctl(fd, CAP_START, &box) != 0) {
					if (errno == EBUSY) {
						if (!busy_logged)
							blog(LOG_INFO, "[fossai-capture] another capture runs, waiting");
						busy_logged = true;
					} else {
						blog(LOG_WARNING, "[fossai-capture] start %ux%u at %d,%d: %s", box.w, box.h,
						     box.x, box.y, strerror(errno));
					}
					close(fd);
					fd = -1;
				} else {
					busy_logged = false;
					spec = box;
					have = 0;
					blog(LOG_INFO, "[fossai-capture] capturing %ux%u at %d,%d, %d fps, pointer %s", spec.w,
					     spec.h, spec.x, spec.y, fps, (spec.flags & CAP_POINTER) ? "on" : "off");
					ring = g_dmabuf >= 0 && start_ring(fd, &spec, format);
					if (!ring)
						start_cpu(&spec, format);
				}
			}
		}

		if (fd >= 0) {
			// OBS's OpenGL could not import the ring: the CPU from now on.
			if (ring && g_dmabuf < 0) {
				blog(LOG_INFO, "[fossai-capture] frames: CPU copy (OpenGL did not take the dma-buf)");
				ring = false;
				have = 0;
				start_cpu(&spec, format);
			}
			bool alive = ring ? ring_frame(fd, &have) : cpu_frame(fd, &have);
			if (!alive && ring) {
				// The ring is gone with a restarted capture: a new one.
				ring = start_ring(fd, &spec, format);
				have = 0;
				if (!ring)
					start_cpu(&spec, format);
			} else if (!alive) {
				// The capture was ended from outside (Win+Alt+R…): start again.
				close(fd);
				fd = -1;
				spec.w = 0;
			}
		}

		// Next frame time.
		uint64_t period = 1000000000ULL / (uint64_t)fps;
		uint64_t spent = os_gettime_ns() - now;
		if (fd < 0)
			os_sleep_ms(250);
		else if (spent < period)
			os_sleepto_ns(now + period);
	}

	if (fd >= 0) {
		ioctl(fd, CAP_STOP, 0);
		close(fd);
	}
	pthread_mutex_lock(&g_frame_lock);
	g_frame.mode = MODE_NONE;
	close_ring_fds_locked();
	close_pending_locked();
	for (int i = 0; i < 2; i++) {
		bfree(g_frame.cpu[i]);
		g_frame.cpu[i] = NULL;
	}
	g_frame.ready = -1;
	pthread_mutex_unlock(&g_frame_lock);
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

static void drop_ring_textures(void)
{
	for (int i = 0; i < RING_MAX; i++) {
		if (g_render.ring_tex[i])
			gs_texture_destroy(g_render.ring_tex[i]);
		g_render.ring_tex[i] = NULL;
	}
}

static void drop_game_textures(void)
{
	for (int i = 0; i < GAME_MAX; i++) {
		if (g_render.game[i].tex)
			gs_texture_destroy(g_render.game[i].tex);
		g_render.game[i].tex = NULL;
		g_render.game[i].id = 0;
		g_frame.known[i] = 0;
	}
}

static void dmabuf_failed(const char *what)
{
	if (g_dmabuf >= 0)
		blog(LOG_WARNING, "[fossai-capture] dma-buf import failed (%s) - CPU copy from now on", what);
	g_dmabuf = -1;
}

// The texture with the newest frame of the box (under `g_frame_lock`);
// NULL — nothing to show yet.
static gs_texture_t *frame_texture_locked(void)
{
	switch (g_frame.mode) {
	case MODE_RING: {
		if (g_render.ring_gen != g_frame.ring_gen) {
			drop_ring_textures();
			g_render.ring_gen = g_frame.ring_gen;
			const struct cap_dmabuf *r = &g_frame.ring;
			bool ok = true;
			for (uint32_t i = 0; i < r->count && i < RING_MAX; i++) {
				g_render.ring_tex[i] = r->fds[i] >= 0 ? import_dmabuf(r->fds[i], r->width, r->height,
										      r->stride, r->fourcc, r->modifier)
								      : NULL;
				ok = ok && g_render.ring_tex[i];
			}
			// The textures keep the buffers; the descriptors are not needed.
			close_ring_fds_locked();
			if (!ok) {
				drop_ring_textures();
				dmabuf_failed("ring");
				return NULL;
			}
			if (g_dmabuf == 0)
				blog(LOG_INFO, "[fossai-capture] dma-buf import works");
			g_dmabuf = 1;
		}
		if (!g_frame.game)
			drop_game_textures();
		if (g_render.logged != MODE_RING) {
			blog(LOG_INFO, "[fossai-capture] frames: dma-buf ring (one kernel copy, no copy in OBS)");
			g_render.logged = MODE_RING;
		}
		return g_frame.slot >= 0 && g_frame.slot < RING_MAX ? g_render.ring_tex[g_frame.slot] : NULL;
	}
	case MODE_GAME: {
		// Import what the thread handed over.
		for (int i = 0; i < GAME_MAX; i++) {
			struct game_buffer *b = &g_frame.pending[i];
			if (!b->id)
				continue;
			int at = 0;
			for (int k = 0; k < GAME_MAX; k++) {
				if (g_render.game[k].id == b->id || !g_render.game[k].tex) {
					at = k;
					break;
				}
				if (g_render.game[k].used < g_render.game[at].used)
					at = k;
			}
			if (g_render.game[at].tex)
				gs_texture_destroy(g_render.game[at].tex);
			g_render.game[at].tex =
				import_dmabuf(b->fd, b->width, b->height, b->stride, b->fourcc, b->modifier);
			g_render.game[at].id = g_render.game[at].tex ? b->id : 0;
			g_render.game[at].used = g_render.frames;
			g_frame.known[at] = g_render.game[at].id;
			if (!g_render.game[at].tex)
				dmabuf_failed("game buffer");
			else
				blog(LOG_INFO, "[fossai-capture] game buffer 0x%llx %ux%u imported",
				     (unsigned long long)b->id, b->width, b->height);
			close(b->fd);
			b->fd = -1;
			b->id = 0;
		}
		if (g_render.logged != MODE_GAME) {
			blog(LOG_INFO, "[fossai-capture] frames: the fullscreen game's own buffer (no copy at all)");
			g_render.logged = MODE_GAME;
		}
		for (int k = 0; k < GAME_MAX; k++) {
			if (g_render.game[k].tex && g_render.game[k].id == g_frame.game) {
				g_render.game[k].used = g_render.frames;
				return g_render.game[k].tex;
			}
		}
		// Not imported yet: the last ring frame meanwhile.
		return g_frame.slot >= 0 && g_frame.slot < RING_MAX ? g_render.ring_tex[g_frame.slot] : NULL;
	}
	case MODE_CPU: {
		if (g_render.logged != MODE_CPU) {
			blog(LOG_INFO, "[fossai-capture] frames: CPU copy (grab + texture upload)");
			g_render.logged = MODE_CPU;
		}
		const struct cap_spec *box = &g_frame.box;
		if (g_render.cpu_tex &&
		    (g_render.cpu_w != box->w || g_render.cpu_h != box->h || g_render.cpu_format != g_frame.format)) {
			gs_texture_destroy(g_render.cpu_tex);
			g_render.cpu_tex = NULL;
		}
		if (g_render.cpu_gen != g_frame.cpu_gen) {
			g_render.cpu_gen = g_frame.cpu_gen;
			g_render.cpu_seq = 0;
		}
		if (!g_render.cpu_tex && box->w && box->h) {
			g_render.cpu_tex = gs_texture_create(box->w, box->h, gs_format_of(g_frame.format), 1, NULL,
							     GS_DYNAMIC);
			g_render.cpu_w = box->w;
			g_render.cpu_h = box->h;
			g_render.cpu_format = g_frame.format;
			g_render.cpu_seq = 0;
		}
		if (g_render.cpu_tex && g_frame.ready >= 0 && g_frame.ready_seq != g_render.cpu_seq &&
		    g_frame.cpu[g_frame.ready]) {
			gs_texture_set_image(g_render.cpu_tex, (const uint8_t *)g_frame.cpu[g_frame.ready], box->w * 4,
					     false);
			g_render.cpu_seq = g_frame.ready_seq;
		}
		return g_render.cpu_seq ? g_render.cpu_tex : NULL;
	}
	default:
		return NULL;
	}
}

static void capture_render(void *data, gs_effect_t *unused)
{
	UNUSED_PARAMETER(unused);
	struct fossai_capture *cap = data;
	pthread_mutex_lock(&g_lock);
	struct cap_spec want = cap->want;
	bool want_ok = cap->want_ok;
	pthread_mutex_unlock(&g_lock);
	if (!want_ok)
		return;

	pthread_mutex_lock(&g_frame_lock);
	g_render.frames++;
	gs_texture_t *tex = frame_texture_locked();
	const struct cap_spec box = g_frame.box;
	if (tex && want.x >= box.x && want.y >= box.y && want.x + (int32_t)want.w <= box.x + (int32_t)box.w &&
	    want.y + (int32_t)want.h <= box.y + (int32_t)box.h) {
		gs_effect_t *effect = obs_get_base_effect(OBS_EFFECT_OPAQUE);
		const bool linear_srgb = gs_get_linear_srgb();
		const bool previous = gs_framebuffer_srgb_enabled();
		gs_enable_framebuffer_srgb(linear_srgb);
		gs_eparam_t *image = gs_effect_get_param_by_name(effect, "image");
		if (linear_srgb)
			gs_effect_set_texture_srgb(image, tex);
		else
			gs_effect_set_texture(image, tex);
		while (gs_effect_loop(effect, "Draw"))
			gs_draw_sprite_subregion(tex, 0, (uint32_t)(want.x - box.x), (uint32_t)(want.y - box.y), want.w,
						 want.h);
		gs_enable_framebuffer_srgb(previous);
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
	// The last source: the thread stops the kernel capture and ends; the
	// textures go with it.
	if (last) {
		pthread_join(thread, NULL);
		bfree((void *)stop);
		obs_enter_graphics();
		pthread_mutex_lock(&g_frame_lock);
		drop_ring_textures();
		drop_game_textures();
		if (g_render.cpu_tex)
			gs_texture_destroy(g_render.cpu_tex);
		g_render.cpu_tex = NULL;
		g_render.ring_gen = 0;
		g_frame.ring_gen = 0;
		g_render.logged = MODE_NONE;
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
	obs_properties_add_bool(props, "show_cursor", "Показывать указатель");
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
	obs_properties_add_bool(props, "show_cursor", "Показывать указатель");
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
	// No descriptors yet (0 is a real one).
	for (int i = 0; i < RING_MAX; i++)
		g_frame.ring.fds[i] = -1;
	for (int i = 0; i < GAME_MAX; i++)
		g_frame.pending[i].fd = -1;
	obs_register_source(&screen_info);
	obs_register_source(&window_info);
	int fd = open(CAP_DEVICE, O_RDWR | O_CLOEXEC);
	blog(LOG_INFO, "[fossai-capture] loaded (%s)", fd >= 0 ? "device present" : "no device");
	if (fd >= 0)
		close(fd);
	probe_dmabuf();
	return true;
}
