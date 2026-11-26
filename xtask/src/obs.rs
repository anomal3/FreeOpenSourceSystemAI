//! OBS Studio для записи и трансляции экрана (23.11.2026): сборка Debian 13
//! (trixie) `obs-studio` 30.2.3 через слой Linux ядра — пакетом `obs-30.fpk`
//! (`/opt/obs` после `pkg install`). Окно — Qt6 через сервер Wayland ядра
//! (`qt6-wayland`, xdg-shell), рисование — OpenGL через EGL Mesa (llvmpipe,
//! как у Proton), звук — ALSA ядра, кодирование — x264 и FFmpeg (libavcodec
//! 61). Захват экрана — свой модуль OBS «Захват экрана FossAi» поверх
//! захвата ядра (`/dev/fossai-capture`), а не PipeWire и xdg-desktop-portal.
//!
//! # Что берётся
//!
//! Как у Chromium: не замыкание `Depends` (300 пакетов: Python, PipeWire,
//! VLC, …), а то, что **загружается**: цепочка `DT_NEEDED` от `obs`,
//! выбранных модулей OBS ([`OBS_PLUGINS`]) и модулей Qt ([`QT_PLUGINS`]) по
//! пакетам [`PACKAGES`] (выписаны из `Packages` trixie снимка 20261020 тем же
//! обходом). Mesa, libstdc++ 14 и ALSA — `mesa::add`, общий с Proton.
//! glibc — своя, 2.41 (`/opt/obs/glibc`), как у Proton: `PT_INTERP` у `obs`
//! и `obs-ffmpeg-mux` переписан на её `ld.so`.
//!
//! # Раскладка
//!
//! OBS ищет данные и модули **от своего исполняемого файла**
//! (`bin/../share/obs`, `bin/../lib/x86_64-linux-gnu/obs-plugins`), поэтому
//! `/opt/obs` повторяет `/usr` Debian: `bin/obs`, `share/obs/…`,
//! `lib/x86_64-linux-gnu/obs-plugins/…`; библиотеки — плоско в `lib/`
//! (`LD_LIBRARY_PATH`), модули Qt — `lib/qt6/plugins` (`QT_PLUGIN_PATH`).
//! Единственный абсолютный путь — `obs-ffmpeg-mux` в `obs-ffmpeg.so` —
//! переписан в файле на `/opt/obs/bin/obs-ffmpeg-mux`.
//!
//! Размер (23.11): пакет около 450 МиБ, из них Mesa с LLVM — 170, FFmpeg с
//! кодеками и фильтрами (их тянет `libavdevice`/`libavfilter`) — 150, Qt6 с
//! ICU — 60. Включается в носитель только с `FOSSAI_WITH_OBS=1`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use fpk::Kind;
use fpk::build::{Builder, Entry};

use crate::arch::Arch;
use crate::chromium::needed;
use crate::debian::{Node, follow, unpack};
use crate::{cbuild, sdk};

/// Имя пакета.
pub const PACKAGE_FILE: &str = "obs-30.fpk";

/// Где лежит собранный пакет.
pub fn package_path() -> PathBuf {
    cbuild::games_dir().join(PACKAGE_FILE)
}

/// Включён ли OBS в носитель: пакет в полгигабайта — только по
/// `FOSSAI_WITH_OBS=1` (как Proton).
pub fn enabled() -> bool {
    std::env::var("FOSSAI_WITH_OBS").is_ok_and(|value| value == "1")
}

