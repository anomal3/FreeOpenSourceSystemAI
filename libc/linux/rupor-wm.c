// rupor-wm (24.11.2026): the smallest X window manager for one program in a
// rootful Xwayland window - the Linux client of Rupor (Avalonia on X11 with a
// WebKitGTK view inside) on the kernel Wayland server of FossAi.
//
// Xwayland runs rootful: its root window is one window of the FossAi desktop
// (title band, resize, maximize - all from the desktop). Inside it there is no
// window manager, and without one an X program keeps the size it asked for and
// sits at 0,0. This manager does what that window needs and nothing more:
//  * main windows (not transient, not a dialog/utility/notification type,
//    at least half the root in both sizes) fill the root and follow it when the desktop window is resized or maximized;
//  * other top-level windows (About, notifications, dialogs) keep their size
//    and position, a window asking for 0,0 is centred;
//  * the newest mapped window gets the input focus, the focus returns to the
//    main window when a window goes away;
//  * _NET_ACTIVE_WINDOW from a program raises and focuses that window.
// No frames, no decorations: the desktop draws the title band of the Xwayland
// window, and there is one program in it.
//
// Built by xtask (rupor.rs) with clang against the Debian 12 sysroot and the
// libx11-dev headers of Debian 13; linked against libX11.so.6.

#include <X11/Xatom.h>
#include <X11/Xlib.h>
#include <X11/Xutil.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define MAX_WINDOWS 256

struct managed {
    Window window;
    int main; /* fills the root */
};

static Display *dpy;
static Window root;
static int root_w, root_h;
static struct managed windows[MAX_WINDOWS];
static int count;
static Atom a_type, a_type_dialog, a_type_utility, a_type_notification, a_type_splash, a_type_tooltip, a_type_popup,
    a_type_dropdown, a_type_menu, a_active, a_supported, a_check, a_wm_name, a_utf8;

static int ignore_errors(Display *d, XErrorEvent *e) {
    (void)d;
    (void)e;
    return 0;
}

static int another_wm(Display *d, XErrorEvent *e) {
    (void)d;
    (void)e;
    fprintf(stderr, "rupor-wm: another window manager is running\n");
    exit(1);
}

static int io_error(Display *d) {
    (void)d;
    fprintf(stderr, "rupor-wm: the X server is gone\n");
    exit(0);
}

static struct managed *find(Window w) {
    for (int i = 0; i < count; i++)
        if (windows[i].window == w)
            return &windows[i];
    return NULL;
}

static void forget(Window w) {
    for (int i = 0; i < count; i++)
        if (windows[i].window == w) {
            windows[i] = windows[--count];
            return;
        }
}

/* A window that is not a main one by its type: dialogs, menus, notifications. */
static int secondary_type(Window w) {
    Atom actual;
    int format;
    unsigned long n = 0, after;
    unsigned char *data = NULL;
    int secondary = 0;
    if (XGetWindowProperty(dpy, w, a_type, 0, 16, False, XA_ATOM, &actual, &format, &n, &after, &data) == Success && data) {
        Atom *types = (Atom *)data;
        for (unsigned long i = 0; i < n; i++)
            if (types[i] == a_type_dialog || types[i] == a_type_utility || types[i] == a_type_notification ||
                types[i] == a_type_splash || types[i] == a_type_tooltip || types[i] == a_type_popup ||
                types[i] == a_type_dropdown || types[i] == a_type_menu)
                secondary = 1;
    }
    if (data)
        XFree(data);
    return secondary;
}

static void fill(Window w) {
    XMoveResizeWindow(dpy, w, 0, 0, (unsigned)root_w, (unsigned)root_h);
}

static void focus(Window w) {
    XWindowAttributes attrs;
    if (XGetWindowAttributes(dpy, w, &attrs) && attrs.map_state == IsViewable)
        XSetInputFocus(dpy, w, RevertToPointerRoot, CurrentTime);
    Window active = w;
    XChangeProperty(dpy, root, a_active, XA_WINDOW, 32, PropModeReplace, (unsigned char *)&active, 1);
}

