# Dictate Mode Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Команда `throisma dictate` — хоткей-тогл «запись голоса → стоп → транскрипт вставляется в активное окно» + desktop-уведомления о ходе записи.

**Architecture:** Первый вызов `dictate` — сам процесс записи (пишет только микрофон во временный WAV, живёт до SIGTERM, затем транскрибирует и вставляет текст через wl-copy+wtype). Второй вызов находит его по отдельному pid-файлу и шлёт SIGTERM. Существующие `record`/`toggle` переиспользуют ту же параметризованную запись и получают уведомления.

**Tech Stack:** Rust, pipewire (запись), whisper-rs (транскрипция), hound (WAV); внешние утилиты: `notify-send`, `wl-copy`, `wtype` (Hyprland/Wayland). Новых крейтов нет.

**Спека:** `docs/superpowers/specs/2026-07-28-dictate-mode-design.md`

## Global Constraints

- Никаких новых зависимостей в Cargo.toml.
- Все пользовательские строки — по-русски, стиль существующих сообщений.
- Уведомления отключаются флагом `--no-notify` и переменной `THROISMA_NO_NOTIFY=1`; отсутствие `notify-send` — молча пропускать.
- Pid-файл диктовки отдельный от pid-файла записи встреч (хоткеи не останавливают чужой тип записи).
- Временный WAV диктовки удаляется только при полном успехе (транскрипция + вставка).
- В `insert` сначала `wl-copy`, потом `wtype`: при падении wtype текст уже в клипборде.
- Юнит-тестов нет (всё I/O: PipeWire, D-Bus, Wayland) — каждая задача проверяется вручную командами из шагов.

---

### Task 1: Модуль notify + уведомления для записи встреч + `--no-notify`

**Files:**
- Create: `src/notify.rs`
- Modify: `src/record.rs` (сигнатуры `record`/`toggle`, уведомления)
- Modify: `src/main.rs` (флаг `--no-notify` у Record/Toggle, `mod notify`)

**Interfaces:**
- Produces: `notify::Notifier` — `Notifier::new(no_notify: bool) -> Notifier`, `fn send(&self, summary: &str, body: &str)`; `record::record(no_notify: bool) -> Result<()>`, `record::toggle(no_notify: bool) -> Result<()>`.

- [ ] **Step 1: Создать `src/notify.rs`**

```rust
//! Уведомления на рабочий стол через notify-send.

use std::process::Command;

pub(crate) struct Notifier {
    enabled: bool,
}

impl Notifier {
    /// `no_notify` — флаг --no-notify; дополнительно учитывается THROISMA_NO_NOTIFY=1.
    pub(crate) fn new(no_notify: bool) -> Notifier {
        let env_off = std::env::var("THROISMA_NO_NOTIFY").is_ok_and(|v| v == "1");
        Notifier { enabled: !no_notify && !env_off }
    }

    /// Показывает уведомление; ошибки (нет notify-send и т.п.) молча игнорируются.
    pub(crate) fn send(&self, summary: &str, body: &str) {
        if !self.enabled {
            return;
        }
        let _ = Command::new("notify-send")
            .args(["--app-name=throisma", summary, body])
            .status();
    }
}
```

- [ ] **Step 2: Подключить модуль и флаги в `src/main.rs`**

К списку модулей добавить `mod notify;`. Сабкоманды Record и Toggle получают флаг:

```rust
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
```

и в `match`:

```rust
        Command::Record { no_notify } => record::record(no_notify),
        Command::Toggle { no_notify } => record::toggle(no_notify),
```

- [ ] **Step 3: Уведомления в `src/record.rs`**

`toggle` и `record` принимают `no_notify: bool`. В начале файла `use crate::notify::Notifier;`.

```rust
/// Если запись уже идёт — остановить её, иначе начать новую.
pub fn toggle(no_notify: bool) -> Result<()> {
    if let Some(pid) = running_recording()? {
        signal::kill(pid, Signal::SIGTERM).context("не удалось остановить запись")?;
        println!("Запись остановлена (pid {pid}).");
        return Ok(());
    }
    record(no_notify)
}
```

В `record(no_notify: bool)`: создать `let notifier = Notifier::new(no_notify);` в начале; сразу после существующего `println!("Идёт запись …")` добавить

```rust
    notifier.send("⏺ Идёт запись встречи", &wav_path.display().to_string());
```