/// Пакеты trixie (снимок 20261020): путь в архиве и SHA-256 из `Packages`.
/// Без тех, что уже в `mesa::PACKAGES`.
const PACKAGES: [(&str, &str); 209] = [
    // Только локаль C.UTF-8 (`usr/lib/locale/C.utf8`): Qt без UTF-8 не живёт,
    // а в корне Debian 12 её нет. Своя - через LOCPATH.
    ("pool/main/g/glibc/libc-bin_2.41-12+deb13u4_amd64.deb", "dc8c79647fe3a5f37f72a400414f70263d0212f010ff2c09f990e30d91e38d1c"),
    // Раскладки Xkb: без них Qt не создаёт контекст xkbcommon («failed to
    // create xkb context») и остаётся без клавиатуры.
    ("pool/main/x/xkeyboard-config/xkb-data_2.42-1_all.deb", "196ff18533382f64e057ea49df2bb486bd4275a4cc0917361edb560b8756dada"),
    // Только `ffprobe` (проверка записанного файла в гостевой системе).
    ("pool/main/f/ffmpeg/ffmpeg_7.1.5-0+deb13u1_amd64.deb", "8669bc77e55393c14e0dd257ba9a916007cb6da0e1074c9b5e1d53812024f2af"),
    ("pool/main/f/fonts-dejavu/fonts-dejavu-core_2.37-8_all.deb", "86635b3d25b3655fc11cb3ecc3af59f0bf19643b02b94f2de48bd10253cdba12"),
    ("pool/main/a/aom/libaom3_3.12.1-1+deb13u1_amd64.deb", "743eec7c94ae30295bc557ebac3dfae948e7390e89a2743c585d56297e9d312e"),
    ("pool/main/liba/libass/libass9_0.17.3-1+deb13u1_amd64.deb", "000faddfaa19c7c1631f2d71a9cd1982cd3a88611967debe631440a1c64617b4"),
    ("pool/main/liba/libasyncns/libasyncns0_0.8-6+b5_amd64.deb", "e7958777b0e3586349c57ac759a95b68f80d28e5bd1c4225705198b39b436115"),
    ("pool/main/g/gcc-14/libatomic1_14.2.0-19_amd64.deb", "212b399aae2f7299203d261a57e49372e09565a9a5ea971905f94a3960366c05"),
    ("pool/main/liba/libavc1394/libavc1394-0_0.5.4-5+b2_amd64.deb", "f76cd439099e941a97587c6f49b53fe14357c6577d7b7d6cbd26b9ca54f64f5f"),
    ("pool/main/f/ffmpeg/libavcodec61_7.1.5-0+deb13u1_amd64.deb", "6c07da16b276827973463bdd22607721df1c2877641c753d2418cf6bf49a0f2e"),
    ("pool/main/f/ffmpeg/libavdevice61_7.1.5-0+deb13u1_amd64.deb", "90a1b5e3256960a9297b9767b230da0ea29281acf9532cc08b0de785c3b901fe"),
    ("pool/main/f/ffmpeg/libavfilter10_7.1.5-0+deb13u1_amd64.deb", "55015c70802ac95c8e14d7cba5f6051c649bd8d2dd4971312b754378edff5867"),
    ("pool/main/f/ffmpeg/libavformat61_7.1.5-0+deb13u1_amd64.deb", "4b580060cebbd33a25589ba71074e3106caf2826bcdb82cf8936b52960f34fb2"),
    ("pool/main/f/ffmpeg/libavutil59_7.1.5-0+deb13u1_amd64.deb", "bb5fe4ea1eef338c8346bf67cc728dac20b94451ad31db391d03d1f2b3fa37c1"),
    ("pool/main/libb/libb2/libb2-1_0.98.1-1.1+b2_amd64.deb", "66bc1be2bffaa14efd2633d357ebb8672096aab63bcd38fc12e7b37183d956c9"),
    ("pool/main/l/lapack/libblas3_3.12.1-6_amd64.deb", "4af245ecafc30bfa1de9d20af02fec96654ead2f06b1dac40cb03fc9eb0bfbe6"),
    ("pool/main/u/util-linux/libblkid1_2.41.5-0+deb13u1_amd64.deb", "81535f3c2c0efc732965907c8749103a0a26377c761622c9ce39b4c92dcde52f"),
    ("pool/main/libb/libbluray/libbluray2_1.3.4-1+b2_amd64.deb", "82f9e58e8dfdfeb57b273111433c335500c26553eb82f2a025e94ed9fdae2c21"),
    ("pool/main/b/brotli/libbrotli1_1.1.0-2+b7_amd64.deb", "0fb79f88db210afbd69282ab9649e525f393ec6950ca34da1a6b359250b8d7db"),
    ("pool/main/libb/libbs2b/libbs2b0_3.1.0+dfsg-8+b1_amd64.deb", "683621e230de5d9cb919f6c6c3a18db4eb7b80706761b983a4a8f2a56747351d"),
    ("pool/main/b/bzip2/libbz2-1.0_1.0.8-6_amd64.deb", "cba4cda04244b5e481bb15524bc3c983a7d1b6f330013b9b381706a2fcb65310"),
    ("pool/main/g/glibc/libc6_2.41-12+deb13u4_amd64.deb", "967aa62605721081c3eb2a17650611a792aa802d76a6511d1840242623d204c9"),
    ("pool/main/libc/libcaca/libcaca0_0.99.beta20-5+deb13u1_amd64.deb", "29d212b4b137aa8cfb0d98d496cf0aca65a2e8832b4c4d38ce1ea276d77c97a0"),
    ("pool/main/c/cairo/libcairo-gobject2_1.18.4-1+b1_amd64.deb", "4fcd432ebe8f93599bd05426f304e2f70409aad271fc6b4b3a2c6243c99c377c"),
    ("pool/main/c/cairo/libcairo2_1.18.4-1+b1_amd64.deb", "01390b049661e4cc8bd39cef78b41c2587d4035619d3b00d6c7846f43d80bbba"),
    ("pool/main/libc/libcap2/libcap2_2.75-10+deb13u1+b3_amd64.deb", "89fc4d34fc7a28ad6f0fcd0c561ab253b9dedf6f77f5a000b47c276c8295bf67"),
    ("pool/main/libc/libcdio-paranoia/libcdio-cdda2t64_10.2+2.0.2-1+b1_amd64.deb", "3d6cb8220d4e67719bb84feb35022656e6b6ca3dec88e823906fc72b57c83e6f"),
    ("pool/main/libc/libcdio-paranoia/libcdio-paranoia2t64_10.2+2.0.2-1+b1_amd64.deb", "8721c897d6f70d694d4b5f09e66ce00a98d236b2746664a8e27635532b7695b8"),
    ("pool/main/libc/libcdio/libcdio19t64_2.2.0-4.1~deb13u1_amd64.deb", "bddfa8b7eca542379962152216d3414e999bca809a4a25543a193b4d1acc4ba1"),
    ("pool/main/c/chromaprint/libchromaprint1_1.5.1-7_amd64.deb", "ed8decea8352ae56e80862c4630c83f4f863d4eea653382ed8ae43ffacda60b5"),
    ("pool/main/c/cjson/libcjson1_1.7.18-3.1+deb13u1_amd64.deb", "e10b7fb966251f4f09690cb35f9e8fed6924194ec2d6973910d9e157cda82657"),
    ("pool/main/c/codec2/libcodec2-1.2_1.2.0-3_amd64.deb", "d68a3b754be7aa2d724d01d2079c542fc7cfa53eaa4ac89fbc6e63d950c02bd6"),
    ("pool/main/e/e2fsprogs/libcom-err2_1.47.2-3+b12_amd64.deb", "bd43e020b8fed399db3eebf098111f10fba4c436fff99ccad72cc7e27c9764ec"),
    ("pool/main/c/curl/libcurl3t64-gnutls_8.14.1-2+deb13u5_amd64.deb", "19da93544615a4c9a3d7df473f5bf8ebca99442454622e9b0460168dadf7b8ae"),
    ("pool/main/libd/libdatrie/libdatrie1_0.2.13-3+b1_amd64.deb", "339eb88f23d72b8f0b7b1aebcefbab6de600d97b88bc6222dd8aad11d2ca9051"),
    ("pool/main/d/dav1d/libdav1d7_1.5.1-1_amd64.deb", "8c64de4465a8ae1ca23e1dada1dfc73d7b431540fd5ae18f21430928576d5b05"),
    ("pool/main/d/db5.3/libdb5.3t64_5.3.28+dfsg2-9_amd64.deb", "18d02510ee78b67e4504ba050176797a200ff24214a7cd318082ab60ad7bf3fc"),
    ("pool/main/d/dbus/libdbus-1-3_1.16.2-2_amd64.deb", "bbcb711daff7e104b5f80f9b05475b6142674bfe65518a36a56e96a91068a3f5"),
    ("pool/main/libd/libdc1394/libdc1394-25_2.2.6-5_amd64.deb", "b8ac6c3a3b397085bcd80cf457c602cc3860caa1bb6dc9bc242bbde6b38d3d79"),
    ("pool/main/d/double-conversion/libdouble-conversion3_3.3.1-1_amd64.deb", "bd43b8c185a78d108548b30da6a695aabb6644ce0dd266892da2d10cfe471067"),
    ("pool/main/d/duktape/libduktape207_2.7.0-2+b2_amd64.deb", "2d92606f49cf79e23fb4ea67ab7702d52e016a671d32edc51482c4f1e3bade7c"),
    ("pool/main/libd/libdvdnav/libdvdnav4_6.1.1-3+b1_amd64.deb", "8368250b65cc26911fd37acdeeed58e9ce5bd390e941f687abecca9d5cd659f0"),
    ("pool/main/libd/libdvdread/libdvdread8t64_6.1.3-2_amd64.deb", "f753a23580c00a2166211607a5511a9c25d75c9ebfc34598b6526637de150fec"),
    ("pool/main/f/fftw3/libfftw3-double3_3.3.10-2+b1_amd64.deb", "5808e90242319a7ec7c0d50b40cdb1ebec9d0e695dcde14a4e7f1cd84657dc96"),
    ("pool/main/f/flac/libflac14_1.5.0+ds-2_amd64.deb", "658b9f77d35638caf75dfaea4f6d5cde196104d5662f0776e23d7cc450bd4cb5"),
    ("pool/main/f/flite/libflite1_2.2-7_amd64.deb", "5945190970be7d4a1f71fb6f7e3e340ba2675e770605df0754051d617a6c5ee6"),
    ("pool/main/f/fontconfig/libfontconfig1_2.15.0-2.3_amd64.deb", "7ae91ec59857cc8e375eb2bdd371a4a69daea38f7787d06d474a847867d852b1"),
    ("pool/main/f/freetype/libfreetype6_2.13.3+dfsg-1+deb13u1_amd64.deb", "e4947f3291528f03d574f2b01d5c5fc45c58c47480f32888a5cdf0f231ab584e"),
    ("pool/main/f/fribidi/libfribidi0_1.0.16-1_amd64.deb", "f8c35ec8606d0843bd3cf020c6cfa39ddfe717ec58ed826b5855ba9927eb0d55"),
    ("pool/main/g/gdk-pixbuf/libgdk-pixbuf-2.0-0_2.42.12+dfsg-4+deb13u1_amd64.deb", "94150fb955b2d3a9a5e054e31c5a4b048bad4a479207e86cde46fbc975c838b4"),
    ("pool/main/g/gcc-14/libgfortran5_14.2.0-19_amd64.deb", "11b62b53f4704de557e44b510e18a1a2106bcd9a3fc31a475a45952a9aa3dbb2"),
    ("pool/main/libg/libglvnd/libgl1_1.7.0-1+b2_amd64.deb", "87fa2f6e5abaed4ed385fac879c8dd735af719ee2300222d901793c66e041678"),
    ("pool/main/g/glib2.0/libglib2.0-0t64_2.84.4-3~deb13u5_amd64.deb", "e2baf92c57d1db1753e5781a61450d9a01e7833c7bd2dfe87776a6811c11ecc2"),
    ("pool/main/libg/libglvnd/libglx0_1.7.0-1+b2_amd64.deb", "2721fdca0fe3bd963cb39482eabc253af52b88f4a7f6dbb69e475549daf5af3b"),
    ("pool/main/g/game-music-emu/libgme0_0.6.3-7+b2_amd64.deb", "fbcef23f20099dd7b88a3d9edb79482359b2ab845632acb929313df7d2c5b6b7"),
    ("pool/main/g/gmp/libgmp10_6.3.0+dfsg-3_amd64.deb", "d0d0265eb01770f17afd0f7c8c0622f80479dcfbbe13653a0debeec61464e622"),
    ("pool/main/g/gnutls28/libgnutls30t64_3.8.9-3+deb13u4_amd64.deb", "18a8bdfd91c7e3bcb01719d55a2b56849c7160b34f1f52b1c4fdfdd41bf1352b"),
    ("pool/main/g/gcc-14/libgomp1_14.2.0-19_amd64.deb", "72b7a1a33c48f3195efeb044111b34ae684b1e640bb6495c832443b798601cd5"),
    ("pool/main/g/graphite2/libgraphite2-3_1.3.14-2+deb13u1_amd64.deb", "1769ba3ed3c1fc75b7fbe02678e32aa24391205813eceef4e1e3500015d12ee3"),
    ("pool/main/libg/libgsm/libgsm1_1.0.22-1+b2_amd64.deb", "388909810b0133b0dd8ea84884cd34db7bcbbd0f2643a7a65296a34644704a5e"),
    ("pool/main/k/krb5/libgssapi-krb5-2_1.21.3-5+deb13u1_amd64.deb", "30847c1fde4240567d7ed3aeab4f655dd591203758b857e85e824045aae70299"),
    ("pool/main/h/harfbuzz/libharfbuzz0b_10.2.0-1+deb13u1_amd64.deb", "932165b933e5eb633293afe984a983ebe89cb27c923ce7208626f1da6c0c7941"),
    ("pool/main/n/nettle/libhogweed6t64_3.10.1-1_amd64.deb", "b059ed155115ac09da295322de48ee1ed58ef5fa45edcc9e12ff0e94636ef25f"),
    ("pool/main/h/highway/libhwy1t64_1.2.0-2+b2_amd64.deb", "540c4ff28255fbfa89468061b722dfc2b773b0c1ecf163a063202482dea5b119"),
    ("pool/main/i/icu/libicu76_76.1-4_amd64.deb", "c1bf762996de9ecba9b9d871e4928a8090f023a3e9e7fa3240b3d90f892a01dc"),
    ("pool/main/libi/libidn2/libidn2-0_2.3.8-2_amd64.deb", "90b039bcdc4578f8e1c4935adf8dbb525e36a164deefdbbb8c45bac347d48278"),
    ("pool/main/libi/libiec61883/libiec61883-0_1.2.0-7_amd64.deb", "688dd2a0f65997d915acbd4ee5415e3b7704caf7854f20fd6b685bc56e254944"),
    ("pool/main/j/jackd2/libjack-jackd2-0_1.9.22~dfsg-4_amd64.deb", "64ec8d64ae0149bfa17b43d414cc8ae9032a27f76123864d185b275681666c45"),
    ("pool/main/j/jansson/libjansson4_2.14-2+b3_amd64.deb", "60707a62fe6c1228c3389b12a13ca4efd76defc5532473e547a29e99cf7d2a6e"),
    ("pool/main/libj/libjpeg-turbo/libjpeg62-turbo_2.1.5-4_amd64.deb", "5504299ad0e3c0a9ca9f962a68a650a040db52f9f416a109e74df48d55601d3f"),
    ("pool/main/j/jpeg-xl/libjxl0.11_0.11.2-0.1~deb13u2_amd64.deb", "cd1ca3a52cb0d8fdce1d994bcbf0663de28c64add1cfe8096639ae8fa9808824"),
    ("pool/main/k/krb5/libk5crypto3_1.21.3-5+deb13u1_amd64.deb", "7da07ee674b47f1f0be7cc89317c25310086a1f1761217d0f72e6ae2c5a69b84"),
    ("pool/main/k/keyutils/libkeyutils1_1.6.3-6_amd64.deb", "0b11ad17be0300b63ad4eeb4c6450fed24d34b7b740f23e5363dcb29ee6d5eba"),
    ("pool/main/k/krb5/libkrb5-3_1.21.3-5+deb13u1_amd64.deb", "47d71d6a7f2e59b9bae5f89602397594805113b95889ad18fa703cd53abafc97"),
    ("pool/main/k/krb5/libkrb5support0_1.21.3-5+deb13u1_amd64.deb", "3a0acd8b37955c0e102c756b52c97df2a31f67b96453c35dab70df218d309117"),
    ("pool/main/l/lapack/liblapack3_3.12.1-6_amd64.deb", "abfc59c8410beee738617bb6daf86b295e45ec0bd08cc0e97bfecc3cfa40fe75"),
    ("pool/main/l/lcms2/liblcms2-2_2.16-2+deb13u2_amd64.deb", "32e53c2002ccea8c60c02779bea0ba49ac132fef702988a30d64af79b6617806"),
    ("pool/main/o/openldap/libldap2_2.6.10+dfsg-1_amd64.deb", "60069e4a550ca890113fa89d3001417781b18e146ec5d637d31e368dec9909bd"),
    ("pool/main/l/lilv/liblilv-0-0_0.24.26-1_amd64.deb", "71f03560980b7c7f89ada7bd49ac3239166db770b2fa752b13d67abbd98ce8bc"),
    ("pool/main/m/mbedtls/libmbedcrypto16_3.6.6-0.1~deb13u1_amd64.deb", "b38baf2ecaca7b28b3005d34b8108ee9b17681426b20662813995e939aea6dfa"),
    ("pool/main/m/mbedtls/libmbedtls21_3.6.6-0.1~deb13u1_amd64.deb", "9e280505afa5f5aa7034e494e7466c82d8e9af2eb3bf2edf46bab0bd7b62e1ac"),
    ("pool/main/m/mbedtls/libmbedx509-7_3.6.6-0.1~deb13u1_amd64.deb", "e0b6630efcf9eae2e05172496576bc3235be134a6bbf8da4cf3570fba65f65b8"),
    ("pool/main/m/md4c/libmd4c0_0.5.2-2+b1_amd64.deb", "0bd88d7ce2bda0626836267c02beb1ddf89e99eae26bd5c717ce5601faa075a9"),
    ("pool/main/u/util-linux/libmount1_2.41.5-0+deb13u1_amd64.deb", "6d00f45f2e80e078e906e3eedecd3ba6913e39fef49bff361ee583f17f00ec05"),
    ("pool/main/l/lame/libmp3lame0_3.100-6+b3_amd64.deb", "3867319c9fbbc8cc25f2d1dc1d75d13548c84c37fa709c1ac9ca02ac051b63ab"),
    ("pool/main/m/mpg123/libmpg123-0t64_1.32.10-1+deb13u1_amd64.deb", "31fa6ef638f1fdbe198faee0b6fe50d9dd468077f8247cc3dbf6b2250abb6101"),
    ("pool/main/libm/libmysofa/libmysofa1_1.3.3+dfsg-1_amd64.deb", "27c71e7a0cb7b4d9a683b4ad721d65f11ff1d83823873ee28ac892078bcdb0a1"),
    ("pool/main/n/ncurses/libncursesw6_6.5+20250216-2_amd64.deb", "47baa2e11579583654583a066b69bd5a5d2f22423081f707ed6c0c3ba6d538b7"),
    ("pool/main/n/nettle/libnettle8t64_3.10.1-1_amd64.deb", "1b03d4a9cd9c8143ba50fe6396f36937e901a317d492d179f4596862c1731cfe"),
    ("pool/main/n/nghttp2/libnghttp2-14_1.64.0-1.1+deb13u1_amd64.deb", "896cb217537c09251fb909b8541349010ac279d802d871e98c06a62a2c67ce2c"),
    ("pool/main/n/nghttp3/libnghttp3-9_1.8.0-1_amd64.deb", "388e4c33b72829cbf0d098afdc8539fa5ba7458eddb077f5538fdb4d5538716b"),
    ("pool/main/n/ngtcp2/libngtcp2-16_1.11.0-1+deb13u1_amd64.deb", "627eec81ebbd48c4e6091f5cd9dc5070b792b7075000eed60ab08c7daa961caf"),
    ("pool/main/n/ngtcp2/libngtcp2-crypto-gnutls8_1.11.0-1+deb13u1_amd64.deb", "2a7f109c0c4db6a800e4661c5e5e34e1f1f83c8162482276183d1ada9da7c96c"),
    ("pool/main/n/norm/libnorm1t64_1.5.9+dfsg-3.1+b2_amd64.deb", "cf48143ffa3886224cd0af63a8ecfac6f87177d80e7a053793c8621781b944ea"),
    ("pool/main/n/numactl/libnuma1_2.0.19-1_amd64.deb", "e578583f73f8208564e5035b18aa729c441fc7ecb389c666923552e123f4d655"),
    ("pool/main/o/obs-studio/libobs0t64_30.2.3+dfsg-3_amd64.deb", "57b7c167cd6e7aec819b97b1269803acfe7f92558c87e68e3681d4068add8d7d"),
    ("pool/main/libo/libogg/libogg0_1.3.5-3+b2_amd64.deb", "911aec914f5bda0788164ff8068b682537378bf8d407b1d6c1b377522f96b83b"),
    ("pool/main/o/openal-soft/libopenal1_1.24.2-1_amd64.deb", "7b888c0854bdf8e61fb000aa2816ea5ed76cf52da85dba94e463cce5e182662d"),
    ("pool/main/libg/libglvnd/libopengl0_1.7.0-1+b2_amd64.deb", "50ea65a1141cbf6b02e41492963656ad0ba71d257cc4066ee9756bca7a6c1ac3"),
    ("pool/main/o/openjpeg2/libopenjp2-7_2.5.3-2.1~deb13u2_amd64.deb", "7ddf6360e8845853ba3c9cfaefc2ad6b92b2627eb8564a257f5f2a4c857f8748"),
    ("pool/main/libo/libopenmpt/libopenmpt0t64_0.7.13-1+b1_amd64.deb", "37537d1b0dc3798d9157aa453427609a3e7f52c0c89b36563764c193194aaa6a"),
    ("pool/main/o/opus/libopus0_1.5.2-2_amd64.deb", "794056db33d71b2ac4bd8b5a4eb23b627bcb8a49d123c33b6e841a996253a067"),
    ("pool/main/p/p11-kit/libp11-kit0_0.25.5-3_amd64.deb", "784bf2063e166c8bc851a32623b74ebd85c499043a9d57c5bbd64fa63447f45a"),
    ("pool/main/p/pango1.0/libpango-1.0-0_1.56.3-1_amd64.deb", "4fa38472696d2ddb10bfc9a4692a9446e8478fdb1f48c769a609fc9fec3adfc5"),
    ("pool/main/p/pango1.0/libpangocairo-1.0-0_1.56.3-1_amd64.deb", "6994729c5b7a9649661ec078cf039991f22eff44600543aa917894f83c89cd05"),
    ("pool/main/p/pango1.0/libpangoft2-1.0-0_1.56.3-1_amd64.deb", "b506cf66f78ddf542ddb4bb4a68b3d748d41caa6e190e4480430a6630c1e5105"),
    ("pool/main/p/pciutils/libpci3_3.13.0-2_amd64.deb", "f8f01883368fe86417b3845aa08ea3508ca59f5e1bb0983673629e227f5ac868"),
    ("pool/main/p/pcre2/libpcre2-16-0_10.46-1~deb13u2_amd64.deb", "0343948654468fc4b273077c464cb77afeee29fc493c0d4c9d81cd19d5bc4e23"),
    ("pool/main/p/pcre2/libpcre2-8-0_10.46-1~deb13u2_amd64.deb", "1252b96a5bc44bb5db982bef8eb18e54f5047cede2aff641bce4f8e1edb91c3e"),
    ("pool/main/libp/libpgm/libpgm-5.3-0t64_5.3.128~dfsg-2.1+b1_amd64.deb", "865c47a7865da63e7d620f0999ab7d10cdae8b638b691061166975c654a0e5f9"),
    ("pool/main/p/pixman/libpixman-1-0_0.44.0-3_amd64.deb", "3862c87596f6b4b60fc916395fd69aa10af28a875a8e5c081a8cf30ddd120bd6"),
    ("pool/main/libp/libplacebo/libplacebo349_7.349.0-3_amd64.deb", "3d2ebdb86ccc35b9d115e8280cfc12c2cdc5ca67d0234af7370259181768dff2"),
    ("pool/main/libp/libpng1.6/libpng16-16t64_1.6.48-1+deb13u5_amd64.deb", "2465b4e9fa85cff54dc10a6da3e64074d8d9292c86e4a8f989b809d6b158e97e"),
    ("pool/main/p/pocketsphinx/libpocketsphinx3_0.8+5prealpha+1-15+b4_amd64.deb", "b2c444a2f9b7456fa804f672c0d7849a33d857f5a1de7710b02355721cfd2750"),
    ("pool/main/f/ffmpeg/libpostproc58_7.1.5-0+deb13u1_amd64.deb", "4a19b79dac4353419018509568f554df3ce937d2283c859f6454c3fe543135b6"),
    ("pool/main/libp/libproxy/libproxy1v5_0.5.9-1_amd64.deb", "794a8526989464cb17f1dec9dab921f0e5afb82bcc665e938fac925be47698eb"),
    ("pool/main/libp/libpsl/libpsl5t64_0.21.2-1.1+b1_amd64.deb", "59d42bb1f9ebc0d1776fe616efb08a7a8568b05982e00f70b03c63863db768ab"),
    ("pool/main/p/pulseaudio/libpulse0_17.0+dfsg1-2+b1_amd64.deb", "786aca79119cb249562708fd24dcdf817604cfe75bbfcb8cda3464042e17f40e"),
    ("pool/main/p/python3.13/libpython3.13_3.13.5-2+deb13u5_amd64.deb", "aa4cdb7e15f5f445ab268160d428518a4daf2a086ee3e6ea30f6082785236dea"),
    ("pool/main/q/qt6-base/libqt6core6t64_6.8.2+dfsg-9+deb13u2_amd64.deb", "c7691937813e6cdf712aa98013bcac9d414f6fdeaea041ff53de5f8ae9b58e10"),
    ("pool/main/q/qt6-base/libqt6dbus6_6.8.2+dfsg-9+deb13u2_amd64.deb", "ea2b6563e4e3f6c8c2838453cb2e0e70933e5e08fde6b8dc5ca7cefad9c80764"),
    ("pool/main/q/qt6-base/libqt6gui6_6.8.2+dfsg-9+deb13u2_amd64.deb", "a39717b874f7b71c69f27199299dc09d5f47b809af3064cf067e5e443b5d011b"),
    ("pool/main/q/qt6-base/libqt6network6_6.8.2+dfsg-9+deb13u2_amd64.deb", "394b3645254a9c2a4a24957d5cf32e3e6aab66c15a6f2cc9aa2298622bbd2f61"),
    ("pool/main/q/qt6-base/libqt6opengl6_6.8.2+dfsg-9+deb13u2_amd64.deb", "0b1fb1147dd6f0af541d541cc2fcc1168214b0973fd63941bf3338f208f9214d"),
    ("pool/main/q/qt6-svg/libqt6svg6_6.8.2-3_amd64.deb", "4fc94aee5785437477f0aa379e90e1a87eb994da6029eb55f968e34094e389ac"),
    ("pool/main/q/qt6-wayland/libqt6waylandclient6_6.8.2-4_amd64.deb", "5b78f1caf65532ce52c6d4c51e043a223390cfe6376c1b9af1a5fd99ea1e7a9d"),
    ("pool/main/q/qt6-base/libqt6widgets6_6.8.2+dfsg-9+deb13u2_amd64.deb", "f61ccd0c809a665a6061c6a28d265965e8712e616599a81e50699ff8394afeea"),
    ("pool/main/q/qt6-base/libqt6xml6_6.8.2+dfsg-9+deb13u2_amd64.deb", "6ba7668e09150684cf98578862ac1c507583495db4581e5eda301dd210c71c4e"),
    ("pool/main/libr/librabbitmq/librabbitmq4_0.15.0-1+deb13u2_amd64.deb", "7b81f891935c05a01ddca461564906dbc3c48f02519b2c20b8ca7590f2868112"),
    ("pool/main/r/rust-rav1e/librav1e0.7_0.7.1-9+b2_amd64.deb", "d5b1373a8d6905ee4523faf1db7a626dc660e03836ec91b616e57ba386f47eb6"),
    ("pool/main/libr/libraw1394/libraw1394-11_2.1.2-2+b2_amd64.deb", "f6f4f76b427f2caa2ed0905e64f13c2a8902fba04c08060e806b32bc35d6daf8"),
    ("pool/main/libr/librist/librist4_0.2.11+dfsg-1_amd64.deb", "a3f66bd87fdd95561ec41894a782ca9e2e9272ed72c925be8a44da95e41a03b2"),
    ("pool/main/libr/librsvg/librsvg2-2_2.60.0+dfsg-1_amd64.deb", "81fd7a825f06e344dc42fb7af92577ec242b6d24237b8a0bf79ea203a7f377c9"),
    ("pool/main/r/rtmpdump/librtmp1_2.4+20151223.gitfa8646d.1-2+b5_amd64.deb", "93baa2004cbe6c8721b9e81beed078612540aef120970b4751305c51c6697368"),
    ("pool/main/r/rubberband/librubberband2_3.3.0+dfsg-2+b3_amd64.deb", "4593fde69a6a10f2356d0fd1e7b1cd61faa48cc83a1a0b01b7d021dfe5252a85"),
    ("pool/main/libs/libsamplerate/libsamplerate0_0.2.2-4+b2_amd64.deb", "2665417fa996c1e249dbaa65c32305ac666e5ae865fad0f109b629935ec0bd18"),
    ("pool/main/c/cyrus-sasl2/libsasl2-2_2.1.28+dfsg1-9_amd64.deb", "66e49d7ae026811b49a0050f18b0325960a2625ffd1ff60d91b7844a815fa9d2"),
    ("pool/main/libs/libsdl2/libsdl2-2.0-0_2.32.4+dfsg-1_amd64.deb", "c61af1033d79fa6a393c49fac987165876de8ff27ebbd6a36cf19f5fc84d7a7a"),
    ("pool/main/libs/libselinux/libselinux1_3.8.1-1_amd64.deb", "68bb8d32bd8d6d7d2f5952a169db03d1484b46ae1e52abccdec42a19dccea5d5"),
    ("pool/main/s/serd/libserd-0-0_0.32.4-1_amd64.deb", "a2e1097666c09bf2fce41bff40b8d4cfb8c3c419da99fcb746ea175c5d48fcd5"),
    ("pool/main/libw/libwebp/libsharpyuv0_1.5.0-0.1_amd64.deb", "51934833d909588f7761fd84362114e90d8d2d38af57edf5df5c4a705e037547"),
    ("pool/main/s/shine/libshine3_3.1.1-2+b2_amd64.deb", "8908eb0c70893d122ee9cbed7186736ad94b45716404ef9622845bdd7d17b29d"),
    ("pool/main/s/slang2/libslang2_2.3.3-5+b2_amd64.deb", "49b52a2cbe0f2257f4f7c68a0eaecd90962edabf08097611548b3ea4bd73292e"),
    ("pool/main/s/snappy/libsnappy1v5_1.2.2-1_amd64.deb", "53ec5f1050d64375a63cd1c30ee4a6d21ef0cfd485f9999aa0b70dd424ca3465"),
    ("pool/main/libs/libsndfile/libsndfile1_1.2.2-2+deb13u1_amd64.deb", "d92263a045554e6f39c6b1c2ee4cca44a3248d9b9339301ff195969bf84b75eb"),
    ("pool/main/libs/libsodium/libsodium23_1.0.18-1+deb13u1_amd64.deb", "9ff9d97bb7ce3aee7f6e6ec3a785cdbe237454a7b9fd84728121bc4ef2bebebe"),
    ("pool/main/s/sord/libsord-0-0_0.16.18-1_amd64.deb", "cbb4167b1a85cfec94faa9f85666ee382a8bc79ce93f5d7457514e8334299e52"),
    ("pool/main/libs/libsoxr/libsoxr0_0.1.3-4+b2_amd64.deb", "384caa47b6cfe780f30d33c5dfb1c18f6579aed9f3bdbf7f8cd0395ab2d537b7"),
    ("pool/main/s/speex/libspeex1_1.2.1-3_amd64.deb", "26623d450c2a51f9f6801b19ee5be1303aee4087557d1389020a0df00afa6e77"),
    ("pool/main/s/speexdsp/libspeexdsp1_1.2.1-3_amd64.deb", "4f0f6fc83b2f9fbb85b5a1c1470033979bbcd4c72d2177fea98a55f014c4391e"),
    ("pool/main/s/sphinxbase/libsphinxbase3t64_0.8+5prealpha+1-21+b1_amd64.deb", "97edf4f50ed24364795e53abecd6e100d1f24278968b2eb01a688c78255eb80b"),
    ("pool/main/s/sratom/libsratom-0-0_0.6.18-1_amd64.deb", "63195bd9233678ed397cfded0aae85d85cf2584ff7c638b34013ca34544962d7"),
    ("pool/main/s/srt/libsrt1.5-gnutls_1.5.4-1+deb13u1_amd64.deb", "234ed0b5fcff17d2ecce4c625d897b78545f94f5096b1ec689e9a678f9067f1c"),
    ("pool/main/s/srt/libsrt1.5-openssl_1.5.4-1+deb13u1_amd64.deb", "040ea4a67c72eae826a3e8067c694649725061fd59030afa12d898ba7b344f31"),
    ("pool/main/libs/libssh/libssh-4_0.11.5-0+deb13u1_amd64.deb", "bc8cbc13617d1b21fa6f686f9f269a4f849f00624d5bb179785a058981701233"),
    ("pool/main/libs/libssh2/libssh2-1t64_1.11.1-1+deb13u2_amd64.deb", "dbb1024c192d4d292b7cfa902b96076cfe81b56a5eed4fb28da36e8bf543e49a"),
    ("pool/main/o/openssl/libssl3t64_3.5.7-1~deb13u2_amd64.deb", "916f7f40b34a06e6ebfaefcdab331bff458328411da672598f126a760472467d"),
    ("pool/main/s/svt-av1/libsvtav1enc2_2.3.0+dfsg-1_amd64.deb", "cdaf961c3cc4fe78a19ff18c17d83d813f78a378fdcc4c86ccbb9b54e84ae599"),
    ("pool/main/f/ffmpeg/libswresample5_7.1.5-0+deb13u1_amd64.deb", "abe0f2f6a27db64a0d2d344e333880be997b2ec5431fc187a100078075680d94"),
    ("pool/main/f/ffmpeg/libswscale8_7.1.5-0+deb13u1_amd64.deb", "cf43cf4fe02a02cd36b25a86a2fcdeeaca9cc22bb1c81239302514db701d4e6d"),
    ("pool/main/s/systemd/libsystemd0_257.13-1~deb13u1_amd64.deb", "ab0d4127b5e46e6f8c015a1db15a62ba9ae274cdefa150083adf90ada0600ea1"),
    ("pool/main/libt/libtasn1-6/libtasn1-6_4.20.0-2+deb13u1_amd64.deb", "23fec6e06583ce2bad9b2c04c9b485e90440e259b1abf8677cd80d3ce60831ad"),
    ("pool/main/libt/libthai/libthai0_0.1.29-2+b1_amd64.deb", "e6364c07aeeffe9a95f267061d9df168e48e9dc9126c3b40e19dc829f67702a7"),
    ("pool/main/libt/libtheora/libtheoradec1_1.2.0~alpha1+dfsg-6_amd64.deb", "31c566d16b0a30f2c100d685d09b831d10d756bd043107ec6594c082274c0c74"),
    ("pool/main/libt/libtheora/libtheoraenc1_1.2.0~alpha1+dfsg-6_amd64.deb", "ce795eba6cdb510a52cd158dee7930a23667ffb4bdf07fbc44d4590edbf6f503"),
    ("pool/main/t/twolame/libtwolame0_0.4.0-2+b2_amd64.deb", "f3e739c8dd8b7c77043406bb4a4eef16d192e2e0aa5f8d9f39e09ff087844e6d"),
    ("pool/main/s/systemd/libudev1_257.13-1~deb13u1_amd64.deb", "5d41c284f5a93b05bc7d648b61a02dd2bb9ff05b2261ad1a8b7d96044a0cfa88"),
    ("pool/main/libu/libudfread/libudfread0_1.1.2-1+b2_amd64.deb", "dbb34708521e4e895d1e061b765548ea408e670c8b2a17e84529166afa88e5f1"),
    ("pool/main/libu/libunibreak/libunibreak6_6.1-3_amd64.deb", "a5d5fa113f195288ed145d48d0e886671c94416ae7c800f943bc21cc4ef628cc"),
    ("pool/main/libu/libunistring/libunistring5_1.3-2_amd64.deb", "6dd3490bef06ea1096f32d10766b7e016cf579bfae8451d1b7df15ce05b8aa46"),
    ("pool/main/libu/libusb-1.0/libusb-1.0-0_1.0.28-1_amd64.deb", "bb6467a002915c4ac464aa1b22204907951a30e43bcb86596ebd3539e84458e9"),
    ("pool/main/u/util-linux/libuuid1_2.41.5-0+deb13u1_amd64.deb", "c1bf4c4c3ff48c57fabf93307dfb56996b60cfa33927afc4158b5db36fb2721e"),
    ("pool/main/libv/libva/libva-drm2_2.22.0-3_amd64.deb", "5dce5007ddc0ce87a61db4d71476ce1a5a135737b5185ce2e8b7067342fcafc6"),
    ("pool/main/libv/libva/libva-x11-2_2.22.0-3_amd64.deb", "97baabc9ae8dfbe718ca80543dc7337d330dd17e2a59138bf42a66a35d59309b"),
    ("pool/main/libv/libva/libva2_2.22.0-3_amd64.deb", "b76bdd330de47a826698aaed10f53435b703e9a7d4415dd68269c97709f46a9b"),
    ("pool/main/libv/libvdpau/libvdpau1_1.5-3+b1_amd64.deb", "7c872493296cc8ce829ab38a68b5e51c351466a5fd0daf485984c829282a94e3"),
    ("pool/main/libv/libvidstab/libvidstab1.1_1.1.0-2+b2_amd64.deb", "248da59fbb0d161666ee40846471f066cef65960735c9a39cc0fa9134db4323f"),
    ("pool/main/libv/libvorbis/libvorbis0a_1.3.7-3_amd64.deb", "0465f7f602f7b00965e58e7273b5876060ab7946f0452a9f95d979046932cc42"),
    ("pool/main/libv/libvorbis/libvorbisenc2_1.3.7-3_amd64.deb", "5fbbf06a5b196235aa2753cd12bbe7f843328571d368f15b1c337578e29f73a2"),
    ("pool/main/libv/libvorbis/libvorbisfile3_1.3.7-3_amd64.deb", "02822ab3da107b6f65bb3f7ac5bc8d68f51f998dd6886dd38f509f4d04d0ccd6"),
    ("pool/main/libv/libvpl/libvpl2_2.14.0-1+b1_amd64.deb", "cee5b3fdfb5c2a944f4366406bf245992c7aaa234cdfb41acffba8f2bfbed1b4"),
    ("pool/main/libv/libvpx/libvpx9_1.15.0-2.1+deb13u1_amd64.deb", "5c2baac5c97a1fe01c4f0a2216743b901dc72437351e7451725a394c08037bcc"),
    ("pool/main/w/wayland/libwayland-client0_1.23.1-3_amd64.deb", "d1607e1db1a7c5378a2e5c6ae7b20f64dc6acf134f953d84a7f5204b74339f33"),
    ("pool/main/w/wayland/libwayland-cursor0_1.23.1-3_amd64.deb", "26fccae2b1228744de0cad6c688550b8d13cd4e8d4ccf0cfdae015634ece673f"),
    ("pool/main/libw/libwebp/libwebp7_1.5.0-0.1_amd64.deb", "c749744b04abdd1426b0c7718e516362c03a557c5151662092d6d4f2cef6e7c1"),
    ("pool/main/libw/libwebp/libwebpmux3_1.5.0-0.1_amd64.deb", "8bc86d509406ad496b6414c7068b3dadc0f8c502027973cbf1765c7bb54fabdf"),
    ("pool/main/x/x264/libx264-164_0.164.3108+git31e19f9-2+b1_amd64.deb", "325c90dc0888be316c8979b94aee546d223d4edf03b30927d1e9d431f67e33b2"),
    ("pool/main/x/x265/libx265-215_4.1-2_amd64.deb", "4bb075b465dbc136b0c5050b956432ea7e443a16d592aa89ef28bcaf4b0ad734"),
    ("pool/main/libx/libxcb/libxcb-render0_1.17.0-2+b1_amd64.deb", "7f6e36978e0f70a590fbad4042a692c70b50ca7cbcdc53fa7e9d9ab87933acc6"),
    ("pool/main/libx/libxcb/libxcb-shape0_1.17.0-2+b1_amd64.deb", "eb94a643d66714929053e78ff8f38a90ce8af4ed854e3cbca0b14f8bbccc7bfc"),
    ("pool/main/libx/libxcb/libxcb-xinput0_1.17.0-2+b1_amd64.deb", "10f588b006a5a1a527c9971f4e9712d9aac12b98acd5d65c3f7b53a10ac36198"),
    ("pool/main/libx/libxcursor/libxcursor1_1.2.3-1_amd64.deb", "bb0dbf58b804faaa74ed9efdae61c4b14bb0b4855680798f63e5ca6a4954976b"),
    ("pool/main/libx/libxfixes/libxfixes3_6.0.0-2+b4_amd64.deb", "3fdd95d86d8e9d63e11f52070935c8f9f912c36aa3b17338d969f7685f3ed4fb"),
    ("pool/main/libx/libxi/libxi6_1.8.2-1_amd64.deb", "093d0903f35bb7a9f6815180ee040e6951fecf9b66c128cd72f064710210606e"),
    ("pool/main/libx/libxkbcommon/libxkbcommon0_1.7.0-2_amd64.deb", "f75ee544f55acc6a271debfab3ea4ae0458afc89d81cfe1a71137e07d4895b86"),
    ("pool/main/libx/libxrandr/libxrandr2_1.5.4-1+b3_amd64.deb", "11e3490de93a8bbee3daba719cb8e1325a26fb3c125525c34bdcb7deb05eb9b2"),
    ("pool/main/libx/libxrender/libxrender1_0.9.12-1_amd64.deb", "9d042dfd5e613be1e02e6ddd0c5c4adef19c5eb08f6db838c2eba672c496dca4"),
    ("pool/main/libx/libxss/libxss1_1.2.3-1+b3_amd64.deb", "302d4a97800429fa0d67ae2e0e8e3adc1228b12d25d3d3cca6c9d24eeff40a6c"),
    ("pool/main/libx/libxv/libxv1_1.0.11-1.1+b3_amd64.deb", "e19f6bdcd20c1c28fefea29f0a95c33232dd5f0d26e0079675ac8ba4af4eda07"),
    ("pool/main/x/xvidcore/libxvidcore4_1.3.7-1+b2_amd64.deb", "a3ea54e60cbc13dcd5f4a64c5635c52701274ae52baa9902d87ad742af4e4977"),
    ("pool/main/z/zimg/libzimg2_3.0.5+ds1-1+b2_amd64.deb", "d021b92ec5d647565db6b67e92a7e06e16b35ae6046c0f36d454a0cff88bbd50"),
    ("pool/main/z/zix/libzix-0-0_0.6.2-1_amd64.deb", "896d3b244020120b17a21ded38508582ee861ffa90fc0fb0fd539e27698a96b6"),
    ("pool/main/z/zeromq3/libzmq5_4.3.5-1+b3_amd64.deb", "82348834b94ae5288dd1dde1c6a59488d30e3ee97fd4183c0b60ed79c3ab0b57"),
    ("pool/main/z/zvbi/libzvbi0t64_0.2.44-1_amd64.deb", "ff9b1227f8812b4d51d24130bfeb436a3cc3159410d507cb370f6908e9059112"),
    ("pool/main/o/obs-studio/obs-plugins_30.2.3+dfsg-3_amd64.deb", "c5956000f6ced91772aa632350bf9fcb194234cb5142f6a2edf325fe66b6fac0"),
    ("pool/main/o/obs-studio/obs-studio_30.2.3+dfsg-3_amd64.deb", "25c24a956f35db063d847dc5797c79ce50eb7931842d6b8201f22b9cfe16d2ee"),
    ("pool/main/o/ocl-icd/ocl-icd-libopencl1_2.3.3-1_amd64.deb", "99dfd409ae92505ee56742feea4e82a1e052818c5b0036ee618a371b5e28c9d1"),
    ("pool/main/q/qt6-svg/qt6-svg-plugins_6.8.2-3_amd64.deb", "7c1586a7ee2cd6cad3cbefd79a1429cd6ae16a4583b28b12006fad19285f5313"),
    ("pool/main/q/qt6-wayland/qt6-wayland_6.8.2-4_amd64.deb", "87b0905041f8f43164f9ff72ca00143a5c12bdcc2f06aacc0f7b178b16fa2c62"),
];