static void manage(Window w) {
    XWindowAttributes attrs;
    if (!XGetWindowAttributes(dpy, w, &attrs) || attrs.override_redirect)
        return;
    struct managed *m = find(w);
    if (!m) {
        if (count == MAX_WINDOWS)
            return;
        m = &windows[count++];
        m->window = w;
        Window owner = None;
        int transient = XGetTransientForHint(dpy, w, &owner) && owner != None;
        int big = attrs.width * 2 >= root_w && attrs.height * 2 >= root_h;
        m->main = !transient && !secondary_type(w) && big;
        XSelectInput(dpy, w, StructureNotifyMask);
        char *name = NULL;
        XFetchName(dpy, w, &name);
        fprintf(stderr, "rupor-wm: window 0x%lx '%s' %dx%d at %d,%d -> %s\n", w, name ? name : "", attrs.width,
                attrs.height, attrs.x, attrs.y, m->main ? "fills the root" : "as asked");
        if (name)
            XFree(name);
    }
    if (m->main) {
        fill(w);
    } else if (attrs.x == 0 && attrs.y == 0) {
        int x = (root_w - attrs.width) / 2, y = (root_h - attrs.height) / 2;
        XMoveWindow(dpy, w, x > 0 ? x : 0, y > 0 ? y : 0);
    }
    XMapRaised(dpy, w);
}

static void configure_request(XConfigureRequestEvent *e) {
    struct managed *m = find(e->window);
    if (m && m->main) {
        /* The size of a main window is the root: tell the program so. */
        fill(e->window);
        XConfigureEvent ce = {0};
        ce.type = ConfigureNotify;
        ce.display = dpy;
        ce.event = e->window;
        ce.window = e->window;
        ce.x = 0;
        ce.y = 0;
        ce.width = root_w;
        ce.height = root_h;
        ce.border_width = 0;
        ce.above = None;
        ce.override_redirect = False;
        XSendEvent(dpy, e->window, False, StructureNotifyMask, (XEvent *)&ce);
        return;
    }
    XWindowChanges changes;
    changes.x = e->x;
    changes.y = e->y;
    changes.width = e->width;
    changes.height = e->height;
    changes.border_width = e->border_width;
    changes.sibling = e->above;
    changes.stack_mode = e->detail;
    XConfigureWindow(dpy, e->window, (unsigned)e->value_mask, &changes);
}

static void root_resized(int w, int h) {
    if (w == root_w && h == root_h)
        return;
    root_w = w;
    root_h = h;
    fprintf(stderr, "rupor-wm: root %dx%d\n", w, h);
    for (int i = 0; i < count; i++)
        if (windows[i].main)
            fill(windows[i].window);
}

static void refocus(void) {
    for (int i = count - 1; i >= 0; i--)
        if (windows[i].main) {
            focus(windows[i].window);
            return;
        }
    XSetInputFocus(dpy, PointerRoot, RevertToPointerRoot, CurrentTime);
}

