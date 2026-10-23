//! Chromium для программ Linux: `chromium` из Debian 12 и библиотеки, которые
//! он загружает, — пакетом `linuxweb-150.fpk` (`/opt/chromium` после `pkg
//! install`). Фаза 71h — без окна (`--headless`), 71i — в окне стола через
//! сервер Wayland ядра (Ozone/Wayland, программная отрисовка в `wl_shm`).
//!
//! # Что берётся
//!
//! Не замыкание зависимостей Debian целиком (у `chromium` это 170 пакетов и
//! 700 МиБ — mesa, LLVM, GTK), а то, что бинарник **загружает**: цепочка
//! `DT_NEEDED` от `chromium` по библиотекам из списка
//! [`PACKAGES`] (замыкание `Depends` пакета `chromium` без GTK, выписанное
//! из `Packages` bookworm), минус то, что уже лежит в корне `debian-12.fpk`.
//! Плюс модули NSS, которые `libnss3` открывает сама (`dlopen`), — без них
//! нет ни криптографии, ни запуска.
//!
//! GTK Chromium открывает сам (`dlopen`) и без него обходится.
//!
//! Библиотеки — в `/opt/chromium/lib`; запуск — `/opt/chromium/bin/chromium`,
//! сценарий bash с `LD_LIBRARY_PATH` (у бинарника нет `RUNPATH`, а корень
//! Debian своих путей поиска не знает). Шрифты DejaVu и свой `fonts.conf` —
//! в `/opt/chromium/fonts` (`FONTCONFIG_FILE`): у корня Debian их нет.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use fpk::Kind;
use fpk::build::{Builder, Entry};

use crate::arch::Arch;
use crate::debian::{Node, follow, unpack};
use crate::{cbuild, sdk};

/// Имя пакета.
pub const PACKAGE_FILE: &str = "linuxweb-150.fpk";

/// Где лежит собранный пакет.
pub fn package_path() -> PathBuf {
    cbuild::games_dir().join(PACKAGE_FILE)
}

/// Бинарник внутри пакета `chromium`.
const BINARY: &str = "usr/lib/chromium/chromium";

/// Где ищет `ld.so` (корень Debian 12, без usrmerge).
const LIB_DIRS: [&str; 4] = ["lib/x86_64-linux-gnu", "usr/lib/x86_64-linux-gnu", "lib", "usr/lib"];

/// Модули NSS, которые `libnss3` открывает сама.
const NSS_MODULES: [&str; 4] = ["libsoftokn3.so", "libfreeblpriv3.so", "libfreebl3.so", "libnssckbi.so"];

/// Что берётся из `chromium-common`: данные ICU и снимки V8 (без них Chromium
/// падает на первой проверке — «Invalid file descriptor to ICU data»),
/// ресурсы интерфейса и английский. GPU (SwiftShader, Vulkan) не нужен: он
/// выключен (`--disable-gpu`).
const COMMON_FILES: [&str; 7] = [
    "icudtl.dat",
    "snapshot_blob.bin",
    "v8_context_snapshot.bin",
    "resources.pak",
    "chrome_100_percent.pak",
    "chrome_200_percent.pak",
    "locales/en-US.pak",
];

/// Шрифты: каталог в пакетах Debian и `fonts.conf`, который смотрит в
/// `/opt/chromium/fonts`.
const FONT_DIR: &str = "usr/share/fonts/truetype/dejavu/";
const FONTS_CONF: &str = "<?xml version=\"1.0\"?>\n\
    <!DOCTYPE fontconfig SYSTEM \"fonts.dtd\">\n\
    <fontconfig>\n\
    \t<dir>/opt/chromium/fonts</dir>\n\
    \t<cachedir>/tmp/fontconfig</cachedir>\n\
    \t<alias><family>sans-serif</family><prefer><family>DejaVu Sans</family></prefer></alias>\n\
    \t<alias><family>serif</family><prefer><family>DejaVu Sans</family></prefer></alias>\n\
    \t<alias><family>monospace</family><prefer><family>DejaVu Sans Mono</family></prefer></alias>\n\
    </fontconfig>\n";