/// Заголовки libobs для сборки своего модуля (в пакет не едут).
const DEV_PACKAGES: [(&str, &str); 1] = [(
    "pool/main/o/obs-studio/libobs-dev_30.2.3+dfsg-3_amd64.deb",
    "4eb6d9459f23d5c26f37bfd630f4ae26629f8cdc4bdc46d133132c0fafa4b293",
)];

/// Свой модуль OBS: исходник в `libc/linux` и имя в `obs-plugins`.
const FOSSAI_PLUGIN: (&str, &str) = ("obs-fossai.c", "obs-fossai.so");

/// Каталог библиотек в пакетах trixie (usrmerge).
const DIR: &str = "usr/lib/x86_64-linux-gnu/";

/// Где ищет `ld.so` в корне Debian 12 (что там есть — в пакет не кладётся).
const LIB_DIRS: [&str; 4] = ["lib/x86_64-linux-gnu", "usr/lib/x86_64-linux-gnu", "lib", "usr/lib"];

/// Модули OBS (`obs-plugins/<имя>`). Не берутся: `frontend-tools`
/// (сценарии Python/Lua — тянет `libobs-scripting` и стандартную библиотеку
/// Python), `linux-capture` (X11), `linux-pipewire` (PipeWire и портал),
/// `linux-pulseaudio`, `linux-jack`, `linux-v4l2` (камер нет), `decklink*`,
/// `vlc-video`, `obs-websocket`.
const OBS_PLUGINS: [&str; 10] = [
    "image-source.so",
    "obs-ffmpeg.so",
    "obs-filters.so",
    "obs-outputs.so",
    "obs-transitions.so",
    "obs-x264.so",
    "rtmp-services.so",
    "text-freetype2.so",
    "linux-alsa.so",
    // Мультиплексор записи: отдельная программа, `obs-ffmpeg` зовёт её
    // через `posix_spawn`.
    "obs-ffmpeg/obs-ffmpeg-mux",
];