int main(void) {
    dpy = XOpenDisplay(NULL);
    if (!dpy) {
        fprintf(stderr, "rupor-wm: cannot open display %s\n", getenv("DISPLAY") ? getenv("DISPLAY") : "");
        return 1;
    }
    root = DefaultRootWindow(dpy);
    XSetIOErrorHandler(io_error);
    XSetErrorHandler(another_wm);
    XSelectInput(dpy, root, SubstructureRedirectMask | SubstructureNotifyMask | StructureNotifyMask);
    XSync(dpy, False);
    XSetErrorHandler(ignore_errors);

    XWindowAttributes ra;
    XGetWindowAttributes(dpy, root, &ra);
    root_w = ra.width;
    root_h = ra.height;

    a_type = XInternAtom(dpy, "_NET_WM_WINDOW_TYPE", False);
    a_type_dialog = XInternAtom(dpy, "_NET_WM_WINDOW_TYPE_DIALOG", False);
    a_type_utility = XInternAtom(dpy, "_NET_WM_WINDOW_TYPE_UTILITY", False);
    a_type_notification = XInternAtom(dpy, "_NET_WM_WINDOW_TYPE_NOTIFICATION", False);
    a_type_splash = XInternAtom(dpy, "_NET_WM_WINDOW_TYPE_SPLASH", False);
    a_type_tooltip = XInternAtom(dpy, "_NET_WM_WINDOW_TYPE_TOOLTIP", False);
    a_type_popup = XInternAtom(dpy, "_NET_WM_WINDOW_TYPE_POPUP_MENU", False);
    a_type_dropdown = XInternAtom(dpy, "_NET_WM_WINDOW_TYPE_DROPDOWN_MENU", False);
    a_type_menu = XInternAtom(dpy, "_NET_WM_WINDOW_TYPE_MENU", False);
    a_active = XInternAtom(dpy, "_NET_ACTIVE_WINDOW", False);
    a_supported = XInternAtom(dpy, "_NET_SUPPORTED", False);
    a_check = XInternAtom(dpy, "_NET_SUPPORTING_WM_CHECK", False);
    a_wm_name = XInternAtom(dpy, "_NET_WM_NAME", False);
    a_utf8 = XInternAtom(dpy, "UTF8_STRING", False);

    /* EWMH: a check window with the manager's name, and what is supported. */
    Window check = XCreateSimpleWindow(dpy, root, -1, -1, 1, 1, 0, 0, 0);
    XChangeProperty(dpy, check, a_check, XA_WINDOW, 32, PropModeReplace, (unsigned char *)&check, 1);
    XChangeProperty(dpy, check, a_wm_name, a_utf8, 8, PropModeReplace, (unsigned char *)"rupor-wm", 8);
    XChangeProperty(dpy, root, a_check, XA_WINDOW, 32, PropModeReplace, (unsigned char *)&check, 1);
    Atom supported[] = {a_active, a_type, a_check, a_wm_name};
    XChangeProperty(dpy, root, a_supported, XA_ATOM, 32, PropModeReplace, (unsigned char *)supported, 4);

    /* Windows mapped before the manager started. */
    Window parent, *children = NULL;
    unsigned int n = 0;
    if (XQueryTree(dpy, root, &root, &parent, &children, &n)) {
        for (unsigned int i = 0; i < n; i++) {
            XWindowAttributes attrs;
            if (children[i] != check && XGetWindowAttributes(dpy, children[i], &attrs) && attrs.map_state == IsViewable)
                manage(children[i]);
        }
        if (children)
            XFree(children);
    }
    fprintf(stderr, "rupor-wm: managing %s, root %dx%d\n", DisplayString(dpy), root_w, root_h);
    fflush(stderr);

    for (;;) {
        XEvent ev;
        XNextEvent(dpy, &ev);
        switch (ev.type) {
        case MapRequest:
            manage(ev.xmaprequest.window);
            break;
        case MapNotify:
            if (ev.xmap.event == root && find(ev.xmap.window))
                focus(ev.xmap.window);
            break;
        case ConfigureRequest:
            configure_request(&ev.xconfigurerequest);
            break;
        case ConfigureNotify:
            if (ev.xconfigure.window == root)
                root_resized(ev.xconfigure.width, ev.xconfigure.height);
            break;
        case UnmapNotify:
        case DestroyNotify: {
            Window w = ev.type == UnmapNotify ? ev.xunmap.window : ev.xdestroywindow.window;
            /* Reparented away (the WebKit view goes into the Avalonia window) or gone. */
            if (find(w)) {
                forget(w);
                refocus();
            }
            break;
        }
        case ReparentNotify:
            if (ev.xreparent.parent != root)
                forget(ev.xreparent.window);
            break;
        case ClientMessage:
            if (ev.xclient.message_type == a_active && find(ev.xclient.window)) {
                XRaiseWindow(dpy, ev.xclient.window);
                focus(ev.xclient.window);
            }
            break;
        }
        fflush(stderr);
    }
}
