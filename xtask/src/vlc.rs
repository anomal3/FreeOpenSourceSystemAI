//! VLC — стандартный видеоплеер FossAi (24.11.2026, Роман: «свой
//! стандартный видеоплеер на основе VLC»): сборка Debian 13 (trixie) `vlc`
//! 3.0.23 через слой Linux ядра — пакетом `vlc-3.0.fpk` (`/opt/vlc` после
//! `pkg install vlc`). Окно — Qt5 через сервер Wayland ядра (`qtwayland5`,
//! платформа `wayland` без EGL: буферы wl_shm), видео — модуль VLC `wl_shm`
//! в подповерхность окна Qt (`wl_subsurface` + `wp_viewport` для масштаба),
//! декодирование — FFmpeg (libavcodec 61) на процессоре, звук — ALSA ядра.
//! Без Mesa и OpenGL: модули вывода видео на GL, VA-API и VDPAU не берутся.
//!
//! # Что берётся
//!
//! Как у OBS (`obs.rs`): не замыкание `Depends`, а то, что **загружается**:
//! цепочка `DT_NEEDED` от `libvlc`, выбранных модулей VLC ([`PLUGINS`],
//! целиком — каталоги [`PLUGIN_DIRS`]) и модулей Qt ([`QT_PLUGINS`]) по
//! пакетам [`PACKAGES`] (снимок 20261020, выписаны тем же обходом по
//! `Contents-amd64`). glibc — своя, 2.41 (`/opt/vlc/glibc`).
//!
//! # Своя программа `vlc`
//!
//! `bin/vlc` Debian не годится: он не работает от root (а программы слоя
//! Linux — uid 0), а модуль Qt собран с X11 и без X-сервера не открывается
//! (`HasX11()`), а с ним запускает Qt с `-platform xcb`. Своя `bin/vlc`
//! (`libc/linux/fossai-vlc.c`) делает то же, что `bin/vlc.c` VLC, через
//! `dlopen` libvlc и подменяет `XOpenDisplay` (без `DISPLAY` — пустышка для
//! пробы) и конструктор `QApplication` (`-platform xcb` → `wayland`): символы
//! исполняемого файла важнее символов библиотек.
//!
//! # Раскладка
//!
//! Модули VLC — по `VLC_PLUGIN_PATH` (`lib/vlc/plugins`; свой поиск от
//! `libvlccore` по `/proc/self/maps` у слоя Linux находит путь программы),
//! данные — по `VLC_DATA_PATH` (`share/vlc`), перевод — `share/locale`
//! (`share/vlc/../locale`). Библиотеки — плоско в `lib/`, модули Qt —
//! `lib/qt5/plugins`. Русский интерфейс: локаль `ru_RU.UTF-8` — копия
//! `C.utf8` glibc под этим именем (своих данных локали у неё нет, нужен
//! только язык сообщений gettext).
//!
//! Размер (24.11): пакет около 230 МиБ, из них FFmpeg с кодеками — 120, Qt5
//! с ICU — 60. Включается в носитель только с `FOSSAI_WITH_VLC=1`.

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
pub const PACKAGE_FILE: &str = "vlc-3.0.fpk";

/// Где лежит собранный пакет.
pub fn package_path() -> PathBuf {
    cbuild::games_dir().join(PACKAGE_FILE)
}

/// Включён ли VLC в носитель: пакет в две сотни мегабайт — только по
/// `FOSSAI_WITH_VLC=1` (как OBS).
pub fn enabled() -> bool {
    std::env::var("FOSSAI_WITH_VLC").is_ok_and(|value| value == "1")
}

