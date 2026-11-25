//! Rupor (24.11.2026, Roman: «собери Linux-клиент Рупора и положи в FossAi»):
//! the Linux client of Roman's messenger Rupor (`RuporAppAvalonia.Desktop`,
//! .NET 9 self-contained, Avalonia 11 on X11 with a WebKitGTK view for the
//! Blazor UI) as the package `rupor-1.9.50.fpk` (`/opt/rupor` after `pkg
//! install`), only with `FOSSAI_WITH_RUPOR=1`.
//!
//! # Why X11
//!
//! Avalonia 11.3 has no Wayland backend (`UsePlatformDetect` on Linux is X11),
//! and the WebView of `WebView.Avalonia.Linux.Cross` forces GTK onto X11
//! (`gdk_set_allowed_backends("x11")`), takes the XID of its GTK window and
//! reparents it into the Avalonia window. So the client gets an X server:
//! **Xwayland 24.1 rootful** (Debian 13 `xwayland`) - one window of the kernel
//! Wayland server, the desktop frames it (`org.freedesktop.Xwayland`), and
//! inside it `rupor-wm` (`libc/linux/rupor-wm.c`, 300 lines of Xlib) keeps the
//! Rupor main window the size of the root and follows its resizes.
//!
//! # What is taken
//!
//! As for OBS and VLC: not the `Depends` closure, but what is **loaded** - the
//! `DT_NEEDED` chains of Xwayland, `xkbcomp`, bash, WebKit's web and network
//! processes, the GIO TLS module, the native libraries of the published app,
//! and the libraries the app opens itself (`dlopen`/P/Invoke: libX11 and its
//! extensions, GTK 3, WebKitGTK 4.1, ICU, OpenSSL, fontconfig) over the trixie
//! packages [`PACKAGES`] (snapshot 20261020). glibc - its own 2.41
//! (`/opt/rupor/glibc`), `PT_INTERP` of every program rewritten to its
//! `ld.so`.
//!
//! # Fixed paths and how they are met
//!
//! * The client asks for `libwebkit2gtk-4.0.so.37` (the soname of WebKitGTK
//!   4.0, which Debian 13 no longer has): a stub library with that soname and
//!   `DT_NEEDED libwebkit2gtk-4.1.so.0` - `dlsym` on its handle finds the 4.1
//!   functions (the C API of the two is the same; only libsoup differs).
//! * WebKit starts its processes from `/usr/lib/x86_64-linux-gnu/webkit2gtk-4.1`:
//!   that string in `libwebkit2gtk-4.1.so.0` is rewritten to `/opt/rupor/lib/…`.
//! * Xwayland compiles keymaps with `/bin/sh -c "/usr/bin/xkbcomp …"`: `/bin/sh`
//!   of the Debian 12 root cannot run with glibc 2.41 in `LD_LIBRARY_PATH`, so
//!   the strings are `/tmp/xs` and `/tmp/xb` in Xwayland, and the launcher puts
//!   bash and xkbcomp of the package there.
//!
//! # Where the app comes from
//!
//! `FOSSAI_RUPOR_PUBLISH=<dir>` - a ready `dotnet publish` for linux-x64;
//! otherwise the sources (`FOSSAI_RUPOR_SRC`, by default Roman's
//! `C:\Users\asdk\RiderProjects\Rupor`) are copied to `build/toolchain/rupor`
//! (the repository itself is not touched - its `global.json` pins SDK 10.0.102,
//! the copy takes any installed SDK) and published there: self-contained, not
//! single-file (no extraction to `~/.net` on every start).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use fpk::Kind;
use fpk::build::{Builder, Entry};

use crate::arch::Arch;
use crate::chromium::needed;
use crate::debian::{Node, follow, unpack};
use crate::{cbuild, sdk};

/// Version of the client (`AssemblyVersion` of `RuporAppAvalonia.Desktop`).
pub const VERSION: &str = "1.9.50";

/// Name of the package file.
pub const PACKAGE_FILE: &str = "rupor-1.9.50.fpk";

/// Where the built package lies.
pub fn package_path() -> PathBuf {
    cbuild::games_dir().join(PACKAGE_FILE)
}

/// Whether Rupor goes onto the install medium: half a gigabyte - only with
/// `FOSSAI_WITH_RUPOR=1` (like OBS and VLC).
pub fn enabled() -> bool {
    std::env::var("FOSSAI_WITH_RUPOR").is_ok_and(|value| value == "1")
}

/// Main package (the thirdparty list wants one): Xwayland.
pub const MAIN_DEB: (&str, &str) =
    ("pool/main/x/xwayland/xwayland_24.1.6-1_amd64.deb", "7e51a144e858d5be7e91d6644c2758a618e425c7222234d2e8f059e5aba6c1f5");

