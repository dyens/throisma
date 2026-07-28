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
    Record,
    /// Начать запись, либо остановить уже идущую (для горячей клавиши)
    Toggle,
    /// Транскрибировать запись (по умолчанию — последнюю)
    Transcribe {
        /// Путь к WAV-файлу
        file: Option<PathBuf>,
        /// Путь к ggml-модели whisper (или переменная THROISMA_MODEL)
        #[arg(short, long)]
        model: Option<PathBuf>,
        /// Бинарь whisper.cpp
        #[arg(long, default_value = "whisper-cli")]
        whisper_bin: String,
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
        Command::Record => record::record(),
        Command::Toggle => record::toggle(),
        Command::Transcribe { file, model, whisper_bin, lang } => {
            transcribe::transcribe(file, model, &whisper_bin, &lang)
        }
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