/// Пакеты trixie (снимок 20261020): путь в архиве и SHA-256 из `Packages`.
const PACKAGES: [(&str, &str); 171] = [
    ("pool/main/f/fonts-dejavu/fonts-dejavu-core_2.37-8_all.deb", "86635b3d25b3655fc11cb3ecc3af59f0bf19643b02b94f2de48bd10253cdba12"),
    ("pool/main/a/aom/libaom3_3.12.1-1+deb13u1_amd64.deb", "743eec7c94ae30295bc557ebac3dfae948e7390e89a2743c585d56297e9d312e"),
    ("pool/main/a/aribb24/libaribb24-0t64_1.0.3-2.1+b2_amd64.deb", "ae84f3e5f53e304cf0addca456419f996597c9828117f6f99d84d5432c32825f"),
    ("pool/main/a/alsa-lib/libasound2-data_1.2.14-1+deb13u1_all.deb", "04688afdff3769c0f685541daed7b2f6f0cb946799ddf1d5847ddf11fe245559"),
    ("pool/main/a/alsa-lib/libasound2t64_1.2.14-1+deb13u1_amd64.deb", "5495496142d57e5ad6d581ac5456ec3c87f872c109a2bfb815d154b673eebc67"),
    ("pool/main/liba/libass/libass9_0.17.3-1+deb13u1_amd64.deb", "000faddfaa19c7c1631f2d71a9cd1982cd3a88611967debe631440a1c64617b4"),
    ("pool/main/g/gcc-14/libatomic1_14.2.0-19_amd64.deb", "212b399aae2f7299203d261a57e49372e09565a9a5ea971905f94a3960366c05"),
    ("pool/main/f/ffmpeg/libavcodec61_7.1.5-0+deb13u1_amd64.deb", "6c07da16b276827973463bdd22607721df1c2877641c753d2418cf6bf49a0f2e"),
    ("pool/main/f/ffmpeg/libavformat61_7.1.5-0+deb13u1_amd64.deb", "4b580060cebbd33a25589ba71074e3106caf2826bcdb82cf8936b52960f34fb2"),
    ("pool/main/f/ffmpeg/libavutil59_7.1.5-0+deb13u1_amd64.deb", "bb5fe4ea1eef338c8346bf67cc728dac20b94451ad31db391d03d1f2b3fa37c1"),
    ("pool/main/u/util-linux/libblkid1_2.41.5-0+deb13u1_amd64.deb", "81535f3c2c0efc732965907c8749103a0a26377c761622c9ce39b4c92dcde52f"),
    ("pool/main/libb/libbluray/libbluray2_1.3.4-1+b2_amd64.deb", "82f9e58e8dfdfeb57b273111433c335500c26553eb82f2a025e94ed9fdae2c21"),
    ("pool/main/b/brotli/libbrotli1_1.1.0-2+b7_amd64.deb", "0fb79f88db210afbd69282ab9649e525f393ec6950ca34da1a6b359250b8d7db"),
    ("pool/main/libb/libbsd/libbsd0_0.12.2-2_amd64.deb", "e5a85986fa6bec3307ab1bc860736b478b331882bc45e17675a7bdf88eecb43a"),
    ("pool/main/b/bzip2/libbz2-1.0_1.0.8-6_amd64.deb", "cba4cda04244b5e481bb15524bc3c983a7d1b6f330013b9b381706a2fcb65310"),
    ("pool/main/g/glibc/libc-bin_2.41-12+deb13u4_amd64.deb", "dc8c79647fe3a5f37f72a400414f70263d0212f010ff2c09f990e30d91e38d1c"),
    ("pool/main/g/glibc/libc6_2.41-12+deb13u4_amd64.deb", "967aa62605721081c3eb2a17650611a792aa802d76a6511d1840242623d204c9"),
    ("pool/main/c/cairo/libcairo-gobject2_1.18.4-1+b1_amd64.deb", "4fcd432ebe8f93599bd05426f304e2f70409aad271fc6b4b3a2c6243c99c377c"),
    ("pool/main/c/cairo/libcairo2_1.18.4-1+b1_amd64.deb", "01390b049661e4cc8bd39cef78b41c2587d4035619d3b00d6c7846f43d80bbba"),
    ("pool/main/libc/libcap2/libcap2_2.75-10+deb13u1+b3_amd64.deb", "89fc4d34fc7a28ad6f0fcd0c561ab253b9dedf6f77f5a000b47c276c8295bf67"),
    ("pool/main/c/chromaprint/libchromaprint1_1.5.1-7_amd64.deb", "ed8decea8352ae56e80862c4630c83f4f863d4eea653382ed8ae43ffacda60b5"),
    ("pool/main/c/cjson/libcjson1_1.7.18-3.1+deb13u1_amd64.deb", "e10b7fb966251f4f09690cb35f9e8fed6924194ec2d6973910d9e157cda82657"),
    ("pool/main/c/codec2/libcodec2-1.2_1.2.0-3_amd64.deb", "d68a3b754be7aa2d724d01d2079c542fc7cfa53eaa4ac89fbc6e63d950c02bd6"),
    ("pool/main/e/e2fsprogs/libcom-err2_1.47.2-3+b12_amd64.deb", "bd43e020b8fed399db3eebf098111f10fba4c436fff99ccad72cc7e27c9764ec"),
    ("pool/main/libd/libdatrie/libdatrie1_0.2.13-3+b1_amd64.deb", "339eb88f23d72b8f0b7b1aebcefbab6de600d97b88bc6222dd8aad11d2ca9051"),
    ("pool/main/d/dav1d/libdav1d7_1.5.1-1_amd64.deb", "8c64de4465a8ae1ca23e1dada1dfc73d7b431540fd5ae18f21430928576d5b05"),
    ("pool/main/d/dbus/libdbus-1-3_1.16.2-2_amd64.deb", "bbcb711daff7e104b5f80f9b05475b6142674bfe65518a36a56e96a91068a3f5"),
    ("pool/main/d/double-conversion/libdouble-conversion3_3.3.1-1_amd64.deb", "bd43b8c185a78d108548b30da6a695aabb6644ce0dd266892da2d10cfe471067"),
    ("pool/main/libd/libdrm/libdrm2_2.4.124-2_amd64.deb", "fe2276901c7cd7b8079de63072d37fe1cbeb4eb001a3bc1f1d662ad89aa0890e"),
    ("pool/main/libd/libdvbpsi/libdvbpsi10_1.3.3-1+b2_amd64.deb", "112f29b9a7c5fcd8b7f316c879e94fecba6ba6576f8ac9bea858b7e704bff456"),
    ("pool/main/libd/libdvdnav/libdvdnav4_6.1.1-3+b1_amd64.deb", "8368250b65cc26911fd37acdeeed58e9ce5bd390e941f687abecca9d5cd659f0"),
    ("pool/main/libd/libdvdread/libdvdread8t64_6.1.3-2_amd64.deb", "f753a23580c00a2166211607a5511a9c25d75c9ebfc34598b6526637de150fec"),
    ("pool/main/libe/libebml/libebml5_1.4.5-1+b1_amd64.deb", "4e0727c7b54394388e9f5257a40d268ad6c615e9848a675528fc5795d21f97a8"),
    ("pool/main/e/expat/libexpat1_2.8.3-1~deb13u1_amd64.deb", "38abe0e710a07688e9c149d74536e67cfee0364bdb64dd6d644c32a1cfad389f"),
    ("pool/main/libf/libffi/libffi8_3.4.8-2_amd64.deb", "0ebdc340de33333639c3c63874cd4b15ac2e83dfa1ef3053b7eefaf4919f4f68"),
    ("pool/main/f/flac/libflac14_1.5.0+ds-2_amd64.deb", "658b9f77d35638caf75dfaea4f6d5cde196104d5662f0776e23d7cc450bd4cb5"),
    ("pool/main/f/fontconfig/libfontconfig1_2.15.0-2.3_amd64.deb", "7ae91ec59857cc8e375eb2bdd371a4a69daea38f7787d06d474a847867d852b1"),
    ("pool/main/f/freetype/libfreetype6_2.13.3+dfsg-1+deb13u1_amd64.deb", "e4947f3291528f03d574f2b01d5c5fc45c58c47480f32888a5cdf0f231ab584e"),
    ("pool/main/f/fribidi/libfribidi0_1.0.16-1_amd64.deb", "f8c35ec8606d0843bd3cf020c6cfa39ddfe717ec58ed826b5855ba9927eb0d55"),
    ("pool/main/g/gcc-14/libgcc-s1_14.2.0-19_amd64.deb", "3c71917b490d1a17aed43196a2787a256ecf060526cdb20216a74bedc061b150"),
    ("pool/main/g/gdk-pixbuf/libgdk-pixbuf-2.0-0_2.42.12+dfsg-4+deb13u1_amd64.deb", "94150fb955b2d3a9a5e054e31c5a4b048bad4a479207e86cde46fbc975c838b4"),
    ("pool/main/libg/libglvnd/libgl1_1.7.0-1+b2_amd64.deb", "87fa2f6e5abaed4ed385fac879c8dd735af719ee2300222d901793c66e041678"),
    ("pool/main/g/glib2.0/libglib2.0-0t64_2.84.4-3~deb13u5_amd64.deb", "e2baf92c57d1db1753e5781a61450d9a01e7833c7bd2dfe87776a6811c11ecc2"),
    ("pool/main/libg/libglvnd/libglvnd0_1.7.0-1+b2_amd64.deb", "887f74008166549ce9e100c906aa937e95d6e5ce1c8d86efe8c95fd953359b9c"),
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
    ("pool/main/libi/libidn/libidn12_1.43-1_amd64.deb", "c87472105da3da12481627412aeb6088062c945cce2ee259c447fd27385a4850"),
    ("pool/main/libi/libidn2/libidn2-0_2.3.8-2_amd64.deb", "90b039bcdc4578f8e1c4935adf8dbb525e36a164deefdbbb8c45bac347d48278"),
    ("pool/main/libj/libjpeg-turbo/libjpeg62-turbo_2.1.5-4_amd64.deb", "5504299ad0e3c0a9ca9f962a68a650a040db52f9f416a109e74df48d55601d3f"),
    ("pool/main/j/jpeg-xl/libjxl0.11_0.11.2-0.1~deb13u2_amd64.deb", "cd1ca3a52cb0d8fdce1d994bcbf0663de28c64add1cfe8096639ae8fa9808824"),
    ("pool/main/k/krb5/libk5crypto3_1.21.3-5+deb13u1_amd64.deb", "7da07ee674b47f1f0be7cc89317c25310086a1f1761217d0f72e6ae2c5a69b84"),
    ("pool/main/k/keyutils/libkeyutils1_1.6.3-6_amd64.deb", "0b11ad17be0300b63ad4eeb4c6450fed24d34b7b740f23e5363dcb29ee6d5eba"),
    ("pool/main/k/krb5/libkrb5-3_1.21.3-5+deb13u1_amd64.deb", "47d71d6a7f2e59b9bae5f89602397594805113b95889ad18fa703cd53abafc97"),
    ("pool/main/k/krb5/libkrb5support0_1.21.3-5+deb13u1_amd64.deb", "3a0acd8b37955c0e102c756b52c97df2a31f67b96453c35dab70df218d309117"),
    ("pool/main/l/lcms2/liblcms2-2_2.16-2+deb13u2_amd64.deb", "32e53c2002ccea8c60c02779bea0ba49ac132fef702988a30d64af79b6617806"),
    ("pool/main/x/xz-utils/liblzma5_5.8.1-1+deb13u1_amd64.deb", "1cfcc6e0dc36f438a79b6e2189facdb9d150b08f57d190a60e01c98075c7f896"),
    ("pool/main/libm/libmatroska/libmatroska7_1.7.1-1+b2_amd64.deb", "c0e57f51e68bd46f9dcfcd54f6533196dd56cf34e53ffa087bf4a5f751cc1b06"),
    ("pool/main/m/mbedtls/libmbedcrypto16_3.6.6-0.1~deb13u1_amd64.deb", "b38baf2ecaca7b28b3005d34b8108ee9b17681426b20662813995e939aea6dfa"),
    ("pool/main/libm/libmd/libmd0_1.1.0-2+b1_amd64.deb", "7244ec3839b61fac0c1884fe08aaa040f26e8f1f35f1f5d3482eacefd30d1b44"),
    ("pool/main/m/md4c/libmd4c0_0.5.2-2+b1_amd64.deb", "0bd88d7ce2bda0626836267c02beb1ddf89e99eae26bd5c717ce5601faa075a9"),
    ("pool/main/u/util-linux/libmount1_2.41.5-0+deb13u1_amd64.deb", "6d00f45f2e80e078e906e3eedecd3ba6913e39fef49bff361ee583f17f00ec05"),
    ("pool/main/l/lame/libmp3lame0_3.100-6+b3_amd64.deb", "3867319c9fbbc8cc25f2d1dc1d75d13548c84c37fa709c1ac9ca02ac051b63ab"),
    ("pool/main/m/mpg123/libmpg123-0t64_1.32.10-1+deb13u1_amd64.deb", "31fa6ef638f1fdbe198faee0b6fe50d9dd468077f8247cc3dbf6b2250abb6101"),
    ("pool/main/n/nettle/libnettle8t64_3.10.1-1_amd64.deb", "1b03d4a9cd9c8143ba50fe6396f36937e901a317d492d179f4596862c1731cfe"),
    ("pool/main/n/norm/libnorm1t64_1.5.9+dfsg-3.1+b2_amd64.deb", "cf48143ffa3886224cd0af63a8ecfac6f87177d80e7a053793c8621781b944ea"),
    ("pool/main/n/numactl/libnuma1_2.0.19-1_amd64.deb", "e578583f73f8208564e5035b18aa729c441fc7ecb389c666923552e123f4d655"),
    ("pool/main/libo/libogg/libogg0_1.3.5-3+b2_amd64.deb", "911aec914f5bda0788164ff8068b682537378bf8d407b1d6c1b377522f96b83b"),
    ("pool/main/o/openjpeg2/libopenjp2-7_2.5.3-2.1~deb13u2_amd64.deb", "7ddf6360e8845853ba3c9cfaefc2ad6b92b2627eb8564a257f5f2a4c857f8748"),
    ("pool/main/libo/libopenmpt/libopenmpt0t64_0.7.13-1+b1_amd64.deb", "37537d1b0dc3798d9157aa453427609a3e7f52c0c89b36563764c193194aaa6a"),
    ("pool/main/o/opus/libopus0_1.5.2-2_amd64.deb", "794056db33d71b2ac4bd8b5a4eb23b627bcb8a49d123c33b6e841a996253a067"),
    ("pool/main/p/p11-kit/libp11-kit0_0.25.5-3_amd64.deb", "784bf2063e166c8bc851a32623b74ebd85c499043a9d57c5bbd64fa63447f45a"),
    ("pool/main/p/pango1.0/libpango-1.0-0_1.56.3-1_amd64.deb", "4fa38472696d2ddb10bfc9a4692a9446e8478fdb1f48c769a609fc9fec3adfc5"),
    ("pool/main/p/pango1.0/libpangocairo-1.0-0_1.56.3-1_amd64.deb", "6994729c5b7a9649661ec078cf039991f22eff44600543aa917894f83c89cd05"),
    ("pool/main/p/pango1.0/libpangoft2-1.0-0_1.56.3-1_amd64.deb", "b506cf66f78ddf542ddb4bb4a68b3d748d41caa6e190e4480430a6630c1e5105"),
    ("pool/main/p/pcre2/libpcre2-16-0_10.46-1~deb13u2_amd64.deb", "0343948654468fc4b273077c464cb77afeee29fc493c0d4c9d81cd19d5bc4e23"),
    ("pool/main/p/pcre2/libpcre2-8-0_10.46-1~deb13u2_amd64.deb", "1252b96a5bc44bb5db982bef8eb18e54f5047cede2aff641bce4f8e1edb91c3e"),
    ("pool/main/libp/libpgm/libpgm-5.3-0t64_5.3.128~dfsg-2.1+b1_amd64.deb", "865c47a7865da63e7d620f0999ab7d10cdae8b638b691061166975c654a0e5f9"),
    ("pool/main/p/pixman/libpixman-1-0_0.44.0-3_amd64.deb", "3862c87596f6b4b60fc916395fd69aa10af28a875a8e5c081a8cf30ddd120bd6"),
    ("pool/main/libp/libpng1.6/libpng16-16t64_1.6.48-1+deb13u5_amd64.deb", "2465b4e9fa85cff54dc10a6da3e64074d8d9292c86e4a8f989b809d6b158e97e"),
    ("pool/main/f/ffmpeg/libpostproc58_7.1.5-0+deb13u1_amd64.deb", "4a19b79dac4353419018509568f554df3ce937d2283c859f6454c3fe543135b6"),
    ("pool/main/q/qtbase-opensource-src/libqt5core5t64_5.15.15+dfsg-6+deb13u1_amd64.deb", "7e60f353da5c7eaf11c8a67a9a3bac201a928489d39ca41515952b7eaacb82bf"),
    ("pool/main/q/qtbase-opensource-src/libqt5dbus5t64_5.15.15+dfsg-6+deb13u1_amd64.deb", "409a15e45c2b43b7531e5af5102639daadc299e8255dc57d2d6d04c4bcad418e"),
    ("pool/main/q/qtbase-opensource-src/libqt5gui5t64_5.15.15+dfsg-6+deb13u1_amd64.deb", "c23769130c0137f8f58badc1f6a85f02d092fff38dae67f149328db8a51e4ba9"),
    ("pool/main/q/qtsvg-opensource-src/libqt5svg5_5.15.15-2_amd64.deb", "2a0b03dfb4ef2a5eea9a6a4230b82518fdd53c2eeab869ecd98ebc910219da85"),
    ("pool/main/q/qtwayland-opensource-src/libqt5waylandclient5_5.15.15-3_amd64.deb", "8185664a8102c1f44c4c9b6eda84e4baac02bb109744c541282f5594a46beb6a"),
    ("pool/main/q/qtbase-opensource-src/libqt5widgets5t64_5.15.15+dfsg-6+deb13u1_amd64.deb", "f7e45b68882fd969e4a1fd387d5d1239905915948820fe044ff96033808f76e9"),
    ("pool/main/q/qtx11extras-opensource-src/libqt5x11extras5_5.15.15-2_amd64.deb", "f8fa513f8ecd9205f8d2d5fbe4e29615d6bc466675e8cbe1dd90b6d784a2a60b"),
    ("pool/main/libr/librabbitmq/librabbitmq4_0.15.0-1+deb13u2_amd64.deb", "7b81f891935c05a01ddca461564906dbc3c48f02519b2c20b8ca7590f2868112"),
    ("pool/main/r/rust-rav1e/librav1e0.7_0.7.1-9+b2_amd64.deb", "d5b1373a8d6905ee4523faf1db7a626dc660e03836ec91b616e57ba386f47eb6"),
    ("pool/main/libr/librist/librist4_0.2.11+dfsg-1_amd64.deb", "a3f66bd87fdd95561ec41894a782ca9e2e9272ed72c925be8a44da95e41a03b2"),
    ("pool/main/libr/librsvg/librsvg2-2_2.60.0+dfsg-1_amd64.deb", "81fd7a825f06e344dc42fb7af92577ec242b6d24237b8a0bf79ea203a7f377c9"),
    ("pool/main/libs/libsamplerate/libsamplerate0_0.2.2-4+b2_amd64.deb", "2665417fa996c1e249dbaa65c32305ac666e5ae865fad0f109b629935ec0bd18"),
    ("pool/main/libs/libselinux/libselinux1_3.8.1-1_amd64.deb", "68bb8d32bd8d6d7d2f5952a169db03d1484b46ae1e52abccdec42a19dccea5d5"),
    ("pool/main/libw/libwebp/libsharpyuv0_1.5.0-0.1_amd64.deb", "51934833d909588f7761fd84362114e90d8d2d38af57edf5df5c4a705e037547"),
    ("pool/main/s/shine/libshine3_3.1.1-2+b2_amd64.deb", "8908eb0c70893d122ee9cbed7186736ad94b45716404ef9622845bdd7d17b29d"),
    ("pool/main/s/snappy/libsnappy1v5_1.2.2-1_amd64.deb", "53ec5f1050d64375a63cd1c30ee4a6d21ef0cfd485f9999aa0b70dd424ca3465"),
    ("pool/main/libs/libsodium/libsodium23_1.0.18-1+deb13u1_amd64.deb", "9ff9d97bb7ce3aee7f6e6ec3a785cdbe237454a7b9fd84728121bc4ef2bebebe"),
    ("pool/main/libs/libsoxr/libsoxr0_0.1.3-4+b2_amd64.deb", "384caa47b6cfe780f30d33c5dfb1c18f6579aed9f3bdbf7f8cd0395ab2d537b7"),
    ("pool/main/s/speex/libspeex1_1.2.1-3_amd64.deb", "26623d450c2a51f9f6801b19ee5be1303aee4087557d1389020a0df00afa6e77"),
    ("pool/main/s/speexdsp/libspeexdsp1_1.2.1-3_amd64.deb", "4f0f6fc83b2f9fbb85b5a1c1470033979bbcd4c72d2177fea98a55f014c4391e"),
    ("pool/main/s/srt/libsrt1.5-gnutls_1.5.4-1+deb13u1_amd64.deb", "234ed0b5fcff17d2ecce4c625d897b78545f94f5096b1ec689e9a678f9067f1c"),
    ("pool/main/libs/libssh/libssh-4_0.11.5-0+deb13u1_amd64.deb", "bc8cbc13617d1b21fa6f686f9f269a4f849f00624d5bb179785a058981701233"),
    ("pool/main/o/openssl/libssl3t64_3.5.7-1~deb13u2_amd64.deb", "916f7f40b34a06e6ebfaefcdab331bff458328411da672598f126a760472467d"),
    ("pool/main/g/gcc-14/libstdc++6_14.2.0-19_amd64.deb", "ab1fa05837aa7a92aae748fd07a18a35f7d18bb4a71c4724fe2bbf0e32089de0"),
    ("pool/main/s/svt-av1/libsvtav1enc2_2.3.0+dfsg-1_amd64.deb", "cdaf961c3cc4fe78a19ff18c17d83d813f78a378fdcc4c86ccbb9b54e84ae599"),
    ("pool/main/f/ffmpeg/libswresample5_7.1.5-0+deb13u1_amd64.deb", "abe0f2f6a27db64a0d2d344e333880be997b2ec5431fc187a100078075680d94"),
    ("pool/main/f/ffmpeg/libswscale8_7.1.5-0+deb13u1_amd64.deb", "cf43cf4fe02a02cd36b25a86a2fcdeeaca9cc22bb1c81239302514db701d4e6d"),
    ("pool/main/s/systemd/libsystemd0_257.13-1~deb13u1_amd64.deb", "ab0d4127b5e46e6f8c015a1db15a62ba9ae274cdefa150083adf90ada0600ea1"),
    ("pool/main/t/taglib/libtag2_2.0.2-2_amd64.deb", "68d986485c5f79971b8bf4dfe39a378b3457c6493dd2e6bc1416f9a242dcfb64"),
    ("pool/main/libt/libtasn1-6/libtasn1-6_4.20.0-2+deb13u1_amd64.deb", "23fec6e06583ce2bad9b2c04c9b485e90440e259b1abf8677cd80d3ce60831ad"),
    ("pool/main/libt/libthai/libthai0_0.1.29-2+b1_amd64.deb", "e6364c07aeeffe9a95f267061d9df168e48e9dc9126c3b40e19dc829f67702a7"),
    ("pool/main/libt/libtheora/libtheoradec1_1.2.0~alpha1+dfsg-6_amd64.deb", "31c566d16b0a30f2c100d685d09b831d10d756bd043107ec6594c082274c0c74"),
    ("pool/main/libt/libtheora/libtheoraenc1_1.2.0~alpha1+dfsg-6_amd64.deb", "ce795eba6cdb510a52cd158dee7930a23667ffb4bdf07fbc44d4590edbf6f503"),
    ("pool/main/t/twolame/libtwolame0_0.4.0-2+b2_amd64.deb", "f3e739c8dd8b7c77043406bb4a4eef16d192e2e0aa5f8d9f39e09ff087844e6d"),
    ("pool/main/libu/libudfread/libudfread0_1.1.2-1+b2_amd64.deb", "dbb34708521e4e895d1e061b765548ea408e670c8b2a17e84529166afa88e5f1"),
    ("pool/main/libu/libunibreak/libunibreak6_6.1-3_amd64.deb", "a5d5fa113f195288ed145d48d0e886671c94416ae7c800f943bc21cc4ef628cc"),
    ("pool/main/libu/libunistring/libunistring5_1.3-2_amd64.deb", "6dd3490bef06ea1096f32d10766b7e016cf579bfae8451d1b7df15ce05b8aa46"),
    ("pool/main/libv/libva/libva-drm2_2.22.0-3_amd64.deb", "5dce5007ddc0ce87a61db4d71476ce1a5a135737b5185ce2e8b7067342fcafc6"),
    ("pool/main/libv/libva/libva-x11-2_2.22.0-3_amd64.deb", "97baabc9ae8dfbe718ca80543dc7337d330dd17e2a59138bf42a66a35d59309b"),
    ("pool/main/libv/libva/libva2_2.22.0-3_amd64.deb", "b76bdd330de47a826698aaed10f53435b703e9a7d4415dd68269c97709f46a9b"),
    ("pool/main/libv/libvdpau/libvdpau1_1.5-3+b1_amd64.deb", "7c872493296cc8ce829ab38a68b5e51c351466a5fd0daf485984c829282a94e3"),
    ("pool/main/v/vlc/libvlc5_3.0.23-0+deb13u1_amd64.deb", "73a7789c1aa31fba279544c48088416e0658702cb7aa4ec4385995dc418a9a73"),
    ("pool/main/v/vlc/libvlccore9_3.0.23-0+deb13u1_amd64.deb", "df7767917a140e331c8d4ed9c71974446c72e6cd9df67c910e391f3ba3d57770"),
    ("pool/main/libv/libvorbis/libvorbis0a_1.3.7-3_amd64.deb", "0465f7f602f7b00965e58e7273b5876060ab7946f0452a9f95d979046932cc42"),
    ("pool/main/libv/libvorbis/libvorbisenc2_1.3.7-3_amd64.deb", "5fbbf06a5b196235aa2753cd12bbe7f843328571d368f15b1c337578e29f73a2"),
    ("pool/main/libv/libvorbis/libvorbisfile3_1.3.7-3_amd64.deb", "02822ab3da107b6f65bb3f7ac5bc8d68f51f998dd6886dd38f509f4d04d0ccd6"),
    ("pool/main/libv/libvpl/libvpl2_2.14.0-1+b1_amd64.deb", "cee5b3fdfb5c2a944f4366406bf245992c7aaa234cdfb41acffba8f2bfbed1b4"),
    ("pool/main/libv/libvpx/libvpx9_1.15.0-2.1+deb13u1_amd64.deb", "5c2baac5c97a1fe01c4f0a2216743b901dc72437351e7451725a394c08037bcc"),
    ("pool/main/w/wayland/libwayland-client0_1.23.1-3_amd64.deb", "d1607e1db1a7c5378a2e5c6ae7b20f64dc6acf134f953d84a7f5204b74339f33"),
    ("pool/main/w/wayland/libwayland-cursor0_1.23.1-3_amd64.deb", "26fccae2b1228744de0cad6c688550b8d13cd4e8d4ccf0cfdae015634ece673f"),
    ("pool/main/libw/libwebp/libwebp7_1.5.0-0.1_amd64.deb", "c749744b04abdd1426b0c7718e516362c03a557c5151662092d6d4f2cef6e7c1"),
    ("pool/main/libw/libwebp/libwebpmux3_1.5.0-0.1_amd64.deb", "8bc86d509406ad496b6414c7068b3dadc0f8c502027973cbf1765c7bb54fabdf"),
    ("pool/main/libx/libx11/libx11-6_1.8.12-1_amd64.deb", "b5a3fd3bf8c8fd0364bfb9bea00dcba7fc301229bd02dded084632d31f5b0fb3"),
    ("pool/main/libx/libx11/libx11-xcb1_1.8.12-1_amd64.deb", "e05f94d21a932fba5b09b9b13d99df776b155d3bc792c0e294451df9ffe1ba25"),
    ("pool/main/x/x264/libx264-164_0.164.3108+git31e19f9-2+b1_amd64.deb", "325c90dc0888be316c8979b94aee546d223d4edf03b30927d1e9d431f67e33b2"),
    ("pool/main/x/x265/libx265-215_4.1-2_amd64.deb", "4bb075b465dbc136b0c5050b956432ea7e443a16d592aa89ef28bcaf4b0ad734"),
    ("pool/main/libx/libxau/libxau6_1.0.11-1_amd64.deb", "689a9f0e0ba3e2c65431f864871e303ee904de69dd28abfc462663fae030227f"),
    ("pool/main/libx/libxcb/libxcb-dri3-0_1.17.0-2+b1_amd64.deb", "f446d42fb5fcebbb3e347368ba83616769fdb271d85b2f49048e337f3163d267"),
    ("pool/main/libx/libxcb/libxcb-render0_1.17.0-2+b1_amd64.deb", "7f6e36978e0f70a590fbad4042a692c70b50ca7cbcdc53fa7e9d9ab87933acc6"),
    ("pool/main/libx/libxcb/libxcb-shm0_1.17.0-2+b1_amd64.deb", "d1777a813f484e89f3dcaadd1d5f2b1a13e9313b6bf70eded16d8ce743208fef"),
    ("pool/main/libx/libxcb/libxcb1_1.17.0-2+b1_amd64.deb", "5c222a72d11b866447da31693254f738430726e3e065a384e82687b2fd2f978b"),
    ("pool/main/libx/libxdmcp/libxdmcp6_1.1.5-1_amd64.deb", "0740dc760916b2008b45417a42a8fd7dd5de370fb57d31373f15034cda8acf0b"),
    ("pool/main/libx/libxext/libxext6_1.3.4-1+b3_amd64.deb", "fc618ec40465e5ce48622606299cb47833efc3fb235ba15543b81f850722f443"),
    ("pool/main/libx/libxfixes/libxfixes3_6.0.0-2+b4_amd64.deb", "3fdd95d86d8e9d63e11f52070935c8f9f912c36aa3b17338d969f7685f3ed4fb"),
    ("pool/main/libx/libxkbcommon/libxkbcommon0_1.7.0-2_amd64.deb", "f75ee544f55acc6a271debfab3ea4ae0458afc89d81cfe1a71137e07d4895b86"),
    ("pool/main/libx/libxml2/libxml2_2.12.7+dfsg+really2.9.14-2.1+deb13u3_amd64.deb", "e0c6b63ce4602a036a526f60fe5e6c1586710688058d98fc1001b9b3147b7efd"),
    ("pool/main/libx/libxrender/libxrender1_0.9.12-1_amd64.deb", "9d042dfd5e613be1e02e6ddd0c5c4adef19c5eb08f6db838c2eba672c496dca4"),
    ("pool/main/x/xvidcore/libxvidcore4_1.3.7-1+b2_amd64.deb", "a3ea54e60cbc13dcd5f4a64c5635c52701274ae52baa9902d87ad742af4e4977"),
    ("pool/main/z/zeromq3/libzmq5_4.3.5-1+b3_amd64.deb", "82348834b94ae5288dd1dde1c6a59488d30e3ee97fd4183c0b60ed79c3ab0b57"),
    ("pool/main/libz/libzstd/libzstd1_1.5.7+dfsg-1_amd64.deb", "2f6a2aeacfc925eba8b00ac9139bc4bfccf8cacb09eb93de067074b26948eef9"),
    ("pool/main/z/zvbi/libzvbi0t64_0.2.44-1_amd64.deb", "ff9b1227f8812b4d51d24130bfeb436a3cc3159410d507cb370f6908e9059112"),
    ("pool/main/o/ocl-icd/ocl-icd-libopencl1_2.3.3-1_amd64.deb", "99dfd409ae92505ee56742feea4e82a1e052818c5b0036ee618a371b5e28c9d1"),
    ("pool/main/q/qtwayland-opensource-src/qtwayland5_5.15.15-3_amd64.deb", "ffb75d0d564d3def82b765a119f6bbcdeacc7bcd97cbf61376584a47df6a47ad"),
    ("pool/main/v/vlc/vlc-bin_3.0.23-0+deb13u1_amd64.deb", "116a6af7bb48414475ae4ec9c852fd440370bfdb4604ad442df5697adf010ffe"),
    ("pool/main/v/vlc/vlc-data_3.0.23-0+deb13u1_all.deb", "4a05a94246c57a04bf713dd4bfcad827d83de01cc38383f057d6e06a861f8e3a"),
    ("pool/main/v/vlc/vlc-l10n_3.0.23-0+deb13u1_all.deb", "6b9b6505e9f070c5cb5a6c7b39dba586e4955780bde65aceaaf9b07cc5b4705f"),
    ("pool/main/v/vlc/vlc-plugin-base_3.0.23-0+deb13u1_amd64.deb", "2a546b92264e1f8f2fbfb8c22963ce2f14854e33bb8d8ed1e41b80c022a476f9"),
    ("pool/main/v/vlc/vlc-plugin-qt_3.0.23-0+deb13u1_amd64.deb", "fff21110e415ac9912bcf7e0490606637ee3269ee3075db31acd2fc5aa958d2d"),
    ("pool/main/v/vlc/vlc-plugin-video-output_3.0.23-0+deb13u1_amd64.deb", "66989412853e506bc683398194a5a1de36d19d09fe10f8cd73c3ef67b212dd91"),
    ("pool/main/x/xkeyboard-config/xkb-data_2.42-1_all.deb", "196ff18533382f64e057ea49df2bb486bd4275a4cc0917361edb560b8756dada"),
    ("pool/main/z/zlib/zlib1g_1.3.dfsg+really1.3.1-1+b1_amd64.deb", "015be740d6236ad114582dea500c1d907f29e16d6db00566ca32fb68d71ac90d"),
];

