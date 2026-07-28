mod dictate;
mod insert;
mod notify;
mod paths;
mod record;
mod transcribe;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "throisma", about = "Простой рекордер речи/встреч с транскрибацией через whisper.cpp")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Начать запись с микрофона (остановка: Ctrl+C или SIGTERM)
    Record {
        /// Не показывать desktop-уведомления
        #[arg(long)]
        no_notify: bool,
    },
    /// Начать запись, либо остановить уже идущую (для горячей клавиши)
    Toggle {
        /// Не показывать desktop-уведомления
        #[arg(long)]
        no_notify: bool,
    },
    /// Диктовка: начать запись голоса, либо остановить и вставить текст (для горячей клавиши)
    Dictate {
        /// Путь к ggml-модели whisper (или переменная THROISMA_MODEL)
        #[arg(short, long)]
        model: Option<PathBuf>,
        /// Язык (auto — автоопределение)
        #[arg(short, long, default_value = "auto")]
        lang: String,
        /// Не показывать desktop-уведомления
        #[arg(long)]
        no_notify: bool,
    },
    /// Транскрибировать запись (по умолчанию — последнюю)
    Transcribe {
        /// Путь к WAV-файлу
        file: Option<PathBuf>,
        /// Путь к ggml-модели whisper (или переменная THROISMA_MODEL)
        #[arg(short, long)]
        model: Option<PathBuf>,
        /// Язык (auto — автоопределение)
        #[arg(short, long, default_value = "auto")]
        lang: String,
    },
    /// Показать список записей
    List,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Record { no_notify } => record::record(no_notify),
        Command::Toggle { no_notify } => record::toggle(no_notify),
        Command::Dictate { model, lang, no_notify } => dictate::dictate(model, &lang, no_notify),
        Command::Transcribe { file, model, lang } => transcribe::transcribe(file, model, &lang),
        Command::List => list(),
    }
}

fn list() -> Result<()> {
    let files = paths::recordings()?;
    if files.is_empty() {
        println!("Записей пока нет ({}).", paths::recordings_dir()?.display());
        return Ok(());
    }
    for f in &files {
        let size_mb = f.metadata().map(|m| m.len()).unwrap_or(0) as f64 / 1_048_576.0;
        let has_txt = f.with_extension("txt").exists();
        println!(
            "{}  {:>7.1} MB  {}",
            f.file_name().unwrap().to_string_lossy(),
            size_mb,
            if has_txt { "[есть транскрипт]" } else { "" }
        );
    }
    Ok(())
}
