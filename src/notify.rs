//! Уведомления на рабочий стол через notify-send.

use std::process::Command;

pub(crate) struct Notifier {
    enabled: bool,
}

impl Notifier {
    /// `no_notify` — флаг --no-notify; дополнительно учитывается THROISMA_NO_NOTIFY=1.
    pub(crate) fn new(no_notify: bool) -> Notifier {
        let env_off = std::env::var("THROISMA_NO_NOTIFY").is_ok_and(|v| v == "1");
        Notifier { enabled: !no_notify && !env_off }
    }

    /// Показывает уведомление, не дожидаясь notify-send — вызывающие не должны
    /// тормозить из-за D-Bus; ошибки (нет notify-send и т.п.) молча игнорируются.
    pub(crate) fn send(&self, summary: &str, body: &str) {
        if !self.enabled {
            return;
        }
        let _ = Command::new("notify-send")
            .args(["--app-name=throisma", summary, body])
            .spawn();
    }
}
