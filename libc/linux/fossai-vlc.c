/*
 * fossai-vlc: the program `vlc` of the VLC package (24.11.2026).
 *
 * VLC 3.0 of Debian 13 runs as `bin/vlc` of /opt/vlc, but Debian's own
 * `vlc` cannot be used as is:
 *
 *  - it refuses to run as root ("VLC is not supposed to be run as root"),
 *    and programs of the Linux layer run as uid 0;
 *  - its Qt interface is built with X11 support, and such a build insists
 *    on an X server: `HasX11()` opens the display and the interface thread
 *    starts QApplication with `-platform xcb`. There is no X server here,
 *    only the Wayland server of the kernel, and VLC 3 can embed its video
 *    into a Qt window on Wayland (a wl_subsurface for the wl_shm output)
 *    once Qt runs on the `wayland` platform.
 *
 * So this program does what `bin/vlc.c` of VLC does (libvlc_new with the
 * command line, the default interface, play, wait) through dlopen of
 * libvlc.so.5, and the executable exports symbols that win over the
 * libraries (the main program comes first in the global lookup scope):
 *
 *  - XOpenDisplay/XCloseDisplay: with no DISPLAY a dummy display for the
 *    probe of the Qt module (it only opens and closes it);
 *  - QApplication::QApplication(int&, char**, int) (base object constructor,
 *    version Qt_5 from the version script): `-platform xcb` becomes
 *    `-platform wayland` (or FOSSAI_VLC_QPA), then the real constructor of
 *    libQt5Widgets runs;
 *  - vlc_memfd (libvlccore): video buffers in memfd memory, not in a file;
 *  - avcodec_default_get_format (libavcodec): hardware pixel formats taken
 *    out of FFmpeg's offer, so VLC makes no video output for VA-API/VDPAU.
 *
 * Built by xtask (vlc.rs) with clang against the Debian glibc sysroot.
 */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <locale.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>

typedef struct libvlc_instance_t libvlc_instance_t;

/* ---- X11 probe ---------------------------------------------------------- */

static char fake_display[4096];

static void *x11_real(const char *name)
{
    void *sym = dlsym(RTLD_NEXT, name);
    if (sym == NULL) {
        void *lib = dlopen("libX11.so.6", RTLD_LAZY | RTLD_GLOBAL);
        if (lib != NULL)
            sym = dlsym(lib, name);
    }
    return sym;
}

void *XOpenDisplay(const char *name)
{
    const char *env = getenv("DISPLAY");
    if ((name == NULL || *name == '\0') && (env == NULL || *env == '\0'))
        return fake_display;
    void *(*real)(const char *) = (void *(*)(const char *))x11_real("XOpenDisplay");
    return real != NULL ? real(name) : NULL;
}

int XCloseDisplay(void *display)
{
    if (display == fake_display)
        return 0;
    int (*real)(void *) = (int (*)(void *))x11_real("XCloseDisplay");
    return real != NULL ? real(display) : 0;
}

/* ---- QApplication ------------------------------------------------------- */

typedef void (*qapp_ctor)(void *, int *, char **, int);

/*
 * QGuiApplication::setDesktopFileName("vlc"): the Wayland app_id of the
 * window. Qt 5 takes it from there, else from /proc/<pid>/exe, and VLC sets
 * neither - the window came with an empty app_id. By "vlc" the desktop
 * frames it: Qt 5 on wl_shm alone (no EGL buffer integration) draws no
 * title bar of its own (QWaylandDisplay::supportsWindowDecoration).
 * A QString is one pointer to its data; this one is never freed.
 */
static void desktop_file_name(void)
{
    void *core = dlopen("libQt5Core.so.5", RTLD_LAZY | RTLD_NOLOAD);
    void *gui = dlopen("libQt5Gui.so.5", RTLD_LAZY | RTLD_NOLOAD);
    void *(*latin1)(const char *, int) = NULL;
    void (*set)(void *const *) = NULL;
    if (core != NULL)
        latin1 = (void *(*)(const char *, int))dlvsym(core, "_ZN7QString17fromLatin1_helperEPKci", "Qt_5");
    if (gui != NULL)
        set = (void (*)(void *const *))dlvsym(gui, "_ZN15QGuiApplication18setDesktopFileNameERK7QString", "Qt_5");
    if (latin1 == NULL || set == NULL) {
        fprintf(stderr, "fossai-vlc: no QGuiApplication::setDesktopFileName, the window has no app_id\n");
        return;
    }
    void *name = latin1("vlc", 3);
    set(&name);
}