/// Пакеты: путь в архиве и SHA-256 из `Packages` bookworm (снимок 20261020).
const PACKAGES: [(&str, &str); 103] = [
    ("pool/main/a/at-spi2-core/at-spi2-common_2.46.0-5_all.deb", "441bc1c6cefdc01f519310abf55823deb868898480dee09a05aa44e8c83cabc2"),
    ("pool/main/c/chromium/chromium_150.0.7871.100-1~deb12u1_amd64.deb", "9eb054fb52bbcd7a8c6407484be2337298ff8b1e19af14af2e7b497fa190ce4f"),
    // Данные ICU, снимки V8, ресурсы и en-US (см. [`COMMON_FILES`]).
    ("pool/main/c/chromium/chromium-common_150.0.7871.100-1~deb12u1_amd64.deb", "31326f6b71ce20a52e7bea9792bf7135d4ff802e1cdbc250960b1b46729998d2"),
    ("pool/main/f/fontconfig/fontconfig_2.14.1-4_amd64.deb", "010e57c24a983eecc39aab8a84219a716c10901e03bacd1927331388656f890f"),
    ("pool/main/f/fontconfig/fontconfig-config_2.14.1-4_amd64.deb", "281c66e46b95f045a0282a6c7a03b33de0e9a08d016897a759aaf4a04adfddbe"),
    ("pool/main/f/fonts-dejavu/fonts-dejavu-core_2.37-6_all.deb", "8892669e51aab4dc56682c8e39d8ddb7d70fad83c369344e1e240bf3ca22bb76"),
    ("pool/main/g/gcc-12/gcc-12-base_12.2.0-14+deb12u1_amd64.deb", "1896a2aacf4ad681ff5eacc24a5b0ca4d5d9c9b9c9e4b6de5197bc1e116ea619"),
    ("pool/main/a/alsa-lib/libasound2_1.2.8-1+b1_amd64.deb", "44c77b076a7b11ae99712439022d822245b1994c435da564ebd320bb676faf4c"),
    ("pool/main/a/alsa-lib/libasound2-data_1.2.8-1_all.deb", "fe0780d2d3674b2977e0acb0d48b448ad72ba1642564b7dc537f55e839984c2d"),
    ("pool/main/liba/libasyncns/libasyncns0_0.8-6+b3_amd64.deb", "cadaff99f4178b1f580331045621b363abaaefc994e78eecba1857774a3cead2"),
    ("pool/main/a/at-spi2-core/libatk-bridge2.0-0_2.46.0-5_amd64.deb", "fbb07714fdd3fdee7b63fd8595053f4fa8021d8e350a5de6281c6ce7cd4f158f"),
    ("pool/main/a/at-spi2-core/libatk1.0-0_2.46.0-5_amd64.deb", "909dd661e03760a9ba1529bfc772f259cec8431999ee38b7c5521186237cdd98"),
    ("pool/main/a/at-spi2-core/libatspi2.0-0_2.46.0-5_amd64.deb", "1c0332f3b712a8e5ee56800bbcab83a8f92098874094f2b0bdfd6b7fd45e8b83"),
    ("pool/main/a/avahi/libavahi-client3_0.8-10+deb12u1_amd64.deb", "a99d635f07a49cfbd24de151155c4f815a5adf867855e7ba13ecc3b25ffa52b9"),
    ("pool/main/a/avahi/libavahi-common-data_0.8-10+deb12u1_amd64.deb", "b21e33c3797c834e9a9ce0f1085e6d3f721db496d550505ab2f364d52097fe6f"),
    ("pool/main/a/avahi/libavahi-common3_0.8-10+deb12u1_amd64.deb", "0c12791afe1be09d8362161244d7a51e82a67a55f3909f4c29ee3dd424c3a5f1"),
    ("pool/main/u/util-linux/libblkid1_2.38.1-5+deb12u3_amd64.deb", "5a083377f29191cafcdf0f91ec124349b4a7895b531556633a136e43c6d53943"),
    ("pool/main/b/brotli/libbrotli1_1.0.9-2+b6_amd64.deb", "563b4caec1aa5e876bd3355b36e7a38e1484baf5a293b48d1e8bd22db786e4d7"),
    ("pool/main/libb/libbsd/libbsd0_0.11.7-2_amd64.deb", "bb31cc8b40f962a85b2cec970f7f79cc704a1ae4bad24257a822055404b2c60b"),
    ("pool/main/c/cairo/libcairo2_1.16.0-7_amd64.deb", "927fa059f8b5791d0cd744d4e3a8c57fa872ef849ce14434dc2222ac8a076a69"),
    ("pool/main/e/e2fsprogs/libcom-err2_1.47.0-2+b2_amd64.deb", "a2bac7015e78fbc0c504df3e441f3a22292f1d2d77f9ccda760057f3690e22f2"),
    ("pool/main/c/cups/libcups2_2.4.2-3+deb12u9_amd64.deb", "8dbfd3b7cd81166a2d79bb7e3898b0203fec05c7e927f60e207122a1e36b95c8"),
    ("pool/main/libd/libdatrie/libdatrie1_0.2.13-2+b1_amd64.deb", "f021f193384929989b2dfd19f606a8cebe54b5f209fe387fc40683e810e01ebe"),
    ("pool/main/d/dav1d/libdav1d6_1.0.0-2+deb12u1_amd64.deb", "197f08108242177aeae4c04aac11825c39b7d18598191ab769f910687ccb387f"),
    ("pool/main/d/dbus/libdbus-1-3_1.14.10-1~deb12u1_amd64.deb", "18ee0ce5fab9f7b671e87da1e9fa18660e36e04a3402f24bdb8635e0ba1d35f6"),
    ("pool/main/d/double-conversion/libdouble-conversion3_3.2.1-1_amd64.deb", "802b880ed900e4a8d54bab77b7551a7f3408b04891c3e5650fdc2368cc1fbc0a"),
    ("pool/main/libd/libdrm/libdrm-common_2.4.114-1_all.deb", "32f9664138b38b224383c6986457d5ad2ec8efd559b1a0ce7749405f7a451aad"),
    ("pool/main/libd/libdrm/libdrm2_2.4.114-1+b1_amd64.deb", "be18fb670797ba32da9628cf3e8acd83160d8db8c8dd842501dd8e401c3b5371"),
    ("pool/main/e/expat/libexpat1_2.5.0-1+deb12u2_amd64.deb", "2255e62fc22a86d2c544b8a3f516da9aee19383ad5742722ab4ce7f66a30dbc8"),
    ("pool/main/libf/libffi/libffi8_3.4.4-1_amd64.deb", "6d9f6c25c30efccce6d4bceaa48ea86c329a3432abb360a141f76ac223a4c34a"),
    ("pool/main/f/flac/libflac12_1.4.2+ds-2_amd64.deb", "4d8431c274eef13ec9ee2c0bf988c34b4342367dec053b7f69a432525be5b1fe"),
    ("pool/main/f/fontconfig/libfontconfig1_2.14.1-4_amd64.deb", "16ee38d374e064f534116dc442b086ef26f9831f1c0af7e5fb4fe4512e700649"),
    ("pool/main/f/freetype/libfreetype6_2.12.1+dfsg-5+deb12u4_amd64.deb", "8043e479f73f29992d652e3f9dfe8b17f9780c7ea6330afe379ec5f9f188ac44"),
    ("pool/main/f/fribidi/libfribidi0_1.0.8-2.1_amd64.deb", "87fce56627aab7b2968501d370aa3ed6d1c792119efa765e71a690bdfe570e62"),
    ("pool/main/m/mesa/libgbm1_22.3.6-1+deb12u2_amd64.deb", "4323840f2b7b3e108990848529125d99c0956ae5a854c1e0568ed08c8faeaa2e"),
    ("pool/main/libg/libgcrypt20/libgcrypt20_1.10.1-3+deb12u1_amd64.deb", "e7a56552139c693904a6f9712234c05fe918353273a7cbdb5e62f423ad71bb97"),
    ("pool/main/g/glib2.0/libglib2.0-0_2.74.6-2+deb12u9_amd64.deb", "7ff85197685d89e150e342b29a59aab1beee400050ba7da73de81cd999ffee5a"),
    ("pool/main/g/gnutls28/libgnutls30_3.7.9-2+deb12u7_amd64.deb", "30abec8c824feb1d2d7e9000a34083cccd19d139625e1b21547e3ac53b922f8e"),
    ("pool/main/libg/libgpg-error/libgpg-error0_1.46-1_amd64.deb", "89944ee11d7370ce6ef46fc52f094c4a6512eff8943ec4c6ebefeae6360ceada"),
    ("pool/main/g/graphite2/libgraphite2-3_1.3.14-1+deb12u1_amd64.deb", "c19a7f6ba9298db7eef041ae27b08985f2c02009e418063f8bccdb5bc5e858dc"),
    ("pool/main/k/krb5/libgssapi-krb5-2_1.20.1-2+deb12u5_amd64.deb", "d8b87aea91b956b416c01a28c44ff6228315ae3b73de40acddac3dc5043607d2"),
    ("pool/main/h/harfbuzz/libharfbuzz-subset0_6.0.0+dfsg-3_amd64.deb", "020de1eeba5fb56052a6ca71b259f8cbe3f6fce5038f4caca751c7e4867ca33f"),
    ("pool/main/h/harfbuzz/libharfbuzz0b_6.0.0+dfsg-3_amd64.deb", "bfce132b7ee67b9c2d2166075b1936a25c8cc6866b6a049f99b8e94baa916e71"),
    ("pool/main/n/nettle/libhogweed6_3.8.1-2_amd64.deb", "ed8185c28b2cb519744a5a462dcd720d3b332c9b88a1d0002eac06dc8550cb94"),
    ("pool/main/libi/libidn2/libidn2-0_2.3.3-1+b1_amd64.deb", "d50716d5824083d667427817d506b45d3f59dc77e1ca52de000f3f62d4918afa"),
    ("pool/main/libj/libjpeg-turbo/libjpeg62-turbo_2.1.5-2_amd64.deb", "95ec30140789a342add8f8371ed018924de51b539056522b66f207b25cba9cad"),
    ("pool/main/k/krb5/libk5crypto3_1.20.1-2+deb12u5_amd64.deb", "7b6e47d15c5c9cdcd456137533d8308aeba608a54cec73b28bd59bfc4b54cf06"),
    ("pool/main/k/keyutils/libkeyutils1_1.6.3-2_amd64.deb", "cfac89e6a7a54ff3c6a4f843310e25efeddaa771baeae470bd98bd588c373563"),
    ("pool/main/k/krb5/libkrb5-3_1.20.1-2+deb12u5_amd64.deb", "c90801be09701af221b54dd502a59cb378b7284ff31e480738baca317d2f7653"),
    ("pool/main/k/krb5/libkrb5support0_1.20.1-2+deb12u5_amd64.deb", "8b9c05f2d8d461a10d657b118f572af5bf8fd6c8f93dcef81e852c5ab0f428ab"),
    ("pool/main/l/lcms2/liblcms2-2_2.14-2+deb12u1_amd64.deb", "333efe13d165b8a98194d95e72fd10b502ebbee4dc2eb43fc2ab83e2b53e5f17"),
    ("pool/main/l/lz4/liblz4-1_1.9.4-1_amd64.deb", "64cde86cef1deaf828bd60297839b59710b5cd8dc50efd4f12643caaee9389d3"),
    ("pool/main/x/xz-utils/liblzma5_5.4.1-1+deb12u1_amd64.deb", "f96d8876b53ec89d76a992bc199679b818cf518225a768954d9451fb556a4eb7"),
    ("pool/main/libm/libmd/libmd0_1.0.4-2_amd64.deb", "03539fd30c509e27101d13a56e52eda9062bdf1aefe337c07ab56def25a13eab"),
    ("pool/main/m/minizip/libminizip1_1.1-8+deb12u1_amd64.deb", "b2999b980cbb9e6dcaa2d5154c9d1f1b9d55f5a0ca8166699de7ab47afaf4a36"),
    ("pool/main/u/util-linux/libmount1_2.38.1-5+deb12u3_amd64.deb", "f46e2d6b006951825adde9757790b6609d7c66c7700f9b68f3db913eaac323c3"),
    ("pool/main/l/lame/libmp3lame0_3.100-6_amd64.deb", "b1292c749172e7623547ecf684da57d3ec6ca22109f08b11bbb9e6be6e6beb95"),
    ("pool/main/m/mpg123/libmpg123-0_1.31.2-1+deb12u1_amd64.deb", "a10ad0d59995b859797a5f20d2833672051d0b2c7dab4c71f2e79258b2e3f631"),
    ("pool/main/n/nettle/libnettle8_3.8.1-2_amd64.deb", "45922e6e289ffd92f0f92d2bb9159e84236ff202d552a461bf10e5335b3f0261"),
    ("pool/main/n/nspr/libnspr4_4.35-1_amd64.deb", "6cca09767e94e4b5d2888ef31ad797bd3e7cec27fbbe98b49b10d4884746ce77"),
    ("pool/main/n/nss/libnss3_3.87.1-1+deb12u2_amd64.deb", "bb4339de2e76be9862225447c6c24b9d759baba5ee861d86dfb558ebf4474d14"),
    ("pool/main/libo/libogg/libogg0_1.3.5-3_amd64.deb", "f67acb0477aed0354f411172e37af338f3ac6bd4f3766134a2cf5539884b5a28"),
    ("pool/main/o/openh264/libopenh264-7_2.3.1+dfsg-3+deb12u2_amd64.deb", "d453c4d81b7b3ff14663da75471250a4894543ae5ab5e902e225aca56aec56e8"),
    ("pool/main/o/openjpeg2/libopenjp2-7_2.5.0-2+deb12u3_amd64.deb", "a127e2f1741d66bdddb6393a718e3cbec327d53f88c89b859b1f8d73f64cf635"),
    ("pool/main/o/opus/libopus0_1.3.1-3_amd64.deb", "c172e212f9039e741916aa8e12f3670d1e049dc0c16685325641288c2d83faa7"),
    ("pool/main/p/p11-kit/libp11-kit0_0.24.1-2_amd64.deb", "251330faddbf013f060fcdb41f4b0c037c8a6e89ba7c09b04bfcc4e3f0807b22"),
    ("pool/main/p/pango1.0/libpango-1.0-0_1.50.12+ds-1_amd64.deb", "851720de07441ae6bb6a7f51fc0f2edb4db7aa6f25b5bf1bf7b72dcab8947b7f"),
    ("pool/main/p/pixman/libpixman-1-0_0.42.2-1_amd64.deb", "9ed75ef81b6b1eba9e1357f8b0ce01d31112b0e07b2b685bee2cf627a8932892"),
    ("pool/main/libp/libpng1.6/libpng16-16_1.6.39-2+deb12u5_amd64.deb", "a56d64bfaa9da12aafb83347909e62e6fd5fd251e6b34c194065911a30359978"),
    ("pool/main/p/pulseaudio/libpulse0_16.1+dfsg1-2+b1_amd64.deb", "7b2c5403cb726312219aad678becc6d6adcee1d8694fdffce0b6ec15ae010831"),
    ("pool/main/libs/libsndfile/libsndfile1_1.2.0-1+deb12u1_amd64.deb", "02dc3f08fe8476e82e005f412e3b7289ff641180f33a9be8073f4bde61c85e8e"),
    ("pool/main/s/sqlite3/libsqlite3-0_3.40.1-2+deb12u2_amd64.deb", "a8d78b40e9b4e422224aeebfe0e4dfc243f6acf3532490b0c05480d4283d41e2"),
    ("pool/main/o/openssl/libssl3_3.0.20-1~deb12u2_amd64.deb", "89be24b41bff568ee6e7caf5680a3d808e80315ed92e407056ce0fa7a5bda025"),
    ("pool/main/g/gcc-12/libstdc++6_12.2.0-14+deb12u1_amd64.deb", "5cd3171216d4ab0fc911cfe9c35509bf2dd8f47761c43b7f6a4296701551a24d"),
    ("pool/main/s/systemd/libsystemd0_252.39-1~deb12u2_amd64.deb", "013c07514a9e4933b2d5c098018695a6aa42d085dbc483ce2c094a954c8a1b0a"),
    ("pool/main/libt/libtasn1-6/libtasn1-6_4.19.0-2+deb12u1_amd64.deb", "2c27c613b69be248d09432839ea3d3382f6ee27fabaa65987aa2d884cd00afda"),
    ("pool/main/libt/libthai/libthai-data_0.1.29-1_all.deb", "eed65a75269411e47d7b393d82bc30471da5c499e9f311abbfd8c54ca1a42d9e"),
    ("pool/main/libt/libthai/libthai0_0.1.29-1_amd64.deb", "37cd66bef851ea0e4af807797ba3ad14d43226f7c4954c1d0a19478e11815bae"),
    ("pool/main/s/systemd/libudev1_252.39-1~deb12u2_amd64.deb", "6f7e08f11c4809d2e5998c775f7b423ab5ec2ce0309494b4a2615037052245e2"),
    ("pool/main/libu/libunistring/libunistring2_1.0-2_amd64.deb", "d466bbfe011d764d793c1d9d777cad9c7cf65b938e11598f27408171ad95a951"),
    ("pool/main/libv/libvorbis/libvorbis0a_1.3.7-1_amd64.deb", "01c14f9d1109650077a4c5c337c285476a6932d4d448f5a190c2b845724dbf21"),
    ("pool/main/libv/libvorbis/libvorbisenc2_1.3.7-1_amd64.deb", "75dd4d6f904c7db82f5112e60d8efea9e81dfedf94e970c5da7af2b3d81643c0"),
    ("pool/main/w/wayland/libwayland-server0_1.21.0-1_amd64.deb", "df0396221d7b794496a687ec61fae82b6465648bc0ab6501ba0a5ed7f56eb8d6"),
    ("pool/main/libx/libx11/libx11-6_1.8.4-2+deb12u2_amd64.deb", "d88c973e79fd9b65838d77624142952757e47a6eb1a58602acf0911cf35989f4"),
    ("pool/main/libx/libx11/libx11-data_1.8.4-2+deb12u2_all.deb", "987a848aeb1c358e4186368871b0526f10bb14c6b53214ab3bf8b69abb830191"),
    ("pool/main/libx/libx11/libx11-xcb1_1.8.4-2+deb12u2_amd64.deb", "f5da45e1d881a793250a96613f28c471a248877f1a0f18a5c90e2a620a76c898"),
    ("pool/main/libx/libxau/libxau6_1.0.9-1_amd64.deb", "679db1c4579ec7c61079adeaae8528adeb2e4bf5465baa6c56233b995d714750"),
    ("pool/main/libx/libxcb/libxcb-render0_1.15-1_amd64.deb", "29c32cc76be9393f409378be79a0f534b9f07dea06ae52fa80a7f27e298de239"),
    ("pool/main/libx/libxcb/libxcb-shm0_1.15-1_amd64.deb", "c1afcef29dc78b95c475159b181b28b1dedaf1d5aa06efd2fa6d90c73bfbe0e5"),
    ("pool/main/libx/libxcb/libxcb1_1.15-1_amd64.deb", "fdc61332a3892168f3cc9cfa1fe9cf11a91dc3e0acacbc47cbc50ebaa234cc71"),
    ("pool/main/libx/libxcomposite/libxcomposite1_0.4.5-1_amd64.deb", "4c26ebf519d2ebc22fc1416dee45e12c4c4ef68aa9b2ed890356830df42b652a"),
    ("pool/main/libx/libxdamage/libxdamage1_1.1.6-1_amd64.deb", "fcdf7e4c29b307ba8711c864e1e646d76315a8f55121198143653e56592fa8a7"),
    ("pool/main/libx/libxdmcp/libxdmcp6_1.1.2-3_amd64.deb", "ecb8536f5fb34543b55bb9dc5f5b14c9dbb4150a7bddb3f2287b7cab6e9d25ef"),
    ("pool/main/libx/libxext/libxext6_1.3.4-1+b1_amd64.deb", "504b7be9d7df4f6f4519e8dd4d6f9d03a9fb911a78530fa23a692fba3058cba6"),
    ("pool/main/libx/libxfixes/libxfixes3_6.0.0-2_amd64.deb", "1cd616396ff2ecae77e6e8b5b7695d414f0146de2d147837a2a02165f99e1a2c"),
    ("pool/main/libx/libxi/libxi6_1.8-1+b1_amd64.deb", "6c2fb5f241b311788990122c6e693915867cd8ed9d6da2e3524aec7ed3cdb4c0"),
    ("pool/main/libx/libxkbcommon/libxkbcommon0_1.5.0-1_amd64.deb", "e3fe045b9a33a101de1c5a912a4a10928db055c3f68930f47eccbb44d7c7d54e"),
    ("pool/main/libx/libxnvctrl/libxnvctrl0_525.85.05-3~deb12u1_amd64.deb", "6fc26e4b6069f4648692e0c225f580161e3b0c04e4bac1c616c8a418a9a9817d"),
    ("pool/main/libx/libxrandr/libxrandr2_1.5.2-2+b1_amd64.deb", "e2e10b672085ddf1d294cd689ed21bde5899325fd13626c2614b7d7b2d49a808"),
    ("pool/main/libx/libxrender/libxrender1_0.9.10-1.1_amd64.deb", "b2a6d160578f359220101947c8d67059b44704a251e688de1d0b744fefedfd1a"),
    ("pool/main/libz/libzstd/libzstd1_1.5.4+dfsg2-5_amd64.deb", "6315b5ac38b724a710fb96bf1042019398cb656718b1522279a5185ed39318fa"),
    ("pool/main/x/xkeyboard-config/xkb-data_2.35.1-1_all.deb", "28a79c61b785f403da93af58c39aed13c022032935e3e54362c6707169cfe982"),
    ("pool/main/z/zlib/zlib1g_1.2.13.dfsg-1_amd64.deb", "d7dd1d1411fedf27f5e27650a6eff20ef294077b568f4c8c5e51466dc7c08ce4"),
];

