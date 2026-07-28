//! Все пути приложения в одном месте.

use anyhow::{Context, Result};
use std::path::PathBuf;

/// ~/.local/share/throisma
fn data_dir() -> Result<PathBuf> {
    Ok(dirs::data_dir()
        .context("не удалось определить каталог данных")?
        .join("throisma"))
}

/// Каталог с записями (создаётся при первом обращении).
pub(crate) fn recordings_dir() -> Result<PathBuf> {
    let dir = data_dir()?.join("recordings");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Все записи, отсортированные от старых к новым (имя файла = время начала).
pub(crate) fn recordings() -> Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(recordings_dir()?)?
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|p| p.extension().is_some_and(|ext| ext == "wav"))
        .collect();
    files.sort();
    Ok(files)
}

/// Последняя запись.
pub(crate) fn latest_recording() -> Result<PathBuf> {
    recordings()?
        .pop()
        .context("записей нет — сначала выполните `throisma record`")
}

/// Модель whisper по умолчанию.
pub(crate) fn default_model() -> Result<PathBuf> {
    Ok(data_dir()?.join("models").join("ggml-base.bin"))
}

/// $XDG_RUNTIME_DIR (fallback — временный каталог).
pub(crate) fn runtime_dir() -> PathBuf {
    dirs::runtime_dir().unwrap_or_else(std::env::temp_dir)
}

/// pid-файл идущей записи.
pub(crate) fn pidfile() -> PathBuf {
    runtime_dir().join("throisma.pid")
}

/// pid-файл идущей диктовки (отдельный от записи встреч).
pub(crate) fn dictate_pidfile() -> PathBuf {
    runtime_dir().join("throisma-dictate.pid")
}

/// Временный WAV диктовки. Имя фиксированное: одновременная диктовка одна
/// (это гарантирует pid-файл), а при ошибке файл остаётся для повтора.
pub(crate) fn dictate_wav() -> PathBuf {
    runtime_dir().join("throisma-dictate.wav")
}
