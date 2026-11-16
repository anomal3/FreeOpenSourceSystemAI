/* Программа Linux x86-64 без libc (16.11.2026): интерфейсы сети так, как их
 * спрашивают glibc (`getifaddrs`, `if_nameindex`, `getaddrinfo` с
 * `AI_ADDRCONFIG`) и `nsiproxy.sys` Wine 11 — сокет `AF_NETLINK`
 * (`NETLINK_ROUTE`), ioctl `SIOCGIF*` на сокете и `/proc/net/dev`.
 *
 * Устроена как `futexv.c`: статическая PIE без перемещений, только `syscall`.
 * Запросы — байт в байт как у glibc: `nlmsghdr` + `rtgenmsg` длиной 17, флаги
 * `NLM_F_REQUEST|NLM_F_DUMP`, ответ — `recvmsg` в буфер 8 КиБ; и как у старой
 * glibc — сперва `MSG_PEEK|MSG_TRUNC` с пустым буфером, чтобы узнать размер.
 *
 * Каждая проверка печатает `netlinkprobe: ok <что>` или
 * `netlinkprobe: FAILED <что>`; найденное — строками `netlinkprobe: link …`,
 * `netlinkprobe: addr …`; итог — `netlinkprobe: N failed`, код выхода —
 * число проваленных. Карта (`eth0`) проверяется, только если она есть.
 */

typedef unsigned long u64;
typedef unsigned int u32;
typedef unsigned short u16;
typedef unsigned char u8;
typedef long i64;

__asm__(".section .note.ABI-tag, \"a\", @note\n"
        ".balign 4\n"
        ".long 4, 16, 1\n"
        ".asciz \"GNU\"\n"
        ".long 0, 3, 2, 0\n"
        ".text\n");

enum {
    SYS_read = 0, SYS_write = 1, SYS_open = 2, SYS_close = 3, SYS_poll = 7, SYS_ioctl = 16,
    SYS_socket = 41, SYS_sendto = 44, SYS_recvfrom = 45, SYS_recvmsg = 47, SYS_bind = 49,
    SYS_getsockname = 51, SYS_getsockopt = 55, SYS_exit_group = 231,
};
enum { EAGAIN = 11, ENODEV = 19, EPERM = 1, EPROTONOSUPPORT = 93 };
enum { AF_INET = 2, AF_NETLINK = 16, SOCK_DGRAM = 2, SOCK_RAW = 3, SOCK_CLOEXEC = 02000000 };
enum { MSG_PEEK = 2, MSG_TRUNC = 0x20, MSG_DONTWAIT = 0x40 };
enum {
    NLMSG_ERROR = 2, NLMSG_DONE = 3, NLM_F_REQUEST = 1, NLM_F_MULTI = 2, NLM_F_ACK = 4, NLM_F_DUMP = 0x300,
    NLM_F_DUMP_INTR = 0x10, RTM_NEWLINK = 16, RTM_GETLINK = 18, RTM_NEWADDR = 20, RTM_GETADDR = 22,
    RTM_NEWROUTE = 24, RTM_GETROUTE = 26, RTM_GETNEIGH = 30,
};
enum { IFLA_ADDRESS = 1, IFLA_IFNAME = 3, IFLA_MTU = 4, IFA_LOCAL = 2, IFA_LABEL = 3 };
enum { IFF_UP = 1, IFF_LOOPBACK = 8 };
enum {
    SIOCGIFNAME = 0x8910, SIOCGIFCONF = 0x8912, SIOCGIFFLAGS = 0x8913, SIOCGIFADDR = 0x8915,
    SIOCGIFNETMASK = 0x891b, SIOCGIFMTU = 0x8921, SIOCGIFHWADDR = 0x8927, SIOCGIFINDEX = 0x8933,
    SIOCGIWNAME = 0x8B01,
};

static i64 sys6(i64 n, i64 a, i64 b, i64 c, i64 d, i64 e, i64 f) {
    register i64 r10 __asm__("r10") = d;
    register i64 r8 __asm__("r8") = e;
    register i64 r9 __asm__("r9") = f;
    i64 ret;
    __asm__ volatile("syscall"
                     : "=a"(ret)
                     : "a"(n), "D"(a), "S"(b), "d"(c), "r"(r10), "r"(r8), "r"(r9)
                     : "rcx", "r11", "memory");
    return ret;
}
#define sys3(n, a, b, c) sys6(n, (i64)(a), (i64)(b), (i64)(c), 0, 0, 0)

