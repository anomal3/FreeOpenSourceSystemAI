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
 * libvlc.so.5, and the executable exports three symbols that win over the
 * libraries (the main program comes first in the global lookup scope):
 *
 *  - XOpenDisplay/XCloseDisplay: with no DISPLAY a dummy display for the
 *    probe of the Qt module (it only opens and closes it);
 *  - QApplication::QApplication(int&, char**, int) (base object constructor,
 *    version Qt_5 from the version script): `-platform xcb` becomes
 *    `-platform wayland` (or FOSSAI_VLC_QPA), then the real constructor of
 *    libQt5Widgets runs.
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
}

void _ZN12QApplicationC2ERiPPci(void *self, int *argc, char **argv, int flags)
{
    qapp("_ZN12QApplicationC2ERiPPci", self, argc, argv, flags);
}

void _ZN12QApplicationC1ERiPPci(void *self, int *argc, char **argv, int flags)
{
    qapp("_ZN12QApplicationC1ERiPPci", self, argc, argv, flags);
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
