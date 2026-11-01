/*
 * Раскладка vDSO FossAi: всё в одном сегменте «чтение и исполнение» от
 * нулевого адреса, страница данных времени (`vvar_page`) — сразу перед ним.
 * Ядро отображает её туда же, и код находит её относительно себя, без
 * перемещений: применять их vDSO некому.
 */
SECTIONS
{
    PROVIDE_HIDDEN(vvar_page = . - 4096);
    . = SIZEOF_HEADERS;

    .hash           : { *(.hash) }          :text
    .gnu.hash       : { *(.gnu.hash) }
    .dynsym         : { *(.dynsym) }
    .dynstr         : { *(.dynstr) }
    .gnu.version    : { *(.gnu.version) }
    .gnu.version_d  : { *(.gnu.version_d) }
    .gnu.version_r  : { *(.gnu.version_r) }

    .dynamic        : { *(.dynamic) }       :text :dynamic

    .rodata         : { *(.rodata*) }       :text
    .text           : { *(.text*) }         :text

    /DISCARD/ : {
        *(.data*) *(.bss*) *(.got*) *(.plt*) *(.eh_frame*) *(.note*) *(.comment*)
    }
}

PHDRS
{
    text    PT_LOAD    FLAGS(5) FILEHDR PHDRS;
    dynamic PT_DYNAMIC FLAGS(4);
}
