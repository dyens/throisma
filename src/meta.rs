//! Метаинформация о записях: реестр records.json в каталоге записей.

use crate::paths;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Метаданные одной записи; структура расширяемая (теги, описание — потом).
#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct RecordMeta {
    pub(crate) name: String,
}

fn registry_path() -> Result<PathBuf> {
    Ok(paths::recordings_dir()?.join("records.json"))
}

/// Читает реестр; отсутствующий файл — пустой реестр, битый — предупреждение
/// и пустой реестр (перезапишется при следующем rename).
pub(crate) fn load() -> Result<HashMap<String, RecordMeta>> {
    let Ok(contents) = std::fs::read_to_string(registry_path()?) else {
        return Ok(HashMap::new());
    };
    match serde_json::from_str(&contents) {
        Ok(map) => Ok(map),
        Err(e) => {
            eprintln!("records.json повреждён ({e}) — игнорирую");
            Ok(HashMap::new())
        }
    }
}

/// Задаёт название записи по стему (имя файла без расширения); заодно
/// вычищает ключи удалённых записей. Запись атомарная: tmp + rename.
fn set_name(stem: &str, name: &str) -> Result<()> {
    let dir = paths::recordings_dir()?;
    let mut map = load()?;
    map.retain(|key, _| dir.join(format!("{key}.wav")).exists());
    map.insert(stem.to_string(), RecordMeta { name: name.to_string() });

    let path = registry_path()?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&map)?)
        .with_context(|| format!("не удалось записать {}", tmp.display()))?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Команда rename: задать название записи (по умолчанию — последней).
pub fn rename(name: &str, file: Option<PathBuf>) -> Result<()> {
    let name = name.trim();
    if name.is_empty() {
        bail!("название пустое");
    }
    let wav = match file {
        Some(f) => f,
        None => paths::latest_recording()?,
    };
    paths::require_exists(&wav)?;
    let wav = wav.canonicalize()?;
    if wav.parent() != Some(paths::recordings_dir()?.canonicalize()?.as_path()) {
        bail!(
            "название можно задать только записи из {}",
            paths::recordings_dir()?.display()
        );
    }
    let stem = wav
        .file_stem()
        .context("не удалось определить имя файла")?
        .to_string_lossy();
    set_name(&stem, name)?;
    println!("{}: «{name}»", wav.display());
    Ok(())
}
