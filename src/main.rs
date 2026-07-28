mod dictate;
mod insert;
mod meta;
mod notify;
mod paths;
mod play;
mod proc;
mod record;
mod transcribe;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "throisma", about = "Простой рекордер речи/встреч с транскрибацией через whisper.cpp")]
struct Cli {
    #[command(subcommand)]
    command: Command,
    /// Не показывать desktop-уведомления
    #[arg(long, global = true)]
    no_notify: bool,
}

/// Общие параметры транскрипции.
#[derive(Args)]
struct WhisperArgs {
    /// Путь к ggml-модели whisper (или переменная THROISMA_MODEL)
    #[arg(short, long)]
    model: Option<PathBuf>,
    /// Язык (auto — автоопределение)
    #[arg(short, long, default_value = "auto")]
    lang: String,
    /// Словарь-подсказка (по умолчанию — файл prompt.txt в каталоге данных)
    #[arg(short, long)]
    prompt: Option<String>,
}

#[derive(Subcommand)]
enum Command {
    /// Начать запись с микрофона (остановка: Ctrl+C или SIGTERM)
    Record,
    /// Начать запись, либо остановить уже идущую (для горячей клавиши)
    Toggle,
    /// Диктовка: начать запись голоса, либо остановить и вставить текст (для горячей клавиши)
    Dictate {
        #[command(flatten)]
        whisper: WhisperArgs,
    },
    /// Транскрибировать запись (по умолчанию — последнюю)
    Transcribe {
        /// Путь к WAV-файлу
        file: Option<PathBuf>,
        #[command(flatten)]
        whisper: WhisperArgs,
    },
    /// Проиграть запись (по умолчанию — последнюю)
    Play {
        /// Путь к WAV-файлу
        file: Option<PathBuf>,
    },
    /// Назвать запись (по умолчанию — последнюю)
    Rename {
        /// Название
        name: String,
        /// Путь к WAV-файлу
        file: Option<PathBuf>,
    },
    /// Показать список записей
    List,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Record => record::record(cli.no_notify),
        Command::Toggle => record::toggle(cli.no_notify),
        Command::Dictate { whisper } => {
            dictate::dictate(whisper.model, &whisper.lang, whisper.prompt.as_deref(), cli.no_notify)
        }
        Command::Transcribe { file, whisper } => {
            transcribe::transcribe(file, whisper.model, &whisper.lang, whisper.prompt.as_deref())
        }
        Command::Play { file } => play::play(file),
        Command::Rename { name, file } => meta::rename(&name, file),
        Command::List => list(),
    }
}

fn list() -> Result<()> {
    let files = paths::recordings()?;
    if files.is_empty() {
        println!("Записей пока нет ({}).", paths::recordings_dir()?.display());
        return Ok(());
    }
    let names = meta::load()?;
    for f in &files {
        let size_mb = f.metadata().map(|m| m.len()).unwrap_or(0) as f64 / 1_048_576.0;
        let has_txt = f.with_extension("txt").exists();
        let stem = f.file_stem().unwrap().to_string_lossy();
        let name = names.get(stem.as_ref()).map_or(String::new(), |m| format!("{}  ", m.name));
        println!(
            "{}  {:>7.1} MB  {}{}",
            f.file_name().unwrap().to_string_lossy(),
            size_mb,
            name,
            if has_txt { "[есть транскрипт]" } else { "" }
        );
    }
    Ok(())
}
