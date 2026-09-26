/* Системный журнал для программ на C (фаза 58).
 *
 * Отдельной службы журнала в FreeOS нет: запись уходит в поток ошибок
 * программы с именем и уровнем впереди — туда, где её увидит человек и
 * стенд. Уровень ниже заданного `setlogmask` отбрасывается, как положено. */

#ifndef FREEOS_SYSLOG_H
#define FREEOS_SYSLOG_H

#include <stdarg.h>

#ifdef __cplusplus
extern "C" {
#endif

#define LOG_EMERG 0
#define LOG_ALERT 1
#define LOG_CRIT 2
#define LOG_ERR 3
#define LOG_WARNING 4
#define LOG_NOTICE 5
#define LOG_INFO 6
#define LOG_DEBUG 7

#define LOG_PID 0x01
#define LOG_CONS 0x02
#define LOG_NDELAY 0x08
#define LOG_USER (1 << 3)
#define LOG_DAEMON (3 << 3)

#define LOG_MASK(priority) (1 << (priority))
#define LOG_UPTO(priority) ((1 << ((priority) + 1)) - 1)

void openlog(const char *ident, int option, int facility);
void syslog(int priority, const char *format, ...);
void vsyslog(int priority, const char *format, va_list args);
void closelog(void);
int setlogmask(int mask);

#ifdef __cplusplus
}
#endif

#endif
