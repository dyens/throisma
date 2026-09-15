use crate::diarize::{self, Turn};
use crate::paths;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

pub fn transcribe(
    file: Option<PathBuf>,
    model: Option<PathBuf>,
    lang: &str,
    prompt: Option<&str>,
    speakers: Option<usize>,
) -> Result<()> {
    let wav = match file {
        Some(f) => f,
        None => paths::latest_recording()?,
    };
    println!("Транскрибирую {} …", wav.display());
    let text = transcribe_wav(&wav, model, lang, prompt, speakers)?;

    let txt = wav.with_extension("txt");
    std::fs::write(&txt, &text)
        .with_context(|| format!("не удалось записать {}", txt.display()))?;

    println!("\nТранскрипт: {}", txt.display());
    Ok(())
}

/// Транскрибирует WAV и возвращает текст. Моно (диктовка, старые записи) —
/// плоский текст; стерео (встречи) — диалог с метками «Я:» (левый канал,
/// микрофон) и «Они:» (правый, системный звук), у каждой реплики — время
/// от начала записи. Если скачаны модели диаризации, собеседники делятся по
/// голосам («Они 1», «Они 2»); `speakers` — их число, если известно.
pub(crate) fn transcribe_wav(
    wav: &Path,
    model: Option<PathBuf>,
    lang: &str,
    prompt: Option<&str>,
    speakers: Option<usize>,
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
             https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{}",
            model.display(),
            model.parent().unwrap_or(&model).display(),
            model.display(),
            model.file_name().unwrap_or_default().to_string_lossy(),
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
            transcribe_channel(&ctx, channels.pop().expect("канал есть"), lang, prompt, vad_model.as_deref(), &[])?;
        return Ok(plain_text(&segments));
    }
    let sys = channels.pop().expect("правый канал есть");
    let mic = channels.pop().expect("левый канал есть");
    // по голосам делим только собеседников: свой голос и так на отдельном
    // канале; диаризация (CPU) идёт в фоне, пока whisper распознаёт микрофон
    let diarization = diarize::start(wav, 1)?;
    let mine = transcribe_channel(&ctx, mic, lang, prompt, vad_model.as_deref(), &[])?;
    let turns = match diarization {
        Some(pending) => {
            println!("Делю собеседников по голосам …");
            pending.wait(speakers).unwrap_or_else(|e| {
                eprintln!("диаризация не удалась ({e:#}) — собеседники без деления по голосам");
                Vec::new()
            })
        }
        None => Vec::new(),
    };
    let theirs = transcribe_channel(&ctx, sys, lang, prompt, vad_model.as_deref(), &turns)?;
    Ok(merge_dialogue(&mine, &theirs))
}

/// Реплика одного канала.
#[derive(Debug, PartialEq)]
struct Piece {
    /// Начало в сотых долях секунды от начала записи.
    start_cs: i64,
    text: String,
    /// Отрезана по паузе от предыдущей реплики того же сегмента whisper.
    cont: bool,
    /// Голос по диаризации; `None` — диаризации не было.
    speaker: Option<usize>,
}

/// Распознаёт один канал; время реплик — от начала записи (после обратного
/// отображения VAD-фильтрации). `turns` — отрезки голосов, пусто — без диаризации.
fn transcribe_channel(
    ctx: &WhisperContext,
    samples: Vec<f32>,
    lang: &str,
    prompt: Option<&str>,
    vad_model: Option<&Path>,
    turns: &[Turn],
) -> Result<Vec<Piece>> {
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
    // время каждого токена — чтобы резать сегменты на реплики (см. split_pieces)
    params.set_token_timestamps(true);
    if let Some(p) = prompt {
        params.set_initial_prompt(p);
    }

    state.full(params, &samples).context("ошибка транскрибации")?;

    let eot = ctx.token_eot();
    let mut segments = Vec::new();
    for segment in state.as_iter() {
        let mut tokens = Vec::new();
        for i in 0..segment.n_tokens() {
            let token = segment.get_token(i).expect("индекс в пределах сегмента");
            // служебные токены ([_BEG_], таймстемпы) идут после EOT
            if token.token_id() >= eot {
                continue;
            }
            let bytes = token.to_bytes().context("не удалось прочитать токен")?;
            let data = token.token_data();
            tokens.push(Token { t0: data.t0, t1: data.t1, bytes: bytes.to_vec() });
        }
        segments.extend(split_pieces(&map, turns, &tokens));
    }
    Ok(segments)
}

/// Токен whisper: начало и конец в отфильтрованном звуке (сотые доли секунды)
/// и байты текста — BPE делит кириллицу посреди символа.
struct Token {
    t0: i64,
    t1: i64,
    bytes: Vec<u8>,
}

