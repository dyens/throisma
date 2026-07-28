//! Запуск внешних утилит.

use anyhow::{bail, Context, Result};
use std::io::Write;
use std::process::{Command, Stdio};

/// Запускает команду, при `stdin` передаёт текст процессу, ждёт завершения
/// и превращает неуспешный exit-код в ошибку.
pub(crate) fn run(mut command: Command, stdin: Option<&str>) -> Result<()> {
    let cmd = command.get_program().to_string_lossy().into_owned();
    if stdin.is_some() {
        command.stdin(Stdio::piped());
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("не удалось запустить {cmd}"))?;
    if let Some(input) = stdin {
        child
            .stdin
            .take()
            .expect("stdin запрошен строкой выше")
            .write_all(input.as_bytes())?;
    }
    let status = child.wait()?;
    if !status.success() {
        bail!("{cmd} завершился с ошибкой");
    }
    Ok(())
}