и после существующего `println!("Готово: …")` в конце добавить

```rust
    notifier.send("Готово", &wav_path.display().to_string());
```

- [ ] **Step 4: Собрать и проверить вручную**

```bash
cargo build 2>&1 | tail -3          # должно собраться без ошибок
cargo run -q -- toggle              # уведомление «⏺ Идёт запись встречи»
cargo run -q -- toggle              # остановка; в первом процессе уведомление «Готово»
cargo run -q -- record --no-notify  # без уведомлений; Ctrl+C
THROISMA_NO_NOTIFY=1 cargo run -q -- record  # без уведомлений; Ctrl+C
```

- [ ] **Step 5: Commit**

```bash
git add src/notify.rs src/record.rs src/main.rs
git commit -m "Add desktop notifications for meeting recording"
```

---

### Task 2: Выделить `transcribe_wav` (рефакторинг без изменения поведения)

**Files:**
- Modify: `src/transcribe.rs`

**Interfaces:**
- Produces: `transcribe::transcribe_wav(wav: &Path, model: Option<PathBuf>, lang: &str) -> Result<String>` — возвращает текст транскрипта (сегменты через `\n`, без trailing-обрезки); резолвит модель (аргумент → `THROISMA_MODEL` → путь по умолчанию) внутри себя. Публичная `transcribe(file, model, lang)` сохраняет текущее поведение (пишет `.txt` рядом с WAV).

- [ ] **Step 1: Рефакторинг `src/transcribe.rs`**

Функция `transcribe` разделяется: всё от резолва модели до сбора сегментов уходит в `transcribe_wav`, запись `.txt` остаётся в `transcribe`.

```rust
use crate::paths;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

pub fn transcribe(file: Option<PathBuf>, model: Option<PathBuf>, lang: &str) -> Result<()> {
    let wav = match file {
        Some(f) => f,
        None => latest_recording()?,
    };
    println!("Транскрибирую {} …", wav.display());
    let text = transcribe_wav(&wav, model, lang)?;

    let txt = wav.with_extension("txt");
    std::fs::write(&txt, &text)
        .with_context(|| format!("не удалось записать {}", txt.display()))?;

    println!("\nТранскрипт: {}", txt.display());
    Ok(())
}

/// Транскрибирует WAV и возвращает текст (сегменты, разделённые \n).
pub(crate) fn transcribe_wav(wav: &Path, model: Option<PathBuf>, lang: &str) -> Result<String> {
    if !wav.exists() {
        bail!("файл не найден: {}", wav.display());
    }

    let model = match model.or_else(|| std::env::var("THROISMA_MODEL").ok().map(PathBuf::from)) {
        Some(m) => m,
        None => paths::default_model()?,
    };
    if !model.exists() {
        bail!(
            "модель не найдена: {}\n\
             Скачайте её, например:\n\
             mkdir -p {} && curl -L -o {} \\\n\
             https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin",
            model.display(),
            model.parent().unwrap_or(&model).display(),
            model.display(),
        );
    }

    let samples = read_wav(wav)?;

    // глушим болтливый лог whisper.cpp/ggml в stderr
    whisper_rs::install_logging_hooks();
    let ctx = WhisperContext::new_with_params(
        model.to_str().context("путь к модели содержит не-UTF-8 символы")?,
        WhisperContextParameters::default(),
    )
    .context("не удалось загрузить модель whisper")?;
    let mut state = ctx.create_state().context("не удалось создать состояние whisper")?;

    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_language(Some(lang));
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);

    state.full(params, &samples).context("ошибка транскрибации")?;

    let mut text = String::new();
    for segment in state.as_iter() {
        text.push_str(&segment.to_str_lossy().context("не удалось прочитать сегмент")?);
        text.push('\n');
    }
    Ok(text.trim_start().to_string())
}
```

`read_wav` меняет сигнатуру на `fn read_wav(wav: &Path) -> Result<Vec<f32>>` (было `&PathBuf`) — тело без изменений. `latest_recording` без изменений. Проверка `wav.exists()` переезжает из `transcribe` в `transcribe_wav` (двойной проверки быть не должно).

- [ ] **Step 2: Проверить, что поведение не изменилось**

