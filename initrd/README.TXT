FreeOS initrd -- FAT32 RAM disk image
=====================================

Everything in the initrd/ directory of the source tree is packed, recursively,
into build/initrd.img by `cargo xtask build`, and the image is placed in the
root of the ESP as \initrd.img. To add a file to this demo, just drop it into
initrd/ -- nothing in xtask has to be edited.

This particular file deliberately has a plain 8.3 name (README.TXT), so a
driver that only understands short directory entries still finds something.
Its neighbours do not: they are here to exercise long file names.

Every text file in this image ends with a line beginning with "END-OF-FILE".
If the kernel prints a file and that line is missing, mangled or duplicated,
the FAT driver lost or repeated part of a cluster chain -- the image is fine,
the reader is not.

END-OF-FILE README.TXT OK
