/* <sys/sysinfo.h> for 7-Zip (ports/7zip): it reads the RAM size with
 * Linux sysinfo() to pick memory limits. picolibc has no such call; the
 * FossAi libc answers sysconf(_SC_PHYS_PAGES) from SYS_SYSINFO. */
#ifndef FOSSAI_7ZIP_SYS_SYSINFO_H
#define FOSSAI_7ZIP_SYS_SYSINFO_H
#include <unistd.h>
#include <string.h>
struct sysinfo {
    long uptime;
    unsigned long loads[3];
    unsigned long totalram;
    unsigned long freeram;
    unsigned long sharedram;
    unsigned long bufferram;
    unsigned long totalswap;
    unsigned long freeswap;
    unsigned short procs;
    unsigned long totalhigh;
    unsigned long freehigh;
    unsigned int mem_unit;
};
static inline int sysinfo(struct sysinfo *info) {
    long pages = sysconf(_SC_PHYS_PAGES);
    long size = sysconf(_SC_PAGESIZE);
    if (pages <= 0 || size <= 0)
        return -1;
    memset(info, 0, sizeof *info);
    info->totalram = (unsigned long)pages;
    info->mem_unit = (unsigned int)size;
    return 0;
}
#endif
