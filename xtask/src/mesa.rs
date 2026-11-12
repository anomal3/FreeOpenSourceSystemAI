//! Mesa из Debian 13 для Proton (12.11.2026, шаги 5-6 плана): OpenGL/EGL
//! на процессоре (llvmpipe) и Vulkan на процессоре (lavapipe) - без
//! видеокарты, для проверки DirectX через wined3d и DXVK. Библиотеки
//! требуют glibc 2.38+ и новый libstdc++ (LLVM 19) - поэтому они в пакете
//! Proton, рядом с его glibc 2.41, а базовые библиотеки берутся из trixie
//! и перекрывают bookworm.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::debian::{Node, follow, unpack};

/// Пакеты trixie (снимок 20261020): путь в архиве и SHA-256 из Packages.
pub const PACKAGES: [(&str, &str); 52] = [
    ("pool/main/libg/libglvnd/libegl1_1.7.0-1+b2_amd64.deb", "62e7bc8ccd47a60e55aad17f83d8d474310d782eb8589bbce2ab714c31712406"),
    ("pool/main/libg/libglvnd/libgles2_1.7.0-1+b2_amd64.deb", "e12c18aa77939d122ee1dea520ba0440b97dc19200b7396e03dff03a3ece74c1"),
    ("pool/main/libg/libglvnd/libglvnd0_1.7.0-1+b2_amd64.deb", "887f74008166549ce9e100c906aa937e95d6e5ce1c8d86efe8c95fd953359b9c"),
    ("pool/main/m/mesa/libegl-mesa0_25.0.7-2+deb13u1_amd64.deb", "0ddebd7e91b048aac812611d6bd008343386cb140b51e5e66df74c6b6263be65"),
    ("pool/main/m/mesa/libgbm1_25.0.7-2+deb13u1_amd64.deb", "31fb6d76b9ceaf13848fa617df53f85f62626b4fe7464a93811c720af6d5f2dd"),
    ("pool/main/m/mesa/libgl1-mesa-dri_25.0.7-2+deb13u1_amd64.deb", "722fad9944bdfd29eb6421576331a2697f83bc4b3551d693edb7aefdde6bb198"),
    ("pool/main/m/mesa/mesa-libgallium_25.0.7-2+deb13u1_amd64.deb", "3e610f29321cdcc61337c86e6b4031ff60f0c9915fe19e6a862ac06480620ff4"),
    ("pool/main/m/mesa/mesa-vulkan-drivers_25.0.7-2+deb13u1_amd64.deb", "caf2f7d296b7522efe74e40a74bce762a3ce84c33b53b5e54e1248ac2e0a13a5"),
    ("pool/main/v/vulkan-loader/libvulkan1_1.4.309.0-1_amd64.deb", "f47da79cd140264fe21cceb08bc87a71bc7fec05819e4a421f3f518d21101a37"),
    ("pool/main/v/vulkan-tools/vulkan-tools_1.4.304.0+dfsg1-1_amd64.deb", "742d1cd78c333a94b8214e21ba455d47884dc32f110016ffe6998cf88f2a2f94"),
    ("pool/main/m/mesa-demos/mesa-utils-bin_9.0.0-2+b2_amd64.deb", "26ab6bf7ff781cb903a6b1b3518a1926a799024289853cc3693ee0dd922e1e13"),
    ("pool/main/l/llvm-toolchain-19/libllvm19_19.1.7-3+b1_amd64.deb", "db0d614d61345ca710fb73e75105f1ec6e38898fa7b3aa99fa7eb7d8b0a11d57"),
    ("pool/main/z/z3/libz3-4_4.13.3-1_amd64.deb", "71383373523ef62d47eccf660cf6535c3febcbb3f88e54cb7a43124014b57359"),
    ("pool/main/libd/libdrm/libdrm2_2.4.124-2_amd64.deb", "fe2276901c7cd7b8079de63072d37fe1cbeb4eb001a3bc1f1d662ad89aa0890e"),
    ("pool/main/libd/libdrm/libdrm-amdgpu1_2.4.124-2_amd64.deb", "c1d97a5e32e2bc68e833b8abeae026b6306198e87ee4396a7ffecd4779caefbf"),
    ("pool/main/e/elfutils/libelf1t64_0.192-4_amd64.deb", "94497b7e17b6f574a0605b380d454e20d3f01a9c63b70c2a2263f679d30053e1"),
    ("pool/main/l/lm-sensors/libsensors5_3.6.2-2_amd64.deb", "f0a994a6d7cfa695dea5343d0d1ba7eed796c0ad920c7282998b95f60049c4f6"),
    ("pool/main/w/wayland/libwayland-server0_1.23.1-3_amd64.deb", "2967212bd582e0dffca443fdc44f4c660e7368d41f7ee3a7f6314e0c3abfe9ea"),
    ("pool/main/libd/libdecor-0/libdecor-0-0_0.2.2-2_amd64.deb", "e4f228a9d4a38af23fab00f8e20219790b567e2e1f7804b3a215a26294350f82"),
    ("pool/main/g/gcc-14/libstdc++6_14.2.0-19_amd64.deb", "ab1fa05837aa7a92aae748fd07a18a35f7d18bb4a71c4724fe2bbf0e32089de0"),
    ("pool/main/g/gcc-14/libgcc-s1_14.2.0-19_amd64.deb", "3c71917b490d1a17aed43196a2787a256ecf060526cdb20216a74bedc061b150"),
    ("pool/main/libz/libzstd/libzstd1_1.5.7+dfsg-1_amd64.deb", "2f6a2aeacfc925eba8b00ac9139bc4bfccf8cacb09eb93de067074b26948eef9"),
    ("pool/main/z/zlib/zlib1g_1.3.dfsg+really1.3.1-1+b1_amd64.deb", "015be740d6236ad114582dea500c1d907f29e16d6db00566ca32fb68d71ac90d"),
    ("pool/main/libx/libxml2/libxml2_2.12.7+dfsg+really2.9.14-2.1+deb13u3_amd64.deb", "e0c6b63ce4602a036a526f60fe5e6c1586710688058d98fc1001b9b3147b7efd"),
    ("pool/main/libe/libedit/libedit2_3.1-20250104-1_amd64.deb", "b002ea172b9c1e34a67bc497c523c67bb74c3f0a4e98113cb083990a1f1d3bfe"),
    ("pool/main/n/ncurses/libtinfo6_6.5+20250216-2_amd64.deb", "8b9f6a7983e9418564e48a627518de4c03917b56efe68d7f3e93bd8fffa1cc10"),
    ("pool/main/libx/libx11/libx11-6_1.8.12-1_amd64.deb", "b5a3fd3bf8c8fd0364bfb9bea00dcba7fc301229bd02dded084632d31f5b0fb3"),
    ("pool/main/libx/libx11/libx11-xcb1_1.8.12-1_amd64.deb", "e05f94d21a932fba5b09b9b13d99df776b155d3bc792c0e294451df9ffe1ba25"),
    ("pool/main/libx/libxau/libxau6_1.0.11-1_amd64.deb", "689a9f0e0ba3e2c65431f864871e303ee904de69dd28abfc462663fae030227f"),
    ("pool/main/libx/libxdmcp/libxdmcp6_1.1.5-1_amd64.deb", "0740dc760916b2008b45417a42a8fd7dd5de370fb57d31373f15034cda8acf0b"),
    ("pool/main/libb/libbsd/libbsd0_0.12.2-2_amd64.deb", "e5a85986fa6bec3307ab1bc860736b478b331882bc45e17675a7bdf88eecb43a"),
    ("pool/main/libm/libmd/libmd0_1.1.0-2+b1_amd64.deb", "7244ec3839b61fac0c1884fe08aaa040f26e8f1f35f1f5d3482eacefd30d1b44"),
    ("pool/main/libx/libxcb/libxcb1_1.17.0-2+b1_amd64.deb", "5c222a72d11b866447da31693254f738430726e3e065a384e82687b2fd2f978b"),
    ("pool/main/libx/libxcb/libxcb-dri3-0_1.17.0-2+b1_amd64.deb", "f446d42fb5fcebbb3e347368ba83616769fdb271d85b2f49048e337f3163d267"),
    ("pool/main/libx/libxcb/libxcb-present0_1.17.0-2+b1_amd64.deb", "db95ea4630c55bd7f3281cb60ccf75b1627ef2f4399e1939f8f6161e584a92fb"),
    ("pool/main/libx/libxcb/libxcb-randr0_1.17.0-2+b1_amd64.deb", "f5d9fe5fdf797918f81f0abf6cfd4270bd54659540cddb4da709ab7154523214"),
    ("pool/main/libx/libxcb/libxcb-shm0_1.17.0-2+b1_amd64.deb", "d1777a813f484e89f3dcaadd1d5f2b1a13e9313b6bf70eded16d8ce743208fef"),
    ("pool/main/libx/libxcb/libxcb-sync1_1.17.0-2+b1_amd64.deb", "0ce4770ed1505be9ddc6473045e4662d062aff2f3077ee684265f01cfb543559"),
    ("pool/main/libx/libxcb/libxcb-xfixes0_1.17.0-2+b1_amd64.deb", "f9c1aafc18bf9e4662e34bbaf3bb00f1f4e8c2fc1dd786fdfb6cb9cb6c64fae7"),
    ("pool/main/libx/libxcb/libxcb-xkb1_1.17.0-2+b1_amd64.deb", "a46b8d860fbff32c8021dac6036d83859c24ad8700cd1b977cda116d37a2dd35"),
    ("pool/main/libx/libxshmfence/libxshmfence1_1.3.3-1_amd64.deb", "7b339e9e5b2349723d35af4df89bcc7aa456bbdf8ba1754358f9b44c3fe1f964"),
    ("pool/main/libx/libxext/libxext6_1.3.4-1+b3_amd64.deb", "fc618ec40465e5ce48622606299cb47833efc3fb235ba15543b81f850722f443"),
    ("pool/main/libx/libxxf86vm/libxxf86vm1_1.1.4-1+b4_amd64.deb", "f9f8487e536293e7ecf91fe7feb14e97e02a3aa652e207c661442dce775d3b14"),
    ("pool/main/libx/libxkbcommon/libxkbcommon-x11-0_1.7.0-2_amd64.deb", "b5cff6287d0d90b66224edfb7208709c5d35511215d87d2836919e4f7d367653"),
    ("pool/main/w/wayland/libwayland-egl1_1.23.1-3_amd64.deb", "7c5c3429028b39b9432792db08cb218c43891df439ebc648b83ebee676a80ec5"),
    ("pool/main/libd/libdrm/libdrm-intel1_2.4.124-2_amd64.deb", "188ab1fd74c838b3c8055d62107351e16a4bb7a25d39c30bbb9aac30ddd37238"),
    ("pool/main/libp/libpciaccess/libpciaccess0_0.17-3+b3_amd64.deb", "d9a0091071635a84e837051e4813005ac445071731becea28fba4d9806df5252"),
    ("pool/main/e/expat/libexpat1_2.8.3-1~deb13u1_amd64.deb", "38abe0e710a07688e9c149d74536e67cfee0364bdb64dd6d644c32a1cfad389f"),
    ("pool/main/libf/libffi/libffi8_3.4.8-2_amd64.deb", "0ebdc340de33333639c3c63874cd4b15ac2e83dfa1ef3053b7eefaf4919f4f68"),
    ("pool/main/x/xz-utils/liblzma5_5.8.1-1+deb13u1_amd64.deb", "1cfcc6e0dc36f438a79b6e2189facdb9d150b08f57d190a60e01c98075c7f896"),
    ("pool/main/a/alsa-lib/libasound2t64_1.2.14-1+deb13u1_amd64.deb", "5495496142d57e5ad6d581ac5456ec3c87f872c109a2bfb815d154b673eebc67"),
    ("pool/main/a/alsa-lib/libasound2-data_1.2.14-1+deb13u1_all.deb", "04688afdff3769c0f685541daed7b2f6f0cb946799ddf1d5847ddf11fe245559"),
];

