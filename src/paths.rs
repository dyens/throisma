//! Все пути приложения в одном месте.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

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

/// Ошибка, если файла нет.
pub(crate) fn require_exists(path: &Path) -> Result<()> {
    if !path.exists() {
        bail!("файл не найден: {}", path.display());
    }
    Ok(())
}

/// Модель whisper по умолчанию: large-v3-turbo, если скачана, иначе base.
/// На base заметно хуже термины и речь собеседников из системного звука,
/// а на GPU turbo медленнее лишь на доли секунды (фраза 8 с: 1.3 с против 0.6).
pub(crate) fn default_model() -> Result<PathBuf> {
    let models = data_dir()?.join("models");
    let turbo = models.join("ggml-large-v3-turbo.bin");
    Ok(if turbo.exists() { turbo } else { models.join("ggml-base.bin") })
}

/// Модель VAD (silero): если файл есть, тишина вырезается до распознавания.
pub(crate) fn vad_model() -> Result<PathBuf> {
    Ok(data_dir()?.join("models").join("ggml-silero-v5.1.2.bin"))
}

/// Модели диаризации (sherpa-onnx): сегментация pyannote и эмбеддинги голоса.
/// Если обе скачаны, собеседники во встрече делятся по голосам.
pub(crate) fn diarization_models() -> Result<(PathBuf, PathBuf)> {
    let models = data_dir()?.join("models");
    Ok((
        models.join("sherpa-onnx-pyannote-segmentation-3-0").join("model.onnx"),
        models.join("wespeaker_en_voxceleb_resnet34_LM.onnx"),
    ))
}

/// Словарь-подсказка для whisper: термины, которые модель иначе калечит.
pub(crate) fn prompt_file() -> Result<PathBuf> {
    Ok(data_dir()?.join("prompt.txt"))
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