/// trixie packages (snapshot 20261020): path in the archive and SHA-256 from
/// `Packages`. The owners of every library the loaded chains need (computed by
/// the `DT_NEEDED` walk below), plus bash, xkb-data, DejaVu and libc-bin (the
/// C.UTF-8 locale).
const PACKAGES: [(&str, &str); 143] = [
    ("pool/main/b/bash/bash_5.2.37-2+b10_amd64.deb", "2fd7b04f1b7caa29c4e683f5216e6af354a41e3cdcf6e75394d7cda680f9ab82"),
    ("pool/main/f/fonts-dejavu/fonts-dejavu-core_2.37-8_all.deb", "86635b3d25b3655fc11cb3ecc3af59f0bf19643b02b94f2de48bd10253cdba12"),
    ("pool/main/g/glib-networking/glib-networking_2.80.1-1_amd64.deb", "183829044aa67f7e8c41a407dcee71d446da0ed3c87113689603a27007099137"),
    ("pool/main/a/abseil/libabsl20240722_20240722.0-4_amd64.deb", "78c6d7692890990a27cb1d5bbd95b7bfe47a4183ffafec5b30f689f9b58ec6f7"),
    ("pool/main/a/aom/libaom3_3.12.1-1+deb13u1_amd64.deb", "743eec7c94ae30295bc557ebac3dfae948e7390e89a2743c585d56297e9d312e"),
    ("pool/main/a/alsa-lib/libasound2t64_1.2.14-1+deb13u1_amd64.deb", "5495496142d57e5ad6d581ac5456ec3c87f872c109a2bfb815d154b673eebc67"),
    ("pool/main/a/at-spi2-core/libatk-bridge2.0-0t64_2.56.2-1+deb13u2_amd64.deb", "c0fe87ea1bdca2f938eaa8286af5ad38ecd15cf18c6cfb5961d7d46ac3192325"),
    ("pool/main/a/at-spi2-core/libatk1.0-0t64_2.56.2-1+deb13u2_amd64.deb", "24786bc90e3ff80c4d1d8bdb5ec18bb5f06df9081a5233eb080f3202fcf6a1c3"),
    ("pool/main/g/gcc-14/libatomic1_14.2.0-19_amd64.deb", "212b399aae2f7299203d261a57e49372e09565a9a5ea971905f94a3960366c05"),
    ("pool/main/a/at-spi2-core/libatspi2.0-0t64_2.56.2-1+deb13u2_amd64.deb", "65e9c9ecbd820fba2d04104cddd0bad33d256cbdfe0d1cb5365f10e363df2315"),
    ("pool/main/liba/libavif/libavif16_1.2.1-1.2_amd64.deb", "ba6c334454d3562560ad583fbe2c998e203b0fc5a0e0e91112ddcd3cff4704bb"),
    ("pool/main/u/util-linux/libblkid1_2.41.5-0+deb13u1_amd64.deb", "81535f3c2c0efc732965907c8749103a0a26377c761622c9ce39b4c92dcde52f"),
    ("pool/main/b/brotli/libbrotli1_1.1.0-2+b7_amd64.deb", "0fb79f88db210afbd69282ab9649e525f393ec6950ca34da1a6b359250b8d7db"),
    ("pool/main/b/bzip2/libbz2-1.0_1.0.8-6_amd64.deb", "cba4cda04244b5e481bb15524bc3c983a7d1b6f330013b9b381706a2fcb65310"),
    ("pool/main/g/glibc/libc-bin_2.41-12+deb13u4_amd64.deb", "dc8c79647fe3a5f37f72a400414f70263d0212f010ff2c09f990e30d91e38d1c"),
    ("pool/main/g/glibc/libc6_2.41-12+deb13u4_amd64.deb", "967aa62605721081c3eb2a17650611a792aa802d76a6511d1840242623d204c9"),
    ("pool/main/c/cairo/libcairo-gobject2_1.18.4-1+b1_amd64.deb", "4fcd432ebe8f93599bd05426f304e2f70409aad271fc6b4b3a2c6243c99c377c"),
    ("pool/main/c/cairo/libcairo2_1.18.4-1+b1_amd64.deb", "01390b049661e4cc8bd39cef78b41c2587d4035619d3b00d6c7846f43d80bbba"),
    ("pool/main/libc/libcap2/libcap2_2.75-10+deb13u1+b3_amd64.deb", "89fc4d34fc7a28ad6f0fcd0c561ab253b9dedf6f77f5a000b47c276c8295bf67"),
    ("pool/main/libc/libcloudproviders/libcloudproviders0_0.3.6-2_amd64.deb", "a626bb23ed9ebc4bc3ce446b552048de63d76768bf0fb492754965c5ee28a7e4"),
    ("pool/main/e/e2fsprogs/libcom-err2_1.47.2-3+b12_amd64.deb", "bd43e020b8fed399db3eebf098111f10fba4c436fff99ccad72cc7e27c9764ec"),
    ("pool/main/libd/libdatrie/libdatrie1_0.2.13-3+b1_amd64.deb", "339eb88f23d72b8f0b7b1aebcefbab6de600d97b88bc6222dd8aad11d2ca9051"),
    ("pool/main/d/dav1d/libdav1d7_1.5.1-1_amd64.deb", "8c64de4465a8ae1ca23e1dada1dfc73d7b431540fd5ae18f21430928576d5b05"),
    ("pool/main/d/dbus/libdbus-1-3_1.16.2-2_amd64.deb", "bbcb711daff7e104b5f80f9b05475b6142674bfe65518a36a56e96a91068a3f5"),
    ("pool/main/libd/libdecor-0/libdecor-0-0_0.2.2-2_amd64.deb", "e4f228a9d4a38af23fab00f8e20219790b567e2e1f7804b3a215a26294350f82"),
    ("pool/main/libd/libdrm/libdrm2_2.4.124-2_amd64.deb", "fe2276901c7cd7b8079de63072d37fe1cbeb4eb001a3bc1f1d662ad89aa0890e"),
    ("pool/main/libg/libglvnd/libegl1_1.7.0-1+b2_amd64.deb", "62e7bc8ccd47a60e55aad17f83d8d474310d782eb8589bbce2ab714c31712406"),
    ("pool/main/e/enchant-2/libenchant-2-2_2.8.2+dfsg1-3_amd64.deb", "83f208d89db1c48e6ede8a23e8ffa2bcddd4f99427bfd1279201247bbf27ca58"),
    ("pool/main/libe/libepoxy/libepoxy0_1.5.10-2_amd64.deb", "4c4c8024f2175086de65bca9fdc3fbb967f2863ebf249d489c66d0c8103dd3a3"),
    ("pool/main/libe/libevdev/libevdev2_1.13.4+dfsg-1_amd64.deb", "02e85fb9461b85b23f775d5fcf5a31c55d37bec40b63fd98ca5d3a2763714ae7"),
    ("pool/main/e/expat/libexpat1_2.8.3-1~deb13u1_amd64.deb", "38abe0e710a07688e9c149d74536e67cfee0364bdb64dd6d644c32a1cfad389f"),
    ("pool/main/libf/libffi/libffi8_3.4.8-2_amd64.deb", "0ebdc340de33333639c3c63874cd4b15ac2e83dfa1ef3053b7eefaf4919f4f68"),
    ("pool/main/f/flite/libflite1_2.2-7_amd64.deb", "5945190970be7d4a1f71fb6f7e3e340ba2675e770605df0754051d617a6c5ee6"),
    ("pool/main/f/fontconfig/libfontconfig1_2.15.0-2.3_amd64.deb", "7ae91ec59857cc8e375eb2bdd371a4a69daea38f7787d06d474a847867d852b1"),
    ("pool/main/libf/libfontenc/libfontenc1_1.1.8-1+b2_amd64.deb", "49f7330e1bbe3f48946fee5c07870df660374f9f77b378840d6edf1a3b3444cd"),
    ("pool/main/f/freetype/libfreetype6_2.13.3+dfsg-1+deb13u1_amd64.deb", "e4947f3291528f03d574f2b01d5c5fc45c58c47480f32888a5cdf0f231ab584e"),
    ("pool/main/f/fribidi/libfribidi0_1.0.16-1_amd64.deb", "f8c35ec8606d0843bd3cf020c6cfa39ddfe717ec58ed826b5855ba9927eb0d55"),
    ("pool/main/libg/libgav1/libgav1-1_0.19.0-3+b1_amd64.deb", "94010041e59c7a8d2d3d1f5e62afd523f002b7b1caa3189419160b66d71c0195"),
    ("pool/main/m/mesa/libgbm1_25.0.7-2+deb13u1_amd64.deb", "31fb6d76b9ceaf13848fa617df53f85f62626b4fe7464a93811c720af6d5f2dd"),
    ("pool/main/g/gcc-14/libgcc-s1_14.2.0-19_amd64.deb", "3c71917b490d1a17aed43196a2787a256ecf060526cdb20216a74bedc061b150"),
    ("pool/main/libg/libgcrypt20/libgcrypt20_1.11.0-7+deb13u1_amd64.deb", "f0eb41f8cb40896e0822e9ab65611e8383c12b721c76c54764f744abb4a8c247"),
    ("pool/main/g/gdk-pixbuf/libgdk-pixbuf-2.0-0_2.42.12+dfsg-4+deb13u1_amd64.deb", "94150fb955b2d3a9a5e054e31c5a4b048bad4a479207e86cde46fbc975c838b4"),
    ("pool/main/libg/libglvnd/libgl1_1.7.0-1+b2_amd64.deb", "87fa2f6e5abaed4ed385fac879c8dd735af719ee2300222d901793c66e041678"),
    ("pool/main/g/glib2.0/libglib2.0-0t64_2.84.4-3~deb13u5_amd64.deb", "e2baf92c57d1db1753e5781a61450d9a01e7833c7bd2dfe87776a6811c11ecc2"),
    ("pool/main/libg/libglvnd/libglvnd0_1.7.0-1+b2_amd64.deb", "887f74008166549ce9e100c906aa937e95d6e5ce1c8d86efe8c95fd953359b9c"),
    ("pool/main/libg/libglvnd/libglx0_1.7.0-1+b2_amd64.deb", "2721fdca0fe3bd963cb39482eabc253af52b88f4a7f6dbb69e475549daf5af3b"),
    ("pool/main/g/gmp/libgmp10_6.3.0+dfsg-3_amd64.deb", "d0d0265eb01770f17afd0f7c8c0622f80479dcfbbe13653a0debeec61464e622"),
    ("pool/main/g/gnutls28/libgnutls30t64_3.8.9-3+deb13u4_amd64.deb", "18a8bdfd91c7e3bcb01719d55a2b56849c7160b34f1f52b1c4fdfdd41bf1352b"),
    ("pool/main/libg/libgpg-error/libgpg-error0_1.51-4_amd64.deb", "22b95570fd41c113ef6f5651563b4d748292844baab1278a46eb940c1dec2322"),
    ("pool/main/g/graphite2/libgraphite2-3_1.3.14-2+deb13u1_amd64.deb", "1769ba3ed3c1fc75b7fbe02678e32aa24391205813eceef4e1e3500015d12ee3"),
    ("pool/main/k/krb5/libgssapi-krb5-2_1.21.3-5+deb13u1_amd64.deb", "30847c1fde4240567d7ed3aeab4f655dd591203758b857e85e824045aae70299"),
    ("pool/main/g/gst-plugins-base1.0/libgstreamer-gl1.0-0_1.26.2-1+deb13u1_amd64.deb", "efacc61acbd44a6a067612e4592b9fe81efdc5011ed4c6cf7afd8ed83c5f834a"),
    ("pool/main/g/gst-plugins-base1.0/libgstreamer-plugins-base1.0-0_1.26.2-1+deb13u1_amd64.deb", "a20aedb3f89c162b4859cec37076f4eba54b0bcc7e5cb254a26cc4a877a88a1e"),
    ("pool/main/g/gstreamer1.0/libgstreamer1.0-0_1.26.2-2_amd64.deb", "08b463307fbde5496a346b1bb300b465cd22bd326f6f4634f482bf441af28a9b"),
    ("pool/main/g/gtk+3.0/libgtk-3-0t64_3.24.49-3_amd64.deb", "4aa8828f01365f937f3bf69f812c8107ee4c8a2b208ffdd7656e8de5de81d98f"),
    ("pool/main/libg/libgudev/libgudev-1.0-0_238-6_amd64.deb", "890a55ea26f52062f377389e6326abeadb72a14741c95ab6a1c5a19c77c3c3d0"),
    ("pool/main/h/harfbuzz/libharfbuzz-icu0_10.2.0-1+deb13u1_amd64.deb", "0b45d951629e98673c0da73b2bcd0886a467d833bb3b7499892a168f247165d2"),
    ("pool/main/h/harfbuzz/libharfbuzz0b_10.2.0-1+deb13u1_amd64.deb", "932165b933e5eb633293afe984a983ebe89cb27c923ce7208626f1da6c0c7941"),
    ("pool/main/h/hidapi/libhidapi-hidraw0_0.14.0-1+b2_amd64.deb", "89c546cb55be403ce63fe668baff2515979100a30fa07e2b97de186b9c07f01e"),
    ("pool/main/n/nettle/libhogweed6t64_3.10.1-1_amd64.deb", "b059ed155115ac09da295322de48ee1ed58ef5fa45edcc9e12ff0e94636ef25f"),
    ("pool/main/h/highway/libhwy1t64_1.2.0-2+b2_amd64.deb", "540c4ff28255fbfa89468061b722dfc2b773b0c1ecf163a063202482dea5b119"),
    ("pool/main/h/hyphen/libhyphen0_2.8.8-7+b2_amd64.deb", "fa9dca7b8cf1381fd52800c44a385c5725a78a88bd705f7c39b1046a5293435e"),
    ("pool/main/libi/libice/libice6_1.1.1-1_amd64.deb", "38185997162793b5dfb768644badc57852879784a2f9d83c6c4e47efa5e36698"),
    ("pool/main/i/icu/libicu76_76.1-4_amd64.deb", "c1bf762996de9ecba9b9d871e4928a8090f023a3e9e7fa3240b3d90f892a01dc"),
    ("pool/main/libi/libidn2/libidn2-0_2.3.8-2_amd64.deb", "90b039bcdc4578f8e1c4935adf8dbb525e36a164deefdbbb8c45bac347d48278"),
    ("pool/main/w/webkit2gtk/libjavascriptcoregtk-4.1-0_2.52.6-1~deb13u1_amd64.deb", "3383ebb3e13f31225c65f41112420be58493a3de112b5993cf5c417cfedc47c8"),
    ("pool/main/libj/libjpeg-turbo/libjpeg62-turbo_2.1.5-4_amd64.deb", "5504299ad0e3c0a9ca9f962a68a650a040db52f9f416a109e74df48d55601d3f"),
    ("pool/main/j/jpeg-xl/libjxl0.11_0.11.2-0.1~deb13u2_amd64.deb", "cd1ca3a52cb0d8fdce1d994bcbf0663de28c64add1cfe8096639ae8fa9808824"),
    ("pool/main/k/krb5/libk5crypto3_1.21.3-5+deb13u1_amd64.deb", "7da07ee674b47f1f0be7cc89317c25310086a1f1761217d0f72e6ae2c5a69b84"),
    ("pool/main/k/keyutils/libkeyutils1_1.6.3-6_amd64.deb", "0b11ad17be0300b63ad4eeb4c6450fed24d34b7b740f23e5363dcb29ee6d5eba"),
    ("pool/main/k/krb5/libkrb5-3_1.21.3-5+deb13u1_amd64.deb", "47d71d6a7f2e59b9bae5f89602397594805113b95889ad18fa703cd53abafc97"),
    ("pool/main/k/krb5/libkrb5support0_1.21.3-5+deb13u1_amd64.deb", "3a0acd8b37955c0e102c756b52c97df2a31f67b96453c35dab70df218d309117"),
    ("pool/main/l/lcms2/liblcms2-2_2.16-2+deb13u2_amd64.deb", "32e53c2002ccea8c60c02779bea0ba49ac132fef702988a30d64af79b6617806"),
    ("pool/main/x/xz-utils/liblzma5_5.8.1-1+deb13u1_amd64.deb", "1cfcc6e0dc36f438a79b6e2189facdb9d150b08f57d190a60e01c98075c7f896"),
    ("pool/main/libm/libmanette/libmanette-0.2-0_0.2.12-1_amd64.deb", "98841a467692c46fd1870275113235691ebbfe71a3524f6b2a0fa7e9c33c6e56"),
    ("pool/main/u/util-linux/libmount1_2.41.5-0+deb13u1_amd64.deb", "6d00f45f2e80e078e906e3eedecd3ba6913e39fef49bff361ee583f17f00ec05"),
    ("pool/main/n/nettle/libnettle8t64_3.10.1-1_amd64.deb", "1b03d4a9cd9c8143ba50fe6396f36937e901a317d492d179f4596862c1731cfe"),
    ("pool/main/n/nghttp2/libnghttp2-14_1.64.0-1.1+deb13u1_amd64.deb", "896cb217537c09251fb909b8541349010ac279d802d871e98c06a62a2c67ce2c"),
    ("pool/main/o/orc/liborc-0.4-0t64_0.4.41-1_amd64.deb", "5b9afe78be0dd71399c0d4d7e5f26beb6ddf207279997942d8280be1b20bd58c"),
    ("pool/main/p/p11-kit/libp11-kit0_0.25.5-3_amd64.deb", "784bf2063e166c8bc851a32623b74ebd85c499043a9d57c5bbd64fa63447f45a"),
    ("pool/main/p/pango1.0/libpango-1.0-0_1.56.3-1_amd64.deb", "4fa38472696d2ddb10bfc9a4692a9446e8478fdb1f48c769a609fc9fec3adfc5"),
    ("pool/main/p/pango1.0/libpangocairo-1.0-0_1.56.3-1_amd64.deb", "6994729c5b7a9649661ec078cf039991f22eff44600543aa917894f83c89cd05"),
    ("pool/main/p/pango1.0/libpangoft2-1.0-0_1.56.3-1_amd64.deb", "b506cf66f78ddf542ddb4bb4a68b3d748d41caa6e190e4480430a6630c1e5105"),
    ("pool/main/p/pcre2/libpcre2-8-0_10.46-1~deb13u2_amd64.deb", "1252b96a5bc44bb5db982bef8eb18e54f5047cede2aff641bce4f8e1edb91c3e"),
    ("pool/main/p/pixman/libpixman-1-0_0.44.0-3_amd64.deb", "3862c87596f6b4b60fc916395fd69aa10af28a875a8e5c081a8cf30ddd120bd6"),
    ("pool/main/libp/libpng1.6/libpng16-16t64_1.6.48-1+deb13u5_amd64.deb", "2465b4e9fa85cff54dc10a6da3e64074d8d9292c86e4a8f989b809d6b158e97e"),
    ("pool/main/libp/libpsl/libpsl5t64_0.21.2-1.1+b1_amd64.deb", "59d42bb1f9ebc0d1776fe616efb08a7a8568b05982e00f70b03c63863db768ab"),
    ("pool/main/r/rust-rav1e/librav1e0.7_0.7.1-9+b2_amd64.deb", "d5b1373a8d6905ee4523faf1db7a626dc660e03836ec91b616e57ba386f47eb6"),
    ("pool/main/libs/libseccomp/libseccomp2_2.6.0-2_amd64.deb", "89d5138e05fb6a86b9afd8b54dc0fb4b14f4c27c05e6c5f767c2132bdf808531"),
    ("pool/main/libs/libsecret/libsecret-1-0_0.21.7-1_amd64.deb", "949c5428cd3427486ac6c4dfc6d9400ce786c504c02334acc903c873aa52d14f"),
    ("pool/main/libs/libselinux/libselinux1_3.8.1-1_amd64.deb", "68bb8d32bd8d6d7d2f5952a169db03d1484b46ae1e52abccdec42a19dccea5d5"),
    ("pool/main/libw/libwebp/libsharpyuv0_1.5.0-0.1_amd64.deb", "51934833d909588f7761fd84362114e90d8d2d38af57edf5df5c4a705e037547"),
    ("pool/main/libs/libsm/libsm6_1.2.6-1_amd64.deb", "03038b3002e6f44322554e335d60d8ff87acfcf2e21c904b6866610a9913ee00"),
    ("pool/main/libs/libsoup3/libsoup-3.0-0_3.6.5-3_amd64.deb", "7031c74a7a355db57d7eb9d48f1e81c7ccbb41da35db7db501e08b3fe3c2c88a"),
    ("pool/main/s/sqlite3/libsqlite3-0_3.46.1-7+deb13u2_amd64.deb", "0a459adaffd901109f7811ab65f58e7a957b4907d05539cf3d1184efdcde0468"),
    ("pool/main/o/openssl/libssl3t64_3.5.7-1~deb13u2_amd64.deb", "916f7f40b34a06e6ebfaefcdab331bff458328411da672598f126a760472467d"),
    ("pool/main/g/gcc-14/libstdc++6_14.2.0-19_amd64.deb", "ab1fa05837aa7a92aae748fd07a18a35f7d18bb4a71c4724fe2bbf0e32089de0"),
    ("pool/main/s/svt-av1/libsvtav1enc2_2.3.0+dfsg-1_amd64.deb", "cdaf961c3cc4fe78a19ff18c17d83d813f78a378fdcc4c86ccbb9b54e84ae599"),
    ("pool/main/s/systemd/libsystemd0_257.13-1~deb13u1_amd64.deb", "ab0d4127b5e46e6f8c015a1db15a62ba9ae274cdefa150083adf90ada0600ea1"),
    ("pool/main/libt/libtasn1-6/libtasn1-6_4.20.0-2+deb13u1_amd64.deb", "23fec6e06583ce2bad9b2c04c9b485e90440e259b1abf8677cd80d3ce60831ad"),
    ("pool/main/libt/libthai/libthai0_0.1.29-2+b1_amd64.deb", "e6364c07aeeffe9a95f267061d9df168e48e9dc9126c3b40e19dc829f67702a7"),
    ("pool/main/n/ncurses/libtinfo6_6.5+20250216-2_amd64.deb", "8b9f6a7983e9418564e48a627518de4c03917b56efe68d7f3e93bd8fffa1cc10"),
    ("pool/main/libt/libtirpc/libtirpc3t64_1.3.6+ds-1_amd64.deb", "ef4536c09bf5063554310e22d586ae90990100ba66d2fe7c35660e9e658913b6"),
    ("pool/main/s/systemd/libudev1_257.13-1~deb13u1_amd64.deb", "5d41c284f5a93b05bc7d648b61a02dd2bb9ff05b2261ad1a8b7d96044a0cfa88"),
    ("pool/main/libu/libunistring/libunistring5_1.3-2_amd64.deb", "6dd3490bef06ea1096f32d10766b7e016cf579bfae8451d1b7df15ce05b8aa46"),
    ("pool/main/u/util-linux/libuuid1_2.41.5-0+deb13u1_amd64.deb", "c1bf4c4c3ff48c57fabf93307dfb56996b60cfa33927afc4158b5db36fb2721e"),
    ("pool/main/w/wayland/libwayland-client0_1.23.1-3_amd64.deb", "d1607e1db1a7c5378a2e5c6ae7b20f64dc6acf134f953d84a7f5204b74339f33"),
    ("pool/main/w/wayland/libwayland-cursor0_1.23.1-3_amd64.deb", "26fccae2b1228744de0cad6c688550b8d13cd4e8d4ccf0cfdae015634ece673f"),
    ("pool/main/w/wayland/libwayland-egl1_1.23.1-3_amd64.deb", "7c5c3429028b39b9432792db08cb218c43891df439ebc648b83ebee676a80ec5"),
    ("pool/main/w/wayland/libwayland-server0_1.23.1-3_amd64.deb", "2967212bd582e0dffca443fdc44f4c660e7368d41f7ee3a7f6314e0c3abfe9ea"),
    ("pool/main/w/webkit2gtk/libwebkit2gtk-4.1-0_2.52.6-1~deb13u1_amd64.deb", "6471d989a61a56a7801e88e420789aa6b0eaba630a9787365bba8b9576b7ba1c"),
    ("pool/main/libw/libwebp/libwebp7_1.5.0-0.1_amd64.deb", "c749744b04abdd1426b0c7718e516362c03a557c5151662092d6d4f2cef6e7c1"),
    ("pool/main/libw/libwebp/libwebpdemux2_1.5.0-0.1_amd64.deb", "b6e4aba184b2c3f7ce88b3e49557fe0c38c909df54528ebe5e167ad651f0f593"),
    ("pool/main/libw/libwebp/libwebpmux3_1.5.0-0.1_amd64.deb", "8bc86d509406ad496b6414c7068b3dadc0f8c502027973cbf1765c7bb54fabdf"),
    ("pool/main/libx/libx11/libx11-6_1.8.12-1_amd64.deb", "b5a3fd3bf8c8fd0364bfb9bea00dcba7fc301229bd02dded084632d31f5b0fb3"),
    ("pool/main/libx/libx11/libx11-xcb1_1.8.12-1_amd64.deb", "e05f94d21a932fba5b09b9b13d99df776b155d3bc792c0e294451df9ffe1ba25"),
    ("pool/main/libx/libxau/libxau6_1.0.11-1_amd64.deb", "689a9f0e0ba3e2c65431f864871e303ee904de69dd28abfc462663fae030227f"),
    ("pool/main/libx/libxcb/libxcb-render0_1.17.0-2+b1_amd64.deb", "7f6e36978e0f70a590fbad4042a692c70b50ca7cbcdc53fa7e9d9ab87933acc6"),
    ("pool/main/libx/libxcb/libxcb-shm0_1.17.0-2+b1_amd64.deb", "d1777a813f484e89f3dcaadd1d5f2b1a13e9313b6bf70eded16d8ce743208fef"),
    ("pool/main/libx/libxcb/libxcb1_1.17.0-2+b1_amd64.deb", "5c222a72d11b866447da31693254f738430726e3e065a384e82687b2fd2f978b"),
    ("pool/main/libx/libxcomposite/libxcomposite1_0.4.6-1_amd64.deb", "20e3c1d9b2135f0c8c4246a9fd26a51a57d5850c3b36ecc173c45d6be7328af3"),
    ("pool/main/libx/libxcursor/libxcursor1_1.2.3-1_amd64.deb", "bb0dbf58b804faaa74ed9efdae61c4b14bb0b4855680798f63e5ca6a4954976b"),
    ("pool/main/libx/libxcvt/libxcvt0_0.1.3-1_amd64.deb", "0faccb95f5d306fc30c8b128572b92ff97bf59c547f1f8a46b57e0b4c6dd045b"),
    ("pool/main/libx/libxdamage/libxdamage1_1.1.6-1+b2_amd64.deb", "e51e43f23f3befbc1f9408271f4df6773d37caece9ce7e1f38abade382f7fbf7"),
    ("pool/main/libx/libxdmcp/libxdmcp6_1.1.5-1_amd64.deb", "0740dc760916b2008b45417a42a8fd7dd5de370fb57d31373f15034cda8acf0b"),
    ("pool/main/libx/libxext/libxext6_1.3.4-1+b3_amd64.deb", "fc618ec40465e5ce48622606299cb47833efc3fb235ba15543b81f850722f443"),
    ("pool/main/libx/libxfixes/libxfixes3_6.0.0-2+b4_amd64.deb", "3fdd95d86d8e9d63e11f52070935c8f9f912c36aa3b17338d969f7685f3ed4fb"),
    ("pool/main/libx/libxfont/libxfont2_2.0.6-1+deb13u1_amd64.deb", "79a7f56e4a0c7b1defa706c7b1f3076f7d80aaeea9de253b2a87406253319681"),
    ("pool/main/libx/libxi/libxi6_1.8.2-1_amd64.deb", "093d0903f35bb7a9f6815180ee040e6951fecf9b66c128cd72f064710210606e"),
    ("pool/main/libx/libxinerama/libxinerama1_1.1.4-3+b4_amd64.deb", "49d4e1628960407b8be2880a42428e7629801715a8a5697d42c0b0ca0a479911"),
    ("pool/main/libx/libxkbcommon/libxkbcommon0_1.7.0-2_amd64.deb", "f75ee544f55acc6a271debfab3ea4ae0458afc89d81cfe1a71137e07d4895b86"),
    ("pool/main/libx/libxkbfile/libxkbfile1_1.1.0-1+b4_amd64.deb", "989b61e0eb7f1f99d0e969a5957e9a857a95941d5c0e86d5e2b502008b4ac28e"),
    ("pool/main/libx/libxml2/libxml2_2.12.7+dfsg+really2.9.14-2.1+deb13u3_amd64.deb", "e0c6b63ce4602a036a526f60fe5e6c1586710688058d98fc1001b9b3147b7efd"),
    ("pool/main/libx/libxrandr/libxrandr2_1.5.4-1+b3_amd64.deb", "11e3490de93a8bbee3daba719cb8e1325a26fb3c125525c34bdcb7deb05eb9b2"),
    ("pool/main/libx/libxrender/libxrender1_0.9.12-1_amd64.deb", "9d042dfd5e613be1e02e6ddd0c5c4adef19c5eb08f6db838c2eba672c496dca4"),
    ("pool/main/libx/libxshmfence/libxshmfence1_1.3.3-1_amd64.deb", "7b339e9e5b2349723d35af4df89bcc7aa456bbdf8ba1754358f9b44c3fe1f964"),
    ("pool/main/libx/libxslt/libxslt1.1_1.1.35-1.2+deb13u3_amd64.deb", "c8e5a745ca3afaa079b01e0c00d80a241101d1ab840407fcd4bb423de3326160"),
    ("pool/main/liby/libyuv/libyuv0_0.0.1904.20250204-1_amd64.deb", "1a6827fe94fa5572a032332df7313ed408e9c0ad78f3a1248391bddc6d920ef2"),
    ("pool/main/libz/libzstd/libzstd1_1.5.7+dfsg-1_amd64.deb", "2f6a2aeacfc925eba8b00ac9139bc4bfccf8cacb09eb93de067074b26948eef9"),
    ("pool/main/x/x11-xkb-utils/x11-xkb-utils_7.7+9_amd64.deb", "745e29c79bb435d057cdbf8bb59a35fa33e818e566cb754674f44d381ccd4317"),
    ("pool/main/x/xkeyboard-config/xkb-data_2.42-1_all.deb", "196ff18533382f64e057ea49df2bb486bd4275a4cc0917361edb560b8756dada"),
    ("pool/main/x/xwayland/xwayland_24.1.6-1_amd64.deb", "7e51a144e858d5be7e91d6644c2758a618e425c7222234d2e8f059e5aba6c1f5"),
    ("pool/main/z/zlib/zlib1g_1.3.dfsg+really1.3.1-1+b1_amd64.deb", "015be740d6236ad114582dea500c1d907f29e16d6db00566ca32fb68d71ac90d"),
];

