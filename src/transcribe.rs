use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use std::process::Command;

pub fn transcribe(
    file: Option<PathBuf>,
    model: Option<PathBuf>,
    whisper_bin: &str,
    lang: &str,
) -> Result<()> {
    let wav = match file {
        Some(f) => f,
        None => latest_recording()?,
    };
    if !wav.exists() {
        bail!("файл не найден: {}", wav.display());
    }

    let model = model
        .or_else(|| std::env::var("THROISMA_MODEL").ok().map(PathBuf::from))
        .unwrap_or_else(default_model_path);
    if !model.exists() {
        bail!(
            "модель не найдена: {}\n\
             Скачайте её, например:\n\
             mkdir -p {} && curl -L -o {} \\\n\
             https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin",
            model.display(),
            default_model_path().parent().unwrap().display(),
            default_model_path().display(),
        );
    }

    // записи и так в формате whisper (16 кГц моно s16) — конвертация не нужна
    let out_base = wav.with_extension("");
    println!("Транскрибирую {} …", wav.display());
    let status = Command::new(whisper_bin)
        .arg("-m")
        .arg(&model)
        .arg("-f")
        .arg(&wav)
        .args(["-l", lang, "-otxt", "-of"])
        .arg(&out_base)
        .status()
        .with_context(|| format!("не удалось запустить {whisper_bin} — whisper.cpp установлен?"))?;
    if !status.success() {
        bail!("{whisper_bin} завершился с ошибкой");
    }

    let txt = out_base.with_extension("txt");
    println!("\nТранскрипт: {}", txt.display());
    Ok(())
}

fn default_model_path() -> PathBuf {
    dirs::data_dir()
        .expect("не удалось определить каталог данных")
        .join("throisma")
        .join("models")
        .join("ggml-base.bin")
}

fn latest_recording() -> Result<PathBuf> {
    let dir = crate::recordings_dir()?;
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "wav"))
        .collect();
    files.sort();
    files.pop().context("записей нет — сначала выполните `throisma record`")
}
