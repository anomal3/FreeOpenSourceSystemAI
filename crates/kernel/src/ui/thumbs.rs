//! Миниатюры окон над кнопкой панели задач (фаза 72e, макет 04).
//!
//! Карточка на окно: заголовок (значок, имя, у карточки под указателем —
//! красный крестик) и уменьшенная копия окна. Рисуются композитором прямо из
//! поверхностей окон при сборке кадра — своей поверхности у полосы нет, и
//! копия всегда та, что сейчас на экране.
//!
//! Размеры — из таблицы макета: миниатюра 200×124 и заголовок 24, карточка
//! 216, отступы и зазоры 8, скругление 8, крестик 24×24, полоса — над кнопкой
//! по центру, в 8 точках от панели.

use alloc::vec::Vec;

use mini_ui::Rect;

use super::window::App;

/// Ширина карточки.
const CARD_W: u32 = 216;
/// Высота заголовка карточки.
pub const HEAD_H: u32 = 24;
/// Миниатюра.
pub const SHOT_W: u32 = 200;
pub const SHOT_H: u32 = 124;
/// Отступ и зазор.
pub const PAD: u32 = 8;
/// Крестик.
pub const CROSS: u32 = 24;

/// Полоса миниатюр.
pub struct Thumbs {
    /// Окна по карточкам слева направо.
    pub apps: Vec<App>,
    /// Где полоса на экране.
    pub rect: Rect,
    /// Карточки в координатах экрана.
    pub cards: Vec<Rect>,
    /// Карточка под указателем и стоит ли он на её крестике.
    pub hovered: Option<usize>,
    pub cross: bool,
    /// Кнопка панели, над которой полоса.
    pub button: Rect,
}

impl Thumbs {
    /// Разложить карточки над `button`: полоса по центру кнопки, не шире
    /// экрана, низ — в `PAD` над `panel_top`.
    #[must_use]
    pub fn new(apps: Vec<App>, button: Rect, screen_w: u32, panel_top: i32, scale: u32) -> Self {
        let px = |value: u32| value * scale;
        let count = apps.len().max(1) as u32;
        let card_h = px(PAD + HEAD_H + SHOT_H + PAD);
        // Сколько карточек влезает в ширину экрана.
        let fits = ((screen_w.saturating_sub(px(PAD))) / px(CARD_W + PAD)).max(1);
        let shown = count.min(fits);
        let width = px(PAD) + shown * px(CARD_W + PAD);
        let height = card_h + px(PAD) * 2;
        let x = (button.x + button.w as i32 / 2 - width as i32 / 2).clamp(0, (screen_w as i32 - width as i32).max(0));
        let y = panel_top - px(PAD) as i32 - height as i32;
        let rect = Rect::new(x, y, width, height);
        let cards = (0..shown)
            .map(|index| {
                Rect::new(x + px(PAD) as i32 + (index * px(CARD_W + PAD)) as i32, y + px(PAD) as i32, px(CARD_W), card_h)
            })
            .collect();
        let mut apps = apps;
        apps.truncate(shown as usize);
        Self { apps, rect, cards, hovered: None, cross: false, button }
    }

    /// Крестик карточки `card` (координаты экрана).
    #[must_use]
    pub fn cross_rect(card: Rect, scale: u32) -> Rect {
        let side = CROSS * scale;
        Rect::new(card.right() - (PAD * scale) as i32 - side as i32 + (4 * scale) as i32, card.y + (4 * scale) as i32, side, side)
    }

    /// Карточка под точкой и попала ли точка в её крестик.
    #[must_use]
    pub fn hit(&self, x: i32, y: i32, scale: u32) -> Option<(usize, bool)> {
        let index = self.cards.iter().position(|card| card.contains(x, y))?;
        Some((index, Self::cross_rect(self.cards[index], scale).contains(x, y)))
    }
}
