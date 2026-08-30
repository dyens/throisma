//! Значок «идёт запись» в системном трее (StatusNotifierItem: waybar, KDE,
//! GNOME с AppIndicator). Уведомление легко пропустить, а значок висит всю
//! запись и напоминает о ней; клик по нему останавливает запись.

use ksni::blocking::{Handle, TrayMethods};
use ksni::menu::StandardItem;
use ksni::{Icon, MenuItem, Status, ToolTip};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Цвет кружка записи (тот же красный, что у кнопки rec).
const RED: [u8; 3] = [0xE0, 0x2B, 0x2B];

struct RecordingTray {
    /// Файл записи — показывается в подсказке.
    wav: String,
    /// Секунд с начала записи.
    elapsed: u64,
    /// Общий с циклом записи флаг остановки: клик по значку = Ctrl+C.
    stop: Arc<AtomicBool>,
}

impl RecordingTray {
    fn hms(&self) -> String {
        format!(
            "{:02}:{:02}:{:02}",
            self.elapsed / 3600,
            self.elapsed / 60 % 60,
            self.elapsed % 60
        )
    }

    fn request_stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

impl ksni::Tray for RecordingTray {
    fn id(&self) -> String {
        "throisma".into()
    }

    fn title(&self) -> String {
        format!("throisma — запись {}", self.hms())
    }

    /// NeedsAttention: панели такой значок подсвечивают — забыть запись сложнее.
    fn status(&self) -> Status {
        Status::NeedsAttention
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        icons()
    }

    fn attention_icon_pixmap(&self) -> Vec<Icon> {
        icons()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: format!("⏺ Идёт запись — {}", self.hms()),
            description: self.wav.clone(),
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.request_stop();
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            StandardItem {
                label: format!("⏺ Идёт запись — {}", self.hms()),
                enabled: false,
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Остановить запись".into(),
                activate: Box::new(|tray: &mut Self| tray.request_stop()),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// Значок в трее; исчезает вместе с этим объектом.
pub(crate) struct RecordingIcon(Handle<RecordingTray>);

impl RecordingIcon {
    /// Ставит значок в трей. `None`, если трея в сессии нет (значок —
    /// удобство, а не условие записи, поэтому ошибка не фатальна).
    pub(crate) fn show(wav: &Path, stop: Arc<AtomicBool>) -> Option<RecordingIcon> {
        let tray =
            RecordingTray { wav: wav.display().to_string(), elapsed: 0, stop };
        match tray.spawn() {
            Ok(handle) => Some(RecordingIcon(handle)),
            Err(e) => {
                eprintln!("значок в трее недоступен: {e}");
                None
            }
        }
    }

    /// Обновляет счётчик времени на значке.
    pub(crate) fn set_elapsed(&self, secs: u64) {
        self.0.update(|tray| tray.elapsed = secs);
    }
}

impl Drop for RecordingIcon {
    fn drop(&mut self) {
        self.0.shutdown().wait();
    }
}

/// Красный кружок записи в нескольких размерах — панель берёт подходящий.
fn icons() -> Vec<Icon> {
    [22, 32, 48].into_iter().map(circle).collect()
}

/// Круг во всю иконку, ARGB32 с сглаженным краем — как требует спецификация SNI.
fn circle(size: i32) -> Icon {
    let radius = size as f32 / 2.0;
    let mut data = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let dx = (x as f32 + 0.5) - radius;
            let dy = (y as f32 + 0.5) - radius;
            // альфа спадает на последнем пикселе радиуса — иначе край «лесенкой»
            let alpha = (radius - 0.5 - (dx * dx + dy * dy).sqrt()).clamp(0.0, 1.0);
            data.push((alpha * 255.0) as u8);
            data.extend_from_slice(&RED);
        }
    }
    Icon { width: size, height: size, data }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn circle_is_argb_of_right_size() {
        let icon = circle(22);
        assert_eq!(icon.data.len(), 22 * 22 * 4);
        // центр — непрозрачный красный, угол — прозрачный
        let center = (11 * 22 + 11) * 4;
        assert_eq!(&icon.data[center..center + 4], &[255, RED[0], RED[1], RED[2]]);
        assert_eq!(icon.data[0], 0);
    }

    #[test]
    fn elapsed_formats_as_hms() {
        let tray = RecordingTray {
            wav: String::new(),
            elapsed: 3 * 3600 + 25 * 60 + 7,
            stop: Arc::new(AtomicBool::new(false)),
        };
        assert_eq!(tray.hms(), "03:25:07");
    }
}
