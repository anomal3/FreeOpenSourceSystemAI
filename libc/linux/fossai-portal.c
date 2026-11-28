// fossai-portal: screen sharing for Linux programs on FossAi (28.11.2026).
//
// Chromium on Wayland takes the screen only through xdg-desktop-portal
// (D-Bus service org.freedesktop.portal.Desktop, interface
// org.freedesktop.portal.ScreenCast) and a PipeWire video stream. FossAi has
// neither a desktop portal nor a screen-sharing compositor protocol, so this
// program is both halves:
//
// * the portal: on the session bus (dbus-daemon of Debian, started by the
//   Chromium launcher) it owns org.freedesktop.portal.Desktop and answers
//   ScreenCast CreateSession / SelectSources / Start / OpenPipeWireRemote
//   (version 4, monitors only, cursor hidden or embedded), the Request
//   objects' Response signals, Session.Close, Properties and Introspect.
//   Other portal interfaces are unknown - Chromium falls back (its file
//   chooser asks for FileChooser's version and gets an error).
// * the producer: for every started session a PipeWire stream (Video/Source,
//   a driver) on the pipewire daemon of Debian, its frames taken from the
//   kernel capture of the chosen monitor (/dev/fossai-capture, FOSSAI_CAP_*,
//   the same sessions OBS and the screen recorder use), BGRx/RGBx in memfd
//   buffers the stream allocates itself (WebRTC drops MemPtr buffers). There is no session manager (WirePlumber): the producer links
//   its output port to every video input stream that targets its node.
//
// The monitor: FOSSAI_SCREENCAST_MONITOR=N (1 - the primary), else the
// primary one. Log lines "fossai-portal: ..." go to stderr and /dev/kmsg.
// FOSSAI_PORTAL_TEST=1 - a test picture instead of the kernel capture;
// FOSSAI_PORTAL_READY=file - made once the name is ours.

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <time.h>
#include <unistd.h>

#include <dbus/dbus.h>
#include <pipewire/pipewire.h>
#include <spa/param/video/format-utils.h>
#include <spa/param/buffers.h>
#include <spa/buffer/meta.h>

// ---- kernel capture (user_abi CapInfo/CapSpec/CapStatus) ------------------

#define CAP_DEVICE "/dev/fossai-capture"
#define CAP_IOC(nr) _IOC(_IOC_READ | _IOC_WRITE, 'F', (nr), 0)
#define CAP_INFO CAP_IOC(1)
#define CAP_START CAP_IOC(2)
#define CAP_GRAB CAP_IOC(3)
#define CAP_STOP CAP_IOC(4)
#define CAP_STATUS CAP_IOC(5)
#define CAP_POINTER 1u
#define PIXEL_RGB 1u

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

struct cap_status {
	uint32_t active;
	uint32_t stop_requested;
	uint32_t w, h;
	uint64_t elapsed_ms;
	uint64_t seq;
};

// FOSSAI_PORTAL_TEST=1: no kernel capture - one 640x360 "monitor" with a
// moving test picture, 30 frames a second (checks of the portal and the
// PipeWire side on any Linux machine).
static bool g_test;
static uint32_t g_test_w = 640, g_test_h = 360;

static uint64_t test_seq(void)
{
	struct timespec ts;
	clock_gettime(CLOCK_MONOTONIC, &ts);
	return ((uint64_t)ts.tv_sec * 1000 + (uint64_t)ts.tv_nsec / 1000000) / 33;
}

static long cap_ioctl(int fd, unsigned long request, void *arg)
{
	if (!g_test)
		return ioctl(fd, request, arg);
	if (request == CAP_INFO) {
		struct cap_info *info = arg;
		memset(info, 0, sizeof(*info));
		info->count = 1;
		info->format = 2;
		info->desk_w = g_test_w;
		info->desk_h = g_test_h;
		info->outputs[0][2] = (int32_t)g_test_w;
		info->outputs[0][3] = (int32_t)g_test_h;
		return 0;
	}
	if (request == CAP_STATUS) {
		struct cap_status *st = arg;
		memset(st, 0, sizeof(*st));
		st->active = 1;
		st->w = g_test_w;
		st->h = g_test_h;
		st->seq = test_seq();
		return 0;
	}
	if (request == CAP_GRAB) {
		uint64_t *pair = arg;
		uint32_t *px = (uint32_t *)(uintptr_t)pair[0];
		uint64_t seq = test_seq();
		for (uint32_t y = 0; y < g_test_h; y++)
			for (uint32_t x = 0; x < g_test_w; x++)
				px[y * g_test_w + x] = ((x + seq * 4) & 0xff) << 16 | (y & 0xff) << 8 | 0x40;
		return (long)seq;
	}
	return 0;
}

// ---- log --------------------------------------------------------------------

static int g_kmsg = -1;

static void say(const char *fmt, ...)
{
	char line[512];
	int n = snprintf(line, sizeof(line), "fossai-portal: ");
	va_list ap;
	va_start(ap, fmt);
	vsnprintf(line + n, sizeof(line) - (size_t)n - 1, fmt, ap);
	va_end(ap);
	size_t len = strlen(line);
	line[len++] = '\n';
	line[len] = 0;
	fputs(line, stderr);
	if (g_kmsg >= 0 && write(g_kmsg, line, len) < 0) {
		// The kernel log is only a copy.
	}
}

static uint64_t now_ms(void)
{
	struct timespec ts;
	clock_gettime(CLOCK_MONOTONIC, &ts);
	return (uint64_t)ts.tv_sec * 1000 + (uint64_t)ts.tv_nsec / 1000000;
}

// ---- state --------------------------------------------------------------------

#define FPS 30
#define MAX_SESSIONS 16
#define MAX_OBJECTS 128
#define MAX_LINKS 4

// One screen stream (a started session).
struct cast {
	struct pw_stream *stream;
	struct spa_hook listener;
	struct spa_source *timer;
	int fd; // /dev/fossai-capture with this cast's kernel capture
	struct cap_spec spec;
	uint32_t format; // SPA_VIDEO_FORMAT_*
	uint32_t node_id;
	uint32_t out_port;
	bool failed;
	bool streaming;
	uint64_t seq; // kernel frame number last sent
	uint64_t sent_ms;
	uint64_t frames;
	uint64_t starved; // cycles without a free buffer
	uint32_t buffers; // memfd buffers allocated
	uint64_t started_ms;
	struct pw_proxy *links[MAX_LINKS];
	uint32_t linked[MAX_LINKS]; // consumer node of each link
};

struct session {
	bool used;
	char path[256];
	char sender[64];
	uint32_t cursor_mode;
	int monitor; // index into cap_info.outputs
	struct cast *cast;
};

