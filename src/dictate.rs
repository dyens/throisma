//! Диктовка: записать голос, транскрибировать, вставить в активное окно.

use crate::insert;
use crate::notify::Notifier;
use crate::paths;
use crate::record::{self, Pidfile, RecordConfig};
use crate::transcribe;
use anyhow::{Context, Result};
use nix::sys::signal::{self, Signal};
use std::path::PathBuf;

/// Текст без реальной речи: пусто или только маркеры whisper вида [BLANK_AUDIO], (music).
fn is_blank(text: &str) -> bool {
    text.split_whitespace().all(|w| {
        (w.starts_with('[') && w.ends_with(']')) || (w.starts_with('(') && w.ends_with(')'))
    })
}

/// Тогл: если диктовка идёт — остановить её, иначе начать новую.
pub fn dictate(model: Option<PathBuf>, lang: &str, no_notify: bool) -> Result<()> {
    if let Some(pid) = record::running_recording(&paths::dictate_pidfile())? {
        signal::kill(pid, Signal::SIGTERM).context("не удалось остановить диктовку")?;
        println!("Диктовка остановлена (pid {pid}).");
        return Ok(());
    }

    let notifier = Notifier::new(no_notify);
    let wav = paths::dictate_wav();
    // Держим pid-файл до конца функции (через транскрипцию и вставку) —
    // повторный хоткей в этом окне шлёт SIGTERM живому процессу, а не
    // запускает вторую запись поверх того же WAV.
    let _pidfile = Pidfile::create(paths::dictate_pidfile())?;
    record::record_to(&RecordConfig { wav: wav.clone(), mic_only: true }, || {
        println!(
            "Диктовка в {} — Ctrl+C или `throisma dictate` для остановки.",
            wav.display()
        );
        notifier.send("🎤 Диктовка…", "Хоткей ещё раз — остановить и вставить текст");
    })?;

    notifier.send("Транскрибирую…", "");
    let result = transcribe::transcribe_wav(&wav, model, lang).and_then(|text| {
        let text = text.trim().to_string();
        if !is_blank(&text) {
            insert::insert_text(&text)?;
        }
        Ok(text)
    });
    match result {
        // тишина или только маркеры whisper: клипборд не трогаем, «Вставлено» не сообщаем
        Ok(text) if is_blank(&text) => {
            let _ = std::fs::remove_file(&wav);
            notifier.send("Речь не распознана", "Пустая транскрипция — ничего не вставлено");
            println!("Речь не распознана — ничего не вставлено.");
            Ok(())
        }
        Ok(text) => {
            let _ = std::fs::remove_file(&wav);
            notifier.send("Вставлено", &preview(&text));
            println!("{text}");
            Ok(())
        }
        Err(e) => {
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
