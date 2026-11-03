/* Заглушка POSIX для инструментов сборки NetSurf на машине сборки (Windows).
 *
 * NetSurf собирает на машине сборки три своих программы: `convert_font`
 * (растровый шрифт в C), `convert_image` (значки PNG в C) и `split-messages`
 * (сообщения интерфейса). Пишутся они под Unix и ждут `<unistd.h>`,
 * `getopt`, `getopt_long` и `getline`, которых у clang поверх библиотек MSVC
 * нет. Всё здесь `static`, в одном заголовке: правило `convert_font` в их
 * `tools/Makefile` компонует без библиотек вовсе, и отдельному файлу с
 * реализацией некуда было бы попасть. Это обход только для машины сборки — в
 * систему ничего отсюда не едет. */
#ifndef NETSURF_HOST_UNISTD_H
#define NETSURF_HOST_UNISTD_H

#include <io.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <BaseTsd.h>

typedef SSIZE_T ssize_t;

#define unlink _unlink

static char *optarg;
static int optind = 1;
static int opterr = 1;
static int optopt;

struct option {
    const char *name;
    int has_arg;
    int *flag;
    int val;
};

#define no_argument 0
#define required_argument 1
#define optional_argument 2

/* Короткие ключи — как у POSIX `getopt`, без перестановки аргументов: им
 * ключи всегда передаются впереди файлов. */
static int netsurf_host_short(int argc, char *const argv[], const char *spec)
{
    static int next = 1;
    const char *arg, *at;
    int c;

    if (optind >= argc || argv[optind][0] != '-' || argv[optind][1] == '\0') {
        return -1;
    }
    if (strcmp(argv[optind], "--") == 0) {
        optind++;
        return -1;
    }
    arg = argv[optind];
    c = (unsigned char)arg[next];
    at = strchr(spec, c);
    if (at == NULL || c == ':') {
        optopt = c;
        if (arg[++next] == '\0') {
            optind++;
            next = 1;
        }
        return '?';
    }
    if (at[1] == ':') {
        if (arg[next + 1] != '\0') {
            optarg = (char *)&arg[next + 1];
        } else if (optind + 1 < argc) {
            optarg = argv[++optind];
        } else {
            optopt = c;
            optind++;
            next = 1;
            return '?';
        }
        optind++;
        next = 1;
        return c;
    }
    if (arg[++next] == '\0') {
        optind++;
        next = 1;
    }
    return c;
}

static int getopt(int argc, char *const argv[], const char *spec)
{
    return netsurf_host_short(argc, argv, spec);
}

static int getopt_long(int argc, char *const argv[], const char *spec, const struct option *longs, int *index)
{
    if (optind < argc && strncmp(argv[optind], "--", 2) == 0 && argv[optind][2] != '\0') {
        const char *name = argv[optind] + 2;
        const char *eq = strchr(name, '=');
        size_t len = eq ? (size_t)(eq - name) : strlen(name);
        int i;
        for (i = 0; longs && longs[i].name; i++) {
            if (strlen(longs[i].name) == len && strncmp(longs[i].name, name, len) == 0) {
                optind++;
                optarg = NULL;
                if (longs[i].has_arg != no_argument) {
                    if (eq) {
                        optarg = (char *)eq + 1;
                    } else if (longs[i].has_arg == required_argument && optind < argc) {
                        optarg = argv[optind++];
                    }
                }
                if (index) {
                    *index = i;
                }
                if (longs[i].flag) {
                    *longs[i].flag = longs[i].val;
                    return 0;
                }
                return longs[i].val;
            }
        }
        optind++;
        return '?';
    }
    return netsurf_host_short(argc, argv, spec);
}

static ssize_t getline(char **line, size_t *size, FILE *file)
{
    size_t used = 0;
    int c;

    if (*line == NULL || *size == 0) {
        *size = 256;
        *line = malloc(*size);
        if (*line == NULL) {
            return -1;
        }
    }
    while ((c = fgetc(file)) != EOF) {
        if (used + 2 > *size) {
            char *grown = realloc(*line, *size * 2);
            if (grown == NULL) {
                return -1;
            }
            *line = grown;
            *size *= 2;
        }
        (*line)[used++] = (char)c;
        if (c == '\n') {
            break;
        }
    }
    if (used == 0) {
        return -1;
    }
    (*line)[used] = '\0';
    return (ssize_t)used;
}

#endif
