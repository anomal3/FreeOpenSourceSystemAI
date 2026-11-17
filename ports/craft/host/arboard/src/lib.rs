//! arboard для FossAi (FreeOS): тот же API (`Clipboard`, `Get`, `Set`,
//! `ImageData`, `Error`), что у arboard 3.x, в объёме приложений Craft.
//!
//! Текст — в общем буфере обмена системы (`SYS_CLIPBOARD`: терминал,
//! Блокнот, «Файлы», OBSOS). Форматов у системного буфера пока нет, поэтому
//! HTML и картинка живут внутри программы и считаются действительными, пока
//! текст в системном буфере тот же, что положили вместе с ними (иначе
//! кто-то другой уже скопировал своё).

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::Mutex;

/// Картинка буфера: RGBA по 4 байта, строки подряд.
#[derive(Debug, Clone)]
pub struct ImageData<'a> {
    pub width: usize,
    pub height: usize,
    pub bytes: Cow<'a, [u8]>,
}

impl ImageData<'_> {
    pub fn to_owned_img(&self) -> ImageData<'static> {
        ImageData { width: self.width, height: self.height, bytes: Cow::Owned(self.bytes.to_vec()) }
    }
}

/// Ошибки — те же варианты, что у arboard.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    ContentNotAvailable,
    ClipboardNotSupported,
    ClipboardOccupied,
    ConversionFailure,
    Unknown { description: String },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::ContentNotAvailable => f.write_str("The clipboard contents were not available in the requested format or the clipboard is empty."),
            Error::ClipboardNotSupported => f.write_str("The selected clipboard is not supported with the current system configuration."),
            Error::ClipboardOccupied => f.write_str("The native clipboard is not accessible due to being held by another party."),
            Error::ConversionFailure => f.write_str("The image or the text that was about the be transferred to/from the clipboard could not be converted to the appropriate format."),
            Error::Unknown { description } => write!(f, "Unknown error while interacting with the clipboard: {description}"),
        }
    }
}

impl std::error::Error for Error {}

/// Что положено вместе с текстом (внутри программы).
struct Extra {
    text: String,
    html: Option<String>,
    image: Option<ImageData<'static>>,
}

static EXTRA: Mutex<Option<Extra>> = Mutex::new(None);

fn system_get() -> Option<String> {
    #[cfg(target_os = "freeos")]
    {
        eframe::freeos::clipboard_get()
    }
    #[cfg(not(target_os = "freeos"))]
    {
        EXTRA.lock().ok()?.as_ref().map(|e| e.text.clone())
    }
}

fn system_set(text: &str) {
    #[cfg(target_os = "freeos")]
    eframe::freeos::clipboard_set(text);
    #[cfg(not(target_os = "freeos"))]
    let _ = text;
}

fn put(text: String, html: Option<String>, image: Option<ImageData<'static>>) {
    system_set(&text);
    if let Ok(mut extra) = EXTRA.lock() {
        *extra = Some(Extra { text, html, image });
    }
}

/// HTML/картинка, если системный буфер всё ещё держит наш текст.
fn current_extra<T>(pick: impl FnOnce(&Extra) -> Option<T>) -> Option<T> {
    let now = system_get().unwrap_or_default();
    let extra = EXTRA.lock().ok()?;
    let extra = extra.as_ref()?;
    (extra.text == now).then(|| pick(extra)).flatten()
}

/// Буфер обмена.
pub struct Clipboard {
    _private: (),
}

impl Clipboard {
    pub fn new() -> Result<Self, Error> {
        Ok(Clipboard { _private: () })
    }

    pub fn get_text(&mut self) -> Result<String, Error> {
        self.get().text()
    }

    pub fn set_text<'a, T: Into<Cow<'a, str>>>(&mut self, text: T) -> Result<(), Error> {
        self.set().text(text)
    }

    pub fn set_html<'a, T: Into<Cow<'a, str>>>(&mut self, html: T, alt_text: Option<T>) -> Result<(), Error> {
        self.set().html(html, alt_text)
    }

    pub fn get_image(&mut self) -> Result<ImageData<'static>, Error> {
        self.get().image()
    }

    pub fn set_image(&mut self, image: ImageData<'_>) -> Result<(), Error> {
        self.set().image(image)
    }

    pub fn clear(&mut self) -> Result<(), Error> {
        put(String::new(), None, None);
        Ok(())
    }

    pub fn get(&mut self) -> Get<'_> {
        Get { _clipboard: self }
    }

    pub fn set(&mut self) -> Set<'_> {
        Set { _clipboard: self }
    }

    pub fn clear_with(&mut self) -> Clear<'_> {
        Clear { _clipboard: self }
    }
}

/// Чтение (`clipboard.get().text()` и т. п.).
pub struct Get<'clipboard> {
    _clipboard: &'clipboard mut Clipboard,
}

impl Get<'_> {
    pub fn text(self) -> Result<String, Error> {
        system_get().filter(|t| !t.is_empty()).ok_or(Error::ContentNotAvailable)
    }

    pub fn html(self) -> Result<String, Error> {
        current_extra(|e| e.html.clone()).ok_or(Error::ContentNotAvailable)
    }

    pub fn image(self) -> Result<ImageData<'static>, Error> {
        current_extra(|e| e.image.clone()).ok_or(Error::ContentNotAvailable)
    }

    /// Файлы из «Файлов» буфер пока не переносит.
    pub fn file_list(self) -> Result<Vec<PathBuf>, Error> {
        Err(Error::ContentNotAvailable)
    }
}

/// Запись (`clipboard.set().html(...)` и т. п.).
pub struct Set<'clipboard> {
    _clipboard: &'clipboard mut Clipboard,
}

impl Set<'_> {
    pub fn text<'a, T: Into<Cow<'a, str>>>(self, text: T) -> Result<(), Error> {
        put(text.into().into_owned(), None, None);
        Ok(())
    }

    pub fn html<'a, T: Into<Cow<'a, str>>>(self, html: T, alt_text: Option<T>) -> Result<(), Error> {
        let html = html.into().into_owned();
        let text = alt_text.map(|t| t.into().into_owned()).unwrap_or_else(|| strip_tags(&html));
        put(text, Some(html), None);
        Ok(())
    }

    pub fn image(self, image: ImageData<'_>) -> Result<(), Error> {
        if image.bytes.len() != image.width * image.height * 4 {
            return Err(Error::ConversionFailure);
        }
        // Текст-метка: по ней узнаётся, что картинка всё ещё «наша».
        let label = format!("[image {}x{}]", image.width, image.height);
        put(label, None, Some(image.to_owned_img()));
        Ok(())
    }

    pub fn file_list(self, _paths: &[impl AsRef<std::path::Path>]) -> Result<(), Error> {
        Err(Error::ClipboardNotSupported)
    }
}

/// Очистка (`clipboard.clear_with().default()`).
pub struct Clear<'clipboard> {
    _clipboard: &'clipboard mut Clipboard,
}

impl Clear<'_> {
    pub fn default(self) -> Result<(), Error> {
        put(String::new(), None, None);
        Ok(())
    }
}

/// Текст HTML без тегов (если `alt_text` не дали).
fn strip_tags(html: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}