/// Модули Qt (`qt6/plugins/…`): платформа Wayland с EGL, оболочка xdg-shell,
/// рамка окна (если сервер не рисует свою), значки SVG тем OBS, картинки.
/// `offscreen` — для проверки без окна (`QT_QPA_PLATFORM=offscreen`).
const QT_PLUGINS: [&str; 13] = [
    "platforms/libqwayland-egl.so",
    "platforms/libqwayland-generic.so",
    "platforms/libqoffscreen.so",
    "wayland-shell-integration/libxdg-shell.so",
    "wayland-decoration-client/libbradient.so",
    "wayland-graphics-integration-client/libqt-plugin-wayland-egl.so",
    "wayland-graphics-integration-client/libshm-emulation-server.so",
    "iconengines/libqsvgicon.so",
    "imageformats/libqsvg.so",
    "imageformats/libqgif.so",
    "imageformats/libqico.so",
    "imageformats/libqjpeg.so",
    "platforminputcontexts/libcomposeplatforminputcontextplugin.so",
];

/// Что из glibc trixie кладётся (`usr/lib/x86_64-linux-gnu/…`).
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

/// Загрузчик Debian в `PT_INTERP` и свой на его место (не длиннее).
const INTERP_OLD: &[u8] = b"/lib64/ld-linux-x86-64.so.2\0";
const INTERP_NEW: &[u8] = b"/opt/obs/glibc/ld.so\0";