// Registry objects the producer links (nodes and ports of other clients).
struct object {
	uint32_t id;
	uint32_t type; // 1 node, 2 port
	uint32_t node; // port: its node
	bool input; // port: direction in; node: a video input stream
	char target[32]; // node: target.object / node.target
};

static struct pw_thread_loop *g_loop;
static struct pw_context *g_context;
static struct pw_core *g_core;
static struct pw_registry *g_registry;
static struct spa_hook g_core_listener, g_registry_listener;
static struct session g_sessions[MAX_SESSIONS];
static struct object g_objects[MAX_OBJECTS];
static DBusConnection *g_bus;
static int g_next_cast = 1;

// ---- PipeWire producer ---------------------------------------------------------

static struct object *object_find(uint32_t id)
{
	for (int i = 0; i < MAX_OBJECTS; i++)
		if (g_objects[i].id == id && g_objects[i].type)
			return &g_objects[i];
	return NULL;
}

static bool linked_to(struct cast *c, uint32_t node)
{
	for (int i = 0; i < MAX_LINKS; i++)
		if (c->links[i] && c->linked[i] == node)
			return true;
	return false;
}

static bool target_is(const struct object *node, const struct cast *c)
{
	if (!node->target[0])
		return false;
	char id[16];
	snprintf(id, sizeof(id), "%u", c->node_id);
	return strcmp(node->target, id) == 0;
}

// Link every video input stream that targets a cast (or targets nothing
// while exactly one cast runs) to the cast's output port. Loop locked.
static void try_links(void)
{
	struct cast *only = NULL;
	int casts = 0;
	for (int i = 0; i < MAX_SESSIONS; i++) {
		struct cast *c = g_sessions[i].used ? g_sessions[i].cast : NULL;
		if (c && c->node_id && c->out_port) {
			only = c;
			casts++;
		}
	}
	for (int n = 0; n < MAX_OBJECTS; n++) {
		struct object *node = &g_objects[n];
		if (node->type != 1 || !node->input)
			continue;
		struct object *port = NULL;
		for (int p = 0; p < MAX_OBJECTS; p++)
			if (g_objects[p].type == 2 && g_objects[p].node == node->id && g_objects[p].input)
				port = &g_objects[p];
		if (!port)
			continue;
		struct cast *to = NULL;
		for (int i = 0; i < MAX_SESSIONS && !to; i++) {
			struct cast *c = g_sessions[i].used ? g_sessions[i].cast : NULL;
			if (c && c->node_id && c->out_port && target_is(node, c))
				to = c;
		}
		if (!to && !node->target[0] && casts == 1)
			to = only;
		if (!to || linked_to(to, node->id))
			continue;
		int slot = -1;
		for (int i = 0; i < MAX_LINKS; i++)
			if (!to->links[i])
				slot = i;
		if (slot < 0)
			continue;
		char out_node[16], out_port[16], in_node[16], in_port[16];
		snprintf(out_node, sizeof(out_node), "%u", to->node_id);
		snprintf(out_port, sizeof(out_port), "%u", to->out_port);
		snprintf(in_node, sizeof(in_node), "%u", node->id);
		snprintf(in_port, sizeof(in_port), "%u", port->id);
		struct pw_properties *props = pw_properties_new(PW_KEY_LINK_OUTPUT_NODE, out_node, PW_KEY_LINK_OUTPUT_PORT, out_port,
								PW_KEY_LINK_INPUT_NODE, in_node, PW_KEY_LINK_INPUT_PORT, in_port,
								PW_KEY_OBJECT_LINGER, "false", NULL);
		to->links[slot] = pw_core_create_object(g_core, "link-factory", PW_TYPE_INTERFACE_Link, PW_VERSION_LINK, &props->dict, 0);
		to->linked[slot] = node->id;
		pw_properties_free(props);
		say("link node %u port %u -> node %u port %u (%s)", to->node_id, to->out_port, node->id, port->id,
		    to->links[slot] ? "made" : "refused");
	}
}

static void registry_global(void *data, uint32_t id, uint32_t permissions, const char *type, uint32_t version,
			    const struct spa_dict *props)
{
	(void)data;
	(void)permissions;
	(void)version;
	bool is_node = strcmp(type, PW_TYPE_INTERFACE_Node) == 0;
	bool is_port = strcmp(type, PW_TYPE_INTERFACE_Port) == 0;
	if ((!is_node && !is_port) || !props)
		return;
	struct object o = {.id = id, .type = is_node ? 1 : 2};
	if (is_node) {
		const char *klass = spa_dict_lookup(props, PW_KEY_MEDIA_CLASS);
		const char *target = spa_dict_lookup(props, PW_KEY_TARGET_OBJECT);
		if (!target)
			target = spa_dict_lookup(props, "node.target");
		o.input = klass && strcmp(klass, "Stream/Input/Video") == 0;
		if (target)
			snprintf(o.target, sizeof(o.target), "%s", target);
		if (o.input) {
			const char *name = spa_dict_lookup(props, PW_KEY_NODE_NAME);
			say("video input stream: node %u '%s', target %s", id, name ? name : "?", o.target[0] ? o.target : "none");
		}
	} else {
		const char *node = spa_dict_lookup(props, PW_KEY_NODE_ID);
		const char *dir = spa_dict_lookup(props, PW_KEY_PORT_DIRECTION);
		if (!node || !dir)
			return;
		o.node = (uint32_t)atoi(node);
		o.input = strcmp(dir, "in") == 0;
		// Our own output port.
		for (int i = 0; i < MAX_SESSIONS; i++) {
			struct cast *c = g_sessions[i].used ? g_sessions[i].cast : NULL;
			if (c && c->node_id && c->node_id == o.node && !o.input && !c->out_port) {
				c->out_port = id;
				say("stream node %u: output port %u", c->node_id, id);
			}
		}
	}
	for (int i = 0; i < MAX_OBJECTS; i++) {
		if (!g_objects[i].type) {
			g_objects[i] = o;
			break;
		}
	}
	try_links();
}

static void registry_global_remove(void *data, uint32_t id)
{
	(void)data;
	struct object *o = object_find(id);
	if (o)
		o->type = 0;
	for (int i = 0; i < MAX_SESSIONS; i++) {
		struct cast *c = g_sessions[i].used ? g_sessions[i].cast : NULL;
		if (!c)
			continue;
		for (int l = 0; l < MAX_LINKS; l++) {
			if (c->links[l] && c->linked[l] == id) {
				say("stream node %u: consumer node %u went away", c->node_id, id);
				pw_proxy_destroy(c->links[l]);
				c->links[l] = NULL;
			}
		}
	}
}

static const struct pw_registry_events registry_events = {
	PW_VERSION_REGISTRY_EVENTS,
	.global = registry_global,
	.global_remove = registry_global_remove,
};

