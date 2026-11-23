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
//  * fossai_screen_capture — the whole desktop or one monitor (CAP_INFO);
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
	int monitor; // -1 — the whole desktop
	char *window;
	bool cursor;
	int fps;
	bool changed;

	pthread_t thread;
	bool thread_running;
	volatile bool stop;
};

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
	int monitor = cap->monitor;
	char *window = cap->window ? bstrdup(cap->window) : NULL;
	spec->flags = cap->cursor ? CAP_POINTER : 0;
	spec->fps = (uint32_t)cap->fps;
	pthread_mutex_unlock(&cap->lock);

	bool ok = false;
	if (cap->window_mode) {
		ok = window && *window && find_window(window, spec);
	} else {
		struct cap_info info;
		if (read_info(&info) && info.desk_w && info.desk_h) {
			if (monitor >= 0 && (uint32_t)monitor < info.count) {
				spec->x = info.outputs[monitor][0];
				spec->y = info.outputs[monitor][1];
				spec->w = (uint32_t)info.outputs[monitor][2];
				spec->h = (uint32_t)info.outputs[monitor][3];
			} else {
				spec->x = 0;
				spec->y = 0;
				spec->w = info.desk_w;
				spec->h = info.desk_h;
			}
			ok = spec->w && spec->h;
		}
	}
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
	struct fossai_capture *cap = data;
	os_set_thread_name("fossai-capture");
	int fd = -1;
	struct cap_spec spec = {0};
	uint32_t *buffer = NULL;
	uint64_t have = 0;
	uint64_t next_check = 0;
	uint32_t format = pixel_format();
	bool busy_logged = false;

	while (!cap->stop) {
		uint64_t now = os_gettime_ns();
		pthread_mutex_lock(&cap->lock);
		bool changed = cap->changed;
		cap->changed = false;
		int fps = cap->fps > 0 ? cap->fps : 30;
		pthread_mutex_unlock(&cap->lock);

		// Settings changed, a window moved, or not capturing yet: (re)start.
		if (changed || fd < 0 || (cap->window_mode && now >= next_check)) {
			struct cap_spec want = {0};
			bool ok = wanted_rect(cap, &want);
			next_check = now + 500000000ULL;
			bool same = fd >= 0 && ok && want.x == spec.x && want.y == spec.y && want.w == spec.w &&
				    want.h == spec.h && want.flags == spec.flags && !changed;
			if (!same) {
				if (fd >= 0) {
					ioctl(fd, CAP_STOP, 0);
					close(fd);
					fd = -1;
				}
				if (ok) {
					fd = open(CAP_DEVICE, O_RDWR | O_CLOEXEC);
					if (fd < 0) {
						blog(LOG_WARNING, "[fossai-capture] %s: %s", CAP_DEVICE, strerror(errno));
					} else if (ioctl(fd, CAP_START, &want) != 0) {
						if (errno == EBUSY) {
							if (!busy_logged)
								blog(LOG_INFO, "[fossai-capture] another capture runs, waiting");
							busy_logged = true;
						} else {
							blog(LOG_WARNING, "[fossai-capture] start %ux%u at %d,%d: %s", want.w,
							     want.h, want.x, want.y, strerror(errno));
						}
						close(fd);
						fd = -1;
					} else {
						busy_logged = false;
						spec = want;
						have = 0;
						bfree(buffer);
						buffer = bmalloc((size_t)spec.w * spec.h * 4);
						blog(LOG_INFO, "[fossai-capture] capturing %ux%u at %d,%d, %d fps, pointer %s",
						     spec.w, spec.h, spec.x, spec.y, fps, (spec.flags & CAP_POINTER) ? "on" : "off");
					}
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
				struct obs_source_frame frame = {0};
				frame.data[0] = (uint8_t *)buffer;
				frame.linesize[0] = spec.w * 4;
				frame.width = spec.w;
				frame.height = spec.h;
				frame.format = format == PIXEL_RGB ? VIDEO_FORMAT_RGBA : VIDEO_FORMAT_BGRX;
				frame.timestamp = now;
				frame.full_range = true;
				obs_source_output_video(cap->source, &frame);
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
	bfree(buffer);
	return NULL;
}

static void apply_settings(struct fossai_capture *cap, obs_data_t *settings)
{
	pthread_mutex_lock(&cap->lock);
	cap->monitor = (int)obs_data_get_int(settings, "monitor");
	bfree(cap->window);
	cap->window = bstrdup(obs_data_get_string(settings, "window"));
	cap->cursor = obs_data_get_bool(settings, "show_cursor");
	cap->fps = (int)obs_data_get_int(settings, "fps");
	if (cap->fps <= 0 || cap->fps > 240)
		cap->fps = 30;
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
	if (pthread_create(&cap->thread, NULL, capture_thread, cap) == 0)
		cap->thread_running = true;
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
	cap->stop = true;
	if (cap->thread_running)
		pthread_join(cap->thread, NULL);
	pthread_mutex_destroy(&cap->lock);
	bfree(cap->window);
	bfree(cap);
}

static void capture_update(void *data, obs_data_t *settings)
{
	apply_settings(data, settings);
}

static void capture_defaults(obs_data_t *settings)
{
	obs_data_set_default_int(settings, "monitor", -1);
	obs_data_set_default_string(settings, "window", "");
	obs_data_set_default_bool(settings, "show_cursor", true);
	obs_data_set_default_int(settings, "fps", 30);
}

static obs_properties_t *screen_properties(void *data)
{
	UNUSED_PARAMETER(data);
	obs_properties_t *props = obs_properties_create();
	obs_property_t *list =
		obs_properties_add_list(props, "monitor", "Монитор", OBS_COMBO_TYPE_LIST, OBS_COMBO_FORMAT_INT);
	struct cap_info info;
	if (read_info(&info)) {
		char text[96];
		snprintf(text, sizeof(text), "Весь рабочий стол (%ux%u)", info.desk_w, info.desk_h);
		obs_property_list_add_int(list, text, -1);
		for (uint32_t i = 0; i < info.count && i < 8; i++) {
			snprintf(text, sizeof(text), "Монитор %u%s (%dx%d в %d,%d)", i + 1, i == 0 ? ", основной" : "",
				 info.outputs[i][2], info.outputs[i][3], info.outputs[i][0], info.outputs[i][1]);
			obs_property_list_add_int(list, text, (long long)i);
		}
	} else {
		obs_property_list_add_int(list, "Весь рабочий стол", -1);
	}
	obs_properties_add_bool(props, "show_cursor", "Показывать указатель");
	obs_properties_add_int(props, "fps", "Кадров в секунду", 1, 240, 1);
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
	obs_properties_add_int(props, "fps", "Кадров в секунду", 1, 240, 1);
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
