/* Адрес сокета в файловой системе (фаза 58). Сокетов этого вида у FreeOS
 * нет; раскладка — ради чужого кода, который описывает её у себя. */
#ifndef FREEOS_SYS_UN_H
#define FREEOS_SYS_UN_H
#include <sys/socket.h>
struct sockaddr_un {
    sa_family_t sun_family;
    char sun_path[108];
};
#endif