/// Headers for `rupor-wm` (Xlib): not in the package.
const DEV_PACKAGES: [(&str, &str); 2] = [
    ("pool/main/libx/libx11/libx11-dev_1.8.12-1_amd64.deb", "530de60c9c6dadd997ae7d74eca2a41b25e849373a5ad5dcdb619aa05713d6e0"),
    ("pool/main/x/xorgproto/x11proto-dev_2024.1-1_all.deb", "6a6764500eb8171d3cd6d793c09902974f8ea87bd20c68d9eac5f1eeaf3fc407"),
];

/// Library directory of the trixie packages (usrmerge).
const DIR: &str = "usr/lib/x86_64-linux-gnu/";

/// Where `ld.so` looks in the Debian 12 root (what is there is not packaged).
const LIB_DIRS: [&str; 4] = ["lib/x86_64-linux-gnu", "usr/lib/x86_64-linux-gnu", "lib", "usr/lib"];

/// glibc of trixie (`usr/lib/x86_64-linux-gnu/…`) - into `glibc/`.
const GLIBC_LIBS: [&str; 12] = [
    "ld-linux-x86-64.so.2",
    "libc.so.6",
    "libm.so.6",
    "libmvec.so.1",
    "libpthread.so.0",
    "libdl.so.2",
    "librt.so.1",
    "libresolv.so.2",
    "libutil.so.1",
    "libanl.so.1",
    "libnss_files.so.2",
    "libnss_dns.so.2",
];