/// Скачать пакеты, собрать библиотеки по `DT_NEEDED` и собрать
/// `linuxweb-150.fpk`.
pub fn build(arch: Arch) -> Result<()> {
    if arch != Arch::X86_64 {
        return Ok(());
    }
    let cache = sdk::root().join("thirdparty/debian");
    fs::create_dir_all(&cache)?;
    let mut base: BTreeMap<String, Node> = BTreeMap::new();
    unpack(&cache, &crate::debian::PACKAGES, &mut base)?;
    let mut extra: BTreeMap<String, Node> = BTreeMap::new();
    unpack(&cache, &PACKAGES, &mut extra)?;

    let binary = match follow(&extra, BINARY, 0) {
        Some((data, _)) => data.clone(),
        None => bail!("в пакетах нет {BINARY}"),
    };
    let mut files: Vec<(String, Vec<u8>, u32)> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut todo: Vec<String> = needed(&binary).context("chromium: не разобран ELF")?;
    todo.extend(NSS_MODULES.iter().map(|name| String::from(*name)));
    let mut from_base = 0;
    while let Some(name) = todo.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        if LIB_DIRS.iter().any(|dir| follow(&base, &format!("{dir}/{name}"), 0).is_some()) {
            from_base += 1;
            continue;
        }
        // Сначала обычные каталоги, потом — где угодно в пакетах: часть
        // библиотек лежит в своих подкаталогах и находится по `RUNPATH`
        // (`pulseaudio/libpulsecommon`); в пакете они ложатся в общий `lib`.
        let suffix = format!("/{name}");
        let found = LIB_DIRS.iter().find_map(|dir| follow(&extra, &format!("{dir}/{name}"), 0)).or_else(|| {
            extra.keys().find(|path| path.ends_with(&suffix)).and_then(|path| follow(&extra, path, 0))
        });
        let Some((data, mode)) = found else {
            // Модули NSS — не все есть в каждой сборке; остальное — отказ.
            if NSS_MODULES.contains(&name.as_str()) {
                continue;
            }
            bail!("библиотеки {name} нет ни в корне Debian, ни в пакетах Chromium");
        };
        todo.extend(needed(data).with_context(|| format!("{name}: не разобран ELF"))?);
        files.push((format!("lib/{name}"), data.clone(), mode));
    }
    say!("chromium: {} библиотек в пакет, {from_base} уже в корне Debian", files.len());

    for (path, node) in &extra {
        if let Some(name) = path.strip_prefix("usr/lib/chromium/") {
            let wanted = name == "chromium" || COMMON_FILES.contains(&name);
            if wanted && matches!(node, Node::File { .. }) {
                if let Some((data, mode)) = follow(&extra, path, 0) {
                    files.push((format!("chrome/{name}"), data.clone(), mode));
                }
            }
        }
    }
    for (path, node) in &extra {
        if let Some(name) = path.strip_prefix(FONT_DIR) {
            if matches!(node, Node::File { .. }) {
                if let Some((data, mode)) = follow(&extra, path, 0) {
                    files.push((format!("fonts/{name}"), data.clone(), mode));
                }
            }
        }
    }
    files.push((String::from("fonts/fonts.conf"), FONTS_CONF.as_bytes().to_vec(), 0o644));
    // `--jitless`: V8 без JIT. Ядро не даёт памяти «на запись и исполнение
    // сразу» (W^X), а JIT V8 просит именно её; интерпретатор её не просит.
    // Окно — через сервер Wayland ядра (фаза 71i); с `--headless` Chromium
    // окна не открывает, и платформа ему не нужна.
    // `--ipc-connection-timeout`: дочерний процесс ждёт соединения от
    // браузера 15 с и уходит; браузер, занятый профилем в отладочной сборке
    // под эмулятором, не успевает — сетевая служба и страницы уходили, не
    // начав работать.
    let script = "#!/bin/bash\n\
        # Chromium (фазы 71h, 71i): библиотеки и шрифты — рядом, песочницы нет.\n\
        export LD_LIBRARY_PATH=/opt/chromium/lib\n\
        export FONTCONFIG_FILE=/opt/chromium/fonts/fonts.conf\n\
        export XDG_RUNTIME_DIR=/run/user/0\n\
        export WAYLAND_DISPLAY=wayland-0\n\
        # Шины D-Bus в системе нет: адрес — сокет, которого нет, и отказ\n\
        # соединения Chromium разбирает сам (без адреса он падает).\n\
        export DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/0/bus\n\
        export DBUS_SYSTEM_BUS_ADDRESS=unix:path=/run/dbus/system_bus_socket\n\
        # Профиль и сокет профиля — в $HOME (дом того, кто вошёл): новое\n\
        # в / легло бы в корень Debian и пережило бы удаление пакета.\n\
        export HOME=${HOME:-/root}\n\
        export TMPDIR=${TMPDIR:-$HOME}\n\
        platform=--ozone-platform=wayland\n\
        case \" $* \" in *\" --headless\"*) platform= ;; esac\n\
        exec /opt/chromium/chrome/chromium --no-sandbox --no-zygote --disable-gpu $platform \\\n\
          --js-flags=--jitless --no-first-run --no-default-browser-check --password-store=basic --ipc-connection-timeout=120 \"$@\"\n";
    files.push((String::from("bin/chromium"), script.as_bytes().to_vec(), 0o755));

    let mut builder = Builder::new(Kind::Package, "chromium", "150");
    builder.field("summary", "Chromium 150 from Debian 12 with the libraries it loads; windows through the kernel Wayland server");
    builder.field("about", "Chromium для программ Linux: в окне стола и без окна");
    builder.requires("debian");
    let mut bytes = 0usize;
    for (path, data, mode) in &files {
        bytes += data.len();
        builder.file(&Entry { path: path.clone(), mode: (*mode & 0o7777) as u16, uid: 0, gid: 0, data: data.clone() });
    }
    let out = package_path();
    fs::create_dir_all(out.parent().unwrap())?;
    fs::write(&out, builder.finish())?;
    say!("chromium: {} — {} файлов, {} МиБ", out.display(), files.len(), bytes / (1024 * 1024));
    Ok(())
}

