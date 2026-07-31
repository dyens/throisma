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

/// Транскрибирует WAV и возвращает текст. Моно (диктовка, старые записи) —
/// плоский текст; стерео (встречи) — диалог с метками «Я:» (левый канал,
/// микрофон) и «Они:» (правый, системный звук).
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

    let mut channels = read_wav(wav)?;

    // глушим болтливый лог whisper.cpp/ggml в stderr
    if std::env::var("THROISMA_DEBUG").is_err() {
        whisper_rs::install_logging_hooks();
    }

    // словарь-подсказка: флаг --prompt, иначе файл prompt.txt (если есть)
    let file_prompt = std::fs::read_to_string(paths::prompt_file()?).ok();
    let prompt = prompt
        .or(file_prompt.as_deref())
        .map(str::trim)
        .filter(|p| !p.is_empty());

    let vad_model = paths::vad_model()?;
    let vad_model = vad_model.exists().then_some(vad_model);

    let ctx = WhisperContext::new_with_params(
        model.to_str().context("путь к модели содержит не-UTF-8 символы")?,
        WhisperContextParameters::default(),
    )
    .context("не удалось загрузить модель whisper")?;

    if channels.len() == 1 {
        let segments =
            transcribe_channel(&ctx, channels.pop().expect("канал есть"), lang, prompt, vad_model.as_deref())?;
        return Ok(plain_text(&segments));
    }
    let sys = channels.pop().expect("правый канал есть");
    let mic = channels.pop().expect("левый канал есть");
    let mine = transcribe_channel(&ctx, mic, lang, prompt, vad_model.as_deref())?;
    let theirs = transcribe_channel(&ctx, sys, lang, prompt, vad_model.as_deref())?;
    Ok(merge_dialogue(&mine, &theirs))
}

/// Распознаёт один канал; сегменты возвращаются с началом в сотых долях
/// секунды от начала записи (после обратного отображения VAD-фильтрации).
fn transcribe_channel(
    ctx: &WhisperContext,
    samples: Vec<f32>,
    lang: &str,
    prompt: Option<&str>,
    vad_model: Option<&Path>,
) -> Result<Vec<(i64, String)>> {
    // VAD: вырезаем тишину до распознавания — уходят галлюцинации на паузах
    // («Спасибо.» и т.п.). Через FullParams VAD включить нельзя:
    // whisper_full_with_state, который зовёт whisper-rs, его игнорирует.
    let (samples, map) = match vad_model {
        Some(m) => keep_speech(m, samples)?,
        None => (samples, SpeechMap::identity()),
    };
    if samples.is_empty() {
        return Ok(Vec::new());
    }

    let mut state = ctx.create_state().context("не удалось создать состояние whisper")?;

    // beam search точнее жадного декодирования на трудном звуке,
    // а с GPU его цена незаметна
    let mut params = FullParams::new(SamplingStrategy::BeamSearch { beam_size: 5, patience: -1.0 });
    params.set_language(Some(lang));
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    if let Some(p) = prompt {
        params.set_initial_prompt(p);
    }

    state.full(params, &samples).context("ошибка транскрибации")?;

    let mut segments = Vec::new();
    for segment in state.as_iter() {
        let text = segment.to_str_lossy().context("не удалось прочитать сегмент")?;
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        segments.push((map.to_original_cs(segment.start_timestamp()), text.to_string()));
    }
    Ok(segments)
}

/// Транскрипт одного канала без меток (диктовка, старые моно-записи).
fn plain_text(segments: &[(i64, String)]) -> String {
    let mut text = segments.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>().join("\n");
    if !text.is_empty() {
        text.push('\n');
    }
    text
}

/// Сливает сегменты каналов в диалог по времени начала.
fn merge_dialogue(mine: &[(i64, String)], theirs: &[(i64, String)]) -> String {
    let mut all: Vec<(i64, &str, &str)> = mine
        .iter()
        .map(|(t, s)| (*t, "Я", s.as_str()))
        .chain(theirs.iter().map(|(t, s)| (*t, "Они", s.as_str())))
        .collect();
    all.sort_by_key(|&(t, ..)| t);

    let mut out = String::new();
    for (_, who, text) in all {
        out.push_str(who);
        out.push_str(": ");
        out.push_str(text);
        out.push('\n');
    }
    out
}