/// Libraries the client opens itself (P/Invoke, `dlopen`) - no `DT_NEEDED`
/// leads to them: Avalonia.X11 (libX11 and extensions, session management),
/// GtkSharp/WebkitGtkSharp, .NET (ICU for cultures, OpenSSL for TLS, zlib),
/// SkiaSharp (fontconfig), Avalonia's GLX probe (libGL; without a driver it
/// falls back to software).
const OPENED: [&str; 24] = [
    "libX11.so.6",
    "libX11-xcb.so.1",
    "libXrandr.so.2",
    "libXi.so.6",
    "libXcursor.so.1",
    "libXext.so.6",
    "libXrender.so.1",
    "libXfixes.so.3",
    "libSM.so.6",
    "libICE.so.6",
    "libgtk-3.so.0",
    "libgdk-3.so.0",
    "libgdk_pixbuf-2.0.so.0",
    "libpangocairo-1.0.so.0",
    "libwebkit2gtk-4.1.so.0",
    "libjavascriptcoregtk-4.1.so.0",
    "libicuuc.so.76",
    "libicui18n.so.76",
    "libssl.so.3",
    "libcrypto.so.3",
    "libz.so.1",
    "libfontconfig.so.1",
    "libGL.so.1",
    "libEGL.so.1",
];