static void core_error(void *data, uint32_t id, int seq, int res, const char *message)
{
	(void)data;
	(void)seq;
	say("pipewire error on object %u: %s (%d)", id, message, res);
}

static const struct pw_core_events core_events = {
	PW_VERSION_CORE_EVENTS,
	.error = core_error,
};

static void cast_state(void *data, enum pw_stream_state old, enum pw_stream_state state, const char *error)
{
	(void)old;
	struct cast *c = data;
	say("stream %u: %s%s%s", c->node_id, pw_stream_state_as_string(state), error ? " - " : "", error ? error : "");
	if (!c->node_id && state >= PW_STREAM_STATE_PAUSED) {
		c->node_id = pw_stream_get_node_id(c->stream);
		say("stream node %u for monitor %ux%u at %d,%d", c->node_id, c->spec.w, c->spec.h, c->spec.x, c->spec.y);
		pw_thread_loop_signal(g_loop, false);
	}
	if (state == PW_STREAM_STATE_ERROR) {
		c->failed = true;
		pw_thread_loop_signal(g_loop, false);
	}
	c->streaming = state == PW_STREAM_STATE_STREAMING;
	if (c->streaming)
		c->sent_ms = 0;
}

static void cast_param_changed(void *data, uint32_t id, const struct spa_pod *param)
{
	struct cast *c = data;
	if (id != SPA_PARAM_Format || !param)
		return;
	struct spa_video_info_raw info;
	if (spa_format_video_raw_parse(param, &info) < 0)
		return;
	say("stream %u: format %ux%u, format id %u, max %u/%u fps", c->node_id, info.size.width, info.size.height, info.format,
	    info.max_framerate.num, info.max_framerate.denom);
	uint8_t buffer[1024];
	struct spa_pod_builder b = SPA_POD_BUILDER_INIT(buffer, sizeof(buffer));
	const struct spa_pod *params[2];
	int stride = (int)c->spec.w * 4;
	params[0] = spa_pod_builder_add_object(
		&b, SPA_TYPE_OBJECT_ParamBuffers, SPA_PARAM_Buffers, SPA_PARAM_BUFFERS_buffers, SPA_POD_CHOICE_RANGE_Int(4, 2, 8),
		SPA_PARAM_BUFFERS_blocks, SPA_POD_Int(1), SPA_PARAM_BUFFERS_size, SPA_POD_Int(stride * (int)c->spec.h),
		SPA_PARAM_BUFFERS_stride, SPA_POD_Int(stride), SPA_PARAM_BUFFERS_dataType,
		// memfd only: WebRTC (shared_screencast_stream.cc) takes MemFd and
		// DmaBuf buffers and silently drops MemPtr ones - with MemPtr offered
		// too, the link settled on MemPtr and Chromium showed no frame.
		SPA_POD_CHOICE_FLAGS_Int(1 << SPA_DATA_MemFd));
	params[1] = spa_pod_builder_add_object(&b, SPA_TYPE_OBJECT_ParamMeta, SPA_PARAM_Meta, SPA_PARAM_META_type,
					       SPA_POD_Id(SPA_META_Header), SPA_PARAM_META_size,
					       SPA_POD_Int(sizeof(struct spa_meta_header)));
	pw_stream_update_params(c->stream, params, 2);
}

// One frame into the next free buffer (the driver's process).
static void cast_process(void *data)
{
	struct cast *c = data;
	struct pw_buffer *b = pw_stream_dequeue_buffer(c->stream);
	if (!b) {
		// Every buffer is with the consumer: it does not give them back.
		c->starved++;
		if (c->starved == 1 || c->starved % 100 == 0)
			say("stream %u: no free buffer (%llu times) - the consumer keeps them", c->node_id,
			    (unsigned long long)c->starved);
		return;
	}
	struct spa_buffer *buf = b->buffer;
	struct spa_data *d = &buf->datas[0];
	uint32_t stride = c->spec.w * 4;
	uint32_t size = stride * c->spec.h;
	if (!d->data || d->maxsize < size) {
		if (c->frames == 0)
			say("stream %u: buffer unusable (data %p, %u bytes, need %u)", c->node_id, d->data, d->maxsize, size);
		d->chunk->size = 0;
		pw_stream_queue_buffer(c->stream, b);
		return;
	}
	uint64_t arg[2] = {(uint64_t)(uintptr_t)d->data, 0};
	long seq = cap_ioctl(c->fd, CAP_GRAB, arg);
	if (seq < 0) {
		if (c->frames == 0)
			say("stream %u: grab failed: %s", c->node_id, strerror(errno));
		d->chunk->size = 0;
		pw_stream_queue_buffer(c->stream, b);
		return;
	}
	c->seq = (uint64_t)seq;
	d->chunk->offset = 0;
	d->chunk->stride = (int32_t)stride;
	d->chunk->size = size;
	struct spa_meta_header *h = spa_buffer_find_meta_data(buf, SPA_META_Header, sizeof(*h));
	if (h) {
		struct timespec ts;
		clock_gettime(CLOCK_MONOTONIC, &ts);
		h->pts = SPA_TIMESPEC_TO_NSEC(&ts);
		h->flags = 0;
		h->seq = c->frames;
		h->dts_offset = 0;
	}
	pw_stream_queue_buffer(c->stream, b);
	c->frames++;
	if (c->frames <= 3 || c->frames == 10 || c->frames % 300 == 0) {
		uint64_t ms = now_ms() - c->started_ms;
		say("stream %u: frame %llu (kernel frame %llu) after %llu ms", c->node_id, (unsigned long long)c->frames,
		    (unsigned long long)c->seq, (unsigned long long)ms);
	}
}

// The stream allocates its buffers (PW_STREAM_FLAG_ALLOC_BUFFERS): one memfd
// each, as xdg-desktop-portal-wlr does. Buffers the daemon allocates reach
// the consumer as MemPtr (inside one shared block), which WebRTC drops.
static void cast_add_buffer(void *data, struct pw_buffer *b)
{
	struct cast *c = data;
	struct spa_data *d = &b->buffer->datas[0];
	size_t size = (size_t)c->spec.w * 4 * c->spec.h;
	if (!(d->type & (1u << SPA_DATA_MemFd))) {
		say("stream %u: the consumer takes no memfd buffers (types 0x%x)", c->node_id, d->type);
		return;
	}
	int fd = memfd_create("fossai-screen", MFD_CLOEXEC | MFD_ALLOW_SEALING);
	if (fd < 0 || ftruncate(fd, (off_t)size) != 0) {
		say("stream %u: memfd of %zu bytes: %s", c->node_id, size, strerror(errno));
		if (fd >= 0)
			close(fd);
		return;
	}
	void *map = mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
	if (map == MAP_FAILED) {
		say("stream %u: mmap of the memfd: %s", c->node_id, strerror(errno));
		close(fd);
		return;
	}
	d->type = SPA_DATA_MemFd;
	d->flags = SPA_DATA_FLAG_READWRITE;
	d->fd = fd;
	d->mapoffset = 0;
	d->maxsize = (uint32_t)size;
	d->data = map;
	if (++c->buffers == 1)
		say("stream %u: memfd buffers of %zu bytes", c->node_id, size);
}