/// Соответствие «время в отфильтрованном VAD звуке → время в оригинале».
struct SpeechMap {
    /// (начало в отфильтрованном, начало в оригинале, длина) — в сэмплах;
    /// спаны непрерывно покрывают отфильтрованный сигнал.
    spans: Vec<(usize, usize, usize)>,
}

impl SpeechMap {
    /// Без фильтрации: время не менялось.
    fn identity() -> SpeechMap {
        SpeechMap { spans: Vec::new() }
    }

    /// Сотые доли секунды в отфильтрованном звуке → сотые в оригинале.
    fn to_original_cs(&self, cs: i64) -> i64 {
        if self.spans.is_empty() {
            return cs;
        }
        let sample = cs.max(0) as usize * 160; // 16 кГц / 100
        for &(filt, orig, len) in &self.spans {
            if sample < filt + len {
                return ((orig + sample.saturating_sub(filt)) / 160) as i64;
            }
        }
        let &(_, orig, len) = self.spans.last().expect("спаны непусты");
        ((orig + len) / 160) as i64
    }
}

/// Оставляет только речевые сегменты (по версии silero-VAD) и возвращает
/// таблицу соответствия времени.
fn keep_speech(vad_model: &Path, samples: Vec<f32>) -> Result<(Vec<f32>, SpeechMap)> {
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
    let mut spans = Vec::new();
    for seg in segments {
        // таймстемпы сегментов — в сотых долях секунды
        let start = ((seg.start / 100.0 * 16_000.0) as usize).min(samples.len());
        let end = ((seg.end / 100.0 * 16_000.0) as usize).min(samples.len());
        if start >= end {
            continue;
        }
        spans.push((speech.len(), start, end - start));
        speech.extend_from_slice(&samples[start..end]);
    }
    Ok((speech, SpeechMap { spans }))
}

// whisper принимает 16 кГц f32; встречи — стерео (микрофон/система), диктовка — моно
fn read_wav(wav: &Path) -> Result<Vec<Vec<f32>>> {
    let mut reader = hound::WavReader::open(wav)
        .with_context(|| format!("не удалось открыть {}", wav.display()))?;
    let spec = reader.spec();
    if spec.sample_rate != 16_000 || spec.bits_per_sample != 16 || !(1..=2).contains(&spec.channels)
    {
        bail!(
            "ожидается WAV 16 кГц s16 (1–2 канала), а {} — {} Гц, {} кан., {} бит",
            wav.display(),
            spec.sample_rate,
            spec.channels,
            spec.bits_per_sample,
        );
    }
    let n = spec.channels as usize;
    let mut channels = vec![Vec::new(); n];
    for (i, sample) in reader.samples::<i16>().enumerate() {
        channels[i % n].push(sample.context("ошибка чтения WAV")? as f32 / 32768.0);
    }
    Ok(channels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_map_keeps_time() {
        assert_eq!(SpeechMap::identity().to_original_cs(1234), 1234);
    }

    #[test]
    fn map_restores_original_time() {
        // речь в оригинале: [1600..4800) и [16000..17600) сэмплов
        let map = SpeechMap { spans: vec![(0, 1600, 3200), (3200, 16000, 1600)] };
        assert_eq!(map.to_original_cs(0), 10); // начало первого спана: 1600/160
        assert_eq!(map.to_original_cs(10), 20); // внутри первого
        assert_eq!(map.to_original_cs(20), 100); // начало второго: 16000/160
        assert_eq!(map.to_original_cs(1000), 110); // за концом — конец речи
    }

    #[test]
    fn merge_orders_segments_by_time() {
        let mine = vec![(0, "привет".to_string()), (300, "как дела".to_string())];
        let theirs = vec![(150, "здравствуй".to_string())];
        assert_eq!(
            merge_dialogue(&mine, &theirs),
            "Я: привет\nОни: здравствуй\nЯ: как дела\n"
        );
    }

    #[test]
    fn plain_text_joins_segments() {
        let segs = vec![(0, "раз".to_string()), (100, "два".to_string())];
        assert_eq!(plain_text(&segs), "раз\nдва\n");
        assert_eq!(plain_text(&[]), "");
    }
}