/// Files of `dotnet publish` that stay out: the LTTng trace provider (needs
/// liblttng-ust, .NET runs without it), the debugger parts and `createdump`.
const APP_SKIPPED: [&str; 4] = ["libcoreclrtraceptprovider.so", "libmscordbi.so", "libmscordaccore.so", "createdump"];

/// The loader of Debian in `PT_INTERP` and ours in its place (not longer).
const INTERP_OLD: &[u8] = b"/lib64/ld-linux-x86-64.so.2";
const INTERP_NEW: &[u8] = b"/opt/rupor/glibc/ld.so";

/// WebKit's directory of its processes and the injected bundle, and ours.
const WEBKIT_OLD: &[u8] = b"/usr/lib/x86_64-linux-gnu/webkit2gtk-4.1";
const WEBKIT_NEW: &[u8] = b"/opt/rupor/lib/webkit2gtk-4.1";

/// Xwayland: `/bin/sh` of `Popen` (keymaps through xkbcomp), the xkbcomp
/// directory, the title of the rootful window.
const XWAYLAND_PATCHES: [(&[u8], &[u8]); 3] =
    [(b"/bin/sh", b"/tmp/xs"), (b"/usr/bin", b"/tmp/xb"), (b"Xwayland on :%s%s", "Рупор".as_bytes())];

/// DejaVu fonts (Cyrillic) and `fonts.conf`.
const FONT_DIR: &str = "usr/share/fonts/truetype/dejavu/";
const FONTS_CONF: &str = "<?xml version=\"1.0\"?>\n\
    <!DOCTYPE fontconfig SYSTEM \"fonts.dtd\">\n\
    <fontconfig>\n\
    \t<dir>/opt/rupor/fonts</dir>\n\
    \t<cachedir>/tmp/fontconfig</cachedir>\n\
    \t<alias><family>sans-serif</family><prefer><family>DejaVu Sans</family></prefer></alias>\n\
    \t<alias><family>serif</family><prefer><family>DejaVu Sans</family></prefer></alias>\n\
    \t<alias><family>monospace</family><prefer><family>DejaVu Sans Mono</family></prefer></alias>\n\
    \t<alias><family>Roboto</family><prefer><family>DejaVu Sans</family></prefer></alias>\n\
    </fontconfig>\n";

/// The launcher (`/opt/rupor/bin/rupor`, bash of the Debian 12 root).
const SCRIPT: &str = r#"#!/bin/bash
# Rupor (24.11.2026): the Linux client of Rupor (.NET 9, Avalonia on X11,
# WebKitGTK 4.1 for the Blazor UI) in a rootful Xwayland window of the kernel
# Wayland server; rupor-wm keeps the client the size of that window.
P=/opt/rupor
export HOME=${HOME:-/root}
export XDG_CONFIG_HOME=${XDG_CONFIG_HOME:-$HOME/.config}
export XDG_DATA_HOME=${XDG_DATA_HOME:-$HOME/.local/share}
export XDG_CACHE_HOME=${XDG_CACHE_HOME:-$HOME/.cache}
export XDG_RUNTIME_DIR=/run/user/0
L=$XDG_CACHE_HOME/rupor
LIBS=$P/glibc:$P/lib
# bash and coreutils of the Debian 12 root run with an empty LD_LIBRARY_PATH;
# the programs of the package (glibc 2.41) get LIBS.
LD_LIBRARY_PATH= mkdir -p "$L" /tmp/.X11-unix /tmp/xb
say() { echo "rupor: $*"; echo "$(LD_LIBRARY_PATH= date '+%F %T') $*" >> "$L/launch.log"; }
# Xwayland runs `/bin/sh -c "/usr/bin/xkbcomp ..."` for every keymap; both
# strings are /tmp/xs and /tmp/xb in our Xwayland (see xtask/src/rupor.rs).
[ /tmp/xs -nt $P/libexec/bash ] || LD_LIBRARY_PATH= cp -f $P/libexec/bash /tmp/xs
[ /tmp/xb/xkbcomp -nt $P/libexec/xkbcomp ] || LD_LIBRARY_PATH= cp -f $P/libexec/xkbcomp /tmp/xb/xkbcomp
LD_LIBRARY_PATH= chmod 755 /tmp/xs /tmp/xb/xkbcomp 2>/dev/null
LD_LIBRARY_PATH= chmod 1777 /tmp/.X11-unix 2>/dev/null
# Window: 1280x800 (the size of the Rupor main window), smaller on a smaller
# primary monitor (the first line of /dev/fossai-capture).
W=1280 H=800
if [ -r /dev/fossai-capture ]; then
    while IFS=$'\t' read -r x y w h rest; do
        case "$w$h" in ''|*[!0-9]*) continue ;; esac
        [ $((w - 80)) -lt $W ] && W=$((w - 80))
        [ $((h - 140)) -lt $H ] && H=$((h - 140))
        break
    done < /dev/fossai-capture
fi
[ "$W" -lt 640 ] && W=640
[ "$H" -lt 480 ] && H=480
LD_LIBRARY_PATH= rm -f "$L/display"
say "Xwayland ${W}x${H}"
WAYLAND_DISPLAY=wayland-0 LD_LIBRARY_PATH=$LIBS $P/libexec/Xwayland -displayfd 5 -geometry ${W}x${H} -shm -dumbSched -noreset \
    -nolisten tcp -fp built-ins -xkbdir $P/share/X11/xkb 5>"$L/display" &
