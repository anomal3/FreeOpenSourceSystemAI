/* major()/minor()/makedev() for 7-Zip (ports/7zip): picolibc has no
 * <sys/sysmacros.h>. FossAi device numbers use the Linux encoding. */
#ifndef FOSSAI_7ZIP_SYS_SYSMACROS_H
#define FOSSAI_7ZIP_SYS_SYSMACROS_H
#include <sys/types.h>
#ifndef major
#define major(dev) ((unsigned)((((dev) >> 32) & 0xfffff000u) | (((dev) >> 8) & 0xfffu)))
#endif
#ifndef minor
#define minor(dev) ((unsigned)((((dev) >> 12) & 0xffffff00u) | ((dev) & 0xffu)))
#endif
#ifndef makedev
#define makedev(x, y) ((dev_t)((((unsigned long long)(x) & 0xfffff000ull) << 32) | (((unsigned long long)(x) & 0xfffull) << 8) | \
    (((unsigned long long)(y) & 0xffffff00ull) << 12) | ((unsigned long long)(y) & 0xffull)))
#endif
#endif