static void qapp(const char *mangled, void *self, int *argc, char **argv, int flags)
{
    const char *want = getenv("FOSSAI_VLC_QPA");
    if (want == NULL || *want == '\0')
        want = "wayland";
    for (int i = 1; argc != NULL && i < *argc; i++) {
        if (argv[i] != NULL && argv[i - 1] != NULL && strcmp(argv[i - 1], "-platform") == 0 && strcmp(argv[i], "xcb") == 0) {
            argv[i] = (char *)want;
            fprintf(stderr, "fossai-vlc: Qt platform xcb -> %s\n", want);
        }
    }
    qapp_ctor real = NULL;
    void *widgets = dlopen("libQt5Widgets.so.5", RTLD_LAZY | RTLD_NOLOAD);
    if (widgets != NULL)
        real = (qapp_ctor)dlvsym(widgets, mangled, "Qt_5");
    if (real == NULL) {
        fprintf(stderr, "fossai-vlc: no %s in libQt5Widgets.so.5: %s\n", mangled, dlerror());
        abort();
    }
    real(self, argc, argv, flags);
    desktop_file_name();
}

void _ZN12QApplicationC2ERiPPci(void *self, int *argc, char **argv, int flags)
{
    qapp("_ZN12QApplicationC2ERiPPci", self, argc, argv, flags);
}

void _ZN12QApplicationC1ERiPPci(void *self, int *argc, char **argv, int flags)
{
    qapp("_ZN12QApplicationC1ERiPPci", self, argc, argv, flags);
}

/* ---- vlc_memfd ---------------------------------------------------------- */

/*
 * The wl_shm video output keeps its pictures in vlc_memfd() of libvlccore:
 * O_TMPFILE in /tmp, else a named file there, unlinked at once - a file of
 * the disk mapped shared and written 30 times a second, which the Linux
 * layer can only do through write-back to a file that no longer has a name.
 * memfd_create is memory the Wayland server reads as is (like Qt's own
 * buffers). Plugins resolve vlc_memfd here first: the executable is first
 * in the global lookup scope.
 */
int vlc_memfd(void)
{
    /* One pool per video output of wl_shm: the count shows how many outputs
     * VLC made (the bench checks a single one at the start of a video). */
    static int pools;
    fprintf(stderr, "fossai-vlc: video buffers #%d\n", ++pools);
    return memfd_create("vlc", MFD_CLOEXEC);
}

/* ---- avcodec_default_get_format ----------------------------------------- */

/*
 * No hardware decoding here (no VA-API/VDPAU device), yet FFmpeg offers the
 * VAAPI and VDPAU pixel formats to every H.264/HEVC/VP9 decoder, and VLC 3's
 * ffmpeg_GetFormat (modules/codec/avcodec/video.c) tries each: ffmpeg_OpenVa
 * calls lavc_UpdateVideoFormat - a new video output for that chroma - before
 * vlc_va_New finds no "hw decoder" module (avcodec-hw=none). Two outputs
 * created and torn down at the start of every video, and with
 * qt-video-autoresize the window jumped three times (ROADMAP, VLC 24.11).
 *
 * ffmpeg_GetFormat first calls avcodec_default_get_format(ctx, fmt), then
 * walks the same list. The list is ff_get_format's own writable copy
 * (av_memdup; FFmpeg itself removes failed choices from it), so this one
 * drops the hardware formats from it in place and lets the real function
 * choose among the rest: VLC sees no hwaccel format and keeps the software
 * one - a single video output. AVPixFmtDescriptor.flags is at offset 16
 * (name pointer, three uint8_t, padding), AV_PIX_FMT_FLAG_HWACCEL = 1 << 3;
 * stable since FFmpeg 1.x.
 */
#define FOSSAI_PIX_FMT_NONE (-1)
#define FOSSAI_PIX_FMT_FLAG_HWACCEL (1ULL << 3)

typedef int (*get_format_fn)(void *, const int *);
typedef const void *(*desc_get_fn)(int);

