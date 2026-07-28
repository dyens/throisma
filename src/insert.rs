//! Вставка текста в активное окно (Wayland).

use anyhow::{bail, Context, Result};
use std::io::Write;
use std::process::{Command, Stdio};

/// Кладёт текст в буфер обмена и «печатает» его в активное окно.
/// Порядок важен: сначала клипборд — если wtype упадёт, текст можно вставить руками.
pub(crate) fn insert_text(text: &str) -> Result<()> {
    pipe("wl-copy", &[], text).context("не удалось скопировать в буфер обмена (wl-copy)")?;
    pipe("wtype", &["-"], text).context("не удалось напечатать текст (wtype)")?;
    Ok(())
}

fn pipe(cmd: &str, args: &[&str], input: &str) -> Result<()> {
    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::piped())
        .spawn()
        .with_context(|| format!("не удалось запустить {cmd}"))?;
    child
        .stdin
        .take()
        .expect("stdin запрошен строкой выше")
        .write_all(input.as_bytes())?;
    let status = child.wait()?;
    if !status.success() {
        bail!("{cmd} завершился с ошибкой");
    }
    Ok(())
}