/// Переписать загрузчик программы на свою glibc. `false` — строки нет.
fn patch_interp(data: &mut [u8]) -> bool {
    patch_string(data, INTERP_OLD, INTERP_NEW)
}

/// Заменить в файле строку C `old` на не более длинную `new` (хвост —
/// нулями). `false` — строки нет.
fn patch_string(data: &mut [u8], old: &[u8], new: &[u8]) -> bool {
    assert!(new.len() <= old.len());
    let Some(at) = data.windows(old.len()).position(|window| window == old) else {
        return false;
    };
    data[at..at + old.len()].fill(0);
    data[at..at + new.len()].copy_from_slice(new);
    true
}

/// Путь `obs-ffmpeg-mux`, зашитый в `obs-ffmpeg.so`, и свой.
const MUX_OLD: &[u8] = b"/usr/lib/x86_64-linux-gnu/obs-plugins/obs-ffmpeg/obs-ffmpeg-mux\0";
const MUX_NEW: &[u8] = b"/opt/obs/bin/obs-ffmpeg-mux\0";

/// Мультиплексор записи OBS запускает с **пустым** окружением
/// (`posix_spawn(..., envp = NULL)` в `os_process_pipe_create2` libobs):
/// без `LD_LIBRARY_PATH` загрузчик не находил `libobs.so.0`, процесс уходил с
/// кодом 127, и запись оставалась пустой. Переходник ставит пути сам; bash
/// корня Debian 12 при пустом окружении glibc 2.41 не видит.
const MUX_SCRIPT: &str = "#!/bin/bash\n\
    export LD_LIBRARY_PATH=/opt/obs/glibc:/opt/obs/lib\n\
    exec /opt/obs/lib/obs-ffmpeg-mux \"$@\"\n";

/// Шрифты DejaVu (кириллица) и `fonts.conf`.
const FONT_DIR: &str = "usr/share/fonts/truetype/dejavu/";
const FONTS_CONF: &str = "<?xml version=\"1.0\"?>\n\
    <!DOCTYPE fontconfig SYSTEM \"fonts.dtd\">\n\
    <fontconfig>\n\
    \t<dir>/opt/obs/fonts</dir>\n\
    \t<cachedir>/tmp/fontconfig</cachedir>\n\
    \t<alias><family>sans-serif</family><prefer><family>DejaVu Sans</family></prefer></alias>\n\
    \t<alias><family>serif</family><prefer><family>DejaVu Sans</family></prefer></alias>\n\
    \t<alias><family>monospace</family><prefer><family>DejaVu Sans Mono</family></prefer></alias>\n\
    </fontconfig>\n";

/// Настройки OBS при первом запуске (`~/.config/obs-studio`, если его нет):
/// русский язык, без мастера автонастройки (`FirstRun`) и без вопросов при
/// выходе во время записи, профиль и коллекция сцен «FossAi».
const GLOBAL_INI: &str = "[General]\n\
    Pre19Defaults=false\n\
    Pre21Defaults=false\n\
    Pre23Defaults=false\n\
    Pre24.1Defaults=false\n\
    FirstRun=true\n\
    LastVersion=503447555\n\
    Language=ru-RU\n\
    EnableAutoUpdates=false\n\
    InfoIncrement=-1\n\
    \n\
    [Basic]\n\
    Profile=FossAi\n\
    ProfileDir=FossAi\n\
    SceneCollection=FossAi\n\
    SceneCollectionFile=FossAi\n\
    ConfigOnNewProfile=true\n\
    \n\
    [BasicWindow]\n\
    WarnBeforeStartingStream=false\n\
    WarnBeforeStoppingStream=false\n\
    WarnBeforeStoppingRecord=false\n\
    SysTrayEnabled=false\n\
    ShowTransitions=true\n";

/// Профиль «FossAi» (25.11, версия 2 — `FossAiProfile=2`; старый профиль
/// `fossai-obs` заменяет один раз): расширенный режим вывода — кодировщик
/// x264 и для записи, и для трансляции (NVENC нет: аппаратного кодирования на
/// NVK пока нет — Vulkan Video encode, TODO в ROADMAP), предустановка
/// `veryfast`, потоков x264 — четверть ЦП, от 2 до 4 (`@X264T@`: игре
/// остальное). Запись — MP4, CRF 21 (`recordEncoder.json`), трансляция — CBR
/// 6000 кбит/с, ключевой кадр раз в 2 с (`streamEncoder.json`), звук AAC 160
/// кбит/с 48 кГц стерео, файлы — `~/Videos`. Простой режим (если его выбрать
/// в настройках) — как раньше: `veryfast`, «Высокое качество, средний размер
/// файла». Горячие клавиши: F9 — начать запись, F10 — остановить (OBS под
/// Wayland ловит их, когда его окно в фокусе).
///
/// Видео (24.11, Роман: «два монитора одной картинкой, качество плохое»):
/// холст — размер **основного** монитора (`@BASECX@`x`@BASECY@`), вывод — он
/// же до 1920x1080, крупнее — к высоте 1080 с теми же пропорциями
/// (`@OUTCX@`x`@OUTCY@`; всё подставляет `fossai-obs` по `/dev/fossai-capture`).
/// Без этих строк OBS берёт экран Qt и уменьшает вывод до 1280x720 и меньше
/// (`OBSBasic::InitBasicConfigDefaults`) — отсюда было мыло. Уменьшение —
/// Lanczos, цвет — NV12, BT.709, ограниченный диапазон (как у OBS по
/// умолчанию, так ждут проигрыватели).
const PROFILE_INI: &str = "[General]\n\
    Name=FossAi\n\
    FossAiProfile=2\n\
    \n\
    [Output]\n\
    Mode=Advanced\n\
    \n\
    [AdvOut]\n\
    TrackIndex=1\n\
    Encoder=obs_x264\n\
    ApplyServiceSettings=true\n\
    Rescale=false\n\
    RecType=Standard\n\
    RecFilePath=@HOME@/Videos\n\
    RecFormat2=mp4\n\
    RecEncoder=obs_x264\n\
    RecRescale=false\n\
    RecTracks=1\n\
    FLVTrack=1\n\
    AudioEncoder=ffmpeg_aac\n\
    RecAudioEncoder=ffmpeg_aac\n\
    Track1Bitrate=160\n\
    \n\
    [SimpleOutput]\n\
    FilePath=@HOME@/Videos\n\
    RecFormat2=mp4\n\
    RecQuality=Small\n\
    RecEncoder=x264\n\
    StreamEncoder=x264\n\
    Preset=veryfast\n\
    VBitrate=6000\n\
    ABitrate=160\n\
    \n\
    [Video]\n\
    BaseCX=@BASECX@\n\
    BaseCY=@BASECY@\n\
    OutputCX=@OUTCX@\n\
    OutputCY=@OUTCY@\n\
    FPSType=0\n\
    FPSCommon=30\n\
    ScaleType=lanczos\n\
    ColorFormat=NV12\n\
    ColorSpace=709\n\
    ColorRange=Partial\n\
    \n\
    [Audio]\n\
    SampleRate=48000\n\
    ChannelSetup=Stereo\n\
    \n\
    [Hotkeys]\n\
    OBSBasic.StartRecording={\"bindings\":[{\"key\":\"OBS_KEY_F9\"}]}\n\
    OBSBasic.StopRecording={\"bindings\":[{\"key\":\"OBS_KEY_F10\"}]}\n";

/// x264 трансляции (расширенный режим, 25.11): CBR 6000 кбит/с — 1080p30/60
/// для Twitch/YouTube, `veryfast`, ключевой кадр раз в 2 с, потоков —
/// `@X264T@`.
const STREAM_ENCODER_JSON: &str = r#"{"rate_control": "CBR", "bitrate": 6000, "keyint_sec": 2, "preset": "veryfast", "profile": "high", "tune": "", "x264opts": "threads=@X264T@"}"#;