/// `DT_NEEDED` разделяемого объекта ELF64 (x86-64, little-endian).
fn needed(elf: &[u8]) -> Result<Vec<String>> {
    let word = |at: usize| -> Result<u64> {
        Ok(u64::from_le_bytes(elf.get(at..at + 8).context("короткий ELF")?.try_into()?))
    };
    let half = |at: usize| -> Result<u16> {
        Ok(u16::from_le_bytes(elf.get(at..at + 2).context("короткий ELF")?.try_into()?))
    };
    if elf.get(..4) != Some(b"\x7fELF".as_slice()) || elf.get(4) != Some(&2) {
        bail!("не ELF64");
    }
    let phoff = word(0x20)? as usize;
    let (phentsize, phnum) = (usize::from(half(0x36)?), usize::from(half(0x38)?));
    let mut loads = Vec::new();
    let mut dynamic = None;
    for index in 0..phnum {
        let at = phoff + index * phentsize;
        let kind = u32::from_le_bytes(elf.get(at..at + 4).context("короткий ELF")?.try_into()?);
        let (offset, vaddr, filesz) = (word(at + 8)?, word(at + 16)?, word(at + 32)?);
        match kind {
            1 => loads.push((vaddr, offset, filesz)),
            2 => dynamic = Some((offset as usize, filesz as usize)),
            _ => {}
        }
    }
    let Some((dyn_at, dyn_len)) = dynamic else {
        return Ok(Vec::new());
    };
    let to_offset = |vaddr: u64| {
        loads.iter().find(|(start, _, len)| vaddr >= *start && vaddr < start + len).map(|(start, offset, _)| (vaddr - start + offset) as usize)
    };
    let mut entries = Vec::new();
    for index in 0..dyn_len / 16 {
        entries.push((word(dyn_at + index * 16)? as i64, word(dyn_at + index * 16 + 8)?));
    }
    let strtab = entries.iter().find(|(tag, _)| *tag == 5).and_then(|(_, value)| to_offset(*value)).context("нет DT_STRTAB")?;
    let mut out = Vec::new();
    for (tag, value) in entries {
        if tag == 1 {
            let start = strtab + value as usize;
            let end = elf[start..].iter().position(|byte| *byte == 0).context("имя без нуля")? + start;
            out.push(String::from_utf8_lossy(&elf[start..end]).into_owned());
        }
    }
    Ok(out)
}
