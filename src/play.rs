//! Воспроизведение записей.

use crate::paths;
use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use std::process::Command;

pub fn play(file: Option<PathBuf>) -> Result<()> {
    let wav = match file {
        Some(f) => f,
        None => paths::latest_recording()?,
    };
    if !wav.exists() {
        bail!("файл не найден: {}", wav.display());
    }
    println!("Играю {} … (Ctrl+C — остановить)", wav.display());
    let status = Command::new("pw-play")
        .arg(&wav)
        .status()
        .context("не удалось запустить pw-play — установлен pipewire-utils?")?;
    if !status.success() {
        bail!("pw-play завершился с ошибкой");
    }
    Ok(())
}