/// x264 записи (25.11): CRF 21, `veryfast`, потоков — `@X264T@`.
const RECORD_ENCODER_JSON: &str = r#"{"rate_control": "CRF", "crf": 21, "keyint_sec": 2, "preset": "veryfast", "profile": "high", "tune": "", "x264opts": "threads=@X264T@"}"#;

/// Коллекция сцен «FossAi»: одна сцена с захватом экрана FossAi (основной
/// монитор — `monitor_id` не задан, модуль берёт основной; с указателем;
/// кадров — как у видео OBS) и звуком рабочего стола — ALSA, устройство `default`
/// (`share/alsa/fossai.conf`).
const SCENES_JSON: &str = r#"{
    "name": "FossAi",
    "current_scene": "Сцена",
    "current_program_scene": "Сцена",
    "scene_order": [{"name": "Сцена"}],
    "current_transition": "Затухание",
    "transition_duration": 300,
    "transitions": [],
    "groups": [],
    "quick_transitions": [],
    "modules": {},
    "sources": [
        {
            "id": "scene",
            "versioned_id": "scene",
            "name": "Сцена",
            "uuid": "6f1a1d2e-0b7a-4c1e-9a10-f05a10000001",
            "settings": {
                "id_counter": 2,
                "custom_size": false,
                "items": [
                    {"name": "Экран", "source_uuid": "6f1a1d2e-0b7a-4c1e-9a10-f05a10000002", "visible": true, "locked": false,
                     "rot": 0.0, "pos": {"x": 0.0, "y": 0.0}, "scale": {"x": 1.0, "y": 1.0}, "align": 5,
                     "bounds_type": 0, "bounds_align": 0, "bounds": {"x": 0.0, "y": 0.0}, "crop_left": 0, "crop_top": 0,
                     "crop_right": 0, "crop_bottom": 0, "id": 1, "group_item_backup": false},
                    {"name": "Звук рабочего стола", "source_uuid": "6f1a1d2e-0b7a-4c1e-9a10-f05a10000003", "visible": true,
                     "locked": false, "rot": 0.0, "pos": {"x": 0.0, "y": 0.0}, "scale": {"x": 1.0, "y": 1.0}, "align": 5,
                     "bounds_type": 0, "bounds_align": 0, "bounds": {"x": 0.0, "y": 0.0}, "crop_left": 0, "crop_top": 0,
                     "crop_right": 0, "crop_bottom": 0, "id": 2, "group_item_backup": false}
                ]
            }
        },
        {
            "id": "fossai_screen_capture",
            "versioned_id": "fossai_screen_capture",
            "name": "Экран",
            "uuid": "6f1a1d2e-0b7a-4c1e-9a10-f05a10000002",
            "settings": {"show_cursor": true}
        },
        {
            "id": "alsa_input_capture",
            "versioned_id": "alsa_input_capture",
            "name": "Звук рабочего стола",
            "uuid": "6f1a1d2e-0b7a-4c1e-9a10-f05a10000003",
            "settings": {"device_id": "default", "rate": 48000}
        }
    ]
}
"#;

/// Коллекция сцен «FossAi-2-monitors» (26.11, стримерам с двумя мониторами):
/// два источника «Захват экрана FossAi» — монитор 1 и монитор 2 (`monitor` —
/// место в списке, основной первым: годится на любом ПК), каждый — свой
/// захват ядра в родном размере монитора, на холсте основного монитора
/// рядом, по половине. Выбрать: «Коллекция сцен» в OBS или
/// `fossai-obs --collection FossAi-2-monitors` (сценарий `obs-monitors`).
const SCENES_2MON_JSON: &str = r#"{
    "name": "FossAi-2-monitors",
    "current_scene": "Два монитора",
    "current_program_scene": "Два монитора",
    "scene_order": [{"name": "Два монитора"}],
    "current_transition": "Затухание",
    "transition_duration": 300,
    "transitions": [],
    "groups": [],
    "quick_transitions": [],
    "modules": {},
    "sources": [
        {
            "id": "scene",
            "versioned_id": "scene",
            "name": "Два монитора",
            "uuid": "6f1a1d2e-0b7a-4c1e-9a10-f05a10000011",
            "settings": {
                "id_counter": 3,
                "custom_size": false,
                "items": [
                    {"name": "Монитор 1", "source_uuid": "6f1a1d2e-0b7a-4c1e-9a10-f05a10000012", "visible": true, "locked": false,
                     "rot": 0.0, "pos": {"x": 0.0, "y": 0.0}, "scale": {"x": 0.5, "y": 0.5}, "align": 5,
                     "bounds_type": 0, "bounds_align": 0, "bounds": {"x": 0.0, "y": 0.0}, "crop_left": 0, "crop_top": 0,
                     "crop_right": 0, "crop_bottom": 0, "id": 1, "group_item_backup": false},
                    {"name": "Монитор 2", "source_uuid": "6f1a1d2e-0b7a-4c1e-9a10-f05a10000013", "visible": true, "locked": false,
                     "rot": 0.0, "pos": {"x": 640.0, "y": 0.0}, "scale": {"x": 0.5, "y": 0.5}, "align": 5,
                     "bounds_type": 0, "bounds_align": 0, "bounds": {"x": 0.0, "y": 0.0}, "crop_left": 0, "crop_top": 0,
                     "crop_right": 0, "crop_bottom": 0, "id": 2, "group_item_backup": false},
                    {"name": "Звук рабочего стола", "source_uuid": "6f1a1d2e-0b7a-4c1e-9a10-f05a10000014", "visible": true,
                     "locked": false, "rot": 0.0, "pos": {"x": 0.0, "y": 0.0}, "scale": {"x": 1.0, "y": 1.0}, "align": 5,
                     "bounds_type": 0, "bounds_align": 0, "bounds": {"x": 0.0, "y": 0.0}, "crop_left": 0, "crop_top": 0,
                     "crop_right": 0, "crop_bottom": 0, "id": 3, "group_item_backup": false}
                ]
            }
        },
        {
            "id": "fossai_screen_capture",
            "versioned_id": "fossai_screen_capture",
            "name": "Монитор 1",
            "uuid": "6f1a1d2e-0b7a-4c1e-9a10-f05a10000012",
            "settings": {"monitor": 0, "show_cursor": true}
        },
        {
            "id": "fossai_screen_capture",
            "versioned_id": "fossai_screen_capture",
            "name": "Монитор 2",
            "uuid": "6f1a1d2e-0b7a-4c1e-9a10-f05a10000013",
            "settings": {"monitor": 1, "show_cursor": true}
        },
        {
            "id": "alsa_input_capture",
            "versioned_id": "alsa_input_capture",
            "name": "Звук рабочего стола",
            "uuid": "6f1a1d2e-0b7a-4c1e-9a10-f05a10000014",
            "settings": {"device_id": "default", "rate": 48000}
        }
    ]
}
"#;

/// Запуск: окружение и `exec` OBS.
const SCRIPT: &str = r#"#!/bin/bash
# OBS Studio (23.11.2026): Debian 13 obs-studio через слой Linux; окно - Qt6
# на сервере Wayland ядра, OpenGL - EGL Mesa (llvmpipe), звук - ALSA ядра.
P=/opt/obs
export HOME=${HOME:-/root}
export TMPDIR=${TMPDIR:-$HOME}
export XDG_CONFIG_HOME=${XDG_CONFIG_HOME:-$HOME/.config}
export XDG_RUNTIME_DIR=/run/user/0
export WAYLAND_DISPLAY=wayland-0
unset DISPLAY
# Своя glibc (2.41) первой; программы bash и coreutils корня Debian 12 ниже
# зовутся с пустым LD_LIBRARY_PATH.
export LD_LIBRARY_PATH=$P/glibc:$P/lib
export QT_PLUGIN_PATH=$P/lib/qt6/plugins
export QT_QPA_PLATFORM=${QT_QPA_PLATFORM:-wayland-egl}
export QT_WAYLAND_SHELL_INTEGRATION=xdg-shell
export QT_WAYLAND_DECORATION=bradient
export FONTCONFIG_FILE=$P/fonts/fonts.conf
# Раскладки - свои.
export XKB_CONFIG_ROOT=$P/share/X11/xkb
# Шины D-Bus нет: адрес в никуда, Qt и OBS это переживают.
export DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/0/bus
# OpenGL на процессоре (Mesa trixie, llvmpipe) через EGL glvnd.
export __EGL_VENDOR_LIBRARY_FILENAMES=$P/share/glvnd/egl_vendor.d/50_mesa.json
export LIBGL_DRIVERS_PATH=$P/lib/dri
export LIBGL_ALWAYS_SOFTWARE=${LIBGL_ALWAYS_SOFTWARE:-1}
export GALLIUM_DRIVER=${GALLIUM_DRIVER:-llvmpipe}
export MESA_SHADER_CACHE_DIR=${MESA_SHADER_CACHE_DIR:-$HOME/.cache/fossai-obs/mesa}
# Звук: libasound со своими настройками (ALSA ядра).
export ALSA_CONFIG_DIR=$P/share/alsa
export ALSA_CONFIG_PATH=$P/share/alsa/alsa.conf:$P/share/alsa/fossai.conf
# `--env=ИМЯ=ЗНАЧЕНИЕ` первыми - своя переменная (у `run` окружения нет).
while case "$1" in --env=*) true ;; *) false ;; esac; do export "${1#--env=}"; shift; done
C=$XDG_CONFIG_HOME/obs-studio
L=$HOME/.cache/fossai-obs
LD_LIBRARY_PATH= mkdir -p "$L" "$MESA_SHADER_CACHE_DIR"
# Строки запуска - в терминал и в ~/.cache/fossai-obs/launch.log (из «Пуска»
# терминала нет).
say() { echo "fossai-obs: $*"; echo "$(LD_LIBRARY_PATH= date '+%F %T') $*" >> "$L/launch.log"; [ -w /dev/kmsg ] && echo "fossai-obs: $*" > /dev/kmsg; }
# OpenGL (25.11, Роман: видеокарта по умолчанию, но только с откатом): есть
# узел рендера (/dev/dri/renderD128 - NVIDIA через узел nouveau ядра) -
# zink (OpenGL поверх Vulkan) на NVK, с проверкой до запуска (eglinfo должен
# назвать zink на видеокарте, не llvmpipe) и после (журнал OBS должен дойти
# до «OpenGL loaded successfully»); иначе - llvmpipe на процессоре.
# `--gpu`/`--cpu` или строка `gpu=1|0|auto` в ~/.config/fossai-obs.conf.
GPU=auto
[ -r "$XDG_CONFIG_HOME/fossai-obs.conf" ] && while IFS='=' read -r k v; do [ "$k" = gpu ] && GPU=$v; done < "$XDG_CONFIG_HOME/fossai-obs.conf"
case "$1" in --gpu) GPU=1; shift ;; --cpu) GPU=0; shift ;; esac
[ "$GPU" = 0 ] && say "OpenGL on the CPU as asked (--cpu or gpu=0)"
cpu_gl() {
    export LIBGL_ALWAYS_SOFTWARE=1 GALLIUM_DRIVER=llvmpipe
    unset MESA_LOADER_DRIVER_OVERRIDE VK_DRIVER_FILES VK_ICD_FILENAMES
    # 25.11 (hw-log19): llvmpipe took all 16 CPUs (16 threads, 9-15 % idle)
    # under a game; 4 threads draw OBS's canvas and leave the game its CPUs.
    export LP_NUM_THREADS=${LP_NUM_THREADS:-4}
    say "OpenGL on the CPU (llvmpipe, $LP_NUM_THREADS threads)"
}
gpu_gl() {
    export LIBGL_ALWAYS_SOFTWARE=0 GALLIUM_DRIVER=zink MESA_LOADER_DRIVER_OVERRIDE=zink
    export VK_DRIVER_FILES=$P/share/vulkan/icd.d/nouveau_icd.x86_64.json
    export VK_ICD_FILENAMES=$VK_DRIVER_FILES
}
if [ "$GPU" != 0 ]; then
    if [ ! -e /dev/dri/renderD128 ]; then
        say "no /dev/dri/renderD128 - OpenGL stays on llvmpipe"
        GPU=0
    else
        gpu_gl
        # Проба: eglinfo на Wayland должен назвать zink (не llvmpipe) и не
        # зависнуть; иначе - процессор.
        # 25.11 (hw-log19): `timeout` and `env` are Debian 12 coreutils - run
        # with the package glibc 2.41 in LD_LIBRARY_PATH their loader exited
        # 127 (eglinfo never ran, every `--gpu` start fell back to llvmpipe:
        # OBS ate 14 of 16 CPUs under GOH and Uot). They get an empty path,
        # eglinfo gets the package one through `env`.
        T=; LD_LIBRARY_PATH= command -v timeout >/dev/null 2>&1 && T="timeout 45"
        out=$( { export LOCPATH=$P/locale LC_ALL=C.UTF-8; } 2>/dev/null; LD_LIBRARY_PATH= $T env LD_LIBRARY_PATH="$LD_LIBRARY_PATH" $P/bin/eglinfo 2>&1 )
        # zink on lavapipe/llvmpipe is the CPU again - not a GPU.
        r=$(printf '%s\n' "$out" | while IFS= read -r line; do case "$line" in *renderer*zink*llvmpipe*|*renderer*zink*lavapipe*) ;; *renderer*zink*) echo "${line#*: }"; break ;; esac; done)
        if [ -n "$r" ]; then
            say "OpenGL on the GPU: $r"
            GPU=1
        else
            say "GPU probe failed (eglinfo found no zink renderer on the GPU) - OpenGL on llvmpipe"
            printf '%s\n' "$out" > "$L/gpu-probe.txt"
            GPU=0
        fi
    fi