```bash
cargo build 2>&1 | tail -3
cp ~/.local/share/throisma/recordings/2026-07-28_15-33-58.txt $CLAUDE_JOB_DIR/tmp/before.txt
cargo run -q -- transcribe
diff $CLAUDE_JOB_DIR/tmp/before.txt ~/.local/share/throisma/recordings/2026-07-28_15-33-58.txt && echo OK
```

Expected: `OK` (транскрипт побайтово тот же).

- [ ] **Step 3: Commit**

```bash
git add src/transcribe.rs
git commit -m "Extract transcribe_wav returning text"
```

---

### Task 3: Параметризовать запись (`record_to`, mic-only, свой pid-файл)

**Files:**
- Modify: `src/record.rs`
- Modify: `src/paths.rs`

**Interfaces:**
- Consumes: `notify::Notifier` из Task 1.
- Produces:
  - `record::RecordConfig { wav: PathBuf, mic_only: bool }` (все поля `pub(crate)`); pid-файл в конфиг не входит — его создаёт и держит вызывающий (`record()`/`dictate()`), а не `record_to`;
  - `record::record_to(cfg: &RecordConfig, on_started: impl FnOnce()) -> Result<()>` — блокируется до SIGTERM/Ctrl+C, финализирует WAV; сам ничего не печатает и не уведомляет и не создаёт pid-файл, но вызывает `on_started` после подключения стримов, прямо перед стартом цикла — чтобы «запись пошла» сообщалось только когда она реально идёт;
  - `record::Pidfile` и `record::Pidfile::create(path: PathBuf) -> Result<Pidfile>` — теперь `pub(crate)`, чтобы `dictate()` тоже мог создавать и держать pid-файл (в т.ч. на время транскрипции и вставки, а не только записи);
  - `record::running_recording(pidfile: &Path) -> Result<Option<Pid>>` — теперь `pub(crate)` и с параметром;
  - `paths::runtime_dir() -> PathBuf` (`dirs::runtime_dir()` c fallback на temp).

- [ ] **Step 1: `src/paths.rs` — выделить runtime_dir**

```rust
/// $XDG_RUNTIME_DIR (fallback — временный каталог).
pub(crate) fn runtime_dir() -> PathBuf {
    dirs::runtime_dir().unwrap_or_else(std::env::temp_dir)
}

/// pid-файл идущей записи.
pub(crate) fn pidfile() -> PathBuf {
    runtime_dir().join("throisma.pid")
}
```

- [ ] **Step 2: `src/record.rs` — конфиг и параметризация**

Добавить структуру и переписать `record`/`record_to`:

```rust
/// Параметры одной записи.
pub(crate) struct RecordConfig {
    pub(crate) wav: PathBuf,
    /// Только микрофон (диктовка) или микрофон + системный звук (встречи).
    pub(crate) mic_only: bool,
}

pub fn record(no_notify: bool) -> Result<()> {
    let notifier = Notifier::new(no_notify);
    let wav = paths::recordings_dir()?
        .join(chrono::Local::now().format("%Y-%m-%d_%H-%M-%S.wav").to_string());
    let _pidfile = Pidfile::create(paths::pidfile())?;
    record_to(&RecordConfig { wav: wav.clone(), mic_only: false }, || {
        println!(
            "Идёт запись (микрофон + системный звук) в {} — Ctrl+C или `throisma toggle` для остановки.",
            wav.display()
        );
        notifier.send("⏺ Идёт запись встречи", &wav.display().to_string());
    })?;
    println!("Готово: {}", wav.display());
    notifier.send("Готово", &wav.display().to_string());
    Ok(())
}

/// Пишет звук в cfg.wav до SIGTERM/Ctrl+C. Сама молчалива: вывод — забота
/// вызывающего; pid-файл создаёт и держит вызывающий. `on_started`
/// вызывается, когда запись реально пошла (стримы подключены).
pub(crate) fn record_to(cfg: &RecordConfig, on_started: impl FnOnce()) -> Result<()> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None).context("PipeWire main loop")?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None).context("не удалось подключиться к PipeWire")?;

    let mixer = Rc::new(RefCell::new(Mixer::create(&cfg.wav, cfg.mic_only)?));
    let mic_stream = capture_stream(&core, Source::Mic, mixer.clone())?;
    let sys_stream = if cfg.mic_only {
        None
    } else {
        Some(capture_stream(&core, Source::SinkMonitor, mixer.clone())?)
    };

    let stop = Arc::new(AtomicBool::new(false));
    ctrlc::set_handler({
        let stop = stop.clone();
        move || stop.store(true, Ordering::SeqCst)
    })?;

    // pipewire-цикл нельзя прервать из обработчика сигнала напрямую —
    // раз в 100 мс проверяем флаг остановки таймером внутри цикла
    let timer = mainloop.loop_().add_timer({
        let mainloop = mainloop.clone();
        let stop = stop.clone();
        move |_| {
            if stop.load(Ordering::SeqCst) {
                mainloop.quit();
            }
        }
    });
    timer
        .update_timer(Some(Duration::from_millis(100)), Some(Duration::from_millis(100)))
        .into_result()?;

    on_started();
    mainloop.run();

    // стримы держат клоны mixer — отпускаем их, чтобы забрать его целиком
    drop((mic_stream, sys_stream));
    Rc::try_unwrap(mixer)
        .map_err(|_| anyhow::anyhow!("mixer всё ещё используется"))?
        .into_inner()
        .finalize()
}
```

