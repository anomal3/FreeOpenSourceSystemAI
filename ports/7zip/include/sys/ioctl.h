/* <sys/ioctl.h> for 7-Zip (ports/7zip): picolibc's names TIOCGWINSZ but
 * keeps `struct winsize` in <sys/termios.h>. 7-Zip asks the terminal width
 * for its progress line; without an answer it keeps 80 columns. */
#ifndef FOSSAI_7ZIP_SYS_IOCTL_H
#define FOSSAI_7ZIP_SYS_IOCTL_H
#include_next <sys/ioctl.h>
#include <sys/termios.h>
#endif
