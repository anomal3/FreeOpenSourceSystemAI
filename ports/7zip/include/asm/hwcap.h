/* <asm/hwcap.h> for 7-Zip on AArch64 (ports/7zip): the bits of AT_HWCAP
 * (Linux numbering) it tests before using the CRC32/SHA/AES instructions.
 * Without them in the auxiliary vector it takes the plain C code. */
#ifndef FOSSAI_7ZIP_ASM_HWCAP_H
#define FOSSAI_7ZIP_ASM_HWCAP_H
#define HWCAP_FP     (1 << 0)
#define HWCAP_ASIMD  (1 << 1)
#define HWCAP_AES    (1 << 3)
#define HWCAP_PMULL  (1 << 4)
#define HWCAP_SHA1   (1 << 5)
#define HWCAP_SHA2   (1 << 6)
#define HWCAP_CRC32  (1 << 7)
#define HWCAP_SHA512 (1 << 21)
#endif