- [ ] **Step 3: `src/record.rs` — сопутствующие правки**

`toggle` и `running_recording`:

```rust
/// Если запись уже идёт — остановить её, иначе начать новую.
pub fn toggle(no_notify: bool) -> Result<()> {
    if let Some(pid) = running_recording(&paths::pidfile())? {
        signal::kill(pid, Signal::SIGTERM).context("не удалось остановить запись")?;
        println!("Запись остановлена (pid {pid}).");
        return Ok(());
    }
    record(no_notify)
}

/// pid идущей записи, если она есть; заодно подчищает устаревший pid-файл.
pub(crate) fn running_recording(pidfile: &Path) -> Result<Option<Pid>> {
    let Ok(contents) = std::fs::read_to_string(pidfile) else {
        return Ok(None);
    };
    let pid = contents.trim().parse::<i32>().map(Pid::from_raw);
    // сигнал 0 — проверка, что процесс жив
    if let Ok(pid) = pid {
        if signal::kill(pid, None).is_ok() {
            return Ok(Some(pid));
        }
    }
    let _ = std::fs::remove_file(pidfile);
    Ok(None)
}
```

`Pidfile` и `Pidfile::create` — `pub(crate)` (создаёт и держит вызывающий:
`record()` до конца функции, а в Task 4 — `dictate()` до конца функции, через
транскрипцию и вставку):

```rust
/// Гарантия «pid-файл существует, пока жив владелец» (записывающая и, для
/// диктовки, транскрибирующая/вставляющая фаза): создаётся вызывающим и
/// удаляется при выходе из scope.
pub(crate) struct Pidfile(PathBuf);

impl Pidfile {
    pub(crate) fn create(path: PathBuf) -> Result<Pidfile> {
        std::fs::write(&path, std::process::id().to_string())?;
        Ok(Pidfile(path))
    }
}
```

`Mixer` учится писать mic-дорожку сразу (mic-only — второй дорожки не будет,
ждать выравнивания нельзя):

```rust
struct Mixer {
    mic: VecDeque<i16>,
    sys: VecDeque<i16>,
    mic_only: bool,
    writer: hound::WavWriter<std::io::BufWriter<std::fs::File>>,
}
```

в `Mixer::create(path: &std::path::Path, mic_only: bool)` добавить `mic_only` в
литерал структуры; в начало `push` добавить:

```rust
        if self.mic_only {
            for s in samples {
                self.writer.write_sample(s)?;
            }
            return Ok(());
        }
```

В `use`-секции добавить `std::path::Path`. `finalize` без изменений (очереди
в mic-only пусты).

- [ ] **Step 4: Проверить, что запись встреч работает как раньше**

```bash
cargo build 2>&1 | tail -3
cargo run -q -- toggle   # сказать пару слов
cargo run -q -- toggle   # остановить
cargo run -q -- transcribe   # текст распознан
```

- [ ] **Step 5: Commit**

```bash
git add src/record.rs src/paths.rs
git commit -m "Parametrize recording: sources, output path, pidfile"
```

---

### Task 4: `insert.rs`, `dictate.rs` и сборка всего вместе

**Files:**
- Create: `src/insert.rs`
- Create: `src/dictate.rs`
- Modify: `src/paths.rs` (пути диктовки)
- Modify: `src/main.rs` (сабкоманда Dictate, модули)

