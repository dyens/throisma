//! Воспроизведение записей.

use crate::paths;
use crate::proc;
use anyhow::{Context, Result};
use std::path::PathBuf;
use std::process::Command;

pub fn play(file: Option<PathBuf>) -> Result<()> {
    let wav = match file {
        Some(f) => f,
        None => paths::latest_recording()?,
    };
    paths::require_exists(&wav)?;
    println!("Играю {} … (Ctrl+C — остановить)", wav.display());
    let mut cmd = Command::new("pw-play");
    cmd.arg(&wav);
    proc::run(cmd, None).context("pw-play входит в pipewire-utils")?;
    Ok(())
}
