//! Диктовка: записать голос, транскрибировать, вставить в активное окно.

use crate::insert;
use crate::notify::Notifier;
use crate::paths;
use crate::record::{self, RecordConfig};
use crate::transcribe;
use anyhow::{Context, Result};
use nix::sys::signal::{self, Signal};
use std::path::PathBuf;

/// Тогл: если диктовка идёт — остановить её, иначе начать новую.
pub fn dictate(model: Option<PathBuf>, lang: &str, no_notify: bool) -> Result<()> {
    if let Some(pid) = record::running_recording(&paths::dictate_pidfile())? {
        signal::kill(pid, Signal::SIGTERM).context("не удалось остановить диктовку")?;
        println!("Диктовка остановлена (pid {pid}).");
        return Ok(());
    }

    let notifier = Notifier::new(no_notify);
    let wav = paths::dictate_wav();
    record::record_to(
        &RecordConfig {
            wav: wav.clone(),
            mic_only: true,
            pidfile: paths::dictate_pidfile(),
        },
        || {
            println!(
                "Диктовка в {} — Ctrl+C или `throisma dictate` для остановки.",
                wav.display()
            );
            notifier.send("🎤 Диктовка…", "Хоткей ещё раз — остановить и вставить текст");
        },
    )?;

    notifier.send("Транскрибирую…", "");
    let result = transcribe::transcribe_wav(&wav, model, lang).and_then(|text| {
        let text = text.trim().to_string();
        insert::insert_text(&text)?;
        Ok(text)
    });
    match result {
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