/// Пауза в оригинальной записи, по которой сегмент режется на реплики.
/// Подобрано на живой встрече: при 1 с неточные таймстемпы токенов рвут
/// фразы на обрывки, при 3 с ответ уже встаёт раньше вопроса.
const MIN_PAUSE_CS: i64 = 200;

/// На сколько отрезки диаризации запаздывают за словами whisper. Подобрано
/// на смеси двух голосов с известной разметкой: без сдвига первое слово новой
/// реплики уходило предыдущему голосу, при 0.6 с — последнее слово следующему.
const VOICE_LAG_CS: i64 = 30;

/// Режет сегмент whisper на реплики по длинным паузам и сменам голоса.
///
/// VAD склеивает речь канала встык, и один сегмент может накрыть реплики,
/// между которыми в оригинале говорил собеседник; по началу сегмента такая
/// реплика встала бы раньше его ответа. Режем там, где слово попадает в
/// другой речевой спан, а пауза до него в оригинале — не меньше
/// MIN_PAUSE_CS, или где по диаризации (`turns`) слово сказал другой голос.
/// Голос слова определяется по концу токена (со сдвигом VOICE_LAG_CS): по
/// началу первые слова новой реплики уходили предыдущему голосу. Текст
/// собирается из байтов, а резать можно только перед началом слова (токен
/// с пробелом).
fn split_pieces(map: &SpeechMap, turns: &[Turn], tokens: &[Token]) -> Vec<Piece> {
    // (начало в оригинале, спан последнего слова, голос, байты текста)
    let mut pieces: Vec<(i64, usize, Option<usize>, Vec<u8>)> = Vec::new();
    for Token { t0, t1, bytes } in tokens {
        let (span, orig) = map.locate(*t0);
        let voice = diarize::speaker_at(turns, map.locate(*t1).1 + VOICE_LAG_CS);
        let word_start = bytes.first() == Some(&b' ');
        match pieces.last_mut() {
            Some((_, last, cur, text))
                if !(word_start
                    && (map.pause_cs(*last, span) >= MIN_PAUSE_CS || voice != *cur)) =>
            {
                text.extend_from_slice(bytes);
                if word_start {
                    *last = span;
                }
            }
            _ => pieces.push((orig, span, voice, bytes.clone())),
        }
    }
    let mut out: Vec<Piece> = Vec::new();
    for (start_cs, _, speaker, bytes) in pieces {
        let text = String::from_utf8_lossy(&bytes).trim().to_string();
        if !text.is_empty() {
            out.push(Piece { start_cs, text, cont: !out.is_empty(), speaker });
        }
    }
    out
}

/// Транскрипт одного канала без меток (диктовка, старые моно-записи):
/// строка на сегмент whisper, отрезанные по паузам куски — обратно в строку.
fn plain_text(pieces: &[Piece]) -> String {
    let mut text = String::new();
    for p in pieces {
        if !text.is_empty() {
            text.push(if p.cont { ' ' } else { '\n' });
        }
        text.push_str(&p.text);
    }
    if !text.is_empty() {
        text.push('\n');
    }
    text
}

/// Сливает реплики каналов в диалог по времени начала: `[ММ:СС] Я: …`.
/// Кусок, отрезанный по паузе, приклеивается обратно к своей строке, если
/// между ними не встала реплика другого голоса.
fn merge_dialogue(mine: &[Piece], theirs: &[Piece]) -> String {
    let mut all: Vec<(String, &Piece)> = mine
        .iter()
        .map(|p| ("Я".to_string(), p))
        .chain(their_labels(theirs).into_iter().zip(theirs))
        .collect();
    all.sort_by_key(|(_, p)| p.start_cs);

    let mut out = String::new();
    let mut prev: Option<String> = None;
    for (who, p) in all {
        if p.cont && prev.as_ref() == Some(&who) {
            out.pop(); // перевод строки
            out.push(' ');
        } else {
            out.push_str(&format!("{} {who}: ", timestamp(p.start_cs)));
        }
        out.push_str(&p.text);
        out.push('\n');
        prev = Some(who);
    }
    out
}

/// Метки собеседников: «Они 1», «Они 2», … по порядку первой реплики;
/// один голос или диаризации не было — просто «Они».
fn their_labels(theirs: &[Piece]) -> Vec<String> {
    let mut order: Vec<usize> = Vec::new();
    for voice in theirs.iter().filter_map(|p| p.speaker) {
        if !order.contains(&voice) {
            order.push(voice);
        }
    }
    theirs
        .iter()
        .map(|p| match p.speaker {
            Some(voice) if order.len() > 1 => {
                format!("Они {}", order.iter().position(|&o| o == voice).expect("голос учтён") + 1)
            }
            _ => "Они".to_string(),
        })
        .collect()
}