XPID=$!
D=
for i in $(LD_LIBRARY_PATH= seq 1 600); do
    [ -s "$L/display" ] && { D=$(<"$L/display"); break; }
    LD_LIBRARY_PATH= kill -0 $XPID 2>/dev/null || break
    LD_LIBRARY_PATH= sleep 0.1
done
if [ -z "$D" ]; then
    say "Xwayland did not start"
    LD_LIBRARY_PATH= kill $XPID 2>/dev/null
    exit 1
fi
export DISPLAY=:${D//[^0-9]/}
say "Xwayland ready on $DISPLAY"
LD_LIBRARY_PATH=$LIBS $P/libexec/rupor-wm &
WPID=$!
stop() { LD_LIBRARY_PATH= kill $WPID $XPID 2>/dev/null; }
trap stop EXIT
unset WAYLAND_DISPLAY
export GDK_BACKEND=x11
# /run/user/0 is only the name of the kernel Wayland socket, not a directory:
# the client gets a real runtime directory (.NET keeps its named mutexes there,
# GTK and WebKit their sockets).
export XDG_RUNTIME_DIR=$L/run
LD_LIBRARY_PATH= mkdir -p "$XDG_RUNTIME_DIR"
LD_LIBRARY_PATH= chmod 700 "$XDG_RUNTIME_DIR"
# Russian: .NET and WebKit read the language from LANG; glibc gets C.UTF-8 data
# under that name (LOCPATH), so setlocale() succeeds with UTF-8.
export LANG=ru_RU.UTF-8 LANGUAGE=ru
unset LC_ALL
export LOCPATH=$P/locale
export FONTCONFIG_FILE=$P/fonts/fonts.conf
export XKB_CONFIG_ROOT=$P/share/X11/xkb
export GIO_MODULE_DIR=$P/lib/gio/modules
export GSETTINGS_BACKEND=memory
export NO_AT_BRIDGE=1 GTK_A11Y=none
export DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/0/bus
# WebKitGTK: no bubblewrap sandbox (no namespaces here), no GPU compositing
# (Skia on the CPU, shared memory to the UI process).
export WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1
export WEBKIT_DISABLE_COMPOSITING_MODE=1
export WEBKIT_DISABLE_DMABUF_RENDERER=1
export WEBKIT_SKIA_ENABLE_CPU_RENDERING=1
export LIBGL_ALWAYS_SOFTWARE=1 GALLIUM_DRIVER=llvmpipe
export __EGL_VENDOR_LIBRARY_FILENAMES=$P/share/glvnd/egl_vendor.d/50_mesa.json
export LIBGL_DRIVERS_PATH=$P/lib/dri
export MESA_SHADER_CACHE_DIR=$L/mesa
export GST_REGISTRY=$L/gst-registry.bin GST_PLUGIN_SYSTEM_PATH=$P/lib/gstreamer-1.0
# .NET: JIT into RWX memory (permission jit) instead of the memfd double
# mapping, a 4 GiB heap limit (else the GC reserves more address space than a
# program gets), no diagnostics server, no LTTng, no X session manager.
export DOTNET_EnableWriteXorExecute=0
export DOTNET_GCHeapHardLimit=0x100000000
export DOTNET_EnableDiagnostics=0
export DOTNET_LTTng=0
export AVALONIA_X11_USE_SESSION_MANAGEMENT=0
# `--env=NAME=VALUE` first - a variable of one's own (`run` has no environment).
while case "$1" in --env=*) true ;; *) false ;; esac; do export "${1#--env=}"; shift; done
say "start $P/app/Rupor $*"
cd $P/app
LD_LIBRARY_PATH=$LIBS $P/app/Rupor "$@"
rc=$?
say "Rupor exited with $rc"
exit $rc
"#;

