//! Диктовка: записать голос, транскрибировать, вставить в активное окно.

use crate::insert;
use crate::notify::Notifier;
use crate::paths;
use crate::record::{self, Pidfile, RecordConfig};
use crate::transcribe;
use anyhow::{Context, Result};
use std::path::PathBuf;

/// Текст без реальной речи: пусто или только маркеры whisper вида [BLANK_AUDIO], (music).
fn is_blank(text: &str) -> bool {
    text.split_whitespace().all(|w| {
        (w.starts_with('[') && w.ends_with(']')) || (w.starts_with('(') && w.ends_with(')'))
    })
}

/// Тогл: если диктовка идёт — остановить её, иначе начать новую.
pub fn dictate(
    model: Option<PathBuf>,
    lang: &str,
    prompt: Option<&str>,
    no_notify: bool,
) -> Result<()> {
    if let Some(pid) =
        record::stop_running(&paths::dictate_pidfile()).context("не удалось остановить диктовку")?
    {
        println!("Диктовка остановлена (pid {pid}).");
        return Ok(());
    }

    let notifier = Notifier::new(no_notify);
    let wav = paths::dictate_wav();
    // Держим pid-файл до конца функции (через транскрипцию и вставку) —
    // повторный хоткей в этом окне шлёт SIGTERM живому процессу, а не
    // запускает вторую запись поверх того же WAV.
    let _pidfile = Pidfile::create(paths::dictate_pidfile())?;
    record::record_to(&RecordConfig { wav: wav.clone(), mic_only: true, tray: false }, || {
        println!(
            "Диктовка в {} — Ctrl+C или `throisma dictate` для остановки.",
            wav.display()
        );
        notifier.send("🎤 Диктовка…", "Хоткей ещё раз — остановить и вставить текст");
    })?;

    notifier.send("Транскрибирую…", "");
    let result = (|| -> Result<()> {
        let text = transcribe::transcribe_wav(&wav, model, lang, prompt)?;
        let text = text.trim();
        if is_blank(text) {
            // тишина или только маркеры whisper: клипборд не трогаем
            notifier.send("Речь не распознана", "Пустая транскрипция — ничего не вставлено");
            println!("Речь не распознана — ничего не вставлено.");
        } else {
            insert::insert_text(text)?;
            notifier.send("Вставлено", &preview(text));
            println!("{text}");
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            let _ = std::fs::remove_file(&wav);
            Ok(())
        }
        Err(e) => {
            // WAV намеренно остаётся — для повтора
            notifier.send(
                "Ошибка диктовки",
                &format!("{e:#}\nПовтор: throisma transcribe {}", wav.display()),
            );
            Err(e)
        }
    }
}

/// Обрезает текст для тела уведомления.
fn preview(text: &str) -> String {
    const MAX: usize = 120;
    if text.chars().count() <= MAX {
        text.to_string()
    } else {
        format!("{}…", text.chars().take(MAX).collect::<String>())
    }
}