static void cast_remove_buffer(void *data, struct pw_buffer *b)
{
	struct cast *c = data;
	struct spa_data *d = &b->buffer->datas[0];
	if (d->type != SPA_DATA_MemFd)
		return;
	if (d->data)
		munmap(d->data, d->maxsize);
	if (d->fd >= 0)
		close((int)d->fd);
	d->data = NULL;
	d->fd = -1;
	if (c->buffers)
		c->buffers--;
}

static const struct pw_stream_events cast_events = {
	PW_VERSION_STREAM_EVENTS,
	.state_changed = cast_state,
	.param_changed = cast_param_changed,
	.add_buffer = cast_add_buffer,
	.remove_buffer = cast_remove_buffer,
	.process = cast_process,
};

// The pace: a new kernel frame, or a second without one, drives a cycle.
static void cast_tick(void *data, uint64_t expirations)
{
	(void)expirations;
	struct cast *c = data;
	if (!c->streaming)
		return;
	struct cap_status st;
	memset(&st, 0, sizeof(st));
	uint64_t t = now_ms();
	if (cap_ioctl(c->fd, CAP_STATUS, &st) == 0 && st.seq == c->seq && t - c->sent_ms < 1000)
		return;
	c->sent_ms = t;
	pw_stream_trigger_process(c->stream);
}

// Make the stream of a session; loop locked. NULL - no capture.
static struct cast *cast_new(int monitor, uint32_t cursor_mode)
{
	struct cap_info info;
	memset(&info, 0, sizeof(info));
	int fd = open(g_test ? "/dev/null" : CAP_DEVICE, O_RDWR | O_CLOEXEC);
	if (fd < 0) {
		say("%s: %s", CAP_DEVICE, strerror(errno));
		return NULL;
	}
	if (cap_ioctl(fd, CAP_INFO, &info) != 0 || info.count == 0) {
		say("no monitors (%s)", strerror(errno));
		close(fd);
		return NULL;
	}
	if (monitor < 0 || (uint32_t)monitor >= info.count || monitor >= 8)
		monitor = 0;
	struct cast *c = calloc(1, sizeof(*c));
	c->fd = fd;
	c->spec.x = info.outputs[monitor][0];
	c->spec.y = info.outputs[monitor][1];
	c->spec.w = (uint32_t)info.outputs[monitor][2];
	c->spec.h = (uint32_t)info.outputs[monitor][3];
	// Hidden only when asked (1); embedded when asked (2) or when the caller
	// says nothing - Chromium sends no cursor_mode, and a shared screen
	// without the pointer is hard to follow.
	c->spec.flags = cursor_mode == 1 ? 0 : CAP_POINTER;
	c->spec.fps = FPS;
	c->format = info.format == PIXEL_RGB ? SPA_VIDEO_FORMAT_RGBx : SPA_VIDEO_FORMAT_BGRx;
	if (cap_ioctl(fd, CAP_START, &c->spec) != 0) {
		say("capture of monitor %d did not start: %s", monitor + 1, strerror(errno));
		close(fd);
		free(c);
		return NULL;
	}
	c->started_ms = now_ms();
	char name[64];
	snprintf(name, sizeof(name), "fossai-screen-%d", g_next_cast++);
	struct pw_properties *props = pw_properties_new(PW_KEY_MEDIA_CLASS, "Video/Source", PW_KEY_MEDIA_TYPE, "Video",
							PW_KEY_MEDIA_CATEGORY, "Capture", PW_KEY_MEDIA_ROLE, "Screen",
							PW_KEY_NODE_DESCRIPTION, "FossAi screen", NULL);
	c->stream = pw_stream_new(g_core, name, props);
	if (!c->stream) {
		say("pw_stream_new: %s", strerror(errno));
		cap_ioctl(fd, CAP_STOP, NULL);
		close(fd);
		free(c);
		return NULL;
	}
	pw_stream_add_listener(c->stream, &c->listener, &cast_events, c);
	uint8_t buffer[1024];
	struct spa_pod_builder b = SPA_POD_BUILDER_INIT(buffer, sizeof(buffer));
	const struct spa_pod *params[1];
	params[0] = spa_pod_builder_add_object(
		&b, SPA_TYPE_OBJECT_Format, SPA_PARAM_EnumFormat, SPA_FORMAT_mediaType, SPA_POD_Id(SPA_MEDIA_TYPE_video),
		SPA_FORMAT_mediaSubtype, SPA_POD_Id(SPA_MEDIA_SUBTYPE_raw), SPA_FORMAT_VIDEO_format, SPA_POD_Id(c->format),
		SPA_FORMAT_VIDEO_size, SPA_POD_Rectangle(&SPA_RECTANGLE(c->spec.w, c->spec.h)), SPA_FORMAT_VIDEO_framerate,
		SPA_POD_Fraction(&SPA_FRACTION(0, 1)), SPA_FORMAT_VIDEO_maxFramerate,
		SPA_POD_CHOICE_RANGE_Fraction(&SPA_FRACTION(FPS, 1), &SPA_FRACTION(1, 1), &SPA_FRACTION(FPS, 1)));
	int res = pw_stream_connect(c->stream, PW_DIRECTION_OUTPUT, PW_ID_ANY,
				    PW_STREAM_FLAG_DRIVER | PW_STREAM_FLAG_ALLOC_BUFFERS, params, 1);
	if (res < 0) {
		say("pw_stream_connect: %s", strerror(-res));
		pw_stream_destroy(c->stream);
		cap_ioctl(fd, CAP_STOP, NULL);
		close(fd);
		free(c);
		return NULL;
	}
	c->timer = pw_loop_add_timer(pw_thread_loop_get_loop(g_loop), cast_tick, c);
	struct timespec every = {0, 1000000000L / FPS};
	pw_loop_update_timer(pw_thread_loop_get_loop(g_loop), c->timer, &every, &every, false);
	return c;
}

// Loop locked.
static void cast_free(struct cast *c)
{
	if (!c)
		return;
	say("stream %u ends after %llu frames", c->node_id, (unsigned long long)c->frames);
	for (int i = 0; i < MAX_LINKS; i++)
		if (c->links[i])
			pw_proxy_destroy(c->links[i]);
	if (c->timer)
		pw_loop_destroy_source(pw_thread_loop_get_loop(g_loop), c->timer);
	spa_hook_remove(&c->listener);
	pw_stream_destroy(c->stream);
	cap_ioctl(c->fd, CAP_STOP, NULL);
	close(c->fd);
	free(c);
}

