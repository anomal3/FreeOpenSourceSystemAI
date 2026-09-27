/* <unistd.h> FreeOS (фаза 60): заголовок picolibc и четыре имени `sysconf`,
 * которых у неё нет.
 *
 * У picolibc этот файл — одна строка, `#include <sys/unistd.h>`, и заменён он
 * целиком ради того, что ниже. Сколько памяти и процессоров у машины, чужая
 * среда исполнения спрашивает у `sysconf`, и спрашивает через `#ifdef`: Mono
 * без `_SC_PHYS_PAGES` печатает «sysconf doesn't correctly report physical
 * memory size» и берёт 128 МиБ наугад, а без `_SC_NPROCESSORS_ONLN` считает,
 * что процессор один. Отвечает на них наш `sysconf` (`posix.c`) — по
 * `SYS_SYSINFO`.
 *
 * Номера — после последнего номера picolibc (137, `_SC_POSIX_26_VERSION`), а
 * не номера Linux: те у picolibc уже заняты другими именами.
 */
#ifndef _UNISTD_H_
#define _UNISTD_H_

#include <sys/unistd.h>

#define _SC_NPROCESSORS_CONF 138
#define _SC_NPROCESSORS_ONLN 139
#define _SC_PHYS_PAGES       140
#define _SC_AVPHYS_PAGES     141

#endif /* _UNISTD_H_ */
