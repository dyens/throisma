use crate::paths;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

pub fn transcribe(
    file: Option<PathBuf>,
    model: Option<PathBuf>,
    lang: &str,
    prompt: Option<&str>,
) -> Result<()> {
    let wav = match file {
        Some(f) => f,
        None => paths::latest_recording()?,
    };
    println!("Транскрибирую {} …", wav.display());
    let text = transcribe_wav(&wav, model, lang, prompt)?;

    let txt = wav.with_extension("txt");
    std::fs::write(&txt, &text)
        .with_context(|| format!("не удалось записать {}", txt.display()))?;

    println!("\nТранскрипт: {}", txt.display());
    Ok(())
}

/// Транскрибирует WAV и возвращает текст (сегменты, разделённые \n).
pub(crate) fn transcribe_wav(
    wav: &Path,
    model: Option<PathBuf>,
    lang: &str,
    prompt: Option<&str>,
) -> Result<String> {
    paths::require_exists(wav)?;

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

    let samples = read_wav(wav)?;

    // глушим болтливый лог whisper.cpp/ggml в stderr
    if std::env::var("THROISMA_DEBUG").is_err() { whisper_rs::install_logging_hooks(); }

    // VAD: если модель silero скачана, вырезаем тишину до распознавания —
    // уходят галлюцинации на паузах («Спасибо.» и т.п.). Через FullParams VAD
    // включить нельзя: whisper_full_with_state, который зовёт whisper-rs,
    // молча его игнорирует — фильтруем сами.
    let vad_model = paths::vad_model()?;
    let samples = if vad_model.exists() { keep_speech(&vad_model, samples)? } else { samples };
    if samples.is_empty() {
        return Ok(String::new());
    }

    let ctx = WhisperContext::new_with_params(
        model.to_str().context("путь к модели содержит не-UTF-8 символы")?,
        WhisperContextParameters::default(),
    )
    .context("не удалось загрузить модель whisper")?;
    let mut state = ctx.create_state().context("не удалось создать состояние whisper")?;

    // beam search точнее жадного декодирования на трудном звуке,
    // а с GPU его цена незаметна
    let mut params = FullParams::new(SamplingStrategy::BeamSearch { beam_size: 5, patience: -1.0 });
    params.set_language(Some(lang));
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);

    // словарь-подсказка: флаг --prompt, иначе файл prompt.txt (если есть)
    let file_prompt = std::fs::read_to_string(paths::prompt_file()?).ok();
    if let Some(p) = prompt.or(file_prompt.as_deref()) {
        let p = p.trim();
        if !p.is_empty() {
            params.set_initial_prompt(p);
        }
    }

    state.full(params, &samples).context("ошибка транскрибации")?;

    let mut text = String::new();
    for segment in state.as_iter() {
        text.push_str(&segment.to_str_lossy().context("не удалось прочитать сегмент")?);
        text.push('\n');
    }
    Ok(text.trim_start().to_string())
}

/// Оставляет только речевые сегменты (по версии silero-VAD).
fn keep_speech(vad_model: &Path, samples: Vec<f32>) -> Result<Vec<f32>> {
    use whisper_rs::{WhisperVadContext, WhisperVadContextParams, WhisperVadParams};
    let mut vad = WhisperVadContext::new(
        vad_model.to_str().context("путь к VAD-модели содержит не-UTF-8 символы")?,
        WhisperVadContextParams::new(),
    )
    .context("не удалось загрузить VAD-модель")?;
    let segments = vad
        .segments_from_samples(WhisperVadParams::new(), &samples)
        .context("ошибка VAD")?;

    let mut speech = Vec::new();
    for seg in segments {
        // таймстемпы сегментов — в сотых долях секунды
        let start = ((seg.start / 100.0 * 16_000.0) as usize).min(samples.len());
        let end = ((seg.end / 100.0 * 16_000.0) as usize).min(samples.len());
        speech.extend_from_slice(&samples[start..end]);
    }
    Ok(speech)
}

// whisper принимает только 16 кГц моно f32; записи рекордера уже в этом формате
fn read_wav(wav: &Path) -> Result<Vec<f32>> {
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