/// Download the packages and build `rupor-1.9.50.fpk`.
pub fn build(arch: Arch) -> Result<()> {
    if arch != Arch::X86_64 {
        return Ok(());
    }
    let app = publish()?;
    let cache = sdk::root().join("thirdparty/debian");
    fs::create_dir_all(&cache)?;
    let mut base: BTreeMap<String, Node> = BTreeMap::new();
    unpack(&cache, &crate::debian::PACKAGES, &mut base)?;
    let mut deb: BTreeMap<String, Node> = BTreeMap::new();
    unpack(&cache, &PACKAGES, &mut deb)?;

    let mut files: Vec<(String, Vec<u8>, u32)> = Vec::new();
    for name in GLIBC_LIBS {
        let Some((data, mode)) = follow(&deb, &format!("{DIR}{name}"), 0) else {
            bail!("rupor: no {name} in libc6 of trixie");
        };
        files.push((format!("glibc/{name}"), data.clone(), mode));
        if name == "ld-linux-x86-64.so.2" {
            files.push((String::from("glibc/ld.so"), data.clone(), 0o755));
        }
    }

    // What to walk by `DT_NEEDED`, and who asked for it.
    let mut todo: Vec<(String, String)> = OPENED.iter().map(|name| (String::from(*name), String::from("the client"))).collect();
    // Mesa (EGL on llvmpipe, shared with Proton/OBS): WebKit's web process
    // aborts without an EGL display ("Could not create default EGL display:
    // EGL_BAD_PARAMETER", QEMU 25.11). No Vulkan, no probes.
    crate::mesa::add(&cache, &mut files, patch_interp)?;
    files.retain(|(path, _, _)| !(path.starts_with("lib/libvulkan_") || path.starts_with("share/vulkan/") || path.starts_with("bin/")));
    for (path, data, _) in files.iter().filter(|(path, _, _)| path.starts_with("lib/") && path.contains(".so")) {
        if let Ok(list) = needed(data) {
            todo.extend(list.into_iter().map(|name| (name, path.clone())));
        }
    }
    let mut add_elf = |files: &mut Vec<(String, Vec<u8>, u32)>, from: &str, to: &str, program: bool| -> Result<()> {
        let Some((data, mode)) = follow(&deb, from, 0) else {
            bail!("rupor: no {from} in the packages");
        };
        let mut data = data.clone();
        if program && !patch_interp(&mut data) {
            bail!("rupor: {from} has no Debian loader");
        }
        if to == "libexec/Xwayland" {
            for (old, new) in XWAYLAND_PATCHES {
                if patch_cstr(&mut data, old, new) != 1 {
                    bail!("rupor: Xwayland has not exactly one string {}", String::from_utf8_lossy(old));
                }
            }
        }
        let by = to.rsplit('/').next().unwrap_or(to).to_string();
        todo.extend(needed(&data).with_context(|| format!("{from}: ELF not parsed"))?.into_iter().map(|name| (name, by.clone())));
        files.push((String::from(to), data, mode | if program { 0o755 } else { 0 }));
        Ok(())
    };
    add_elf(&mut files, "usr/bin/Xwayland", "libexec/Xwayland", true)?;
    add_elf(&mut files, "usr/bin/xkbcomp", "libexec/xkbcomp", true)?;
    add_elf(&mut files, "usr/bin/bash", "libexec/bash", true)?;
    for process in ["WebKitWebProcess", "WebKitNetworkProcess"] {
        add_elf(&mut files, &format!("{DIR}webkit2gtk-4.1/{process}"), &format!("lib/webkit2gtk-4.1/{process}"), true)?;
    }
    add_elf(
        &mut files,
        &format!("{DIR}webkit2gtk-4.1/injected-bundle/libwebkit2gtkinjectedbundle.so"),
        "lib/webkit2gtk-4.1/injected-bundle/libwebkit2gtkinjectedbundle.so",
        false,
    )?;
    // TLS for WebKit's network process (pictures and styles over https).
    add_elf(&mut files, &format!("{DIR}gio/modules/libgiognutls.so"), "lib/gio/modules/libgiognutls.so", false)?;

    // Our programs: the window manager and the soname stub of WebKitGTK 4.0.
    let (mut wm, stub) = build_own(&cache, &deb)?;
    if !patch_interp(&mut wm) {
        bail!("rupor: rupor-wm has no Debian loader");
    }
    todo.extend(needed(&wm)?.into_iter().map(|name| (name, String::from("rupor-wm"))));
    files.push((String::from("libexec/rupor-wm"), wm, 0o755));
    files.push((String::from("lib/libwebkit2gtk-4.0.so.37"), stub, 0o644));

    // The published client: managed assemblies, its native libraries, wwwroot.
    let mut app_files = 0;
    let mut app_bytes = 0usize;
    for (rel, path) in walk(&app)? {
        let name = rel.rsplit('/').next().unwrap_or(&rel);
        if APP_SKIPPED.contains(&name) {
            continue;
        }
        let mut data = fs::read(&path)?;
        let mut mode = 0o644;
        if rel == "Rupor" {
            if !patch_interp(&mut data) {
                bail!("rupor: the apphost {} has no Debian loader", path.display());
            }
            mode = 0o755;
        } else if name.ends_with(".so") && !rel.contains('/') {
            todo.extend(needed(&data).with_context(|| format!("{rel}: ELF not parsed"))?.into_iter().map(|lib| (lib, rel.clone())));
        }
        app_bytes += data.len();
        app_files += 1;
        files.push((format!("app/{rel}"), data, mode));
    }
    say!("rupor: the client - {app_files} files, {} MiB", app_bytes / (1024 * 1024));
    // `index.html` links `Rupor.styles.css`, the Razor SDK names the bundle of
    // the shared project after it (RuporAppAvalonia.styles.css) and the copy
    // rule of the csproj looks for the other name - the scoped styles were
    // missing in every Linux build. Taken from the bundle if it is there.
    if !files.iter().any(|(path, _, _)| path == "app/wwwroot/Rupor.styles.css") {
        if let Some(bundle) = scoped_css() {
            files.push((String::from("app/wwwroot/Rupor.styles.css"), bundle, 0o644));
            say!("rupor: wwwroot/Rupor.styles.css from the scoped CSS bundle");
        }
    }

    let have: BTreeSet<String> = files.iter().filter_map(|(path, _, _)| path.strip_prefix("lib/").map(String::from)).collect();
    let app_libs: BTreeSet<String> =
        files.iter().filter_map(|(path, _, _)| path.strip_prefix("app/")).filter(|rel| !rel.contains('/')).map(String::from).collect();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut missing: Vec<String> = Vec::new();
    let (mut from_deb, mut from_base) = (0, 0);
    while let Some((name, by)) = todo.pop() {
        if !seen.insert(name.clone()) || have.contains(&name) || app_libs.contains(&name) || GLIBC_LIBS.contains(&name.as_str()) {
            continue;
        }
        let suffix = format!("/{name}");
        let found = LIB_DIRS.iter().find_map(|dir| follow(&deb, &format!("{dir}/{name}"), 0)).or_else(|| {
            deb.keys().find(|path| path.ends_with(&suffix) && path.starts_with(DIR)).and_then(|path| follow(&deb, path, 0))
        });
        if let Some((data, mode)) = found {
            let mut data = data.clone();
            todo.extend(needed(&data).with_context(|| format!("{name}: ELF not parsed"))?.into_iter().map(|next| (next, name.clone())));
            if name == "libwebkit2gtk-4.1.so.0" {
                let count = patch_cstr(&mut data, WEBKIT_OLD, WEBKIT_NEW);
                if count == 0 {
                    bail!("rupor: no {} in {name}", String::from_utf8_lossy(WEBKIT_OLD));
                }
                say!("rupor: {name}: {count} paths of WebKit's processes -> /opt/rupor/lib");
            }
            files.push((format!("lib/{name}"), data, mode));
            from_deb += 1;
            continue;
        }
        if LIB_DIRS.iter().any(|dir| follow(&base, &format!("{dir}/{name}"), 0).is_some()) {
            from_base += 1;
            continue;
        }
        missing.push(format!("{name} (needed by {by})"));
    }
    say!("rupor: libraries from trixie {from_deb}, from the Debian 12 root {from_base}");
    if !missing.is_empty() {
        bail!("rupor: libraries not found: {}", missing.join(", "));
    }

    // Data: keymaps, fonts, the C.UTF-8 locale (also as ru_RU.UTF-8).
    for path in deb.keys() {
        let target = if let Some(rest) = path.strip_prefix("usr/share/X11/xkb/") {
            vec![format!("share/X11/xkb/{rest}")]
        } else if let Some(name) = path.strip_prefix(FONT_DIR) {
            vec![format!("fonts/{name}")]
        } else if let Some(rest) = path.strip_prefix("usr/lib/locale/C.utf8/") {
            vec![format!("locale/C.utf8/{rest}"), format!("locale/ru_RU.utf8/{rest}")]
        } else {
            continue;
        };
        if let Some((data, mode)) = follow(&deb, path, 0) {
            for to in target {
                files.push((to, data.clone(), mode));
            }
        }
    }
    files.push((String::from("fonts/fonts.conf"), FONTS_CONF.as_bytes().to_vec(), 0o644));
    files.push((String::from("bin/rupor"), SCRIPT.as_bytes().to_vec(), 0o755));

    let icon_field = match app_icon(&app).map(|png| crate::fimg::from_png(&png)) {
        Some(Ok(fimg)) => {
            files.push((String::from("icon.fimg"), fimg, 0o644));
            "/opt/rupor/icon.fimg"
        }
        _ => {
            say!("rupor: no icon, the set's icon instead");
            "chat"
        }
    };

    let mut builder = Builder::new(Kind::Package, "rupor", VERSION);
    builder.field(
        "summary",
        "Rupor 1.9.50 - Linux client of the Rupor messenger (.NET 9, Avalonia on X11, WebKitGTK 4.1) in a rootful Xwayland window",
    );
    builder.field("about", "Рупор: мессенджер (клиент для Linux)");
    builder.field("caption", "Рупор");
    builder.field("start", "/opt/debian/bin/bash /opt/rupor/bin/rupor");
    builder.field("desktop", "yes");
    builder.field("icon", icon_field);
    builder.field("permissions", "windows files net jit");
    builder.requires("debian");
    let mut bytes = 0usize;
    for (path, data, mode) in &files {
        bytes += data.len();
        builder.file(&Entry { path: path.clone(), mode: (*mode & 0o7777) as u16, uid: 0, gid: 0, data: data.clone() });
    }
    let out = package_path();
    fs::create_dir_all(out.parent().unwrap())?;
    fs::write(&out, builder.finish())?;
    say!("rupor: {} - {} files, {} MiB", out.display(), files.len(), bytes / (1024 * 1024));
    Ok(())
}

/// Rewrite the loader in `PT_INTERP` (the first `/lib64/ld-linux-x86-64.so.2`
/// of the file, NUL-terminated) to our glibc. `false` - not there.
fn patch_interp(data: &mut [u8]) -> bool {
    let Some(at) = data.windows(INTERP_OLD.len() + 1).position(|window| window[..INTERP_OLD.len()] == *INTERP_OLD && window[INTERP_OLD.len()] == 0)
    else {
        return false;
    };
    data[at..at + INTERP_OLD.len()].fill(0);
    data[at..at + INTERP_NEW.len()].copy_from_slice(INTERP_NEW);
    true
}

/// Replace every C string that **starts** with `old` (the byte before it is
/// NUL) by the same string with `new` instead of that start; the string keeps
/// its place and length (the tail is NUL-padded). `new` must not be longer.
/// Answer - how many strings were changed.
fn patch_cstr(data: &mut [u8], old: &[u8], new: &[u8]) -> usize {
    assert!(new.len() <= old.len());
    let mut count = 0;
    let mut at = 1;
    while at + old.len() <= data.len() {
        if data[at - 1] != 0 || &data[at..at + old.len()] != old {
            at += 1;
            continue;
        }
        let end = data[at..].iter().position(|byte| *byte == 0).map_or(data.len(), |len| at + len);
        let mut string = new.to_vec();
        string.extend_from_slice(&data[at + old.len()..end]);
        string.resize(end - at, 0);
        data[at..end].copy_from_slice(&string);
        count += 1;
        at = end;
    }
    count
}

