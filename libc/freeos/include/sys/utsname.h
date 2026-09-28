/* Имя системы (фаза 61b). У picolibc этого заголовка нет вовсе: `uname` —
 * вопрос к ядру, а не к библиотеке. Его зовут библиотеки классов Mono —
 * `System.Drawing` и `System.dll` через `[DllImport("libc")]`, чтобы отличить
 * macOS от прочих Unix, — а без ответа первое же обращение к `System.Drawing`
 * кончается `TypeInitializationException`.
 *
 * Размер полей — как у Linux (65 байт): кто прочтёт структуру по чужому
 * описанию, прочтёт её верно. */
#ifndef FREEOS_SYS_UTSNAME_H
#define FREEOS_SYS_UTSNAME_H
#ifdef __cplusplus
extern "C" {
#endif
#define _UTSNAME_LENGTH 65
struct utsname {
    char sysname[_UTSNAME_LENGTH];
    char nodename[_UTSNAME_LENGTH];
    char release[_UTSNAME_LENGTH];
    char version[_UTSNAME_LENGTH];
    char machine[_UTSNAME_LENGTH];
};
int uname(struct utsname *name);
#ifdef __cplusplus
}
#endif
#endif
