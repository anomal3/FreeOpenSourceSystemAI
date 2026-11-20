//! rfd для FossAi (FreeOS): тот же API (`FileDialog`, `AsyncFileDialog`,
//! `FileHandle`, `MessageDialog`, `AsyncMessageDialog`), что у rfd 0.17, в
//! объёме приложений Craft.
//!
//! Системного окна выбора файла у FreeOS пока нет (у «Файлов» нет вызова для
//! других программ), поэтому диалог — отдельное окно стола со своим egui
//! (`eframe::freeos::run_modal`): папки, файлы с фильтром по расширению, имя
//! файла для сохранения. Асинхронные диалоги показываются, когда их будущее
//! впервые опрашивают, — у Craft это поток `craft-dialogs`, главное окно в это
//! время продолжает рисоваться.

use std::path::{Path, PathBuf};

use raw_window_handle::{HasDisplayHandle, HasWindowHandle};

#[cfg(target_os = "freeos")]
mod picker;

/// Фильтр «имя — расширения».
#[derive(Debug, Clone)]
pub(crate) struct Filter {
    pub name: String,
    pub extensions: Vec<String>,
}

/// Что выбирают.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    File,
    Files,
    Folder,
    Folders,
    Save,
}

/// Окно выбора файла (блокирующее).
#[derive(Debug, Clone, Default)]
pub struct FileDialog {
    pub(crate) filters: Vec<Filter>,
    pub(crate) directory: Option<PathBuf>,
    pub(crate) file_name: Option<String>,
    pub(crate) title: Option<String>,
    pub(crate) can_create_directories: bool,
}

impl FileDialog {
    pub fn new() -> Self {
        Self { can_create_directories: true, ..Default::default() }
    }

    pub fn add_filter(mut self, name: impl Into<String>, extensions: &[impl ToString]) -> Self {
        let extensions = extensions.iter().map(|e| plain_extension(&e.to_string())).collect();
        self.filters.push(Filter { name: name.into(), extensions });
        self
    }

    pub fn set_directory<P: AsRef<Path>>(mut self, path: P) -> Self {
        let path = path.as_ref();
        if !path.as_os_str().is_empty() {
            self.directory = Some(path.to_path_buf());
        }
        self
    }

    pub fn set_file_name(mut self, file_name: impl Into<String>) -> Self {
        self.file_name = Some(file_name.into());
        self
    }

    pub fn set_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Родителя у окна FreeOS не бывает — диалог отдельным окном поверх.
    pub fn set_parent<W: HasWindowHandle + HasDisplayHandle + ?Sized>(self, _parent: &W) -> Self {
        self
    }

    pub fn set_can_create_directories(mut self, can: bool) -> Self {
        self.can_create_directories = can;
        self
    }

    pub fn pick_file(self) -> Option<PathBuf> {
        self.run(Mode::File).and_then(|v| v.into_iter().next())
    }

    pub fn pick_files(self) -> Option<Vec<PathBuf>> {
        self.run(Mode::Files)
    }

    pub fn pick_folder(self) -> Option<PathBuf> {
        self.run(Mode::Folder).and_then(|v| v.into_iter().next())
    }

    pub fn pick_folders(self) -> Option<Vec<PathBuf>> {
        self.run(Mode::Folders)
    }

    pub fn save_file(self) -> Option<PathBuf> {
        self.run(Mode::Save).and_then(|v| v.into_iter().next())
    }

    fn run(self, mode: Mode) -> Option<Vec<PathBuf>> {
        #[cfg(target_os = "freeos")]
        {
            picker::pick(self, mode)
        }
        #[cfg(not(target_os = "freeos"))]
        {
            let _ = mode;
            None
        }
    }
}

/// Расширение без `*.`/`.`, в нижнем регистре; классы знаков, которыми
/// приложения пишут «без учёта регистра» (`[pP][dD][fF]` у PdfCraft), — первой
/// буквой: `pdf`.
pub(crate) fn plain_extension(pattern: &str) -> String {
    let pattern = pattern.trim_start_matches("*.").trim_start_matches('.');
    let mut out = String::new();
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        if c == '[' {
            let class: String = chars.by_ref().take_while(|&c| c != ']').collect();
            if let Some(first) = class.chars().next() {
                out.push(first);
            }
        } else {
            out.push(c);
        }
    }
    out.to_lowercase()
}

/// Окно выбора файла (асинхронное: показывается при первом опросе).
#[derive(Debug, Clone, Default)]
pub struct AsyncFileDialog {
    file: FileDialog,
}

impl AsyncFileDialog {
    pub fn new() -> Self {
        Self { file: FileDialog::new() }
    }

    pub fn add_filter(self, name: impl Into<String>, extensions: &[impl ToString]) -> Self {
        Self { file: self.file.add_filter(name, extensions) }
    }

    pub fn set_directory<P: AsRef<Path>>(self, path: P) -> Self {
        Self { file: self.file.set_directory(path) }
    }

    pub fn set_file_name(self, file_name: impl Into<String>) -> Self {
        Self { file: self.file.set_file_name(file_name) }
    }

    pub fn set_title(self, title: impl Into<String>) -> Self {
        Self { file: self.file.set_title(title) }
    }

    pub fn set_parent<W: HasWindowHandle + HasDisplayHandle + ?Sized>(self, _parent: &W) -> Self {
        self
    }

    pub fn set_can_create_directories(self, can: bool) -> Self {
        Self { file: self.file.set_can_create_directories(can) }
    }