void *memset(void *d, int c, u64 n) {
    unsigned char *p = d;
    while (n--) *p++ = (unsigned char)c;
    return d;
}
void *memcpy(void *d, const void *s, u64 n) {
    unsigned char *p = d;
    const unsigned char *q = s;
    while (n--) *p++ = *q++;
    return d;
}

static u64 strlen_(const char *s) {
    u64 n = 0;
    while (s[n]) n++;
    return n;
}
static int streq(const char *a, const char *b) {
    while (*a && *a == *b) a++, b++;
    return *a == *b;
}
static int contains(const char *hay, u64 len, const char *needle) {
    u64 n = strlen_(needle);
    for (u64 i = 0; i + n <= len; i++) {
        u64 j = 0;
        while (j < n && hay[i + j] == needle[j]) j++;
        if (j == n) return 1;
    }
    return 0;
}

/* Строка вывода: собирается кусками и уходит одним `write`. */
static char line[256];
static u64 line_len;
static void put_s(const char *s) {
    while (*s && line_len < sizeof line - 2) line[line_len++] = *s++;
}
static void put_u(u64 v) {
    char tmp[24];
    int n = 0;
    do tmp[n++] = (char)('0' + v % 10), v /= 10;
    while (v);
    while (n) {
        if (line_len < sizeof line - 2) line[line_len++] = tmp[--n];
        else n--;
    }
}
static void put_hex2(u8 v) {
    const char *digits = "0123456789abcdef";
    char s[3] = {digits[v >> 4], digits[v & 15], 0};
    put_s(s);
}
static void put_mac(const u8 *mac) {
    for (int i = 0; i < 6; i++) {
        if (i) put_s(":");
        put_hex2(mac[i]);
    }
}
static void put_ip(const u8 *ip) {
    for (int i = 0; i < 4; i++) {
        if (i) put_s(".");
        put_u(ip[i]);
    }
}
static void flush(void) {
    line[line_len++] = '\n';
    sys3(SYS_write, 1, line, line_len);
    line_len = 0;
}

static int failed;
static void check(int good, const char *what) {
    put_s(good ? "netlinkprobe: ok " : "netlinkprobe: FAILED ");
    put_s(what);
    flush();
    if (!good) failed++;
}

struct nlmsghdr { u32 len; u16 type; u16 flags; u32 seq; u32 pid; };
struct sockaddr_nl { u16 family; u16 pad; u32 pid; u32 groups; };
struct iovec { void *base; u64 len; };
struct msghdr { void *name; u32 namelen; u32 pad0; struct iovec *iov; u64 iovlen; void *control; u64 controllen; int flags; int pad1; };

/* Запрос glibc: заголовок и `rtgenmsg` (один байт), длина 17 без
 * выравнивания. */
struct request { struct nlmsghdr nlh; u8 family; u8 pad[3]; };

static i64 send_dump(int fd, u16 type, u32 seq, u8 family) {
    struct request req;
    memset(&req, 0, sizeof req);
    req.nlh.len = 17;
    req.nlh.type = type;
    req.nlh.flags = NLM_F_REQUEST | NLM_F_DUMP;
    req.nlh.seq = seq;
    req.family = family;
    struct sockaddr_nl kernel = {AF_NETLINK, 0, 0, 0};
    return sys6(SYS_sendto, fd, (i64)&req, 17, 0, (i64)&kernel, sizeof kernel);
}

static i64 recv_msg(int fd, void *buf, u64 len, int flags, struct sockaddr_nl *from, int *msg_flags) {
    struct iovec iov = {buf, len};
    struct msghdr msg;
    memset(&msg, 0, sizeof msg);
    msg.name = from;
    msg.namelen = from ? sizeof *from : 0;
    msg.iov = &iov;
    msg.iovlen = 1;
    i64 r = sys3(SYS_recvmsg, fd, &msg, flags);
    if (msg_flags) *msg_flags = msg.flags;
    return r;
}

/* Что нашлось в списках. */
struct link { int index; char name[16]; u8 mac[6]; u32 mtu; u32 flags; };
static struct link links[8];
static int link_count;
struct addr { int index; u8 prefix; u8 ip[4]; char label[16]; };
static struct addr addrs[8];
static int addr_count;

static u8 buffer[8192];

/* Прочитать ответ на список до `NLMSG_DONE`, как glibc: каждое сообщение —
 * с нашими `seq` и `pid`, отправитель — ядро (`nl_pid` 0), флаг `MULTI`, без
 * `DUMP_INTR`. Сообщения нужного типа отдаются `each`. Ответ — сколько их
 * было или -1. */