int avcodec_default_get_format(void *ctx, const int *fmt)
{
    static get_format_fn real;
    static desc_get_fn desc_get;
    static int said;
    if (real == NULL) {
        void *avcodec = dlopen("libavcodec.so.61", RTLD_LAZY | RTLD_NOLOAD);
        void *avutil = dlopen("libavutil.so.59", RTLD_LAZY | RTLD_NOLOAD);
        if (avcodec != NULL)
            real = (get_format_fn)dlsym(avcodec, "avcodec_default_get_format");
        if (avutil != NULL)
            desc_get = (desc_get_fn)dlsym(avutil, "av_pix_fmt_desc_get");
        if (real == NULL) {
            fprintf(stderr, "fossai-vlc: no avcodec_default_get_format in libavcodec.so.61\n");
            return fmt != NULL ? fmt[0] : FOSSAI_PIX_FMT_NONE;
        }
    }
    if (fmt != NULL && desc_get != NULL) {
        int *list = (int *)fmt;
        int kept = 0, dropped = 0, soft = 0;
        unsigned char hard[256];
        int n = 0;
        for (; list[n] != FOSSAI_PIX_FMT_NONE && n < 256; n++) {
            const unsigned char *desc = desc_get(list[n]);
            unsigned long long flags = 0;
            if (desc != NULL)
                memcpy(&flags, desc + 16, sizeof flags);
            hard[n] = (flags & FOSSAI_PIX_FMT_FLAG_HWACCEL) != 0;
            soft += !hard[n];
        }
        /* Only hardware formats (or an odd list): leave it to FFmpeg. */
        if (soft > 0 && list[n] == FOSSAI_PIX_FMT_NONE) {
            for (int i = 0; i < n; i++) {
                if (hard[i])
                    dropped++;
                else
                    list[kept++] = list[i];
            }
            list[kept] = FOSSAI_PIX_FMT_NONE;
        }
        if (dropped > 0 && !said) {
            said = 1;
            fprintf(stderr, "fossai-vlc: %d hardware pixel format(s) of FFmpeg dropped (no VA-API/VDPAU): one video output\n", dropped);
        }
    }
    return real(ctx, fmt);
}

/* ---- libvlc ------------------------------------------------------------- */

#define VLC_VERSION "3.0.23"

int main(int argc, char **argv)
{
    setlocale(LC_ALL, "");
    signal(SIGPIPE, SIG_IGN);
    signal(SIGCHLD, SIG_DFL);

    void *lib = dlopen("libvlc.so.5", RTLD_NOW | RTLD_GLOBAL);
    if (lib == NULL) {
        fprintf(stderr, "fossai-vlc: %s\n", dlerror());
        return 1;
    }
    libvlc_instance_t *(*vlc_new)(int, const char *const *) = (libvlc_instance_t * (*)(int, const char *const *)) dlsym(lib, "libvlc_new");
    void (*vlc_release)(libvlc_instance_t *) = (void (*)(libvlc_instance_t *))dlsym(lib, "libvlc_release");
    int (*vlc_add_intf)(libvlc_instance_t *, const char *) = (int (*)(libvlc_instance_t *, const char *))dlsym(lib, "libvlc_add_intf");
    void (*vlc_wait)(libvlc_instance_t *) = (void (*)(libvlc_instance_t *))dlsym(lib, "libvlc_wait");
    void (*vlc_play)(libvlc_instance_t *, int, int, char **) = (void (*)(libvlc_instance_t *, int, int, char **))dlsym(lib, "libvlc_playlist_play");
    void (*vlc_agent)(libvlc_instance_t *, const char *, const char *) = (void (*)(libvlc_instance_t *, const char *, const char *))dlsym(lib, "libvlc_set_user_agent");
    void (*vlc_app_id)(libvlc_instance_t *, const char *, const char *, const char *) =
        (void (*)(libvlc_instance_t *, const char *, const char *, const char *))dlsym(lib, "libvlc_set_app_id");
    if (vlc_new == NULL || vlc_release == NULL || vlc_add_intf == NULL || vlc_wait == NULL || vlc_play == NULL) {
        fprintf(stderr, "fossai-vlc: libvlc.so.5 lacks a function\n");
        return 1;
    }

    /* As bin/vlc.c: read the configuration (libvlc ignores it by default),
     * then the command line; file names in it go to the playlist. */
    const char *args[argc + 2];
    int count = 0;
    args[count++] = "--no-ignore-config";
    for (int i = 1; i < argc; i++)
        args[count++] = argv[i];
    args[count] = NULL;

    libvlc_instance_t *vlc = vlc_new(count, args);
    if (vlc == NULL) {
        fprintf(stderr, "fossai-vlc: libvlc_new failed\n");
        return 1;
    }
    if (vlc_agent != NULL)
        vlc_agent(vlc, "VLC media player", "VLC/" VLC_VERSION " LibVLC/" VLC_VERSION);
    if (vlc_app_id != NULL)
        vlc_app_id(vlc, "org.videolan.vlc", VLC_VERSION, "vlc");
    int rc = 1;
    /* As bin/vlc.c: the hotkeys interface first - it turns the key events of
     * the Qt window (space, F, arrows...) into actions; without it the keys
     * of the player did nothing. globalhotkeys is X11 only. */
    vlc_add_intf(vlc, "hotkeys,none");
    if (vlc_add_intf(vlc, NULL) == 0) {
        vlc_play(vlc, -1, 0, NULL);
        vlc_wait(vlc);
        rc = 0;
    } else {
        fprintf(stderr, "fossai-vlc: no interface\n");
    }
    vlc_release(vlc);
    return rc;
}
