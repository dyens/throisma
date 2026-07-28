use crate::notify::Notifier;
use crate::paths;
use anyhow::{Context, Result};
use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;
use pipewire as pw;
use pw::spa;
use spa::param::audio::{AudioFormat, AudioInfoRaw};
use spa::pod::Pod;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Пишем сразу в формате whisper: 16 кГц, моно, s16 — ресемплит сам PipeWire.
const RATE: u32 = 16_000;

/// Откуда захватываем звук.
#[derive(Clone, Copy)]
enum Source {
    /// Дефолтный микрофон.
    Mic,
    /// Monitor дефолтного аудиовыхода — то, что слышно в колонках (собеседники).
    SinkMonitor,
}

impl Source {
    /// Номер дорожки в миксере.
    fn track(self) -> usize {
        match self {
            Source::Mic => 0,
            Source::SinkMonitor => 1,
        }
    }
}

/// Если запись уже идёт — остановить её, иначе начать новую.
pub fn toggle(no_notify: bool) -> Result<()> {
    if let Some(pid) = stop_running(&paths::pidfile()).context("не удалось остановить запись")? {
        println!("Запись остановлена (pid {pid}).");
        return Ok(());
    }
    record(no_notify)
}

/// Шлёт SIGTERM процессу из pid-файла, если тот жив; возвращает его pid.
pub(crate) fn stop_running(pidfile: &Path) -> Result<Option<Pid>> {
    let Some(pid) = running_recording(pidfile)? else {
        return Ok(None);
    };
    signal::kill(pid, Signal::SIGTERM)?;
    Ok(Some(pid))
}

/// pid идущей записи, если она есть; заодно подчищает устаревший pid-файл.
fn running_recording(pidfile: &Path) -> Result<Option<Pid>> {
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

impl Drop for Pidfile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Копит дорожки (микрофон и, для встреч, системный звук) и пишет их сумму
/// в WAV по мере поступления: очередной сэмпл уходит в файл, как только он
/// есть во всех дорожках.
struct Mixer {
    tracks: Vec<VecDeque<i16>>,
    writer: hound::WavWriter<std::io::BufWriter<std::fs::File>>,
}

impl Mixer {
    fn create(path: &Path, n_tracks: usize) -> Result<Mixer> {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        Ok(Mixer {
            tracks: vec![VecDeque::new(); n_tracks],
            writer: hound::WavWriter::create(path, spec)?,
        })
    }

    fn push(&mut self, track: usize, samples: impl Iterator<Item = i16>) -> Result<()> {
        self.tracks[track].extend(samples);
        let ready = self.tracks.iter().map(VecDeque::len).min().unwrap_or(0);
        for _ in 0..ready {
            let sum: i32 = self
                .tracks
                .iter_mut()
                .map(|q| q.pop_front().expect("длина проверена через ready") as i32)
                .sum();
            self.writer.write_sample(clamp(sum))?;
        }
        Ok(())
    }

    /// Дописывает несведённые хвосты дорожек и закрывает файл.
    fn finalize(mut self) -> Result<()> {
        while self.tracks.iter().any(|q| !q.is_empty()) {
            let sum: i32 = self
                .tracks
                .iter_mut()
                .filter_map(VecDeque::pop_front)
                .map(i32::from)
                .sum();
            self.writer.write_sample(clamp(sum))?;
        }
        self.writer.finalize()?;
        Ok(())
    }
}

fn clamp(sum: i32) -> i16 {
    sum.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

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
/// вызывающего; pid-файл создаёт и держит вызывающий. `on_started` вызывается,
/// когда запись реально пошла (стримы подключены).
pub(crate) fn record_to(cfg: &RecordConfig, on_started: impl FnOnce()) -> Result<()> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None).context("PipeWire main loop")?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None).context("не удалось подключиться к PipeWire")?;

    let mixer = Rc::new(RefCell::new(Mixer::create(&cfg.wav, if cfg.mic_only { 1 } else { 2 })?));
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

type StreamHandle<'c> = (pw::stream::StreamBox<'c>, pw::stream::StreamListener<()>);

fn capture_stream<'c>(
    core: &'c pw::core::CoreRc,
    source: Source,
    mixer: Rc<RefCell<Mixer>>,
) -> Result<StreamHandle<'c>> {
    let mut props = pw::properties::properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "Communication",
    };
    let name = match source {
        Source::Mic => "throisma-mic",
        Source::SinkMonitor => {
            props.insert("stream.capture.sink", "true");
            "throisma-sys"
        }
    };

    let stream = pw::stream::StreamBox::new(core, name, props)?;
    let listener = stream
        .add_local_listener::<()>()
        .process(move |stream, _| {
            let Some(mut buffer) = stream.dequeue_buffer() else { return };
            let datas = buffer.datas_mut();
            let Some(data) = datas.first_mut() else { return };
            let n = data.chunk().size() as usize;
            let Some(bytes) = data.data() else { return };
            let samples = bytes[..n]
                .chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]]));
            if let Err(e) = mixer.borrow_mut().push(source.track(), samples) {
                eprintln!("ошибка записи в WAV: {e}");
            }
        })
        .register()?;

    let pod_bytes = whisper_format_pod();
    let mut params = [Pod::from_bytes(&pod_bytes).context("некорректный format pod")?];
    stream.connect(
        spa::utils::Direction::Input,
        None,
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut params,
    )?;
    Ok((stream, listener))
}

/// SPA-параметр «отдавайте 16 кГц моно s16» для подключения стрима.
fn whisper_format_pod() -> Vec<u8> {
    let mut info = AudioInfoRaw::new();
    info.set_format(AudioFormat::S16LE);
    info.set_rate(RATE);
    info.set_channels(1);
    spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(spa::pod::Object {
            type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
            id: spa::param::ParamType::EnumFormat.as_raw(),
            properties: info.into(),
        }),
    )
    .expect("сериализация формата в память не падает")
    .0
    .into_inner()
}