static int read_dump(int fd, u32 seq, u32 pid, u16 want, void (*each)(struct nlmsghdr *)) {
    int count = 0;
    for (int round = 0; round < 64; round++) {
        struct sockaddr_nl from;
        memset(&from, 0xAA, sizeof from);
        int msg_flags = 0;
        i64 got = recv_msg(fd, buffer, sizeof buffer, 0, &from, &msg_flags);
        if (got < 16 || from.family != AF_NETLINK || from.pid != 0 || (msg_flags & MSG_TRUNC)) return -1;
        u64 at = 0;
        while (at + 16 <= (u64)got) {
            struct nlmsghdr *h = (struct nlmsghdr *)(buffer + at);
            if (h->len < 16 || at + h->len > (u64)got) return -1;
            if (h->seq != seq || h->pid != pid || !(h->flags & NLM_F_MULTI) || (h->flags & NLM_F_DUMP_INTR)) return -1;
            if (h->type == NLMSG_DONE) return count;
            if (h->type == NLMSG_ERROR) return -1;
            if (h->type == want) {
                count++;
                if (each) each(h);
            }
            at += (h->len + 3) & ~3u;
        }
    }
    return -1;
}

static void copy_name(char *to, const u8 *from, u64 len) {
    u64 n = 0;
    while (n < len && n < 15 && from[n]) to[n] = (char)from[n], n++;
    to[n] = 0;
}

static void on_link(struct nlmsghdr *h) {
    if (link_count >= 8) return;
    struct link *l = &links[link_count++];
    memset(l, 0, sizeof *l);
    u8 *ifi = (u8 *)(h + 1);
    l->index = *(int *)(ifi + 4);
    l->flags = *(u32 *)(ifi + 8);
    u8 *a = ifi + 16;
    u8 *end = (u8 *)h + h->len;
    while (a + 4 <= end) {
        u16 len = *(u16 *)a, type = *(u16 *)(a + 2);
        if (len < 4 || a + len > end) break;
        if (type == IFLA_IFNAME) copy_name(l->name, a + 4, len - 4u);
        if (type == IFLA_ADDRESS && len >= 10) memcpy(l->mac, a + 4, 6);
        if (type == IFLA_MTU && len >= 8) l->mtu = *(u32 *)(a + 4);
        a += (len + 3) & ~3u;
    }
}

static void on_addr(struct nlmsghdr *h) {
    if (addr_count >= 8) return;
    struct addr *d = &addrs[addr_count++];
    memset(d, 0, sizeof *d);
    u8 *ifa = (u8 *)(h + 1);
    d->prefix = ifa[1];
    d->index = (int)*(u32 *)(ifa + 4);
    u8 *a = ifa + 8;
    u8 *end = (u8 *)h + h->len;
    while (a + 4 <= end) {
        u16 len = *(u16 *)a, type = *(u16 *)(a + 2);
        if (len < 4 || a + len > end) break;
        if (type == IFA_LOCAL && len >= 8) memcpy(d->ip, a + 4, 4);
        if (type == IFA_LABEL) copy_name(d->label, a + 4, len - 4u);
        a += (len + 3) & ~3u;
    }
}

static struct link *link_named(const char *name) {
    for (int i = 0; i < link_count; i++)
        if (streq(links[i].name, name)) return &links[i];
    return 0;
}

/* `struct ifreq`: имя и объединение. */
struct ifreq { char name[16]; union { struct { u16 family; u8 data[14]; } addr; short flags; int value; u8 raw[24]; } u; };

static void set_name(struct ifreq *r, const char *name) {
    memset(r, 0, sizeof *r);
    for (int i = 0; name[i] && i < 15; i++) r->name[i] = name[i];
}

static char file_buf[4096];
static i64 read_file(const char *path) {
    i64 fd = sys3(SYS_open, path, 0, 0);
    if (fd < 0) return fd;
    i64 total = 0;
    for (;;) {
        i64 got = sys3(SYS_read, fd, file_buf + total, sizeof file_buf - 1 - total);
        if (got <= 0) break;
        total += got;
    }
    sys3(SYS_close, fd, 0, 0);
    file_buf[total] = 0;
    return total;
}

