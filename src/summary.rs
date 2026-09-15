//! Итоги встречи: транскрипт → Claude Code (`claude -p`) → `<запись>.summary.md`.

use crate::meta;
use crate::paths;
use crate::proc;
use crate::transcribe;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub fn summary(
    file: Option<PathBuf>,
    focus: Option<&str>,
    model: Option<PathBuf>,
    lang: &str,
    prompt: Option<&str>,
    speakers: Option<usize>,
) -> Result<()> {
    let file = match file {
        Some(f) => f,
        None => paths::latest_recording()?,
    };
    // можно дать и запись, и готовый транскрипт
    let txt = file.with_extension("txt");
    if !txt.exists() {
        transcribe::transcribe(Some(file.clone()), model, lang, prompt, speakers)?;
    }
    let transcript = std::fs::read_to_string(&txt)
        .with_context(|| format!("не удалось прочитать {}", txt.display()))?;

    let stem = file.file_stem().unwrap_or_default().to_string_lossy().into_owned();
    let title = meta::load()?.remove(&stem).map(|m| m.name);
    let dictionary = std::fs::read_to_string(paths::prompt_file()?).ok();
    let request = build_request(&transcript, title.as_deref(), dictionary.as_deref(), focus);

    let out = file.with_extension("summary.md");
    println!("Составляю итоги встречи через Claude …");
    write_atomically(&out, |tmp| {
        let mut claude = Command::new("claude");
        // без инструментов: модель только читает транскрипт из stdin
        claude
            .args(["-p", "--tools", "", "--no-session-persistence"])
            .stdout(Stdio::from(std::fs::File::create(tmp)?));
        proc::run(claude, Some(&request))
            .context("нужен Claude Code CLI (`claude`) в PATH и выполненный вход")
    })?;

    println!("Итоги: {}", out.display());
    Ok(())
}

/// Пишет файл через временный рядом: при ошибке не остаётся обрезанного результата.
fn write_atomically(path: &Path, write: impl FnOnce(&Path) -> Result<()>) -> Result<()> {
    let tmp = path.with_extension("md.tmp");
    let result = write(&tmp).and_then(|()| {
        std::fs::rename(&tmp, path).with_context(|| format!("не удалось записать {}", path.display()))
    });
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Запрос к модели: как читать транскрипт, какие разделы нужны, сам транскрипт.
fn build_request(
    transcript: &str,
    title: Option<&str>,
    dictionary: Option<&str>,
    focus: Option<&str>,
) -> String {
    let mut req = String::from(
        "Ниже транскрипт рабочей встречи, автоматически распознанный whisper. \
         «Я» — автор записи, «Они» (или «Они 1», «Они 2», …) — собеседники; \
         в начале строк время от начала записи.\n\
         Распознавание неточное: термины, особенно английские, бывают искажены \
         или записаны кириллицей по звучанию — восстанавливай их по контексту",
    );
    match dictionary.map(str::trim).filter(|d| !d.is_empty()) {
        Some(d) => req.push_str(&format!(" и словарю терминов команды: {d}.\n")),
        None => req.push_str(".\n"),
    }
    req.push_str(
        "Не выдумывай того, чего в транскрипте нет; неясное так и помечай.\n\n\
         Составь итоги встречи в Markdown на языке встречи. Ответ — только \
         Markdown, без вступления. Разделы:\n\
         ## TL;DR — 2–3 предложения.\n\
         ## Темы — списком, у каждой время начала обсуждения.\n\
         ## Решения\n\
         ## Action items — `- [ ] Задача — **Ответственный**`, если он понятен.\n\
         ## Открытые вопросы\n\
         ## Риски и блокеры\n\
         Если разделу нечего сказать — «Нет».\n",
    );
    if let Some(t) = title {
        req.push_str(&format!("\nНазвание встречи: {t}\n"));
    }
    if let Some(f) = focus {
        req.push_str(&format!("\nОсобое внимание: {f}\n"));
    }
    req.push_str(&format!("\n<transcript>\n{}\n</transcript>\n", transcript.trim_end()));
    req
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_contains_transcript_and_options() {
        let req = build_request("[00:00] Я: пурш\n", Some("Синк"), Some("purge, tenant"), Some("риски"));
        assert!(req.contains("словарю терминов команды: purge, tenant."));
        assert!(req.contains("Название встречи: Синк"));
        assert!(req.contains("Особое внимание: риски"));
        assert!(req.ends_with("<transcript>\n[00:00] Я: пурш\n</transcript>\n"));
    }

    #[test]
    fn request_without_options() {
        let req = build_request("текст", None, Some("  \n"), None);
        assert!(req.contains("по контексту.\n"));
        assert!(!req.contains("Название встречи"));
        assert!(!req.contains("Особое внимание"));
    }
}