static int pipewire_start(void)
{
	pw_init(NULL, NULL);
	g_loop = pw_thread_loop_new("fossai-portal", NULL);
	g_context = pw_context_new(pw_thread_loop_get_loop(g_loop), NULL, 0);
	if (!g_context) {
		say("pw_context_new: %s", strerror(errno));
		return -1;
	}
	if (pw_thread_loop_start(g_loop) < 0)
		return -1;
	pw_thread_loop_lock(g_loop);
	g_core = pw_context_connect(g_context, NULL, 0);
	if (!g_core) {
		say("no pipewire daemon: %s", strerror(errno));
		pw_thread_loop_unlock(g_loop);
		return -1;
	}
	pw_core_add_listener(g_core, &g_core_listener, &core_events, NULL);
	g_registry = pw_core_get_registry(g_core, PW_VERSION_REGISTRY, 0);
	pw_registry_add_listener(g_registry, &g_registry_listener, &registry_events, NULL);
	pw_thread_loop_unlock(g_loop);
	return 0;
}

// ---- D-Bus portal ----------------------------------------------------------------

#define PORTAL_NAME "org.freedesktop.portal.Desktop"
#define PORTAL_PATH "/org/freedesktop/portal/desktop"
#define SCREENCAST "org.freedesktop.portal.ScreenCast"
#define REQUEST "org.freedesktop.portal.Request"
#define SESSION "org.freedesktop.portal.Session"
#define PROPS "org.freedesktop.DBus.Properties"

// Source types: monitor. Cursor modes: hidden, embedded.
#define SOURCE_TYPES 1u
#define CURSOR_MODES 3u
#define VERSION 4u

static const char INTROSPECT[] =
	"<!DOCTYPE node PUBLIC \"-//freedesktop//DTD D-BUS Object Introspection 1.0//EN\" "
	"\"http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd\">\n"
	"<node>\n"
	" <interface name=\"org.freedesktop.DBus.Introspectable\"><method name=\"Introspect\"><arg type=\"s\" direction=\"out\"/></method></interface>\n"
	" <interface name=\"org.freedesktop.DBus.Properties\">\n"
	"  <method name=\"Get\"><arg type=\"s\" direction=\"in\"/><arg type=\"s\" direction=\"in\"/><arg type=\"v\" direction=\"out\"/></method>\n"
	"  <method name=\"GetAll\"><arg type=\"s\" direction=\"in\"/><arg type=\"a{sv}\" direction=\"out\"/></method>\n"
	" </interface>\n"
	" <interface name=\"org.freedesktop.portal.ScreenCast\">\n"
	"  <method name=\"CreateSession\"><arg type=\"a{sv}\" direction=\"in\"/><arg type=\"o\" direction=\"out\"/></method>\n"
	"  <method name=\"SelectSources\"><arg type=\"o\" direction=\"in\"/><arg type=\"a{sv}\" direction=\"in\"/><arg type=\"o\" direction=\"out\"/></method>\n"
	"  <method name=\"Start\"><arg type=\"o\" direction=\"in\"/><arg type=\"s\" direction=\"in\"/><arg type=\"a{sv}\" direction=\"in\"/><arg type=\"o\" direction=\"out\"/></method>\n"
	"  <method name=\"OpenPipeWireRemote\"><arg type=\"o\" direction=\"in\"/><arg type=\"a{sv}\" direction=\"in\"/><arg type=\"h\" direction=\"out\"/></method>\n"
	"  <property name=\"AvailableSourceTypes\" type=\"u\" access=\"read\"/>\n"
	"  <property name=\"AvailableCursorModes\" type=\"u\" access=\"read\"/>\n"
	"  <property name=\"version\" type=\"u\" access=\"read\"/>\n"
	" </interface>\n"
	"</node>\n";

// The options a{sv} of a call: string and uint values by key.
struct options {
	char handle_token[64];
	char session_token[64];
	uint32_t types;
	uint32_t cursor_mode;
};

static void read_options(DBusMessageIter *array, struct options *o)
{
	if (dbus_message_iter_get_arg_type(array) != DBUS_TYPE_ARRAY)
		return;
	DBusMessageIter entries;
	dbus_message_iter_recurse(array, &entries);
	while (dbus_message_iter_get_arg_type(&entries) == DBUS_TYPE_DICT_ENTRY) {
		DBusMessageIter entry, value;
		const char *key = NULL;
		dbus_message_iter_recurse(&entries, &entry);
		dbus_message_iter_get_basic(&entry, &key);
		dbus_message_iter_next(&entry);
		dbus_message_iter_recurse(&entry, &value);
		int type = dbus_message_iter_get_arg_type(&value);
		if (type == DBUS_TYPE_STRING) {
			const char *s = NULL;
			dbus_message_iter_get_basic(&value, &s);
			if (strcmp(key, "handle_token") == 0)
				snprintf(o->handle_token, sizeof(o->handle_token), "%s", s);
			else if (strcmp(key, "session_handle_token") == 0)
				snprintf(o->session_token, sizeof(o->session_token), "%s", s);
		} else if (type == DBUS_TYPE_UINT32) {
			uint32_t u = 0;
			dbus_message_iter_get_basic(&value, &u);
			if (strcmp(key, "types") == 0)
				o->types = u;
			else if (strcmp(key, "cursor_mode") == 0)
				o->cursor_mode = u;
		}
		dbus_message_iter_next(&entries);
	}
}

// ":1.23" -> "1_23" (the sender part of request and session paths).
static void sender_part(const char *sender, char *out, size_t len)
{
	size_t n = 0;
	for (const char *p = sender[0] == ':' ? sender + 1 : sender; *p && n + 1 < len; p++)
		out[n++] = *p == '.' ? '_' : *p;
	out[n] = 0;
}

static void token_path(const char *kind, const char *sender, const char *token, char *out, size_t len)
{
	char part[64];
	sender_part(sender, part, sizeof(part));
	if (!token[0]) {
		static unsigned counter;
		char made[32];
		snprintf(made, sizeof(made), "fossai%u", ++counter);
		snprintf(out, len, PORTAL_PATH "/%s/%s/%s", kind, part, made);
	} else {
		snprintf(out, len, PORTAL_PATH "/%s/%s/%s", kind, part, token);
	}
}

static void reply_path(DBusMessage *call, const char *path)
{
	DBusMessage *reply = dbus_message_new_method_return(call);
	dbus_message_append_args(reply, DBUS_TYPE_OBJECT_PATH, &path, DBUS_TYPE_INVALID);
	dbus_connection_send(g_bus, reply, NULL);
	dbus_message_unref(reply);
}