    pub fn pick_file(self) -> impl Future<Output = Option<FileHandle>> + Send {
        let file = self.file;
        async move { file.pick_file().map(FileHandle::from) }
    }

    pub fn pick_files(self) -> impl Future<Output = Option<Vec<FileHandle>>> + Send {
        let file = self.file;
        async move { file.pick_files().map(|v| v.into_iter().map(FileHandle::from).collect()) }
    }

    pub fn pick_folder(self) -> impl Future<Output = Option<FileHandle>> + Send {
        let file = self.file;
        async move { file.pick_folder().map(FileHandle::from) }
    }

    pub fn pick_folders(self) -> impl Future<Output = Option<Vec<FileHandle>>> + Send {
        let file = self.file;
        async move { file.pick_folders().map(|v| v.into_iter().map(FileHandle::from).collect()) }
    }

    pub fn save_file(self) -> impl Future<Output = Option<FileHandle>> + Send {
        let file = self.file;
        async move { file.save_file().map(FileHandle::from) }
    }
}

/// Выбранный файл (как у rfd на настольных ОС — путь).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileHandle(PathBuf);

impl FileHandle {
    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn file_name(&self) -> String {
        self.0.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    pub async fn read(&self) -> Vec<u8> {
        std::fs::read(&self.0).unwrap_or_default()
    }

    pub async fn write(&self, data: &[u8]) -> std::io::Result<()> {
        std::fs::write(&self.0, data)
    }

    #[cfg(feature = "file-handle-inner")]
    pub fn inner(&self) -> &Path {
        &self.0
    }
}

impl From<PathBuf> for FileHandle {
    fn from(path: PathBuf) -> Self {
        Self(path)
    }
}

impl From<&Path> for FileHandle {
    fn from(path: &Path) -> Self {
        Self(path.to_path_buf())
    }
}

impl From<FileHandle> for PathBuf {
    fn from(handle: FileHandle) -> Self {
        handle.0
    }
}

/// Значок сообщения.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MessageLevel {
    #[default]
    Info,
    Warning,
    Error,
}

/// Кнопки сообщения.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum MessageButtons {
    #[default]
    Ok,
    OkCancel,
    YesNo,
    YesNoCancel,
    OkCustom(String),
    OkCancelCustom(String, String),
    YesNoCancelCustom(String, String, String),
}

/// Ответ на сообщение.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageDialogResult {
    Yes,
    No,
    Ok,
    Cancel,
    Custom(String),
}

/// Сообщение (блокирующее).
#[derive(Debug, Clone, Default)]
pub struct MessageDialog {
    pub(crate) level: MessageLevel,
    pub(crate) title: String,
    pub(crate) description: String,
    pub(crate) buttons: MessageButtons,
}

impl MessageDialog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_level(mut self, level: MessageLevel) -> Self {
        self.level = level;
        self
    }

    pub fn set_title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    pub fn set_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    pub fn set_buttons(mut self, buttons: MessageButtons) -> Self {
        self.buttons = buttons;
        self
    }

    pub fn set_parent<W: HasWindowHandle + HasDisplayHandle + ?Sized>(self, _parent: &W) -> Self {
        self
    }

    pub fn show(self) -> MessageDialogResult {
        #[cfg(target_os = "freeos")]
        {
            picker::message(self)
        }
        #[cfg(not(target_os = "freeos"))]
        {
            cancel_of(&self.buttons)
        }
    }
}

/// Ответ «закрыли окно крестиком» — как отмена (или «Ок», если отмены нет).
pub(crate) fn cancel_of(buttons: &MessageButtons) -> MessageDialogResult {
    match buttons {
        MessageButtons::Ok => MessageDialogResult::Ok,
        MessageButtons::OkCustom(ok) => MessageDialogResult::Custom(ok.clone()),
        MessageButtons::YesNo => MessageDialogResult::No,
        MessageButtons::OkCancelCustom(_, cancel) | MessageButtons::YesNoCancelCustom(_, _, cancel) => MessageDialogResult::Custom(cancel.clone()),
        MessageButtons::OkCancel | MessageButtons::YesNoCancel => MessageDialogResult::Cancel,
    }
}

/// Сообщение (асинхронное: показывается при первом опросе).
#[derive(Debug, Clone, Default)]
pub struct AsyncMessageDialog {
    dialog: MessageDialog,
}

impl AsyncMessageDialog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_level(self, level: MessageLevel) -> Self {
        Self { dialog: self.dialog.set_level(level) }
    }

    pub fn set_title(self, title: impl Into<String>) -> Self {
        Self { dialog: self.dialog.set_title(title) }
    }

    pub fn set_description(self, description: impl Into<String>) -> Self {
        Self { dialog: self.dialog.set_description(description) }
    }

    pub fn set_buttons(self, buttons: MessageButtons) -> Self {
        Self { dialog: self.dialog.set_buttons(buttons) }
    }

    pub fn set_parent<W: HasWindowHandle + HasDisplayHandle + ?Sized>(self, _parent: &W) -> Self {
        self
    }

    pub fn show(self) -> impl Future<Output = MessageDialogResult> + Send {
        let dialog = self.dialog;
        async move { dialog.show() }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn extensions_are_plain() {
        assert_eq!(super::plain_extension("[pP][dD][fF]"), "pdf");
        assert_eq!(super::plain_extension("*.XLSX"), "xlsx");
        assert_eq!(super::plain_extension(".csv"), "csv");
    }
}