/// Каталог библиотек в пакетах trixie (usrmerge).
const DIR: &str = "usr/lib/x86_64-linux-gnu/";

/// Модули VLC в пакетах.
const PLUGIN_DIR: &str = "usr/lib/x86_64-linux-gnu/vlc/plugins/";

/// Где ищет `ld.so` в корне Debian 12 (что там есть — в пакет не кладётся).
const LIB_DIRS: [&str; 4] = ["lib/x86_64-linux-gnu", "usr/lib/x86_64-linux-gnu", "lib", "usr/lib"];

/// Каталоги модулей VLC целиком: упаковщики потоков, преобразования цвета,
/// фильтры видео (меню «Эффекты и фильтры»), смесители звука.
const PLUGIN_DIRS: [&str; 4] = ["packetizer", "video_chroma", "video_filter", "audio_mixer"];

/// Выбранные модули VLC: каталог и имена (`lib<имя>_plugin.so`). Не
/// берутся: вывод видео на OpenGL/EGL/GLX, X11 (xcb), VA-API и VDPAU (нет
/// Mesa и видеокарты — декодирование на процессоре), PulseAudio, Lua
/// (расширения, разбор YouTube), диски и ТВ (DVD, Blu-ray, DVB, CD),
/// трансляция и перекодирование (`stream_out`, `mux`), сетевые службы
/// (UPnP, SAP, Avahi, SMB, NFS, SFTP), ncurses и skins2.
const PLUGINS: [(&str, &[&str]); 16] = [
    ("access", &["filesystem", "attachment", "idummy", "http", "https", "tcp"]),
    (
        "audio_filter",
        &[
            "audio_format", "equalizer", "compressor", "gain", "normvol", "remap", "scaletempo", "scaletempo_pitch",
            "simple_channel_mixer", "trivial_channel_mixer", "speex_resampler", "soxr", "ugly_resampler",
            "headphone_channel_mixer", "dolby_surround_decoder", "mono", "stereo_widen", "spatializer", "param_eq",
            "karaoke", "chorus_flanger", "samplerate",
        ],
    ),
    ("audio_output", &["alsa", "adummy", "afile", "amem"]),
    (
        "codec",
        &[
            "avcodec", "araw", "adpcm", "lpcm", "flac", "opus", "vorbis", "mpg123", "dav1d", "subsdec", "substx3g",
            "subsusf", "webvtt", "ttml", "cc", "spudec", "dvbsub", "png", "jpeg", "rawvideo", "speex", "theora",
            "libass", "g711", "aes3", "ddummy", "edummy", "textst", "stl", "cvdsub", "svcdsub",
        ],
    ),
    ("control", &["hotkeys", "dummy", "oldrc"]),
    (
        "demux",
        &[
            "mp4", "mkv", "avi", "asf", "es", "flacsys", "ogg", "wav", "ts", "ps", "mpgv", "h26x", "avformat",
            "playlist", "subtitle", "aiff", "au", "caf", "vobsub", "voc", "xa", "rawaud", "rawvid", "nsv", "nuv",
            "real", "directory_demux", "noseek", "vc1", "tta", "mjpeg", "image",
        ],
    ),
    ("gui", &["qt"]),
    ("keystore", &["memory_keystore", "file_keystore"]),
    ("logger", &["console_logger", "file_logger"]),
    ("meta_engine", &["folder", "taglib"]),
    ("misc", &["logger", "stats", "xml", "export", "gnutls"]),
    ("services_discovery", &["mediadirs"]),
    ("spu", &["subsdelay", "marq", "logo"]),
    ("stream_filter", &["cache_block", "cache_read", "prefetch", "skiptags", "inflate", "record", "adf"]),
    ("text_renderer", &["freetype", "tdummy"]),
    // Видео в окно Wayland на буферах wl_shm (в окне Qt — подповерхность;
    // `xdg_shell` — своё окно, если VLC запущен без Qt).
    ("video_output", &["wl_shm", "xdg_shell", "wl_shell", "vdummy", "vmem", "yuv"]),
];

