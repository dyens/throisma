//! Вставка текста в активное окно (Wayland).

use crate::proc;
use anyhow::{Context, Result};
use std::process::Command;

/// Кладёт текст в буфер обмена и «печатает» его в активное окно.
/// Порядок важен: сначала клипборд — если wtype упадёт, текст можно вставить руками.
pub(crate) fn insert_text(text: &str) -> Result<()> {
    proc::run(Command::new("wl-copy"), Some(text))
        .context("не удалось скопировать в буфер обмена (wl-copy)")?;
    let mut wtype = Command::new("wtype");
    wtype.arg("-");
    proc::run(wtype, Some(text)).context("не удалось напечатать текст (wtype)")?;
    Ok(())
}
