use crate::paths;
use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

pub fn transcribe(file: Option<PathBuf>, model: Option<PathBuf>, lang: &str) -> Result<()> {
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

    let samples = read_wav(&wav)?;

    println!("Транскрибирую {} …", wav.display());
    // глушим болтливый лог whisper.cpp/ggml в stderr
    whisper_rs::install_logging_hooks();
    let ctx = WhisperContext::new_with_params(
        model.to_str().context("путь к модели содержит не-UTF-8 символы")?,
        WhisperContextParameters::default(),
    )
    .context("не удалось загрузить модель whisper")?;
    let mut state = ctx.create_state().context("не удалось создать состояние whisper")?;

    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_language(Some(lang));
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);

    state.full(params, &samples).context("ошибка транскрибации")?;

    let mut text = String::new();
    for segment in state.as_iter() {
        text.push_str(&segment.to_str_lossy().context("не удалось прочитать сегмент")?);
        text.push('\n');
    }

    let txt = wav.with_extension("txt");
    std::fs::write(&txt, text.trim_start())
        .with_context(|| format!("не удалось записать {}", txt.display()))?;

    println!("\nТранскрипт: {}", txt.display());
    Ok(())
}

// whisper принимает только 16 кГц моно f32; записи рекордера уже в этом формате
fn read_wav(wav: &PathBuf) -> Result<Vec<f32>> {
    let mut reader = hound::WavReader::open(wav)
        .with_context(|| format!("не удалось открыть {}", wav.display()))?;
    let spec = reader.spec();
    if spec.sample_rate != 16_000 || spec.channels != 1 || spec.bits_per_sample != 16 {
        bail!(
            "ожидается WAV 16 кГц моно s16, а {} — {} Гц, {} кан., {} бит",
            wav.display(),
            spec.sample_rate,
            spec.channels,
            spec.bits_per_sample,
        );
    }
    reader
        .samples::<i16>()
        .map(|s| s.map(|v| v as f32 / 32768.0).context("ошибка чтения WAV"))
        .collect()
}

fn latest_recording() -> Result<PathBuf> {
    paths::recordings()?
        .pop()
        .context("записей нет — сначала выполните `throisma record`")
}
