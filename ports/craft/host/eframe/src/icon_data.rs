//! Значок окна из PNG (как `eframe::icon_data`). Окну FreeOS значок не
//! передаётся — его берут из пакета (`icon.fimg`), но приложения его готовят.

use egui::IconData;

pub trait IconDataExt {
    fn to_image(&self) -> Result<image::RgbaImage, String>;
    fn to_png_bytes(&self) -> Result<Vec<u8>, String>;
}

pub fn from_png_bytes(png_bytes: &[u8]) -> Result<IconData, image::ImageError> {
    let image = image::load_from_memory(png_bytes)?.into_rgba8();
    Ok(IconData { width: image.width(), height: image.height(), rgba: image.into_raw() })
}

impl IconDataExt for IconData {
    fn to_image(&self) -> Result<image::RgbaImage, String> {
        image::RgbaImage::from_raw(self.width, self.height, self.rgba.clone()).ok_or_else(|| "Invalid IconData".to_owned())
    }

    fn to_png_bytes(&self) -> Result<Vec<u8>, String> {
        let image = self.to_image()?;
        let mut png: Vec<u8> = Vec::new();
        image.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).map_err(|err| err.to_string())?;
        Ok(png)
    }
}