static void reply_error(DBusMessage *call, const char *name, const char *text)
{
	DBusMessage *reply = dbus_message_new_error(call, name, text);
	dbus_connection_send(g_bus, reply, NULL);
	dbus_message_unref(reply);
}

static void dict_uint(DBusMessageIter *dict, const char *key, uint32_t value)
{
	DBusMessageIter entry, variant;
	dbus_message_iter_open_container(dict, DBUS_TYPE_DICT_ENTRY, NULL, &entry);
	dbus_message_iter_append_basic(&entry, DBUS_TYPE_STRING, &key);
	dbus_message_iter_open_container(&entry, DBUS_TYPE_VARIANT, "u", &variant);
	dbus_message_iter_append_basic(&variant, DBUS_TYPE_UINT32, &value);
	dbus_message_iter_close_container(&entry, &variant);
	dbus_message_iter_close_container(dict, &entry);
}

static void dict_string(DBusMessageIter *dict, const char *key, const char *value)
{
	DBusMessageIter entry, variant;
	dbus_message_iter_open_container(dict, DBUS_TYPE_DICT_ENTRY, NULL, &entry);
	dbus_message_iter_append_basic(&entry, DBUS_TYPE_STRING, &key);
	dbus_message_iter_open_container(&entry, DBUS_TYPE_VARIANT, "s", &variant);
	dbus_message_iter_append_basic(&variant, DBUS_TYPE_STRING, &value);
	dbus_message_iter_close_container(&entry, &variant);
	dbus_message_iter_close_container(dict, &entry);
}

static void dict_pair(DBusMessageIter *dict, const char *key, int32_t a, int32_t b)
{
	DBusMessageIter entry, variant, pair;
	dbus_message_iter_open_container(dict, DBUS_TYPE_DICT_ENTRY, NULL, &entry);
	dbus_message_iter_append_basic(&entry, DBUS_TYPE_STRING, &key);
	dbus_message_iter_open_container(&entry, DBUS_TYPE_VARIANT, "(ii)", &variant);
	dbus_message_iter_open_container(&variant, DBUS_TYPE_STRUCT, NULL, &pair);
	dbus_message_iter_append_basic(&pair, DBUS_TYPE_INT32, &a);
	dbus_message_iter_append_basic(&pair, DBUS_TYPE_INT32, &b);
	dbus_message_iter_close_container(&variant, &pair);
	dbus_message_iter_close_container(&entry, &variant);
	dbus_message_iter_close_container(dict, &entry);
}

// Request.Response(u response, a{sv} results) to the caller; `cast` adds
// "streams", `session` adds "session_handle".
static void respond(const char *request, const char *sender, uint32_t code, const char *session, struct cast *cast)
{
	DBusMessage *signal = dbus_message_new_signal(request, REQUEST, "Response");
	dbus_message_set_destination(signal, sender);
	DBusMessageIter it, dict;
	dbus_message_iter_init_append(signal, &it);
	dbus_message_iter_append_basic(&it, DBUS_TYPE_UINT32, &code);
	dbus_message_iter_open_container(&it, DBUS_TYPE_ARRAY, "{sv}", &dict);
	if (session)
		dict_string(&dict, "session_handle", session);
	if (cast) {
		DBusMessageIter entry, variant, streams, stream, props;
		const char *key = "streams";
		dbus_message_iter_open_container(&dict, DBUS_TYPE_DICT_ENTRY, NULL, &entry);
		dbus_message_iter_append_basic(&entry, DBUS_TYPE_STRING, &key);
		dbus_message_iter_open_container(&entry, DBUS_TYPE_VARIANT, "a(ua{sv})", &variant);
		dbus_message_iter_open_container(&variant, DBUS_TYPE_ARRAY, "(ua{sv})", &streams);
		dbus_message_iter_open_container(&streams, DBUS_TYPE_STRUCT, NULL, &stream);
		dbus_message_iter_append_basic(&stream, DBUS_TYPE_UINT32, &cast->node_id);
		dbus_message_iter_open_container(&stream, DBUS_TYPE_ARRAY, "{sv}", &props);
		dict_pair(&props, "position", cast->spec.x, cast->spec.y);
		dict_pair(&props, "size", (int32_t)cast->spec.w, (int32_t)cast->spec.h);
		dict_uint(&props, "source_type", 1);
		dbus_message_iter_close_container(&stream, &props);
		dbus_message_iter_close_container(&streams, &stream);
		dbus_message_iter_close_container(&variant, &streams);
		dbus_message_iter_close_container(&entry, &variant);
		dbus_message_iter_close_container(&dict, &entry);
	}
	dbus_message_iter_close_container(&it, &dict);
	dbus_connection_send(g_bus, signal, NULL);
	dbus_message_unref(signal);
}

static struct session *session_find(const char *path, const char *sender)
{
	for (int i = 0; i < MAX_SESSIONS; i++)
		if (g_sessions[i].used && strcmp(g_sessions[i].path, path) == 0 && (!sender || strcmp(g_sessions[i].sender, sender) == 0))
			return &g_sessions[i];
	return NULL;
}

static void session_close(struct session *s, const char *why)
{
	say("session %s closed (%s)", s->path, why);
	pw_thread_loop_lock(g_loop);
	cast_free(s->cast);
	s->cast = NULL;
	s->used = false;
	pw_thread_loop_unlock(g_loop);
}

static int chosen_monitor(void)
{
	const char *env = getenv("FOSSAI_SCREENCAST_MONITOR");
	int n = env ? atoi(env) : 1;
	return n >= 1 ? n - 1 : 0;
}

static void create_session(DBusMessage *call, const char *sender)
{
	DBusMessageIter it;
	struct options o = {0};
	if (dbus_message_iter_init(call, &it))
		read_options(&it, &o);
	char request[256], path[256];
	token_path("request", sender, o.handle_token, request, sizeof(request));
	token_path("session", sender, o.session_token, path, sizeof(path));
	struct session *s = NULL;
	for (int i = 0; i < MAX_SESSIONS && !s; i++)
		if (!g_sessions[i].used)
			s = &g_sessions[i];
	reply_path(call, request);
	if (!s) {
		respond(request, sender, 2, NULL, NULL);
		return;
	}
	memset(s, 0, sizeof(*s));
	s->used = true;
	snprintf(s->path, sizeof(s->path), "%s", path);
	snprintf(s->sender, sizeof(s->sender), "%s", sender);
	s->monitor = chosen_monitor();
	say("CreateSession from %s: %s", sender, path);
	respond(request, sender, 0, path, NULL);
}

