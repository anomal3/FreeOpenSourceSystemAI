/* Контекст потока — то, что получил бы обработчик сигнала третьим аргументом
 * (фаза 58b).
 *
 * Ядро сигналов не посылает (см. `sigaction` в `posix.c`), поэтому этот тип
 * никто не заполняет: он нужен коду, который **описывает**, как читать
 * регистры из контекста, — чужой среде исполнения, собираемой целиком. Раскладка
 * — как у Linux, до поля: если сигналы когда-нибудь появятся, ядро будет
 * класть контекст в этом виде, и собранные программы не придётся пересобирать.
 *
 * `getcontext`/`setcontext`/`makecontext` не объявлены: их нет.
 */

#ifndef FREEOS_UCONTEXT_H
#define FREEOS_UCONTEXT_H

#include <signal.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#if defined(__x86_64__)

typedef long long greg_t;
#define NGREG 23
typedef greg_t gregset_t[NGREG];

/* Номера регистров в `gregs` — те же, что у Linux. */
#define REG_R8 0
#define REG_R9 1
#define REG_R10 2
#define REG_R11 3
#define REG_R12 4
#define REG_R13 5
#define REG_R14 6
#define REG_R15 7
#define REG_RDI 8
#define REG_RSI 9
#define REG_RBP 10
#define REG_RBX 11
#define REG_RDX 12
#define REG_RAX 13
#define REG_RCX 14
#define REG_RSP 15
#define REG_RIP 16
#define REG_EFL 17
#define REG_CSGSFS 18
#define REG_ERR 19
#define REG_TRAPNO 20
#define REG_OLDMASK 21
#define REG_CR2 22

/* Образ `fxsave`: 512 байт. */
struct _libc_fpstate {
    uint16_t cwd;
    uint16_t swd;
    uint16_t ftw;
    uint16_t fop;
    uint64_t rip;
    uint64_t rdp;
    uint32_t mxcsr;
    uint32_t mxcr_mask;
    uint32_t _st[32];
    uint32_t _xmm[64];
    uint32_t _padding[24];
};
typedef struct _libc_fpstate *fpregset_t;

typedef struct {
    gregset_t gregs;
    fpregset_t fpregs;
    unsigned long long __reserved1[8];
} mcontext_t;

typedef struct ucontext_t {
    unsigned long uc_flags;
    struct ucontext_t *uc_link;
    stack_t uc_stack;
    mcontext_t uc_mcontext;
    sigset_t uc_sigmask;
    struct _libc_fpstate __fpregs_mem;
} ucontext_t;

#elif defined(__aarch64__)

/* `__reserved` — место под записи расширений (FPSIMD и прочие), как у Linux. */
typedef struct {
    unsigned long long fault_address;
    unsigned long long regs[31];
    unsigned long long sp;
    unsigned long long pc;
    unsigned long long pstate;
    unsigned char __reserved[4096] __attribute__((aligned(16)));
} mcontext_t;

typedef struct ucontext_t {
    unsigned long uc_flags;
    struct ucontext_t *uc_link;
    stack_t uc_stack;
    sigset_t uc_sigmask;
    mcontext_t uc_mcontext;
} ucontext_t;

#else
#error "ucontext.h: unknown architecture"
#endif

#ifdef __cplusplus
}
#endif

#endif