**Interfaces:**
- Consumes: `record::{record_to, running_recording, RecordConfig, Pidfile}` (Task 3; `record_to(cfg, on_started)` — колбэк вызывается, когда запись реально пошла; `dictate()` сам создаёт `Pidfile` до вызова `record_to` и держит его до конца функции — через транскрипцию и вставку), `transcribe::transcribe_wav` (Task 2), `notify::Notifier` (Task 1), `paths::runtime_dir` (Task 3).
- Produces: `dictate::dictate(model: Option<PathBuf>, lang: &str, no_notify: bool) -> Result<()>`; `insert::insert_text(text: &str) -> Result<()>`.

- [ ] **Step 1: `src/paths.rs` — пути диктовки**

```rust
/// pid-файл идущей диктовки (отдельный от записи встреч).
pub(crate) fn dictate_pidfile() -> PathBuf {
    runtime_dir().join("throisma-dictate.pid")
}

/// Временный WAV диктовки. Имя фиксированное: одновременная диктовка одна
/// (это гарантирует pid-файл), а при ошибке файл остаётся для повтора.
pub(crate) fn dictate_wav() -> PathBuf {
    runtime_dir().join("throisma-dictate.wav")
}
```

- [ ] **Step 2: Создать `src/insert.rs`**

```rust
//! Вставка текста в активное окно (Wayland).

use anyhow::{bail, Context, Result};
use std::io::Write;
use std::process::{Command, Stdio};

/// Кладёт текст в буфер обмена и «печатает» его в активное окно.
/// Порядок важен: сначала клипборд — если wtype упадёт, текст можно вставить руками.
pub(crate) fn insert_text(text: &str) -> Result<()> {
    pipe("wl-copy", &[], text).context("не удалось скопировать в буфер обмена (wl-copy)")?;
    pipe("wtype", &["-"], text).context("не удалось напечатать текст (wtype)")?;
    Ok(())
}

fn pipe(cmd: &str, args: &[&str], input: &str) -> Result<()> {
    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::piped())
        .spawn()
        .with_context(|| format!("не удалось запустить {cmd}"))?;
    child
        .stdin
        .take()
        .expect("stdin запрошен строкой выше")
        .write_all(input.as_bytes())?;
    let status = child.wait()?;
    if !status.success() {
        bail!("{cmd} завершился с ошибкой");
    }
    Ok(())
}
```

- [ ] **Step 3: Создать `src/dictate.rs`**

```rust
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
```

- [ ] **Step 4: `src/main.rs` — сабкоманда Dictate**

Модули: добавить `mod dictate;` и `mod insert;`. В enum после Toggle:

```rust
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
```

в `match`:

```rust
        Command::Dictate { model, lang, no_notify } => dictate::dictate(model, &lang, no_notify),
```

- [ ] **Step 5: Собрать и прогнать ручной end-to-end**

```bash
cargo build 2>&1 | tail -3
```

Сценарии (в терминале на Hyprland):

1. `cargo run -q -- dictate` → уведомление «🎤 Диктовка…»; сказать фразу;
   в другом терминале `cargo run -q -- dictate` → первый процесс: уведомления
   «Транскрибирую…» и «Вставлено», текст напечатан в активное окно и лежит в
   клипборде (`wl-paste`); `ls $XDG_RUNTIME_DIR/throisma-dictate.wav` — файла нет.
2. Ошибка: `THROISMA_MODEL=/nonexistent cargo run -q -- dictate`, остановить →
   уведомление «Ошибка диктовки» с подсказкой; WAV остался; `cargo run -q --
   transcribe $XDG_RUNTIME_DIR/throisma-dictate.wav` работает; удалить WAV.
3. Независимость: запустить `toggle`, затем `dictate` — dictate не убивает
   запись встречи; остановить оба, каждый своим тоглом.
4. `cargo run -q -- dictate --no-notify` — без уведомлений.

- [ ] **Step 6: Commit**

```bash
git add src/insert.rs src/dictate.rs src/paths.rs src/main.rs
git commit -m "Add dictate mode: record, transcribe, insert into active window"
```

---

## Хоткеи (для пользователя, после реализации)

`hyprland.conf`:

```
bind = SUPER, D, exec, throisma dictate
bind = SUPER SHIFT, R, exec, throisma toggle
```

(бинарь — `cargo install --path .` или полный путь к `target/release/throisma`).