static void select_sources(DBusMessage *call, const char *sender)
{
	DBusMessageIter it;
	const char *path = NULL;
	struct options o = {0};
	if (!dbus_message_iter_init(call, &it) || dbus_message_iter_get_arg_type(&it) != DBUS_TYPE_OBJECT_PATH) {
		reply_error(call, DBUS_ERROR_INVALID_ARGS, "session expected");
		return;
	}
	dbus_message_iter_get_basic(&it, &path);
	dbus_message_iter_next(&it);
	read_options(&it, &o);
	char request[256];
	token_path("request", sender, o.handle_token, request, sizeof(request));
	struct session *s = session_find(path, sender);
	reply_path(call, request);
	if (!s) {
		respond(request, sender, 2, NULL, NULL);
		return;
	}
	s->cursor_mode = o.cursor_mode;
	say("SelectSources %s: types %u, cursor mode %u -> monitor %d", path, o.types, o.cursor_mode, s->monitor + 1);
	respond(request, sender, 0, NULL, NULL);
}

static void start(DBusMessage *call, const char *sender)
{
	DBusMessageIter it;
	const char *path = NULL, *parent = NULL;
	struct options o = {0};
	if (!dbus_message_iter_init(call, &it) || dbus_message_iter_get_arg_type(&it) != DBUS_TYPE_OBJECT_PATH) {
		reply_error(call, DBUS_ERROR_INVALID_ARGS, "session expected");
		return;
	}
	dbus_message_iter_get_basic(&it, &path);
	dbus_message_iter_next(&it);
	if (dbus_message_iter_get_arg_type(&it) == DBUS_TYPE_STRING)
		dbus_message_iter_get_basic(&it, &parent);
	dbus_message_iter_next(&it);
	read_options(&it, &o);
	char request[256];
	token_path("request", sender, o.handle_token, request, sizeof(request));
	struct session *s = session_find(path, sender);
	reply_path(call, request);
	if (!s) {
		respond(request, sender, 2, NULL, NULL);
		return;
	}
	pw_thread_loop_lock(g_loop);
	if (!s->cast)
		s->cast = cast_new(s->monitor, s->cursor_mode);
	struct cast *c = s->cast;
	uint64_t until = now_ms() + 10000;
	// The loop thread does the stream's work while the lock is free.
	while (c && !c->node_id && !c->failed && now_ms() < until) {
		pw_thread_loop_unlock(g_loop);
		usleep(10000);
		pw_thread_loop_lock(g_loop);
	}
	bool ok = c && c->node_id && !c->failed;
	pw_thread_loop_unlock(g_loop);
	if (!ok) {
		say("Start %s: no stream", path);
		respond(request, sender, 2, NULL, NULL);
		return;
	}
	say("Start %s: stream node %u, monitor %d (%ux%u)", path, c->node_id, s->monitor + 1, c->spec.w, c->spec.h);
	respond(request, sender, 0, NULL, c);
}

static void open_remote(DBusMessage *call, const char *sender)
{
	const char *path = NULL;
	DBusMessageIter it;
	if (dbus_message_iter_init(call, &it) && dbus_message_iter_get_arg_type(&it) == DBUS_TYPE_OBJECT_PATH)
		dbus_message_iter_get_basic(&it, &path);
	if (!path || !session_find(path, sender)) {
		reply_error(call, "org.freedesktop.portal.Error.NotFound", "no such session");
		return;
	}
	const char *dir = getenv("PIPEWIRE_RUNTIME_DIR");
	if (!dir)
		dir = getenv("XDG_RUNTIME_DIR");
	struct sockaddr_un addr = {.sun_family = AF_UNIX};
	snprintf(addr.sun_path, sizeof(addr.sun_path), "%s/pipewire-0", dir ? dir : "/run/user/0");
	int fd = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
	if (fd < 0 || connect(fd, (struct sockaddr *)&addr, sizeof(addr)) != 0) {
		say("OpenPipeWireRemote: %s: %s", addr.sun_path, strerror(errno));
		if (fd >= 0)
			close(fd);
		reply_error(call, DBUS_ERROR_FAILED, "no pipewire");
		return;
	}
	DBusMessage *reply = dbus_message_new_method_return(call);
	dbus_message_append_args(reply, DBUS_TYPE_UNIX_FD, &fd, DBUS_TYPE_INVALID);
	dbus_connection_send(g_bus, reply, NULL);
	dbus_message_unref(reply);
	close(fd);
	say("OpenPipeWireRemote %s: a pipewire connection handed to %s", path, sender);
}

static bool screencast_property(const char *name, uint32_t *value)
{
	if (strcmp(name, "AvailableSourceTypes") == 0)
		*value = SOURCE_TYPES;
	else if (strcmp(name, "AvailableCursorModes") == 0)
		*value = CURSOR_MODES;
	else if (strcmp(name, "version") == 0)
		*value = VERSION;
	else
		return false;
	return true;
}

static void properties(DBusMessage *call, const char *member)
{
	const char *iface = NULL, *name = NULL;
	DBusError err;
	dbus_error_init(&err);
	if (strcmp(member, "Get") == 0) {
		uint32_t value;
		if (!dbus_message_get_args(call, &err, DBUS_TYPE_STRING, &iface, DBUS_TYPE_STRING, &name, DBUS_TYPE_INVALID)) {
			dbus_error_free(&err);
			reply_error(call, DBUS_ERROR_INVALID_ARGS, "Get(ss)");
			return;
		}
		if (strcmp(iface, SCREENCAST) != 0 || !screencast_property(name, &value)) {
			reply_error(call, DBUS_ERROR_UNKNOWN_PROPERTY, "no such interface or property");
			return;
		}
		DBusMessage *reply = dbus_message_new_method_return(call);
		DBusMessageIter it, variant;
		dbus_message_iter_init_append(reply, &it);
		dbus_message_iter_open_container(&it, DBUS_TYPE_VARIANT, "u", &variant);
		dbus_message_iter_append_basic(&variant, DBUS_TYPE_UINT32, &value);
		dbus_message_iter_close_container(&it, &variant);
		dbus_connection_send(g_bus, reply, NULL);
		dbus_message_unref(reply);
	} else if (strcmp(member, "GetAll") == 0) {
		if (!dbus_message_get_args(call, &err, DBUS_TYPE_STRING, &iface, DBUS_TYPE_INVALID)) {
			dbus_error_free(&err);
			reply_error(call, DBUS_ERROR_INVALID_ARGS, "GetAll(s)");
			return;
		}
		if (strcmp(iface, SCREENCAST) != 0) {
			reply_error(call, DBUS_ERROR_UNKNOWN_INTERFACE, "no such interface");
			return;
		}
		DBusMessage *reply = dbus_message_new_method_return(call);
		DBusMessageIter it, dict;
		dbus_message_iter_init_append(reply, &it);
		dbus_message_iter_open_container(&it, DBUS_TYPE_ARRAY, "{sv}", &dict);
		dict_uint(&dict, "AvailableSourceTypes", SOURCE_TYPES);
		dict_uint(&dict, "AvailableCursorModes", CURSOR_MODES);
		dict_uint(&dict, "version", VERSION);
		dbus_message_iter_close_container(&it, &dict);
		dbus_connection_send(g_bus, reply, NULL);
		dbus_message_unref(reply);
	} else {
		reply_error(call, DBUS_ERROR_UNKNOWN_METHOD, "read-only properties");
	}
}