/// Модули Qt5 (`qt5/plugins/…`): платформа Wayland на wl_shm, оболочка
/// xdg-shell, рамка окна, картинки (значки интерфейса VLC — PNG и SVG),
/// клавиатура (compose). `offscreen` — для проверки без окна.
const QT_PLUGINS: [&str; 11] = [
    "platforms/libqwayland-generic.so",
    "platforms/libqoffscreen.so",
    "wayland-shell-integration/libxdg-shell.so",
    "wayland-decoration-client/libbradient.so",
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

/// Загрузчик своей программы `vlc`.
const INTERP: &str = "/opt/vlc/glibc/ld.so";

/// Шрифты DejaVu (кириллица) и `fonts.conf`.
const FONT_DIR: &str = "usr/share/fonts/truetype/dejavu/";
const FONTS_CONF: &str = "<?xml version=\"1.0\"?>\n\
    <!DOCTYPE fontconfig SYSTEM \"fonts.dtd\">\n\
    <fontconfig>\n\
    \t<dir>/opt/vlc/fonts</dir>\n\
    \t<cachedir>/tmp/fontconfig</cachedir>\n\
    \t<alias><family>sans-serif</family><prefer><family>DejaVu Sans</family></prefer></alias>\n\
    \t<alias><family>serif</family><prefer><family>DejaVu Sans</family></prefer></alias>\n\
    \t<alias><family>monospace</family><prefer><family>DejaVu Sans Mono</family></prefer></alias>\n\
    </fontconfig>\n";

/// Настройки VLC при первом запуске (`~/.config/vlc/vlcrc`, если его нет):
/// без вопроса о доступе к сети (`qt-privacy-ask`) и проверки обновлений,
/// без загрузки обложек и сведений из сети, аппаратное декодирование
/// выключено (VA-API/VDPAU нет), одно окно (видео внутри окна VLC), окно
/// не подстраивается под видео (с подстройкой Qt и сервер качали размер
/// 600x420 <-> 640x432 без конца), звук — ALSA.
const VLCRC: &str = "###\n\
    ### vlc 3.0.23 - FossAi defaults (fossai-vlc)\n\
    ###\n\
    \n\
    [qt]\n\
    qt-privacy-ask=0\n\
    qt-updates-notif=0\n\
    qt-video-autoresize=0\n\
    qt-recentplay=1\n\
    qt-system-tray=0\n\
    qt-notification=0\n\
    \n\
    [avcodec]\n\
    avcodec-hw=none\n\
    \n\
    [alsa]\n\
    alsa-audio-device=default\n\
    \n\
    [core]\n\
    metadata-network-access=0\n\
    embedded-video=1\n\
    aout=alsa\n\
    volume-save=1\n\
    one-instance=0\n\
    snapshot-path=@HOME@/Pictures\n";

/// Запуск: окружение и `exec` своей `vlc`.
const SCRIPT: &str = r#"#!/bin/bash
# VLC (24.11.2026): Debian 13 vlc 3.0.23 через слой Linux; окно - Qt5 на
# сервере Wayland ядра (wl_shm), видео - подповерхность окна (модуль wl_shm),
# декодирование - FFmpeg на процессоре, звук - ALSA ядра.
P=/opt/vlc
export HOME=${HOME:-/root}
export TMPDIR=${TMPDIR:-$HOME}
export XDG_CONFIG_HOME=${XDG_CONFIG_HOME:-$HOME/.config}
export XDG_RUNTIME_DIR=/run/user/0
export WAYLAND_DISPLAY=wayland-0
unset DISPLAY
# Своя glibc (2.41) первой; программы bash и coreutils корня Debian 12 ниже
# зовутся с пустым LD_LIBRARY_PATH.
export LD_LIBRARY_PATH=$P/glibc:$P/lib
export QT_PLUGIN_PATH=$P/lib/qt5/plugins
export QT_QPA_PLATFORM=wayland
export QT_WAYLAND_SHELL_INTEGRATION=xdg-shell
export QT_WAYLAND_CLIENT_BUFFER_INTEGRATION=none
export QT_WAYLAND_DECORATION=bradient
export QT_AUTO_SCREEN_SCALE_FACTOR=${QT_AUTO_SCREEN_SCALE_FACTOR:-0}
export FONTCONFIG_FILE=$P/fonts/fonts.conf
export XKB_CONFIG_ROOT=$P/share/X11/xkb
# Шины D-Bus нет: адрес в никуда, Qt и VLC это переживают.
export DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/0/bus
export ALSA_CONFIG_DIR=$P/share/alsa
export ALSA_CONFIG_PATH=$P/share/alsa/alsa.conf:$P/share/alsa/fossai.conf
export VLC_DATA_PATH=$P/share/vlc
# Модули: VLC ищет их от libvlccore по /proc/self/maps, а там у слоя Linux
# путь самой программы (/opt/vlc/bin/vlc/plugins) - путь явно.
export VLC_PLUGIN_PATH=$P/lib/vlc/plugins
# `--env=ИМЯ=ЗНАЧЕНИЕ` первыми - своя переменная (у `run` окружения нет).
while case "$1" in --env=*) true ;; *) false ;; esac; do export "${1#--env=}"; shift; done
C=$XDG_CONFIG_HOME/vlc
L=$HOME/.cache/fossai-vlc
LD_LIBRARY_PATH= mkdir -p "$L" "$C"
say() { echo "fossai-vlc: $*"; printf '%(%F %T)T %s\n' -1 "$*" >> "$L/launch.log"; }
# Первый запуск: свои настройки (без вопроса о сети, без аппаратного
# декодирования).
if [ ! -e "$C/vlcrc" ]; then
    t=$(<$P/share/fossai/vlcrc); t=${t//@HOME@/$HOME}
    printf '%s\n' "$t" > "$C/vlcrc"
    say "first run: $C/vlcrc"
fi
say "start: $*"
cd "$HOME"
# Русский интерфейс: gettext берёт язык из LC_MESSAGES; ru_RU.UTF-8 - своя
# (LOCPATH), bash Debian 12 выше её не видит и молчит.
{ export LOCPATH=$P/locale LC_ALL=ru_RU.UTF-8 LANG=ru_RU.UTF-8 LANGUAGE=ru; } 2>/dev/null
exec $P/bin/vlc "$@"
"#;

/// Расширения, которые VLC открывает (`opens=` манифеста — «Открыть с
/// помощью»; по умолчанию — `sysconf::store::BUILTIN`).
const OPENS: &str = "mp4,m4v,mkv,webm,avi,mov,wmv,flv,ts,m2ts,mpg,mpeg,ogv,3gp,mp3,flac,ogg,oga,opus,wav,m4a,aac,wma,mka";

/// Скачать пакеты и собрать `vlc-3.0.fpk`.
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

    // Что разобрать по `DT_NEEDED` и кто это потребовал.
    let mut todo: Vec<(String, String)> = Vec::new();
    let add_elf = |files: &mut Vec<(String, Vec<u8>, u32)>, todo: &mut Vec<(String, String)>, from: &str, to: String| -> Result<()> {
        let Some((data, mode)) = follow(&deb, from, 0) else {
            bail!("в пакетах нет {from}");
        };
        let by = to.rsplit('/').next().unwrap_or(&to).to_string();
        todo.extend(needed(data).with_context(|| format!("{from}: не разобран ELF"))?.into_iter().map(|name| (name, by.clone())));
        files.push((to, data.clone(), mode));
        Ok(())
    };
    // libvlc своя `vlc` открывает сама (`dlopen`).
    add_elf(&mut files, &mut todo, &format!("{DIR}libvlc.so.5"), String::from("lib/libvlc.so.5"))?;
    let mut plugins = 0;
    for (dir, names) in PLUGINS {
        for name in names {
            let from = format!("{PLUGIN_DIR}{dir}/lib{name}_plugin.so");
            add_elf(&mut files, &mut todo, &from, format!("lib/vlc/plugins/{dir}/lib{name}_plugin.so"))?;
            plugins += 1;
        }
    }
    let whole: Vec<String> = deb
        .keys()
        .filter(|path| {
            path.strip_prefix(PLUGIN_DIR).is_some_and(|rest| PLUGIN_DIRS.iter().any(|dir| rest.starts_with(&format!("{dir}/")) && rest.ends_with("_plugin.so")))
        })
        .cloned()
        .collect();
    for from in whole {
        let rest = from.strip_prefix(PLUGIN_DIR).unwrap_or(&from).to_string();
        add_elf(&mut files, &mut todo, &from, format!("lib/vlc/plugins/{rest}"))?;
        plugins += 1;
    }
    say!("vlc: модулей VLC {plugins}");
    for plugin in QT_PLUGINS {
        add_elf(&mut files, &mut todo, &format!("{DIR}qt5/plugins/{plugin}"), format!("lib/qt5/plugins/{plugin}"))?;
    }

    let have: BTreeSet<String> = files.iter().filter_map(|(path, _, _)| path.strip_prefix("lib/").map(String::from)).collect();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut missing: Vec<String> = Vec::new();
    let (mut from_deb, mut from_base) = (0, 0);
    while let Some((name, by)) = todo.pop() {
        if !seen.insert(name.clone()) || have.contains(&name) || GLIBC_LIBS.contains(&name.as_str()) {
            continue;
        }
        let suffix = format!("/{name}");
        let found = LIB_DIRS.iter().find_map(|dir| follow(&deb, &format!("{dir}/{name}"), 0)).or_else(|| {
            deb.keys().find(|path| path.ends_with(&suffix) && !path.contains("/plugins/")).and_then(|path| follow(&deb, path, 0))
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
    say!("vlc: библиотек из пакетов trixie {from_deb}, из корня Debian 12 {from_base}");
    if !missing.is_empty() {
        bail!("vlc: не нашлись библиотеки: {}", missing.join(", "));
    }

    // Данные: `share/vlc` (значок; VLC_DATA_PATH) и только русский перевод.
    for path in ["usr/share/vlc/vlc.ico"] {
        if let Some((data, mode)) = follow(&deb, path, 0) {
            files.push((format!("share/vlc/{}", path.rsplit('/').next().unwrap_or(path)), data.clone(), mode));
        }
    }
    for lang in ["ru"] {
        let Some((data, mode)) = follow(&deb, &format!("usr/share/locale/{lang}/LC_MESSAGES/vlc.mo"), 0) else {
            bail!("в vlc-l10n нет перевода {lang}");
        };
        files.push((format!("share/locale/{lang}/LC_MESSAGES/vlc.mo"), data.clone(), mode));
    }
    // Локали: C.UTF-8 glibc 2.41 и она же под именем ru_RU.UTF-8 (язык
    // сообщений; Qt и VLC другого от неё не ждут).
    for path in deb.keys() {
        if let Some(rest) = path.strip_prefix("usr/lib/locale/C.utf8/") {
            if let Some((data, mode)) = follow(&deb, path, 0) {
                files.push((format!("locale/C.utf8/{rest}"), data.clone(), mode));
                files.push((format!("locale/ru_RU.utf8/{rest}"), data.clone(), mode));
            }
        }
    }
    for path in deb.keys() {
        if let Some(name) = path.strip_prefix(FONT_DIR) {
            if let Some((data, mode)) = follow(&deb, path, 0) {
                files.push((format!("fonts/{name}"), data.clone(), mode));
            }
        }
    }
    files.push((String::from("fonts/fonts.conf"), FONTS_CONF.as_bytes().to_vec(), 0o644));
    for path in deb.keys() {
        if let Some(rest) = path.strip_prefix("usr/share/X11/xkb/") {
            if let Some((data, mode)) = follow(&deb, path, 0) {
                files.push((format!("share/X11/xkb/{rest}"), data.clone(), mode));
            }
        }
    }
    // Настройки ALSA (libasound2-data) и своё: «default» — plug поверх
    // единственного устройства ядра (как у OBS и Proton, `mesa.rs`).
    for path in deb.keys() {
        if let Some(rest) = path.strip_prefix("usr/share/alsa/") {
            if let Some((data, mode)) = follow(&deb, path, 0) {
                files.push((format!("share/alsa/{rest}"), data.clone(), mode));
            }
        }
    }
    // plug поверх mmap_emul (как у Proton, e51ddd0): VLC отдаёт float32, а
    // перевод формата у plug пишет в устройство только через mmap - ALSA ядра
    // даёт RW_INTERLEAVED, и без mmap_emul «cannot commit hardware parameters».
    files.push((
        String::from("share/alsa/fossai.conf"),
        b"pcm.fossai_rw { type mmap_emul slave.pcm \"hw:0,0\" }\npcm.!default { type plug slave.pcm \"fossai_rw\" }\nctl.!default { type hw card 0 }\n".to_vec(),
        0o644,
    ));
    files.push((String::from("share/fossai/vlcrc"), VLCRC.as_bytes().to_vec(), 0o644));
    files.push((String::from("bin/fossai-vlc"), SCRIPT.as_bytes().to_vec(), 0o755));
    files.push((String::from("bin/vlc"), build_launcher()?, 0o755));

    let icon = follow(&deb, "usr/share/icons/hicolor/256x256/apps/vlc.png", 0).map(|(data, _)| data.clone());
    let icon_field = match icon.as_deref().map(crate::fimg::from_png) {
        Some(Ok(fimg)) => {
            files.push((String::from("icon.fimg"), fimg, 0o644));
            "/opt/vlc/icon.fimg"
        }
        _ => {
            say!("vlc: значок не разобран, будет значок набора");
            "video"
        }
    };

    let mut builder = Builder::new(Kind::Package, "vlc", "3.0");
    builder.field("summary", "VLC media player 3.0.23 from Debian 13: Qt5 on the kernel Wayland server (wl_shm), FFmpeg decoding on the CPU, ALSA sound");
    builder.field("about", "VLC: видеоплеер и аудиоплеер");
    builder.field("caption", "Видеоплеер VLC");
    builder.field("start", "/opt/debian/bin/bash /opt/vlc/bin/fossai-vlc");
    builder.field("opens", OPENS);
    builder.field("desktop", "yes");
    builder.field("icon", icon_field);
    builder.field("permissions", "windows files net");
    builder.requires("debian");
    let mut bytes = 0usize;
    for (path, data, mode) in &files {
        bytes += data.len();
        builder.file(&Entry { path: path.clone(), mode: (*mode & 0o7777) as u16, uid: 0, gid: 0, data: data.clone() });
    }
    let out = package_path();
    fs::create_dir_all(out.parent().unwrap())?;
    fs::write(&out, builder.finish())?;
    say!("vlc: {} — {} файлов, {} МиБ", out.display(), files.len(), bytes / (1024 * 1024));
    Ok(())
}

/// Собрать свою `vlc` (`libc/linux/fossai-vlc.c`) clang-ом: sysroot glibc
/// Debian 12, загрузчик — своя glibc пакета, конструкторы `QApplication`
/// наружу с версией `Qt_5` (их ищет модуль Qt VLC), `XOpenDisplay` —
/// без версии.
fn build_launcher() -> Result<Vec<u8>> {
    let work = sdk::root().join("linux/vlc");
    fs::create_dir_all(&work)?;
    let script = work.join("fossai-vlc.map");
    fs::write(&script, "Qt_5 {\n  global:\n    _ZN12QApplicationC1ERiPPci;\n    _ZN12QApplicationC2ERiPPci;\n};\n")?;
    let sysroot = crate::debian::glibc_sysroot()?;
    let lib = sysroot.join("usr/lib/x86_64-linux-gnu");
    let output = work.join("vlc");
    let mut cmd = std::process::Command::new(cbuild::llvm_tool("clang")?);
    cmd.arg("--target=x86_64-linux-gnu")
        .arg(format!("--sysroot={}", sysroot.display()))
        .args(["-O2", "-Wall", "-Werror", "-fPIE", "-pie", "-fuse-ld=lld", "-nostdlib"])
        .arg(format!("-Wl,--dynamic-linker={INTERP}"))
        .arg("-Wl,--export-dynamic")
        .arg(format!("-Wl,--version-script={}", script.display()))
        .arg(lib.join("Scrt1.o"))
        .arg(lib.join("crti.o"))
        .arg(cbuild::libc_dir().join("linux").join("fossai-vlc.c"))
        .arg(format!("-L{}", lib.display()))
        .arg(format!("-L{}", sysroot.join("lib/x86_64-linux-gnu").display()))
        .arg("-lc")
        .arg(lib.join("crtn.o"))
        .arg("-o")
        .arg(&output);
    say!("> clang (glibc) fossai-vlc.c");
    let status = cmd.status().context("не удалось запустить clang")?;
    if !status.success() {
        bail!("fossai-vlc.c не собрался ({status})");
    }
    Ok(fs::read(&output)?)
}
