// OBS Studio module «Захват экрана FossAi» (23.11.2026): screen and window
// capture through the kernel capture of FossAi (`/dev/fossai-capture`,
// crates/kernel/src/linux/fossaicap.rs), instead of PipeWire and the
// xdg-desktop-portal OBS uses on Linux/Wayland.
//
// The kernel keeps a mirror of the captured rectangle filled by the
// compositor; a grab is one copy into our buffer and answers a sequence
// number (the same number — nothing changed, nothing copied). The module is
// an asynchronous video source: a thread grabs at the chosen rate and hands
// changed frames to OBS (`obs_source_output_video`), which converts them on
// the GPU like any camera frame.
//
// Two sources:
//  * fossai_screen_capture — one monitor (by its connector name, `DP-1`; by
//    default the primary one) or the whole desktop. The monitors come from
//    `read` of the device (24.11): `x\ty\tw\th\tprimary\tname\tdescription`
//    lines, the primary first, in capture coordinates;
//  * fossai_window_capture — the rectangle of a window by its title
//    (CAP_WINDOWS), followed when the window moves or resizes. It shows what
//    is on the screen there (windows above it too), like a region capture.
// The kernel runs one capture at a time: a second source waits (EBUSY) and
// takes over when the first one stops.
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

#define CAP_POINTER 1u
#define PIXEL_RGB 1u
#define PIXEL_BGR 2u

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
			if (used > 0)
				snprintf(m->description, sizeof(m->description), "%s", line + used);
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
// sources want and hands each source its part (a frame pointing into the
// shared buffer, no copy).
static pthread_mutex_t g_lock = PTHREAD_MUTEX_INITIALIZER;
static struct fossai_capture *g_sources;
static pthread_t g_thread;
static bool g_thread_running;
// The running thread's own stop flag (a thread being joined keeps its flag;
// a new one started meanwhile gets a fresh one).
static volatile bool *g_stop;

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

static void *capture_thread(void *data)
{
	volatile bool *stop = data;
	os_set_thread_name("fossai-capture");
	int fd = -1;
	struct cap_spec spec = {0};
	uint32_t *buffer = NULL;
	uint64_t have = 0;
	uint32_t format = pixel_format();
	bool busy_logged = false;

	while (!*stop) {
		uint64_t now = os_gettime_ns();
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
			if (want_fps <= 0) {
				// As OBS video (the canvas rate): 30 or 60 without stutter.
				struct obs_video_info ovi;
				want_fps = obs_get_video_info(&ovi) && ovi.fps_den
						   ? (int)((ovi.fps_num + ovi.fps_den - 1) / ovi.fps_den)
						   : 30;
			}
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
					bfree(buffer);
					buffer = bmalloc((size_t)spec.w * spec.h * 4);
					blog(LOG_INFO, "[fossai-capture] capturing %ux%u at %d,%d, %d fps, pointer %s", spec.w,
					     spec.h, spec.x, spec.y, fps, (spec.flags & CAP_POINTER) ? "on" : "off");
				}
			}
		}

		if (fd >= 0 && buffer) {
			struct cap_pair arg = {(uint64_t)(uintptr_t)buffer, have};
			long seq = ioctl(fd, CAP_GRAB, &arg);
			if (seq < 0) {
				// The capture was ended from outside (Win+Alt+R…): start again.
				close(fd);
				fd = -1;
			} else if ((uint64_t)seq != have) {
				have = (uint64_t)seq;
				if (format == PIXEL_RGB) {
					// No RGBX in OBS: make the alpha byte opaque.
					size_t n = (size_t)spec.w * spec.h;
					for (size_t i = 0; i < n; i++)
						buffer[i] |= 0xFF000000u;
				}
				// Each source gets its rectangle of the box (OBS copies the
				// frame; the rows are as they are, nothing is scaled).
				for (struct fossai_capture *cap = g_sources; cap; cap = cap->next) {
					const struct cap_spec *w = &cap->want;
					if (!cap->want_ok || w->x < spec.x || w->y < spec.y ||
					    w->x + (int32_t)w->w > spec.x + (int32_t)spec.w ||
					    w->y + (int32_t)w->h > spec.y + (int32_t)spec.h)
						continue;
					struct obs_source_frame frame = {0};
					size_t offset = (size_t)(w->y - spec.y) * spec.w + (size_t)(w->x - spec.x);
					frame.data[0] = (uint8_t *)(buffer + offset);
					frame.linesize[0] = spec.w * 4;
					frame.width = w->w;
					frame.height = w->h;
					frame.format = format == PIXEL_RGB ? VIDEO_FORMAT_RGBA : VIDEO_FORMAT_BGRX;
					frame.timestamp = now;
					frame.full_range = true;
					obs_source_output_video(cap->source, &frame);
				}
			}
		}
		pthread_mutex_unlock(&g_lock);

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
	bfree(buffer);
	return NULL;
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
	obs_source_set_async_unbuffered(source, true);
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
	// The last source: the thread stops the kernel capture and ends.
	if (last) {
		pthread_join(thread, NULL);
		bfree((void *)stop);
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
		snprintf(text, sizeof(text), "Монитор %d — %s %d×%d%s%s%s", i + 1, m->name, m->w, m->h,
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

static struct obs_source_info screen_info = {
	.id = "fossai_screen_capture",
	.type = OBS_SOURCE_TYPE_INPUT,
	.output_flags = OBS_SOURCE_ASYNC_VIDEO | OBS_SOURCE_DO_NOT_DUPLICATE,
	.get_name = screen_name,
	.create = screen_create,
	.destroy = capture_destroy,
	.update = capture_update,
	.get_defaults = capture_defaults,
	.get_properties = screen_properties,
	.icon_type = OBS_ICON_TYPE_DESKTOP_CAPTURE,
};

static struct obs_source_info window_info = {
	.id = "fossai_window_capture",
	.type = OBS_SOURCE_TYPE_INPUT,
	.output_flags = OBS_SOURCE_ASYNC_VIDEO | OBS_SOURCE_DO_NOT_DUPLICATE,
	.get_name = window_name,
	.create = window_create,
	.destroy = capture_destroy,
	.update = capture_update,
	.get_defaults = capture_defaults,
	.get_properties = window_properties,
	.icon_type = OBS_ICON_TYPE_WINDOW_CAPTURE,
};

bool obs_module_load(void)
{
	obs_register_source(&screen_info);
	obs_register_source(&window_info);
	int fd = open(CAP_DEVICE, O_RDWR | O_CLOEXEC);
	blog(LOG_INFO, "[fossai-capture] loaded (%s)", fd >= 0 ? "device present" : "no device");
	if (fd >= 0)
		close(fd);
	return true;
}
