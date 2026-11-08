/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! System font list for FreeOS (ROADMAP N5, 08.11.2026).
//!
//! Written for FreeOS after the shape of Servo's Android `font_list.rs`: no
//! Fontconfig (it would need `dlopen` and `/etc/fonts` configuration). Fonts
//! are found by walking the font directories once and reading each file's own
//! `name` and `OS/2` tables with `skrifa` (family, weight, width, style).
//! Copied into `components/fonts/platform/freetype/freeos/font_list.rs` by
//! `ports/servo/builder/patch-crates.sh`.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use fonts_traits::{FontTemplate, FontTemplateDescriptor, LocalFontIdentifier};
use log::debug;
use skrifa::attribute::Style;
use skrifa::raw::FileRef;
use skrifa::string::StringId;
use skrifa::MetadataProvider;
use style::Atom;
use style::values::computed::font::GenericFontFamily;
use style::values::computed::{FontStyle, FontWeight, FontWidth};

use crate::{FallbackFontSelectionOptions, FontIdentifier, LowercaseFontFamilyName};

/// Where FreeOS keeps fonts: the system set (`/usr/share/fonts`, DejaVu and
/// the desktop's fonts) and fonts a user installs.
const FONT_DIRS: &[&str] = &["/usr/share/fonts", "/usr/local/share/fonts"];

/// Generic families, in order of preference: the first one present wins.
const SANS_SERIF: &[&str] = &["Noto Sans", "DejaVu Sans", "Inter"];
const SERIF: &[&str] = &["DejaVu Serif", "Noto Serif"];
const MONOSPACE: &[&str] = &["DejaVu Sans Mono", "Noto Sans Mono", "JetBrains Mono", "Source Code Pro"];

struct Face {
    family: String,
    path: String,
    index: u16,
    weight: f32,
    width: f32,
    style: Style,
}

struct FontList {
    faces: Vec<Face>,
}

static FONT_LIST: LazyLock<FontList> = LazyLock::new(FontList::scan);

impl FontList {
    fn scan() -> FontList {
        let mut files = Vec::new();
        for dir in FONT_DIRS {
            walk(Path::new(dir), &mut files);
        }
        files.sort();
        let mut faces = Vec::new();
        for path in files {
            let Ok(data) = std::fs::read(&path) else {
                continue;
            };
            let Ok(file) = FileRef::new(&data) else {
                continue;
            };
            for (index, font) in file.fonts().enumerate() {
                let Ok(font) = font else {
                    continue;
                };
                let Some(family) = font
                    .localized_strings(StringId::TYPOGRAPHIC_FAMILY_NAME)
                    .english_or_first()
                    .or_else(|| font.localized_strings(StringId::FAMILY_NAME).english_or_first())
                else {
                    continue;
                };
                let attributes = font.attributes();
                faces.push(Face {
                    family: family.to_string(),
                    path: path.to_string_lossy().into_owned(),
                    index: index as u16,
                    weight: attributes.weight.value(),
                    width: attributes.stretch.ratio(),
                    style: attributes.style,
                });
            }
        }
        debug!("freeos font list: {} faces", faces.len());
        FontList { faces }
    }

    fn has_family(&self, name: &str) -> bool {
        self.faces.iter().any(|face| face.family.eq_ignore_ascii_case(name))
    }

    /// A family name or a generic name (`serif`, `sans-serif`, `monospace`…)
    /// → the family to take faces from.
    fn resolve<'a>(&'a self, name: &'a str) -> Option<&'a str> {
        if self.has_family(name) {
            return Some(name);
        }
        let candidates = match name.to_ascii_lowercase().as_str() {
            "sans-serif" | "system-ui" | "cursive" | "fantasy" => SANS_SERIF,
            "serif" => SERIF,
            "monospace" => MONOSPACE,
            _ => return None,
        };
        candidates.iter().copied().find(|family| self.has_family(family))
    }
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out);
        } else if path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| matches!(ext.to_ascii_lowercase().as_str(), "ttf" | "otf" | "ttc" | "otc"))
        {
            out.push(path);
        }
    }
}

pub(crate) fn for_each_available_family<F>(mut callback: F)
where
    F: FnMut(&str),
{
    let mut seen: Vec<&str> = Vec::new();
    for face in &FONT_LIST.faces {
        if !seen.iter().any(|family| family.eq_ignore_ascii_case(&face.family)) {
            seen.push(&face.family);
            callback(&face.family);
        }
    }
}

pub(crate) fn for_each_variation<F>(family_name: &str, mut callback: F)
where
    F: FnMut(FontTemplate),
{
    let Some(family) = FONT_LIST.resolve(family_name) else {
        return;
    };
    for face in FONT_LIST.faces.iter().filter(|face| face.family.eq_ignore_ascii_case(family)) {
        let local_font_identifier = LocalFontIdentifier {
            path: Atom::from(face.path.as_str()),
            face_index: face.index,
            named_instance_index: 0,
        };
        let style = match face.style {
            Style::Normal => FontStyle::NORMAL,
            Style::Italic => FontStyle::ITALIC,
            Style::Oblique(_) => FontStyle::OBLIQUE,
        };
        let descriptor = FontTemplateDescriptor::new(
            FontWeight::from_float(face.weight),
            FontWidth::from_percentage(face.width * 100.0),
            style,
        );
        callback(FontTemplate::new(
            FontIdentifier::Local(local_font_identifier),
            descriptor,
            None,
        ));
    }
}

/// Families to try for a character the page's fonts lack. DejaVu covers
/// Latin, Cyrillic, Greek, Armenian, Georgian, Hebrew, Arabic and many
/// symbols; Noto is added when installed.
pub fn fallback_font_families(_options: FallbackFontSelectionOptions) -> Vec<&'static str> {
    vec!["DejaVu Sans", "Noto Sans", "DejaVu Sans Mono", "Noto Sans Symbols", "Noto Sans CJK SC"]
}

pub(crate) fn default_system_generic_font_family(generic: GenericFontFamily) -> LowercaseFontFamilyName {
    match generic {
        GenericFontFamily::None | GenericFontFamily::Serif => "serif",
        GenericFontFamily::SansSerif | GenericFontFamily::SystemUi => "sans-serif",
        GenericFontFamily::Monospace => "monospace",
        GenericFontFamily::Cursive => "cursive",
        GenericFontFamily::Fantasy => "fantasy",
    }
    .into()
}