static DBusHandlerResult on_message(DBusConnection *bus, DBusMessage *msg, void *data)
{
	(void)bus;
	(void)data;
	int type = dbus_message_get_type(msg);
	const char *iface = dbus_message_get_interface(msg);
	const char *member = dbus_message_get_member(msg);
	const char *path = dbus_message_get_path(msg);
	const char *sender = dbus_message_get_sender(msg);
	if (type == DBUS_MESSAGE_TYPE_SIGNAL && iface && member && strcmp(iface, "org.freedesktop.DBus") == 0 &&
	    strcmp(member, "NameOwnerChanged") == 0) {
		const char *name = NULL, *old = NULL, *new_owner = NULL;
		if (dbus_message_get_args(msg, NULL, DBUS_TYPE_STRING, &name, DBUS_TYPE_STRING, &old, DBUS_TYPE_STRING, &new_owner,
					  DBUS_TYPE_INVALID) &&
		    name[0] == ':' && !new_owner[0]) {
			for (int i = 0; i < MAX_SESSIONS; i++)
				if (g_sessions[i].used && strcmp(g_sessions[i].sender, name) == 0)
					session_close(&g_sessions[i], "the program left the bus");
		}
		return DBUS_HANDLER_RESULT_HANDLED;
	}
	if (type != DBUS_MESSAGE_TYPE_METHOD_CALL || !member || !path || !sender)
		return DBUS_HANDLER_RESULT_NOT_YET_HANDLED;
	// Chromium registers its application id with the host portal registry
	// (xdg-desktop-portal 1.19): nothing to keep, a plain success.
	if (iface && strcmp(iface, "org.freedesktop.host.portal.Registry") == 0 && strcmp(member, "Register") == 0) {
		DBusMessage *reply = dbus_message_new_method_return(msg);
		dbus_connection_send(g_bus, reply, NULL);
		dbus_message_unref(reply);
		return DBUS_HANDLER_RESULT_HANDLED;
	}
	if (strcmp(path, PORTAL_PATH) == 0) {
		if (iface && strcmp(iface, "org.freedesktop.DBus.Introspectable") == 0 && strcmp(member, "Introspect") == 0) {
			DBusMessage *reply = dbus_message_new_method_return(msg);
			const char *xml = INTROSPECT;
			dbus_message_append_args(reply, DBUS_TYPE_STRING, &xml, DBUS_TYPE_INVALID);
			dbus_connection_send(g_bus, reply, NULL);
			dbus_message_unref(reply);
		} else if (iface && strcmp(iface, PROPS) == 0) {
			properties(msg, member);
		} else if (!iface || strcmp(iface, SCREENCAST) == 0) {
			if (strcmp(member, "CreateSession") == 0)
				create_session(msg, sender);
			else if (strcmp(member, "SelectSources") == 0)
				select_sources(msg, sender);
			else if (strcmp(member, "Start") == 0)
				start(msg, sender);
			else if (strcmp(member, "OpenPipeWireRemote") == 0)
				open_remote(msg, sender);
			else
				reply_error(msg, DBUS_ERROR_UNKNOWN_METHOD, member);
		} else {
			say("%s.%s from %s: not here", iface, member, sender);
			reply_error(msg, DBUS_ERROR_UNKNOWN_INTERFACE, iface);
		}
		return DBUS_HANDLER_RESULT_HANDLED;
	}
	if (strncmp(path, PORTAL_PATH "/session/", strlen(PORTAL_PATH "/session/")) == 0 && strcmp(member, "Close") == 0) {
		struct session *s = session_find(path, sender);
		if (s)
			session_close(s, "Session.Close");
		DBusMessage *reply = dbus_message_new_method_return(msg);
		dbus_connection_send(g_bus, reply, NULL);
		dbus_message_unref(reply);
		return DBUS_HANDLER_RESULT_HANDLED;
	}
	if (strncmp(path, PORTAL_PATH "/request/", strlen(PORTAL_PATH "/request/")) == 0 && strcmp(member, "Close") == 0) {
		// Requests answer at once: nothing to cancel.
		DBusMessage *reply = dbus_message_new_method_return(msg);
		dbus_connection_send(g_bus, reply, NULL);
		dbus_message_unref(reply);
		return DBUS_HANDLER_RESULT_HANDLED;
	}
	return DBUS_HANDLER_RESULT_NOT_YET_HANDLED;
}

int main(int argc, char **argv)
{
	(void)argc;
	(void)argv;
	g_kmsg = open("/dev/kmsg", O_WRONLY | O_CLOEXEC);
	g_test = getenv("FOSSAI_PORTAL_TEST") && getenv("FOSSAI_PORTAL_TEST")[0] == '1';
	if (g_test) {
		if (g_kmsg >= 0)
			close(g_kmsg);
		g_kmsg = -1;
	}
	setvbuf(stderr, NULL, _IOLBF, 0);
	if (pipewire_start() != 0)
		return 1;
	DBusError err;
	dbus_error_init(&err);
	g_bus = dbus_bus_get_private(DBUS_BUS_SESSION, &err);
	if (!g_bus) {
		say("no session bus: %s", err.message);
		return 1;
	}
	dbus_connection_set_exit_on_disconnect(g_bus, TRUE);
	int got = dbus_bus_request_name(g_bus, PORTAL_NAME, DBUS_NAME_FLAG_DO_NOT_QUEUE, &err);
	if (got != DBUS_REQUEST_NAME_REPLY_PRIMARY_OWNER) {
		say("%s is not ours (%d%s%s)", PORTAL_NAME, got, err.message ? ": " : "", err.message ? err.message : "");
		return 1;
	}
	dbus_bus_add_match(g_bus, "type='signal',sender='org.freedesktop.DBus',interface='org.freedesktop.DBus',member='NameOwnerChanged'", NULL);
	dbus_connection_add_filter(g_bus, on_message, NULL, NULL);
	say("ready: %s (ScreenCast %u) on the session bus, pipewire connected%s", PORTAL_NAME, VERSION,
	    g_test ? " (test picture)" : "");
	// The launcher (fossai-screencast) waits for this file.
	const char *ready = getenv("FOSSAI_PORTAL_READY");
	if (ready) {
		int f = open(ready, O_CREAT | O_WRONLY | O_TRUNC | O_CLOEXEC, 0644);
		if (f >= 0)
			close(f);
	}
	while (dbus_connection_read_write_dispatch(g_bus, -1)) {
	}
	say("session bus gone");
	return 0;
}