/// Сотые доли секунды от начала записи → `[ММ:СС]`, с часа — `[Ч:ММ:СС]`.
fn timestamp(cs: i64) -> String {
    let secs = cs.max(0) / 100;
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 {
        format!("[{h}:{m:02}:{s:02}]")
    } else {
        format!("[{m:02}:{s:02}]")
    }
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

    /// Момент в отфильтрованном звуке (сотые доли секунды) → индекс спана,
    /// в который он попадает, и тот же момент в оригинале (сотые).
    fn locate(&self, cs: i64) -> (usize, i64) {
        if self.spans.is_empty() {
            return (0, cs);
        }
        let sample = cs.max(0) as usize * 160; // 16 кГц / 100
        for (i, &(filt, orig, len)) in self.spans.iter().enumerate() {
            if sample < filt + len {
                return (i, ((orig + sample.saturating_sub(filt)) / 160) as i64);
            }
        }
        let last = self.spans.len() - 1;
        let (_, orig, len) = self.spans[last];
        (last, ((orig + len) / 160) as i64)
    }

    /// Пауза в оригинале от конца спана `from` до начала более позднего
    /// спана `to`, в сотых долях секунды; 0 — если `to` не позже.
    fn pause_cs(&self, from: usize, to: usize) -> i64 {
        if to <= from {
            return 0;
        }
        let (_, from_orig, from_len) = self.spans[from];
        let (_, to_orig, _) = self.spans[to];
        (to_orig.saturating_sub(from_orig + from_len) / 160) as i64
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
        assert_eq!(SpeechMap::identity().locate(1234), (0, 1234));
    }

    #[test]
    fn map_restores_original_time() {
        // речь в оригинале: [1600..4800) и [16000..17600) сэмплов
        let map = SpeechMap { spans: vec![(0, 1600, 3200), (3200, 16000, 1600)] };
        assert_eq!(map.locate(0), (0, 10)); // начало первого спана: 1600/160
        assert_eq!(map.locate(10), (0, 20)); // внутри первого
        assert_eq!(map.locate(20), (1, 100)); // начало второго: 16000/160
        assert_eq!(map.locate(1000), (1, 110)); // за концом — конец речи
    }

    #[test]
    fn pause_between_spans() {
        // первый спан кончается на 4800 сэмпле, второй начинается на 16000
        let map = SpeechMap { spans: vec![(0, 1600, 3200), (3200, 16000, 1600)] };
        assert_eq!(map.pause_cs(0, 1), 70); // (16000-4800)/160
        assert_eq!(map.pause_cs(1, 0), 0);
        assert_eq!(map.pause_cs(1, 1), 0);
    }

    /// Токены (начало в отфильтрованном звуке, текст).
    fn tokens(list: &[(i64, &str)]) -> Vec<Token> {
        list.iter().map(|(t, s)| Token { t0: *t, t1: *t, bytes: s.as_bytes().to_vec() }).collect()
    }

    /// Реплика для тестов.
    fn piece(start_cs: i64, text: &str, cont: bool) -> Piece {
        Piece { start_cs, text: text.to_string(), cont, speaker: None }
    }

    /// Реплика собеседника с голосом.
    fn voiced(start_cs: i64, text: &str, cont: bool, speaker: usize) -> Piece {
        Piece { speaker: Some(speaker), ..piece(start_cs, text, cont) }
    }

    #[test]
    fn split_at_long_pause() {
        // спаны по 1 с; между вторым и третьим в оригинале пауза 30 с
        let map = SpeechMap {
            spans: vec![(0, 0, 16000), (16000, 20800, 16000), (32000, 516800, 16000)],
        };
        let toks = tokens(&[(10, " Нет,"), (50, " не"), (120, " скидывал."), (210, " Шаришь")]);
        assert_eq!(
            split_pieces(&map, &[], &toks),
            vec![piece(10, "Нет, не скидывал.", false), piece(3240, "Шаришь", true)]
        );
    }

    #[test]
    fn short_pause_does_not_split() {
        // пауза между спанами 0.3 с — одна реплика
        let map = SpeechMap { spans: vec![(0, 0, 16000), (16000, 20800, 16000)] };
        let toks = tokens(&[(10, " раз"), (150, " два")]);
        assert_eq!(split_pieces(&map, &[], &toks), vec![piece(10, "раз два", false)]);
    }

    #[test]
    fn split_only_before_word_start() {
        // слово «Шаришь» разбито на токены, и хвост уже в новом спане:
        // резать посреди слова нельзя, режем перед следующим словом
        let map = SpeechMap { spans: vec![(0, 0, 16000), (16000, 500000, 16000)] };
        let toks = tokens(&[(10, " Ша"), (110, "ришь"), (150, " да")]);
        assert_eq!(
            split_pieces(&map, &[], &toks),
            vec![piece(10, "Шаришь", false), piece(3175, "да", true)]
        );
    }

    #[test]
    fn split_keeps_multibyte_tokens_whole() {
        // «ж» (0xD0 0xB6) разрезан между токенами — текст собирается из байтов
        let toks = vec![
            Token { t0: 0, t1: 5, bytes: vec![b' ', 0xD0] },
            Token { t0: 5, t1: 9, bytes: vec![0xB6] },
        ];
        assert_eq!(split_pieces(&SpeechMap::identity(), &[], &toks), vec![piece(0, "ж", false)]);
    }

    #[test]
    fn split_at_voice_change() {
        // без VAD и пауз; с 1.00 с говорит другой голос — режем перед словом
        let turns = vec![
            Turn { start_cs: 0, end_cs: 100, speaker: 0 },
            Turn { start_cs: 100, end_cs: 300, speaker: 1 },
        ];
        let toks = tokens(&[(10, " Да,"), (50, " согласен."), (110, " А"), (150, " сроки?")]);
        assert_eq!(
            split_pieces(&SpeechMap::identity(), &turns, &toks),
            vec![voiced(10, "Да, согласен.", false, 0), voiced(110, "А сроки?", true, 1)]
        );
    }

    #[test]
    fn voice_by_token_end() {
        // «Окей» по началу токена (0.60 с) ещё в первом голосе, но конец
        // токена со сдвигом VOICE_LAG_CS (0.80 + 0.30 с) — уже во втором
        let turns = vec![
            Turn { start_cs: 0, end_cs: 100, speaker: 0 },
            Turn { start_cs: 100, end_cs: 300, speaker: 1 },
        ];
        let toks = vec![
            Token { t0: 10, t1: 50, bytes: " Да.".as_bytes().to_vec() },
            Token { t0: 60, t1: 80, bytes: " Окей,".as_bytes().to_vec() },
            Token { t0: 130, t1: 160, bytes: " смотрю.".as_bytes().to_vec() },
        ];
        assert_eq!(
            split_pieces(&SpeechMap::identity(), &turns, &toks),
            vec![voiced(10, "Да.", false, 0), voiced(60, "Окей, смотрю.", true, 1)]
        );
    }

    #[test]
    fn labels_number_voices_by_first_appearance() {
        let mine = vec![piece(0, "Начнём?", false)];
        let theirs = vec![
            voiced(100, "Да.", false, 3),
            voiced(200, "Давайте.", false, 0),
            voiced(300, "Сроки?", true, 3), // cont, но перед ним другой голос
        ];
        assert_eq!(
            merge_dialogue(&mine, &theirs),
            "[00:00] Я: Начнём?\n[00:01] Они 1: Да.\n[00:02] Они 2: Давайте.\n[00:03] Они 1: Сроки?\n"
        );
    }

    #[test]
    fn single_voice_stays_they() {
        let theirs = vec![voiced(0, "Раз.", false, 2), voiced(100, "Два.", false, 2)];
        assert_eq!(merge_dialogue(&[], &theirs), "[00:00] Они: Раз.\n[00:01] Они: Два.\n");
    }

    #[test]
    fn merge_orders_segments_by_time() {
        let mine = vec![piece(0, "привет", false), piece(300, "как дела", false)];
        let theirs = vec![piece(150, "здравствуй", false)];
        assert_eq!(
            merge_dialogue(&mine, &theirs),
            "[00:00] Я: привет\n[00:01] Они: здравствуй\n[00:03] Я: как дела\n"
        );
    }

    #[test]
    fn merge_rejoins_cut_piece_without_interruption() {
        // «так.» отрезан по паузе, но собеседник в паузе молчал
        let mine = vec![piece(0, "Как-то", false), piece(200, "так.", true)];
        let theirs = vec![piece(500, "Вот.", false)];
        assert_eq!(merge_dialogue(&mine, &theirs), "[00:00] Я: Как-то так.\n[00:05] Они: Вот.\n");
    }

    #[test]
    fn merge_keeps_cut_when_interrupted() {
        let mine = vec![piece(0, "По-моему, нет.", false), piece(900, "Шаришь.", true)];
        let theirs = vec![piece(300, "Я тебе скинул скрипт?", false)];
        assert_eq!(
            merge_dialogue(&mine, &theirs),
            "[00:00] Я: По-моему, нет.\n[00:03] Они: Я тебе скинул скрипт?\n[00:09] Я: Шаришь.\n"
        );
    }

    #[test]
    fn timestamp_formats() {
        assert_eq!(timestamp(0), "[00:00]");
        assert_eq!(timestamp(19_299), "[03:12]");
        assert_eq!(timestamp(379_250), "[1:03:12]");
    }

    #[test]
    fn plain_text_joins_segments() {
        let pieces = vec![piece(0, "раз", false), piece(50, "полтора", true), piece(100, "два", false)];
        assert_eq!(plain_text(&pieces), "раз полтора\nдва\n");
        assert_eq!(plain_text(&[]), "");
    }
}