/// Каталог библиотек в пакетах trixie (usrmerge).
const DIR: &str = "usr/lib/x86_64-linux-gnu/";

/// Чего из этих пакетов не класть: X11 и GLX (Wine на Wayland рисует через
/// EGL) и драйверы Vulkan видеокарт (нужен только lavapipe).
fn skipped(name: &str) -> bool {
    name.starts_with("libGLX")
        || name.starts_with("libGL.so")
        || (name.starts_with("libvulkan_") && !name.starts_with("libvulkan_lvp"))
}

/// Пробы: из usr/bin пакетов в bin/ с загрузчиком своей glibc.
const PROGRAMS: [&str; 5] = ["es2gears_wayland", "eglinfo", "vkgears", "vkcube", "vulkaninfo"];

/// Положить файл в пакет; такой же путь (библиотека bookworm) - заменить.
fn put(files: &mut Vec<(String, Vec<u8>, u32)>, path: String, data: Vec<u8>, mode: u32) {
    if let Some(entry) = files.iter_mut().find(|(have, _, _)| *have == path) {
        *entry = (path, data, mode);
    } else {
        files.push((path, data, mode));
    }
}

/// Положить Mesa в файлы пакета Proton: библиотеки - в lib/ (поверх таких же
/// из bookworm), драйверы DRI - в lib/dri/, описатели вендоров EGL и Vulkan -
/// в share/, пробы - в bin/. patch переписывает загрузчик программы на
/// /opt/proton/glibc/ld.so.
pub fn add(cache: &Path, files: &mut Vec<(String, Vec<u8>, u32)>, patch: fn(&mut [u8]) -> bool) -> Result<()> {
    let mut mesa: BTreeMap<String, Node> = BTreeMap::new();
    unpack(cache, &PACKAGES, &mut mesa)?;
    let paths: Vec<String> = mesa.keys().cloned().collect();
    let (mut libs, mut drivers) = (0, 0);
    for path in &paths {
        let Some(rest) = path.strip_prefix(DIR) else { continue };
        if let Some(name) = rest.strip_prefix("dri/") {
            if name.ends_with(".so") {
                if let Some((data, mode)) = follow(&mesa, path, 0) {
                    put(files, format!("lib/dri/{name}"), data.clone(), mode);
                    drivers += 1;
                }
            }
            continue;
        }
        if rest.contains("/") || !rest.contains(".so") || skipped(rest) {
            continue;
        }
        if let Some((data, mode)) = follow(&mesa, path, 0) {
            put(files, format!("lib/{rest}"), data.clone(), mode);
            libs += 1;
        }
    }
    // Настройки ALSA (libasound2-data) - в share/alsa/ (ALSA_CONFIG_DIR) и
    // своё: «default» - plug поверх единственного устройства ядра (S16_LE,
    // 2 канала, 48 кГц, `linux/snd.rs`), остальное libasound переведёт сама.
    let mut alsa = 0;
    for path in &paths {
        let Some(rest) = path.strip_prefix("usr/share/alsa/") else { continue };
        if let Some((data, mode)) = follow(&mesa, path, 0) {
            put(files, format!("share/alsa/{rest}"), data.clone(), mode);
            alsa += 1;
        }
    }
    let fossai = "pcm.!default { type plug slave.pcm \"hw:0,0\" }\nctl.!default { type hw card 0 }\n";
    put(files, String::from("share/alsa/fossai.conf"), fossai.as_bytes().to_vec(), 0o644);
    say!("mesa: настроек ALSA {alsa}");
    // Вендор EGL (glvnd) - Mesa по имени библиотеки: она в LD_LIBRARY_PATH.
    let egl = "{\"file_format_version\" : \"1.0.0\", \"ICD\" : { \"library_path\" : \"libEGL_mesa.so.0\" }}\n";
    put(files, String::from("share/glvnd/egl_vendor.d/50_mesa.json"), egl.as_bytes().to_vec(), 0o644);
    // Драйвер Vulkan - lavapipe.
    let vk = "{\"file_format_version\" : \"1.0.0\", \"ICD\" : { \"library_path\" : \"/opt/proton/lib/libvulkan_lvp.so\", \"api_version\" : \"1.4.305\" }}\n";
    put(files, String::from("share/vulkan/icd.d/lvp_icd.x86_64.json"), vk.as_bytes().to_vec(), 0o644);
    for name in PROGRAMS {
        // mesa-utils-bin кладёт их с суффиксом архитектуры.
        let found = follow(&mesa, &format!("usr/bin/{name}"), 0).or_else(|| follow(&mesa, &format!("usr/bin/{name}.x86_64-linux-gnu"), 0));
        let Some((data, mode)) = found else {
            say!("mesa: в пакетах нет usr/bin/{name}");
            continue;
        };
        let mut data = data.clone();
        if !patch(&mut data) {
            say!("mesa: у {name} нет загрузчика Debian");
            continue;
        }
        put(files, format!("bin/{name}"), data, mode);
    }
    say!("mesa: {libs} библиотек, {drivers} драйверов DRI");
    Ok(())
}