void start_c(u64 *sp) {
    (void)sp;
    put_s("netlinkprobe: start");
    flush();

    check(sys3(SYS_socket, AF_NETLINK, SOCK_RAW, 15) == -EPROTONOSUPPORT, "NETLINK_KOBJECT_UEVENT is EPROTONOSUPPORT");

    int fd = (int)sys3(SYS_socket, AF_NETLINK, SOCK_RAW | SOCK_CLOEXEC, 0);
    check(fd >= 0, "socket AF_NETLINK NETLINK_ROUTE");

    struct sockaddr_nl sa;
    u32 salen = sizeof sa;
    memset(&sa, 0xAA, sizeof sa);
    i64 r = sys3(SYS_getsockname, fd, &sa, &salen);
    check(r == 0 && sa.family == AF_NETLINK && sa.pid == 0 && salen == 12, "getsockname before bind is AF_NETLINK, pid 0");

    struct sockaddr_nl local = {AF_NETLINK, 0, 0, 0};
    check(sys3(SYS_bind, fd, &local, sizeof local) == 0, "bind with nl_pid 0");
    salen = sizeof sa;
    r = sys3(SYS_getsockname, fd, &sa, &salen);
    u32 pid = sa.pid;
    check(r == 0 && sa.family == AF_NETLINK && pid != 0 && salen == 12, "getsockname after bind gives an nl_pid");

    int type = 0;
    u32 tlen = sizeof type;
    r = sys6(SYS_getsockopt, fd, 1 /* SOL_SOCKET */, 39 /* SO_DOMAIN */, (i64)&type, (i64)&tlen, 0);
    check(r == 0 && type == AF_NETLINK, "SO_DOMAIN is AF_NETLINK");

    /* Пусто: неждущее чтение — EAGAIN, poll — не готов. */
    check(recv_msg(fd, buffer, sizeof buffer, MSG_DONTWAIT, 0, 0) == -EAGAIN, "an empty queue with MSG_DONTWAIT is EAGAIN");
    struct { int fd; short events, revents; } pfd = {fd, 1 /* POLLIN */, 0};
    check(sys3(SYS_poll, &pfd, 1, 0) == 0, "poll on an empty socket times out");

    /* RTM_GETLINK, как `if_nameindex` и `getifaddrs`. */
    u32 seq = 0x1234;
    check(send_dump(fd, RTM_GETLINK, seq, 0) == 17, "sendto RTM_GETLINK dump of 17 bytes");
    pfd.revents = 0;
    check(sys3(SYS_poll, &pfd, 1, 1000) == 1 && (pfd.revents & 1), "poll reports the reply");
    /* Старая glibc: размер датаграммы — `MSG_PEEK|MSG_TRUNC` без буфера. */
    int msg_flags = 0;
    i64 size = recv_msg(fd, buffer, 0, MSG_PEEK | MSG_TRUNC, 0, &msg_flags);
    check(size > 32 && size <= 4096 && (msg_flags & MSG_TRUNC), "MSG_PEEK|MSG_TRUNC gives the datagram size");
    int n = read_dump(fd, seq, pid, RTM_NEWLINK, on_link);
    check(n >= 1 && n == link_count, "RTM_GETLINK dump ends with NLMSG_DONE, seq and pid echoed");
    for (int i = 0; i < link_count; i++) {
        put_s("netlinkprobe: link ");
        put_u((u64)links[i].index);
        put_s(" ");
        put_s(links[i].name);
        put_s(" mtu ");
        put_u(links[i].mtu);
        put_s(" mac ");
        put_mac(links[i].mac);
        flush();
    }
    struct link *lo = link_named("lo");
    check(lo && lo->index == 1 && lo->mtu == 65536 && (lo->flags & (IFF_UP | IFF_LOOPBACK)) == (IFF_UP | IFF_LOOPBACK),
          "lo is index 1, up, loopback, mtu 65536");
    struct link *eth = link_named("eth0");
    if (eth) check(eth->index == 2 && eth->mtu == 1500 && (eth->mac[0] | eth->mac[1] | eth->mac[5]), "eth0 is index 2 with a MAC");

    /* RTM_GETADDR, следующий `seq` — как `getifaddrs`. */
    seq++;
    send_dump(fd, RTM_GETADDR, seq, 0);
    n = read_dump(fd, seq, pid, RTM_NEWADDR, on_addr);
    check(n >= 1 && n == addr_count, "RTM_GETADDR dump ends with NLMSG_DONE");
    int indices_known = 1, lo_addr = 0;
    for (int i = 0; i < addr_count; i++) {
        int known = 0;
        for (int j = 0; j < link_count; j++) known |= links[j].index == addrs[i].index;
        indices_known &= known;
        if (addrs[i].index == 1 && addrs[i].ip[0] == 127 && addrs[i].ip[3] == 1 && addrs[i].prefix == 8 && streq(addrs[i].label, "lo"))
            lo_addr = 1;
        put_s("netlinkprobe: addr ");
        put_s(addrs[i].label);
        put_s(" ");
        put_ip(addrs[i].ip);
        put_s("/");
        put_u(addrs[i].prefix);
        put_s(" index ");
        put_u((u64)addrs[i].index);
        flush();
    }
    check(indices_known, "every address names a listed link");
    check(lo_addr, "lo has 127.0.0.1/8");

    /* AF_INET6 — пусто, только DONE (`check_pf` glibc). */
    seq++;
    send_dump(fd, RTM_GETADDR, seq, 10);
    check(read_dump(fd, seq, pid, RTM_NEWADDR, 0) == 0, "RTM_GETADDR for AF_INET6 is an empty dump");
    seq++;
    send_dump(fd, RTM_GETROUTE, seq, 0);
    check(read_dump(fd, seq, pid, RTM_NEWROUTE, 0) >= 0, "RTM_GETROUTE dump ends with NLMSG_DONE");
    seq++;
    send_dump(fd, RTM_GETNEIGH, seq, 0);
    check(read_dump(fd, seq, pid, 28, 0) == 0, "RTM_GETNEIGH is an empty dump");

    /* Смена адреса — NLMSG_ERROR EPERM с копией запроса. */
    struct { struct nlmsghdr nlh; u8 ifa[8]; } add;
    memset(&add, 0, sizeof add);
    add.nlh.len = sizeof add;
    add.nlh.type = RTM_NEWADDR;
    add.nlh.flags = NLM_F_REQUEST | NLM_F_ACK;
    add.nlh.seq = 77;
    sys6(SYS_sendto, fd, (i64)&add, sizeof add, 0, 0, 0);
    r = recv_msg(fd, buffer, sizeof buffer, 0, 0, 0);
    struct nlmsghdr *h = (struct nlmsghdr *)buffer;
    check(r == 36 && h->type == NLMSG_ERROR && h->seq == 77 && *(int *)(h + 1) == -EPERM &&
              ((struct nlmsghdr *)(buffer + 20))->type == RTM_NEWADDR,
          "RTM_NEWADDR is NLMSG_ERROR -EPERM with the request header");

    /* `if_nameindex` своим сокетом: другой `nl_pid`, тот же список. */
    int fd2 = (int)sys3(SYS_socket, AF_NETLINK, SOCK_DGRAM, 0);
    send_dump(fd2, RTM_GETLINK, 1, 0);
    salen = sizeof sa;
    sys3(SYS_getsockname, fd2, &sa, &salen);
    int before = link_count;
    link_count = 0;
    n = read_dump(fd2, 1, sa.pid, RTM_NEWLINK, on_link);
    check(fd2 >= 0 && sa.pid != 0 && sa.pid != pid && n == before && link_named("lo"),
          "a second socket autobinds a different nl_pid and lists the same links");
    sys3(SYS_close, fd2, 0, 0);
    sys3(SYS_close, fd, 0, 0);

    /* ioctl — на сокете AF_INET, как `nsiproxy.sys`. */
    int ifd = (int)sys3(SYS_socket, AF_INET, SOCK_DGRAM, 0);
    check(ifd >= 0, "socket AF_INET SOCK_DGRAM for ioctls");
    struct { int len; int pad; void *buf; } conf = {0, 0, 0};
    r = sys3(SYS_ioctl, ifd, SIOCGIFCONF, &conf);
    int need = conf.len;
    static struct ifreq reqs[8];
    conf.len = sizeof reqs;
    conf.buf = reqs;
    i64 r2 = sys3(SYS_ioctl, ifd, SIOCGIFCONF, &conf);
    int conf_lo = 0;
    for (int i = 0; r2 == 0 && i < conf.len / 40; i++) {
        put_s("netlinkprobe: ifconf ");
        put_s(reqs[i].name);
        put_s(" ");
        put_ip(reqs[i].u.addr.data + 2);
        flush();
        conf_lo |= streq(reqs[i].name, "lo") && reqs[i].u.addr.family == AF_INET && reqs[i].u.addr.data[2] == 127;
    }
    check(r == 0 && need >= 40 && need % 40 == 0 && r2 == 0 && conf.len == need && conf_lo, "SIOCGIFCONF lists lo 127.0.0.1");

    struct ifreq q;
    set_name(&q, "lo");
    r = sys3(SYS_ioctl, ifd, SIOCGIFHWADDR, &q);
    check(r == 0 && q.u.addr.family == 772, "SIOCGIFHWADDR lo is ARPHRD_LOOPBACK");
    if (eth) {
        set_name(&q, "eth0");
        r = sys3(SYS_ioctl, ifd, SIOCGIFHWADDR, &q);
        put_s("netlinkprobe: eth0 hwaddr ");
        put_mac(q.u.addr.data);
        flush();
        int same = 1;
        for (int i = 0; i < 6; i++) same &= q.u.addr.data[i] == eth->mac[i];
        check(r == 0 && q.u.addr.family == 1 && same, "SIOCGIFHWADDR eth0 is ARPHRD_ETHER with the netlink MAC");
    }
    set_name(&q, "lo");
    r = sys3(SYS_ioctl, ifd, SIOCGIFFLAGS, &q);
    check(r == 0 && (q.u.flags & (IFF_UP | IFF_LOOPBACK)) == (IFF_UP | IFF_LOOPBACK), "SIOCGIFFLAGS lo is up and loopback");
    set_name(&q, "lo");
    r = sys3(SYS_ioctl, ifd, SIOCGIFMTU, &q);
    check(r == 0 && q.u.value == 65536, "SIOCGIFMTU lo is 65536");
    set_name(&q, "lo");
    r = sys3(SYS_ioctl, ifd, SIOCGIFINDEX, &q);
    check(r == 0 && q.u.value == 1, "SIOCGIFINDEX lo is 1");
    set_name(&q, "");
    q.u.value = 1;
    r = sys3(SYS_ioctl, ifd, SIOCGIFNAME, &q);
    check(r == 0 && streq(q.name, "lo"), "SIOCGIFNAME 1 is lo");
    set_name(&q, "lo");
    r = sys3(SYS_ioctl, ifd, SIOCGIFADDR, &q);
    i64 r3 = sys3(SYS_ioctl, ifd, SIOCGIFNETMASK, &q);
    check(r == 0 && r3 == 0 && q.u.addr.family == AF_INET && q.u.addr.data[2] == 255 && q.u.addr.data[3] == 0,
          "SIOCGIFADDR and SIOCGIFNETMASK lo");
    set_name(&q, "lo");
    check(sys3(SYS_ioctl, ifd, SIOCGIWNAME, &q) == -ENODEV, "SIOCGIWNAME is ENODEV (not wireless)");
    set_name(&q, "nosuch0");
    check(sys3(SYS_ioctl, ifd, SIOCGIFFLAGS, &q) == -ENODEV, "an unknown interface is ENODEV");
    sys3(SYS_close, ifd, 0, 0);

    /* Файлы: счётчики и `/sys/class/net`. */
    i64 len = read_file("/proc/net/dev");
    check(len > 0 && contains(file_buf, (u64)len, "Inter-|") && contains(file_buf, (u64)len, "    lo:"), "/proc/net/dev lists lo");
    if (eth) check(len > 0 && contains(file_buf, (u64)len, "  eth0:"), "/proc/net/dev lists eth0");
    len = read_file("/sys/class/net/lo/mtu");
    check(len > 0 && streq(file_buf, "65536\n"), "/sys/class/net/lo/mtu is 65536");
    len = read_file("/proc/net/route");
    check(len > 0 && contains(file_buf, (u64)len, "Iface\tDestination\tGateway"), "/proc/net/route has the header");
    len = read_file("/proc/net/snmp");
    check(len > 0 && contains(file_buf, (u64)len, "Tcp: RtoAlgorithm") && contains(file_buf, (u64)len, "Udp: "), "/proc/net/snmp has Tcp and Udp");
    check(read_file("/proc/net/if_inet6") == 0, "/proc/net/if_inet6 is empty");

    put_s("netlinkprobe: ");
    put_u((u64)failed);
    put_s(" failed");
    flush();
    sys3(SYS_exit_group, failed, 0, 0);
    for (;;) {
    }
}

__asm__(".text\n"
        ".globl _start\n"
        "_start:\n"
        "  xor %rbp, %rbp\n"
        "  mov %rsp, %rdi\n"
        "  and $-16, %rsp\n"
        "  call start_c\n"
        "  hlt\n");
