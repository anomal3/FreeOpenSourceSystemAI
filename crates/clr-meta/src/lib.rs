//! Чтение сборок .NET: первый слой своей среды выполнения (веха v0.7c, фаза N1).
//!
//! # Что здесь
//!
//! Сборка, которую пишет `dotnet build`, — это файл PE/COFF (тот же формат, что у
//! любого `.exe` Windows), внутри которого лежит заголовок CLI, а за ним —
//! метаданные ECMA-335: таблицы типов, методов и ссылок, четыре кучи (строки
//! имён, строки программы, двоичные сигнатуры, GUID) и тела методов на IL. Этот
//! крейт читает всё перечисленное и ничего не исполняет: интерпретатор IL —
//! фаза N2, и он будет брать отсюда байты кода и описание типов.
//!
//! # Почему без кучи и без зависимостей
//!
//! Разбор обязан работать там же, где будет среда, — в программе третьего
//! кольца, и в тестах на машине разработчика. Всё, что он отдаёт, — срезы
//! исходного файла и числа; копировать сборку в структуры незачем, она и так
//! лежит в памяти целиком.
//!
//! # Чем проверено
//!
//! Не своими же тестами: `cargo xtask clr-check` печатает одни и те же факты
//! о сборке нашим разбором и `System.Reflection.Metadata` от Microsoft и
//! сверяет построчно — на пробных программах, `System.Private.CoreLib` и
//! `System.Windows.Forms`. Ошибка в ширине одного столбца таблицы сдвигает все
//! строки за ней, и такая сверка ловит её сразу.

#![no_std]

#[cfg(test)]
extern crate std;

pub mod body;
mod bytes;
pub mod heaps;
pub mod metadata;
pub mod pe;
pub mod sig;
pub mod tables;

#[cfg(test)]
mod tests;

pub use body::{ExceptionClause, MethodBody};
pub use bytes::{compressed_i32, compressed_u32};
pub use heaps::{Blobs, Guids, Strings, UserString, UserStrings};
pub use metadata::Root;
pub use pe::{CliHeader, Image, Section};
pub use tables::{Coded, Tables, Token};

/// Что не так со сборкой.
///
/// Строка внутри называет **место** — какой структуре не хватило байтов или
/// какая сигнатура не сошлась, — потому что «файл повреждён» без места ничего
/// не говорит ни человеку, ни тому, кто будет чинить разбор.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Файл кончился раньше, чем структура.
    Truncated(&'static str),
    /// Не та сигнатура в начале структуры.
    BadMagic(&'static str),
    /// Файл PE, но не управляемый: заголовка CLI нет.
    NotManaged,
    /// Адрес внутри образа не попадает ни в один раздел.
    RvaOutside(u32),
    /// В метаданных таблица, которой нет в ECMA-335.
    UnknownTable(u8),
    /// Куча повреждена: нет завершающего нуля, неверная длина, не UTF-8.
    BadHeap(&'static str),
    /// Номер строки или столбца за пределами таблицы.
    BadIndex(&'static str),
    /// Тело метода не разбирается.
    BadBody(&'static str),
    /// Сигнатура не разбирается.
    BadSignature(&'static str),
}

/// Сборка целиком: образ, заголовок CLI, корень метаданных и таблицы.
#[derive(Clone, Copy)]
pub struct Assembly<'a> {
    pub image: Image<'a>,
    pub cli: CliHeader,
    pub root: Root<'a>,
    pub tables: Tables<'a>,
}

impl<'a> Assembly<'a> {
    /// Разобрать сборку из байтов файла.
    pub fn parse(data: &'a [u8]) -> Result<Self, Error> {
        let image = Image::parse(data)?;
        let cli = CliHeader::parse(image.at_rva(image.cli_rva, pe::CLI_HEADER_SIZE)?)?;
        let metadata = image.at_rva(cli.metadata_rva, cli.metadata_size)?;
        let root = Root::parse(metadata)?;
        let tables = Tables::parse(root.tables, root.tables_offset)?;
        Ok(Self { image, cli, root, tables })
    }

    /// Тело метода по его RVA из таблицы `MethodDef`. Ноль — тела нет
    /// (абстрактный метод, `extern`, метод среды выполнения).
    pub fn method_body(&self, rva: u32) -> Result<Option<MethodBody<'a>>, Error> {
        if rva == 0 {
            return Ok(None);
        }
        MethodBody::parse(self.image.from_rva(rva)?).map(Some)
    }
}
