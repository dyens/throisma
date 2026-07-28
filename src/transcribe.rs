use crate::paths;
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

    let model = match model.or_else(|| std::env::var("THROISMA_MODEL").ok().map(PathBuf::from)) {
        Some(m) => m,
        None => paths::default_model()?,
    };
    if !model.exists() {
        bail!(
            "модель не найдена: {}\n\
             Скачайте её, например:\n\
             mkdir -p {} && curl -L -o {} \\\n\
             https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin",
            model.display(),
            model.parent().unwrap_or(&model).display(),
            model.display(),
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

    println!("\nТранскрипт: {}", out_base.with_extension("txt").display());
    Ok(())
}

fn latest_recording() -> Result<PathBuf> {
    paths::recordings()?
        .pop()
        .context("записей нет — сначала выполните `throisma record`")
}