fi
[ "$GPU" = 1 ] || cpu_gl
# `--run=ПРОГРАММА`: своя программа пакета с тем же окружением (eglinfo,
# es2gears_wayland, ffprobe).
case "$1" in --run=*) r=${1#--run=}; shift; { export LOCPATH=$P/locale LC_ALL=C.UTF-8; } 2>/dev/null; exec $P/bin/$r "$@" ;; esac
# `--probe-last`: ffprobe последней записи в ~/Videos (проверка записи в
# сценарии `obs` и руками из терминала).
if [ "$1" = --probe-last ]; then
    f=
    for m in "$HOME"/Videos/*.mp4 "$HOME"/Videos/*.mkv; do [ -e "$m" ] && { [ -z "$f" ] || [ "$m" -nt "$f" ]; } && f=$m; done
    [ -z "$f" ] && { echo "fossai-obs: no recording in $HOME/Videos"; exit 1; }
    s=$(LD_LIBRARY_PATH= stat -c %s "$f")
    echo "fossai-obs: probe $f ($s bytes)"
    exec $P/bin/ffprobe -hide_banner -v error -show_entries format=format_name,duration,size,bit_rate:stream=codec_type,codec_name,profile,width,height,pix_fmt,color_space,color_range,r_frame_rate,bit_rate,sample_rate,channels -of compact "$f"
fi
# Основной монитор (первая строка `/dev/fossai-capture`): холст OBS - его
# размер, вывод - он же до 1920x1080, крупнее - к высоте 1080.
BW=1920 BH=1080 MON=
if [ -r /dev/fossai-capture ]; then
    while IFS=$'\t' read -r x y w h p n d; do
        case "$w$h" in ''|*[!0-9]*) continue ;; esac
        BW=$w BH=$h MON=$n; break
    done < /dev/fossai-capture
fi
if [ $((BW * BH)) -le $((1920 * 1080)) ]; then OW=$BW OH=$BH; else OH=1080; OW=$(( (BW * 1080 / BH + 1) / 2 * 2 )); fi
# x264 (25.11): NVENC нет (аппаратное кодирование - Vulkan Video на NVK -
# TODO), кодирует процессор: потоков x264 - четверть ЦП, от 2 до 4, игре
# остальное.
N=0
while read -r k _; do [ "$k" = processor ] && N=$((N + 1)); done < /proc/cpuinfo
X264T=$((N / 4)); [ "$X264T" -lt 2 ] && X264T=2; [ "$X264T" -gt 4 ] && X264T=4
# Первый запуск: свои настройки (русский, профиль и сцена «FossAi»). Профиль
# до версии 2 (23.11 - без холста, 24.11 - простой режим без потоков x264)
# заменяется один раз.
f=$C/basic/profiles/FossAi/basic.ini
if [ ! -e "$C/global.ini" ] || { [ -e "$f" ] && ! LD_LIBRARY_PATH= grep -q '^FossAiProfile=2' "$f"; }; then
    LD_LIBRARY_PATH= mkdir -p "$C/basic/profiles/FossAi" "$HOME/Videos"
    LD_LIBRARY_PATH= cp -rn $P/share/fossai/obs-studio/. "$C/"
    for g in basic.ini streamEncoder.json recordEncoder.json; do
        LD_LIBRARY_PATH= cp $P/share/fossai/obs-studio/basic/profiles/FossAi/$g "$C/basic/profiles/FossAi/$g"
        t=$(<"$C/basic/profiles/FossAi/$g"); t=${t//@HOME@/$HOME}; t=${t//@BASECX@/$BW}; t=${t//@BASECY@/$BH}; t=${t//@OUTCX@/$OW}; t=${t//@OUTCY@/$OH}; t=${t//@X264T@/$X264T}
        printf '%s\n' "$t" > "$C/basic/profiles/FossAi/$g"
    done
    say "profile FossAi: canvas ${BW}x${BH} (monitor ${MON:-?}), output ${OW}x${OH}, x264 veryfast with $X264T threads of $N CPUs"
fi
# Коллекции сцен пакета, которых ещё нет (26.11: «FossAi-2-monitors» у тех,
# кто запускал OBS раньше), - без замены своих.
LD_LIBRARY_PATH= mkdir -p "$C/basic/scenes"
LD_LIBRARY_PATH= cp -n $P/share/fossai/obs-studio/basic/scenes/*.json "$C/basic/scenes/" 2>/dev/null
cd $P/bin
# Qt требует UTF-8: локаль C.UTF-8 glibc 2.41 - своя (LOCPATH), только для
# программ пакета (bash и coreutils Debian 12 выше её не видят; bash сам
# ругается, что такой локали не знает, - молча).
{ export LOCPATH=$P/locale LC_ALL=C.UTF-8 LANG=C.UTF-8; } 2>/dev/null
# The newest OBS log into $NEWEST (no subshell: every fork is a line in the
# kernel log).
newest() { local m; NEWEST=; for m in "$C"/logs/*.txt; do [ -e "$m" ] && { [ -z "$NEWEST" ] || [ "$m" -nt "$NEWEST" ]; } && NEWEST=$m; done; }
newest; before=$NEWEST
# 25.11: what OBS renders on and how the screen reaches it - from OBS's log
# into launch.log and the kernel log (Roman sends that one): `OBS: Loading
# up OpenGL on adapter ...`, `OBS: [fossai-capture] frames: ...` (the
# encoder lines too if recording starts early). Every 15 s for 2 minutes.
relay() {
    local i log line n shown=0
    for ((i = 0; i < 8; i++)); do
        LD_LIBRARY_PATH= sleep 15
        newest; log=$NEWEST
        { [ -z "$log" ] || [ "$log" = "$1" ]; } && continue
        n=0
        while IFS= read -r line; do
            case "$line" in
                *"Loading up OpenGL on adapter"*|*"[fossai-capture] frames:"*|*"[fossai-capture] dma-buf"*|*"[fossai-capture] game buffer"*|*"[x264 encoder"*"preset:"*|*"[x264 encoder"*"threads"*)
                    n=$((n + 1)); [ $n -gt $shown ] && { shown=$n; say "OBS: ${line#*: }"; } ;;
            esac
        done < "$log"
    done
}
relay "$before" &
# Ниже игры (25.11): OBS - через `nice` (coreutils Debian 12 - с пустым
# LD_LIBRARY_PATH, OBS - с пакетным через `env`). FOSSAI_OBS_NICE=0 - без.
NICE=${FOSSAI_OBS_NICE:-5}
if [ "$NICE" != 0 ] && LD_LIBRARY_PATH= command -v nice >/dev/null 2>&1; then
    set -- env LD_LIBRARY_PATH="$LD_LIBRARY_PATH" $P/bin/obs --disable-shutdown-check "$@"
    RUN="LD_LIBRARY_PATH= nice -n $NICE"
else
    set -- $P/bin/obs --disable-shutdown-check "$@"
    RUN=
fi
# `--disable-shutdown-check`: «безопасный режим» после аварии не спрашивать.
if [ "$GPU" != 1 ]; then
    if [ -n "$RUN" ]; then LD_LIBRARY_PATH= exec nice -n "$NICE" "$@"; else exec "$@"; fi
fi
# Видеокарта: OBS - дочерний; если его журнал не дошёл до «OpenGL loaded
# successfully» (zink/NVK не поднялся, «Failed to initialize video»), OBS
# запускается снова на llvmpipe.
if [ -n "$RUN" ]; then LD_LIBRARY_PATH= nice -n "$NICE" "$@"; else "$@"; fi
rc=$?
newest; log=$NEWEST
[ "$log" = "$before" ] && log=
# Без журнала - по коду выхода (закрыли сами - 0, не откатываться).
ok=$([ "$rc" = 0 ] && echo 1 || echo 0)
if [ -n "$log" ]; then
    ok=0
    while IFS= read -r line; do
        case "$line" in *"Failed to initialize video"*) ok=0; break ;; *"OpenGL loaded successfully"*) ok=1 ;; esac
    done < "$log"
fi
[ "$ok" = 1 ] && exit $rc
say "OBS did not start OpenGL on the GPU (exit $rc, log ${log:-none}) - restarting on llvmpipe"
cpu_gl
relay "$log" &
if [ -n "$RUN" ]; then LD_LIBRARY_PATH= exec nice -n "$NICE" "$@"; else exec "$@"; fi
"#;

/// Скачать пакеты и собрать `obs-30.fpk`.
pub fn build(arch: Arch) -> Result<()> {
    if arch != Arch::X86_64 {
        return Ok(());
    }
    let cache = sdk::root().join("thirdparty/debian");
    fs::create_dir_all(&cache)?;
    let mut base: BTreeMap<String, Node> = BTreeMap::new();
    unpack(&cache, &crate::debian::PACKAGES, &mut base)?;
    let mut deb: BTreeMap<String, Node> = BTreeMap::new();
    unpack(&cache, &PACKAGES, &mut deb)?;

    let mut files: Vec<(String, Vec<u8>, u32)> = Vec::new();
    // Своя glibc и её загрузчик под коротким именем.
    for name in GLIBC_LIBS {
        let Some((data, mode)) = follow(&deb, &format!("{DIR}{name}"), 0) else {
            bail!("в libc6 trixie нет {name}");
        };
        files.push((format!("glibc/{name}"), data.clone(), mode));
        if name == "ld-linux-x86-64.so.2" {
            files.push((String::from("glibc/ld.so"), data.clone(), 0o755));
        }
    }
    // Mesa (EGL, llvmpipe), libstdc++ 14, ALSA - как у Proton.
    crate::mesa::add(&cache, &mut files, patch_interp)?;
    // Из Mesa OBS нужен OpenGL (EGL): llvmpipe, а на NVIDIA — zink поверх NVK
    // (`fossai-obs --gpu`). lavapipe и пробы Vulkan — только у Proton.
    files.retain(|(path, _, _)| {
        !(path.starts_with("lib/libvulkan_lvp") || path.ends_with("/lvp_icd.x86_64.json") || ["bin/vkcube", "bin/vkgears", "bin/vulkaninfo", "bin/vkprobe"].contains(&path.as_str()))
    });
    for (path, data, _) in files.iter_mut() {
        if path.starts_with("share/vulkan/icd.d/") {
            *data = String::from_utf8_lossy(data).replace("/opt/proton/lib/", "/opt/obs/lib/").into_bytes();
        }
    }

    // Что разобрать по `DT_NEEDED` и кто это потребовал.
    let mut todo: Vec<(String, String)> = Vec::new();
    let add_elf = |files: &mut Vec<(String, Vec<u8>, u32)>, todo: &mut Vec<(String, String)>, from: &str, to: String, interp: bool| -> Result<()> {
        let Some((data, mode)) = follow(&deb, from, 0) else {
            bail!("в пакетах нет {from}");
        };
        let mut data = data.clone();
        if interp && !patch_interp(&mut data) {
            bail!("{from}: нет загрузчика Debian");
        }
        if to.ends_with("obs-ffmpeg.so") && !patch_string(&mut data, MUX_OLD, MUX_NEW) {
            bail!("{from}: нет пути obs-ffmpeg-mux");
        }
        let by = to.rsplit('/').next().unwrap_or(&to).to_string();
        todo.extend(needed(&data).with_context(|| format!("{from}: не разобран ELF"))?.into_iter().map(|name| (name, by.clone())));
        files.push((to, data, mode));
        Ok(())
    };
    add_elf(&mut files, &mut todo, "usr/bin/obs", String::from("bin/obs"), true)?;
    add_elf(&mut files, &mut todo, "usr/bin/ffprobe", String::from("bin/ffprobe"), true)?;
    for plugin in OBS_PLUGINS {
        let from = format!("{DIR}obs-plugins/{plugin}");
        if plugin == "obs-ffmpeg/obs-ffmpeg-mux" {
            add_elf(&mut files, &mut todo, &from, String::from("lib/obs-ffmpeg-mux"), true)?;
        } else {
            add_elf(&mut files, &mut todo, &from, format!("lib/x86_64-linux-gnu/obs-plugins/{plugin}"), false)?;
        }
    }
    // Свой модуль: захват экрана и окна через `/dev/fossai-capture`.
    let plugin = build_plugin(&cache, &deb)?;
    todo.extend(needed(&plugin)?.into_iter().map(|name| (name, String::from(FOSSAI_PLUGIN.1))));
    files.push((format!("lib/x86_64-linux-gnu/obs-plugins/{}", FOSSAI_PLUGIN.1), plugin, 0o755));
    // Графика libobs открывается сама (`dlopen("libobs-opengl.so.1")`).
    add_elf(&mut files, &mut todo, &format!("{DIR}libobs-opengl.so.1"), String::from("lib/libobs-opengl.so.1"), false)?;
    for plugin in QT_PLUGINS {
        add_elf(&mut files, &mut todo, &format!("{DIR}qt6/plugins/{plugin}"), format!("lib/qt6/plugins/{plugin}"), false)?;
    }
    // Библиотеки Mesa (их зависимости тоже должны найтись).
    let mesa_libs: Vec<(String, Vec<u8>)> =
        files.iter().filter(|(path, _, _)| path.starts_with("lib/") && path.contains(".so")).map(|(path, data, _)| (path.clone(), data.clone())).collect();
    for (path, data) in &mesa_libs {
        if let Ok(list) = needed(data) {
            todo.extend(list.into_iter().map(|name| (name, path.clone())));
        }
    }

    let have: BTreeSet<String> = files.iter().filter_map(|(path, _, _)| path.strip_prefix("lib/").map(String::from)).collect();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut missing: Vec<String> = Vec::new();
    let (mut from_deb, mut from_base) = (0, 0);
    while let Some((name, by)) = todo.pop() {
        if !seen.insert(name.clone()) || have.contains(&name) || GLIBC_LIBS.contains(&name.as_str()) {
            continue;
        }
        // Сначала обычные каталоги, потом - где угодно (`blas/libblas.so.3`,
        // `pulseaudio/libpulsecommon`: их ищут по альтернативам и RUNPATH).
        let suffix = format!("/{name}");
        let found = LIB_DIRS.iter().find_map(|dir| follow(&deb, &format!("{dir}/{name}"), 0)).or_else(|| {
            deb.keys().find(|path| path.ends_with(&suffix) && !path.contains("/qt6/plugins/")).and_then(|path| follow(&deb, path, 0))
        });
        if let Some((data, mode)) = found {
            todo.extend(needed(data).with_context(|| format!("{name}: не разобран ELF"))?.into_iter().map(|next| (next, name.clone())));
            files.push((format!("lib/{name}"), data.clone(), mode));
            from_deb += 1;
            continue;
        }
        if LIB_DIRS.iter().any(|dir| follow(&base, &format!("{dir}/{name}"), 0).is_some()) {
            from_base += 1;
            continue;
        }
        missing.push(format!("{name} (нужна {by})"));
    }
    say!("obs: библиотек из пакетов trixie {from_deb}, из корня Debian 12 {from_base}");
    if !missing.is_empty() {
        bail!("obs: не нашлись библиотеки: {}", missing.join(", "));
    }

    // Данные: libobs (эффекты), интерфейс (темы, переводы), модули.
    let wanted_data: Vec<String> = OBS_PLUGINS
        .iter()
        .filter(|name| name.ends_with(".so"))
        .map(|name| format!("usr/share/obs/obs-plugins/{}/", name.trim_end_matches(".so")))
        .collect();
    let mut data_files = 0;
    for path in deb.keys() {
        let Some(rest) = path.strip_prefix("usr/share/obs/") else { continue };
        let keep = rest.starts_with("libobs/") || rest.starts_with("obs-studio/") || wanted_data.iter().any(|dir| path.starts_with(dir.as_str()));
        if !keep || !matches!(deb.get(path), Some(Node::File { .. }) | Some(Node::Link(_))) {
            continue;
        }
        if let Some((data, mode)) = follow(&deb, path, 0) {
            files.push((format!("share/obs/{rest}"), data.clone(), mode));
            data_files += 1;
        }
    }
    say!("obs: файлов данных {data_files}");
    for path in deb.keys() {
        if let Some(name) = path.strip_prefix(FONT_DIR) {
            if let Some((data, mode)) = follow(&deb, path, 0) {
                files.push((format!("fonts/{name}"), data.clone(), mode));
            }
        }
    }
    files.push((String::from("fonts/fonts.conf"), FONTS_CONF.as_bytes().to_vec(), 0o644));
    for path in deb.keys() {
        if let Some(rest) = path.strip_prefix("usr/lib/locale/C.utf8/") {
            if let Some((data, mode)) = follow(&deb, path, 0) {
                files.push((format!("locale/C.utf8/{rest}"), data.clone(), mode));
            }
        }
    }
    for path in deb.keys() {
        if let Some(rest) = path.strip_prefix("usr/share/X11/xkb/") {
            if let Some((data, mode)) = follow(&deb, path, 0) {
                files.push((format!("share/X11/xkb/{rest}"), data.clone(), mode));
            }
        }
    }
    for (path, text) in [
        ("global.ini", GLOBAL_INI),
        ("basic/profiles/FossAi/basic.ini", PROFILE_INI),
        ("basic/profiles/FossAi/streamEncoder.json", STREAM_ENCODER_JSON),
        ("basic/profiles/FossAi/recordEncoder.json", RECORD_ENCODER_JSON),
        ("basic/scenes/FossAi.json", SCENES_JSON),
        ("basic/scenes/FossAi-2-monitors.json", SCENES_2MON_JSON),
    ] {
        files.push((format!("share/fossai/obs-studio/{path}"), text.as_bytes().to_vec(), 0o644));
    }
    files.push((String::from("bin/fossai-obs"), SCRIPT.as_bytes().to_vec(), 0o755));
    files.push((String::from("bin/obs-ffmpeg-mux"), MUX_SCRIPT.as_bytes().to_vec(), 0o755));

    let icon = ["256x256", "128x128", "512x512"].iter().find_map(|size| {
        follow(&deb, &format!("usr/share/icons/hicolor/{size}/apps/com.obsproject.Studio.png"), 0).map(|(data, _)| data.clone())
    });
    let icon_field = match icon.as_deref().map(crate::fimg::from_png) {
        Some(Ok(fimg)) => {
            files.push((String::from("icon.fimg"), fimg, 0o644));
            "/opt/obs/icon.fimg"
        }
        _ => {
            say!("obs: значок не разобран, будет значок набора");
            "video"
        }
    };

    let mut builder = Builder::new(Kind::Package, "obs", "30");
    builder.field("summary", "OBS Studio 30.2 from Debian 13: Qt6 on the kernel Wayland server, OpenGL through zink on NVK (GPU) or Mesa llvmpipe, x264/FFmpeg, FossAi screen capture with dma-buf frames");
    builder.field("about", "OBS Studio: запись и трансляция экрана");
    builder.field("caption", "OBS Studio");
    builder.field("start", "/opt/debian/bin/bash /opt/obs/bin/fossai-obs");
    // Вторая строка «Пуска»: 24.11 — видеокарта (`--gpu`); с 25.11
    // видеокарта — по умолчанию (с откатом), а вторая строка — только
    // процессор (`--cpu`), на случай, если откат не спас.
    builder.field("start2", "/opt/debian/bin/bash /opt/obs/bin/fossai-obs --cpu");
    builder.field("caption2", "OBS Studio (процессор)");
    builder.field("about2", "OBS Studio: OpenGL на процессоре (llvmpipe), без видеокарты");
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
    say!("obs: {} — {} файлов, {} МиБ", out.display(), files.len(), bytes / (1024 * 1024));
    Ok(())
}

/// Собрать `obs-fossai.so` clang-ом: sysroot glibc Debian 12, заголовки
/// libobs-dev, связь с `libobs.so.0` пакета. `simde/x86/sse2.h` (его тянет
/// `util/sse-intrin.h`) на x86-64 — просто `<emmintrin.h>`: SIMDe там
/// сводится к родным SSE2, и ставить libsimde-dev ради этого незачем.
fn build_plugin(cache: &std::path::Path, deb: &BTreeMap<String, Node>) -> Result<Vec<u8>> {
    let mut dev: BTreeMap<String, Node> = BTreeMap::new();
    unpack(cache, &DEV_PACKAGES, &mut dev)?;
    let work = sdk::root().join("linux/obs");
    let _ = fs::remove_dir_all(&work);
    let include = work.join("include");
    for (path, node) in &dev {
        if let (Some(rest), Node::File { data, .. }) = (path.strip_prefix("usr/include/obs/"), node) {
            let target = include.join(rest);
            fs::create_dir_all(target.parent().unwrap())?;
            fs::write(&target, data)?;
        }
    }
    let shim = work.join("shim/simde/x86");
    fs::create_dir_all(&shim)?;
    fs::write(shim.join("sse2.h"), "#include <emmintrin.h>\n")?;
    let Some((libobs, _)) = follow(deb, &format!("{DIR}libobs.so.0"), 0) else {
        bail!("в пакетах нет libobs.so.0");
    };
    fs::write(work.join("libobs.so.0"), libobs)?;
    let sysroot = crate::debian::glibc_sysroot()?;
    let lib = sysroot.join("usr/lib/x86_64-linux-gnu");
    let output = work.join(FOSSAI_PLUGIN.1);
    let mut cmd = std::process::Command::new(cbuild::llvm_tool("clang")?);
    cmd.arg("--target=x86_64-linux-gnu")
        .arg(format!("--sysroot={}", sysroot.display()))
        .args(["-O2", "-Wall", "-Wno-unused-parameter", "-fPIC", "-shared", "-fuse-ld=lld", "-nostdlib"])
        .arg(format!("-I{}", work.join("shim").display()))
        .arg(format!("-I{}", include.display()))
        .arg(cbuild::libc_dir().join("linux").join(FOSSAI_PLUGIN.0))
        .arg(work.join("libobs.so.0"))
        .arg(format!("-L{}", lib.display()))
        .arg(format!("-L{}", sysroot.join("lib/x86_64-linux-gnu").display()))
        .arg("-lc")
        .arg("-o")
        .arg(&output);
    say!("> clang (glibc) {}", FOSSAI_PLUGIN.0);
    let status = cmd.status().context("не удалось запустить clang")?;
    if !status.success() {
        bail!("{} не собрался ({status})", FOSSAI_PLUGIN.0);
    }
    Ok(fs::read(&output)?)
}