/// Build `rupor-wm` and the stub `libwebkit2gtk-4.0.so.37` with clang against
/// the glibc sysroot of Debian 12 and the Xlib headers of trixie.
fn build_own(cache: &Path, deb: &BTreeMap<String, Node>) -> Result<(Vec<u8>, Vec<u8>)> {
    let mut dev: BTreeMap<String, Node> = BTreeMap::new();
    unpack(cache, &DEV_PACKAGES, &mut dev)?;
    let work = sdk::root().join("linux/rupor");
    let _ = fs::remove_dir_all(&work);
    let include = work.join("include");
    for (path, node) in &dev {
        if let (Some(rest), Node::File { data, .. }) = (path.strip_prefix("usr/include/"), node) {
            let target = include.join(rest);
            fs::create_dir_all(target.parent().unwrap())?;
            fs::write(&target, data)?;
        }
    }
    for name in ["libX11.so.6", "libwebkit2gtk-4.1.so.0"] {
        let Some((data, _)) = follow(deb, &format!("{DIR}{name}"), 0) else {
            bail!("rupor: no {name} in the packages");
        };
        fs::write(work.join(name), data)?;
    }
    let sysroot = crate::debian::glibc_sysroot()?;
    let lib = sysroot.join("usr/lib/x86_64-linux-gnu");
    let clang = cbuild::llvm_tool("clang")?;
    let base = |cmd: &mut std::process::Command| {
        cmd.arg("--target=x86_64-linux-gnu")
            .arg(format!("--sysroot={}", sysroot.display()))
            .args(["-O2", "-Wall", "-Werror", "-Wno-unused-parameter", "-fuse-ld=lld", "-nostdlib"])
            .arg(format!("-L{}", lib.display()))
            .arg(format!("-L{}", sysroot.join("lib/x86_64-linux-gnu").display()))
            .arg("-Wl,--allow-shlib-undefined");
    };

    let wm = work.join("rupor-wm");
    let mut cmd = std::process::Command::new(&clang);
    base(&mut cmd);
    cmd.args(["-fPIE", "-pie"])
        .arg("-Wl,--dynamic-linker=/lib64/ld-linux-x86-64.so.2")
        .arg(format!("-I{}", include.display()))
        .arg(lib.join("Scrt1.o"))
        .arg(lib.join("crti.o"))
        .arg(cbuild::libc_dir().join("linux/rupor-wm.c"))
        .arg(work.join("libX11.so.6"))
        .arg("-lc")
        .arg(lib.join("crtn.o"))
        .arg("-o")
        .arg(&wm);
    say!("> clang (glibc) rupor-wm.c");
    if !cmd.status().context("clang did not start")?.success() {
        bail!("rupor-wm.c did not build");
    }

    let source = work.join("webkit-4.0-stub.c");
    fs::write(&source, "/* libwebkit2gtk-4.0.so.37 -> libwebkit2gtk-4.1.so.0 (xtask/src/rupor.rs). */\n")?;
    let stub = work.join("libwebkit2gtk-4.0.so.37");
    let mut cmd = std::process::Command::new(&clang);
    base(&mut cmd);
    cmd.args(["-shared", "-fPIC", "-Wl,-soname,libwebkit2gtk-4.0.so.37", "-Wl,--no-as-needed"])
        .arg(&source)
        .arg(work.join("libwebkit2gtk-4.1.so.0"))
        .arg("-o")
        .arg(&stub);
    say!("> clang (glibc) libwebkit2gtk-4.0.so.37");
    if !cmd.status().context("clang did not start")?.success() {
        bail!("libwebkit2gtk-4.0.so.37 did not build");
    }
    Ok((fs::read(&wm)?, fs::read(&stub)?))
}

/// Files of a directory, recursively: (path from the directory with `/`, path).
fn walk(dir: &Path) -> Result<Vec<(String, PathBuf)>> {
    let mut out = Vec::new();
    let mut stack = vec![(String::new(), dir.to_path_buf())];
    while let Some((prefix, path)) = stack.pop() {
        for entry in fs::read_dir(&path).with_context(|| format!("{} is not readable", path.display()))? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let rel = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
            if entry.file_type()?.is_dir() {
                stack.push((rel, entry.path()));
            } else {
                out.push((rel, entry.path()));
            }
        }
    }
    out.sort();
    Ok(out)
}

/// The Rupor sources: `FOSSAI_RUPOR_SRC` or Roman's repository.
fn source_dir() -> PathBuf {
    std::env::var_os("FOSSAI_RUPOR_SRC").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Users\asdk\RiderProjects\Rupor"))
}

/// Projects and files of the repository the desktop client builds from.
const SOURCE_DIRS: [&str; 7] = ["Core", "Rupor.UI", "ICSharpCode.SharpZipLibCore", "Rsync", "Shared", "RuporAppAvalonia", "Libs"];
const SOURCE_FILES: [&str; 2] = ["Directory.Build.props", "NuGet.Config"];
const PROJECT: &str = "RuporAppAvalonia/RuporAppAvalonia.Desktop/RuporAppAvalonia.Desktop.csproj";

/// The published client: `FOSSAI_RUPOR_PUBLISH`, or a copy of the sources
/// published into `build/toolchain/rupor/publish`.
fn publish() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("FOSSAI_RUPOR_PUBLISH") {
        let dir = PathBuf::from(dir);
        if !dir.join("Rupor").is_file() {
            bail!("rupor: no Rupor in FOSSAI_RUPOR_PUBLISH={}", dir.display());
        }
        say!("rupor: the client from {}", dir.display());
        return Ok(dir);
    }
    let src = source_dir();
    if !src.join(PROJECT).is_file() {
        bail!("rupor: no {PROJECT} in {} (FOSSAI_RUPOR_SRC or FOSSAI_RUPOR_PUBLISH)", src.display());
    }
    let work = sdk::root().join("rupor");
    let copy = work.join("src");
    for dir in SOURCE_DIRS {
        if src.join(dir).is_dir() {
            copy_tree(&src.join(dir), &copy.join(dir))?;
        }
    }
    for file in SOURCE_FILES {
        if src.join(file).is_file() {
            fs::copy(src.join(file), copy.join(file))?;
        }
    }
    // The repository pins SDK 10.0.102; the copy builds on whatever SDK is
    // installed (net9.0 targets build on 9, 10 and 11).
    fs::write(copy.join("global.json"), "{\"sdk\":{\"version\":\"9.0.100\",\"rollForward\":\"latestMajor\",\"allowPrerelease\":true}}\n")?;
    let out = work.join("publish");
    let _ = fs::remove_dir_all(&out);
    let mut cmd = std::process::Command::new("dotnet");
    cmd.current_dir(&copy)
        .args(["publish", PROJECT, "-c", "Release", "-r", "linux-x64", "--self-contained", "true", "-o"])
        .arg(&out)
        .args([
            "-p:PublishSingleFile=false",
            "-p:PublishTrimmed=false",
            "-p:DebugType=None",
            "-p:DebugSymbols=false",
            "-p:GenerateDocumentationFile=false",
            "--nologo",
            "-v",
            "quiet",
        ]);
    say!("> dotnet publish {PROJECT} (linux-x64, self-contained) in {}", copy.display());
    if !cmd.status().context("dotnet did not start")?.success() {
        bail!("rupor: dotnet publish failed");
    }
    Ok(out)
}

/// Copy a directory tree without `bin`, `obj`, `.git`, `.vs`; files that did
/// not change (same length, not newer) are left alone.
fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        let target = to.join(&name);
        if entry.file_type()?.is_dir() {
            if ["bin", "obj", ".git", ".vs", ".idea"].iter().any(|skip| name == *skip) {
                continue;
            }
            copy_tree(&entry.path(), &target)?;
            continue;
        }
        let meta = entry.metadata()?;
        if let Ok(old) = target.metadata() {
            if old.len() == meta.len() && old.modified().ok() >= meta.modified().ok() {
                continue;
            }
        }
        fs::copy(entry.path(), &target).with_context(|| format!("{} not copied", entry.path().display()))?;
    }
    Ok(())
}

/// The scoped CSS bundle of `RuporAppAvalonia` (Release build of the copy, or
/// of the sources when the client came ready).
fn scoped_css() -> Option<Vec<u8>> {
    let rel = "RuporAppAvalonia/RuporAppAvalonia/obj/Release/net9.0/scopedcss/bundle/RuporAppAvalonia.styles.css";
    [sdk::root().join("rupor/src").join(rel), source_dir().join(rel)].iter().find_map(|path| fs::read(path).ok())
}

/// The icon: the 256x256 PNG of `Rupor.ico` (the app's own), else
/// `Assets/Rupor.png`.
fn app_icon(app: &Path) -> Option<Vec<u8>> {
    let ico = ["RuporAppAvalonia/RuporAppAvalonia.Desktop/Rupor.ico"]
        .iter()
        .flat_map(|rel| [sdk::root().join("rupor/src").join(rel), source_dir().join(rel)])
        .chain([app.join("Rupor.ico")])
        .find_map(|path| fs::read(path).ok());
    if let Some(ico) = ico {
        // ICONDIR: 6 bytes, then 16-byte entries (size at 8, offset at 12).
        let count = u16::from_le_bytes([*ico.get(4)?, *ico.get(5)?]) as usize;
        let mut best: Option<(u32, &[u8])> = None;
        for index in 0..count {
            let entry = ico.get(6 + index * 16..6 + index * 16 + 16)?;
            let side = if entry[0] == 0 { 256 } else { u32::from(entry[0]) };
            let size = u32::from_le_bytes(entry[8..12].try_into().ok()?) as usize;
            let offset = u32::from_le_bytes(entry[12..16].try_into().ok()?) as usize;
            let data = ico.get(offset..offset + size)?;
            if data.starts_with(b"\x89PNG") && best.is_none_or(|(have, _)| side > have) {
                best = Some((side, data));
            }
        }
        if let Some((_, png)) = best {
            return Some(png.to_vec());
        }
    }
    fs::read(source_dir().join("RuporAppAvalonia/RuporAppAvalonia/Assets/Rupor.png")).ok()
}
