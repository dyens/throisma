//! Помощник диаризации для throisma: печатает отрезки голосов канала WAV
//! строками `начало конец голос` (сотые доли секунды).
//!
//! Отдельный бинарь, потому что статический onnxruntime из sherpa-onnx и
//! whisper.cpp в одном процессе делят инстанциации std::regex, собранные
//! разными компиляторами, и onnxruntime падает при инициализации.
//!
//! Использование: throisma-diarize <wav> <канал> <сегментация.onnx> <эмбеддинг.onnx> [голосов]

use sherpa_onnx::{
    FastClusteringConfig, OfflineSpeakerDiarization, OfflineSpeakerDiarizationConfig,
    OfflineSpeakerSegmentationModelConfig, OfflineSpeakerSegmentationPyannoteModelConfig,
    SpeakerEmbeddingExtractorConfig,
};
use std::process::ExitCode;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("throisma-diarize: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [wav, channel, segmentation, embedding, rest @ ..] = args.as_slice() else {
        return Err("использование: <wav> <канал> <сегментация.onnx> <эмбеддинг.onnx> [голосов]".into());
    };
    let channel: usize = channel.parse().map_err(|_| "канал — число")?;
    let speakers: i32 = match rest.first() {
        Some(n) => n.parse().map_err(|_| "число голосов — число")?,
        None => -1,
    };

    let mut reader = hound::WavReader::open(wav).map_err(|e| format!("{wav}: {e}"))?;
    let spec = reader.spec();
    if spec.sample_rate != 16_000 || spec.bits_per_sample != 16 {
        return Err(format!("{wav}: ожидается WAV 16 кГц s16"));
    }
    let n = spec.channels as usize;
    if channel >= n {
        return Err(format!("{wav}: нет канала {channel}"));
    }
    let mut samples = Vec::new();
    for (i, s) in reader.samples::<i16>().enumerate() {
        let s = s.map_err(|e| format!("{wav}: {e}"))?;
        if i % n == channel {
            samples.push(s as f32 / 32768.0);
        }
    }

    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()) as i32;
    let config = OfflineSpeakerDiarizationConfig {
        segmentation: OfflineSpeakerSegmentationModelConfig {
            pyannote: OfflineSpeakerSegmentationPyannoteModelConfig {
                model: Some(segmentation.clone()),
                ..Default::default()
            },
            num_threads: threads,
            ..Default::default()
        },
        embedding: SpeakerEmbeddingExtractorConfig {
            model: Some(embedding.clone()),
            num_threads: threads,
            ..Default::default()
        },
        clustering: FastClusteringConfig { num_clusters: speakers, ..Default::default() },
        ..Default::default()
    };
    let diarizer =
        OfflineSpeakerDiarization::create(&config).ok_or("не удалось загрузить модели диаризации")?;
    let result = diarizer.process(&samples).ok_or("ошибка диаризации")?;
    for s in result.sort_by_start_time() {
        println!("{} {} {}", (s.start * 100.0) as i64, (s.end * 100.0) as i64, s.speaker.max(0));
    }
    Ok(())
}
