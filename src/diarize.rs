//! Диаризация канала собеседников: какой голос когда говорит. Считает её
//! помощник throisma-diarize (src/bin/throisma-diarize.rs) — в одном
//! процессе с whisper.cpp sherpa-onnx падает; здесь запуск и чистка результата.

use crate::paths;
use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::{Child, Command, Stdio};

/// Отрезок речи одного голоса, в сотых долях секунды от начала записи.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Turn {
    pub(crate) start_cs: i64,
    pub(crate) end_cs: i64,
    pub(crate) speaker: usize,
}

/// Диаризация, идущая в фоне.
pub(crate) struct Pending(Child);

/// Запускает в фоне диаризацию канала `channel` файла `wav`.
/// `None` — модели или помощник не установлены.
pub(crate) fn start(wav: &Path, channel: usize) -> Result<Option<Pending>> {
    let (segmentation, embedding) = paths::diarization_models()?;
    if !segmentation.exists() || !embedding.exists() {
        return Ok(None);
    }
    let helper = std::env::current_exe()?.with_file_name("throisma-diarize");
    if !helper.exists() {
        eprintln!(
            "модели диаризации скачаны, но нет {} — собеседники не делятся по голосам",
            helper.display()
        );
        return Ok(None);
    }
    let child = Command::new(&helper)
        .arg(wav)
        .arg(channel.to_string())
        .arg(segmentation)
        .arg(embedding)
        .stdout(Stdio::piped())
        .spawn()
        .with_context(|| format!("не удалось запустить {}", helper.display()))?;
    Ok(Some(Pending(child)))
}

impl Pending {
    /// Ждёт помощника и чистит результат; `speakers` — сколько голосов оставить.
    pub(crate) fn wait(self, speakers: Option<usize>) -> Result<Vec<Turn>> {
        let out = self.0.wait_with_output().context("ошибка ожидания throisma-diarize")?;
        if !out.status.success() {
            bail!("throisma-diarize завершился с ошибкой");
        }
        Ok(keep_main_voices(parse(&String::from_utf8_lossy(&out.stdout))?, speakers))
    }
}

/// Строки помощника `начало конец голос` → отрезки.
fn parse(output: &str) -> Result<Vec<Turn>> {
    output
        .lines()
        .map(|line| {
            let nums: Vec<i64> = line
                .split_whitespace()
                .map(str::parse)
                .collect::<Result<_, _>>()
                .with_context(|| format!("непонятная строка диаризации: {line}"))?;
            let [start_cs, end_cs, speaker] = nums[..] else {
                bail!("непонятная строка диаризации: {line}");
            };
            Ok(Turn { start_cs, end_cs, speaker: speaker as usize })
        })
        .collect()
}

/// Сколько голос должен наговорить, чтобы считаться отдельным собеседником.
const MIN_VOICE_CS: i64 = 3000;

/// Отбрасывает мелкие кластеры: чаще это тот же голос в другой интонации или
/// шум, чем новый собеседник (на встрече с одним собеседником набегало до
/// 17 с при 155 с основного голоса). Остаются `speakers` самых разговорчивых
/// голосов, а без него — наговорившие хотя бы MIN_VOICE_CS (и хотя бы один
/// голос); реплики остальных достанутся ближайшему оставшемуся (speaker_at).
/// Задать число кластеров самой диаризации хуже: она склеивает разных людей.
fn keep_main_voices(turns: Vec<Turn>, speakers: Option<usize>) -> Vec<Turn> {
    let mut totals: Vec<(usize, i64)> = Vec::new();
    for t in &turns {
        match totals.iter_mut().find(|(s, _)| *s == t.speaker) {
            Some((_, total)) => *total += t.end_cs - t.start_cs,
            None => totals.push((t.speaker, t.end_cs - t.start_cs)),
        }
    }
    totals.sort_by_key(|&(_, total)| std::cmp::Reverse(total));
    let keep: Vec<usize> = match speakers {
        Some(n) => totals.iter().take(n.max(1)).map(|&(s, _)| s).collect(),
        None => totals
            .iter()
            .enumerate()
            .filter(|&(i, &(_, total))| i == 0 || total >= MIN_VOICE_CS)
            .map(|(_, &(s, _))| s)
            .collect(),
    };
    turns.into_iter().filter(|t| keep.contains(&t.speaker)).collect()
}

/// Голос в момент `cs`: отрезок, который его накрывает, иначе ближайший.
/// `None` — отрезков нет.
pub(crate) fn speaker_at(turns: &[Turn], cs: i64) -> Option<usize> {
    turns
        .iter()
        .min_by_key(|t| (t.start_cs - cs).max(cs - t.end_cs).max(0))
        .map(|t| t.speaker)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(start_cs: i64, end_cs: i64, speaker: usize) -> Turn {
        Turn { start_cs, end_cs, speaker }
    }

    #[test]
    fn speaker_inside_and_between_turns() {
        let turns = vec![turn(0, 100, 0), turn(300, 400, 1)];
        assert_eq!(speaker_at(&turns, 50), Some(0));
        assert_eq!(speaker_at(&turns, 350), Some(1));
        assert_eq!(speaker_at(&turns, 150), Some(0)); // ближе к концу первого
        assert_eq!(speaker_at(&turns, 280), Some(1)); // ближе к началу второго
        assert_eq!(speaker_at(&[], 50), None);
    }

    #[test]
    fn parses_helper_output() {
        assert_eq!(parse("10 250 0\n300 420 2\n").unwrap(), vec![turn(10, 250, 0), turn(300, 420, 2)]);
        assert_eq!(parse("").unwrap(), vec![]);
        assert!(parse("10 abc 0").is_err());
        assert!(parse("10 20").is_err());
    }

    #[test]
    fn minor_voices_dropped() {
        // голос 0 — 60 с, голос 1 — 40 с, голос 2 — 17 с шума
        let turns = vec![turn(0, 6000, 0), turn(6000, 10000, 1), turn(10000, 11700, 2)];
        assert_eq!(keep_main_voices(turns, None), vec![turn(0, 6000, 0), turn(6000, 10000, 1)]);
    }

    #[test]
    fn short_recording_keeps_top_voice() {
        let turns = vec![turn(0, 500, 1), turn(500, 700, 0)];
        assert_eq!(keep_main_voices(turns, None), vec![turn(0, 500, 1)]);
    }

    #[test]
    fn speakers_keeps_most_talkative() {
        // голос 2 наговорил мало, но просили троих
        let turns = vec![turn(0, 6000, 0), turn(6000, 6500, 2), turn(6500, 9000, 1), turn(9000, 9100, 3)];
        assert_eq!(
            keep_main_voices(turns, Some(3)),
            vec![turn(0, 6000, 0), turn(6000, 6500, 2), turn(6500, 9000, 1)]
        );
    }
}
