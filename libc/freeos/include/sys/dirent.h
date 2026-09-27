/*
 * SPDX-License-Identifier: BSD-3-Clause
 *
 * Copyright © 2026 Keith Packard
 *
 * Redistribution and use in source and binary forms, with or without
 * modification, are permitted provided that the following conditions
 * are met:
 *
 * 1. Redistributions of source code must retain the above copyright
 *    notice, this list of conditions and the following disclaimer.
 *
 * 2. Redistributions in binary form must reproduce the above
 *    copyright notice, this list of conditions and the following
 *    disclaimer in the documentation and/or other materials provided
 *    with the distribution.
 *
 * 3. Neither the name of the copyright holder nor the names of its
 *    contributors may be used to endorse or promote products derived
 *    from this software without specific prior written permission.
 *
 * THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS
 * "AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT
 * LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS
 * FOR A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE
 * COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT,
 * INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES
 * (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
 * SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION)
 * HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT,
 * STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE)
 * ARISING IN ANY WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED
 * OF THE POSSIBILITY OF SUCH DAMAGE.
 */

#ifndef _SYS_DIRENT_H_
#define _SYS_DIRENT_H_

#include <sys/cdefs.h>
#include <sys/_types.h>

_BEGIN_STD_C

#ifndef _INO_T_DECLARED
typedef __ino_t ino_t; /* inode number */
#define _INO_T_DECLARED
#endif

typedef size_t reclen_t;

#ifndef _SSIZE_T_DECLARED
typedef __ssize_t ssize_t;
#define _SSIZE_T_DECLARED
#endif

struct dirent {
    ino_t     d_ino; /* Inode number */
    __uint8_t d_type;
    char      d_name[__NAME_MAX + 1]; /* Null-terminated filename */
};

struct posix_dent {
    ino_t         d_ino;
    reclen_t      d_reclen;
    unsigned char d_type;
    char          d_name[];
};

typedef struct {
    int           fd;
    size_t        offset;
    size_t        count;
    struct dirent dirent;
    union {
        char       buf[512];
        __uint64_t align;
    };
} DIR;

/*
 * FreeOS (фаза 60): числа — те же, что у Linux и BSD, а не свои picolibc
 * (1…7 подряд). Чужой код их сверяет: System.Native из Mono проверяет каждое
 * `static_assert`-ом и с другими числами не собирается. Внутри picolibc их не
 * читает никто (своих readdir, scandir, nftw в ней нет — они в нашей libc), так
 * что замена ничего не ломает. Весь остальной файл — заголовок picolibc как
 * есть.
 */
#define DT_UNKNOWN 0
#define DT_FIFO    1
#define DT_CHR     2
#define DT_DIR     4
#define DT_BLK     6
#define DT_REG     8
#define DT_LNK     10
#define DT_SOCK    12
#define DT_WHT     14

_END_STD_C

#endif
