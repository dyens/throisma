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
use std::path::PathBuf;
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

/// Если запись уже идёт — остановить её, иначе начать новую.
pub fn toggle() -> Result<()> {
    if let Some(pid) = running_recording()? {
        signal::kill(pid, Signal::SIGTERM).context("не удалось остановить запись")?;
        println!("Запись остановлена (pid {pid}).");
        return Ok(());
    }
    record()
}

/// pid идущей записи, если она есть; заодно подчищает устаревший pid-файл.
fn running_recording() -> Result<Option<Pid>> {
    let Ok(contents) = std::fs::read_to_string(paths::pidfile()) else {
        return Ok(None);
    };
    let pid = contents.trim().parse::<i32>().map(Pid::from_raw);
    // сигнал 0 — проверка, что процесс жив
    if let Ok(pid) = pid {
        if signal::kill(pid, None).is_ok() {
            return Ok(Some(pid));
        }
    }
    let _ = std::fs::remove_file(paths::pidfile());
    Ok(None)
}

/// Гарантия «pid-файл существует, пока идёт запись»:
/// создаётся на время записи, удаляется при выходе из scope.
struct Pidfile(PathBuf);

impl Pidfile {
    fn create() -> Result<Pidfile> {
        let path = paths::pidfile();
        std::fs::write(&path, std::process::id().to_string())?;
        Ok(Pidfile(path))
    }
}

impl Drop for Pidfile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Копит сэмплы обеих дорожек и пишет их сумму в WAV по мере поступления.
struct Mixer {
    mic: VecDeque<i16>,
    sys: VecDeque<i16>,
    writer: hound::WavWriter<std::io::BufWriter<std::fs::File>>,
}

impl Mixer {
    fn create(path: &std::path::Path) -> Result<Mixer> {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        Ok(Mixer {
            mic: VecDeque::new(),
            sys: VecDeque::new(),
            writer: hound::WavWriter::create(path, spec)?,
        })
    }

    fn push(&mut self, source: Source, samples: impl Iterator<Item = i16>) -> Result<()> {
        match source {
            Source::Mic => self.mic.extend(samples),
            Source::SinkMonitor => self.sys.extend(samples),
        }
        let ready = self.mic.len().min(self.sys.len());
        for (a, b) in self.mic.drain(..ready).zip(self.sys.drain(..ready)) {
            self.writer.write_sample(mix(a, b))?;
        }
        Ok(())
    }

    /// Дописывает хвост более длинной дорожки и закрывает файл.
    fn finalize(mut self) -> Result<()> {
        for s in self.mic.drain(..).chain(self.sys.drain(..)) {
            self.writer.write_sample(s)?;
        }
        self.writer.finalize()?;
        Ok(())
    }
}

fn mix(a: i16, b: i16) -> i16 {
    (a as i32 + b as i32).clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

pub fn record() -> Result<()> {
    let wav_path = paths::recordings_dir()?
        .join(chrono::Local::now().format("%Y-%m-%d_%H-%M-%S.wav").to_string());

    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None).context("PipeWire main loop")?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None).context("не удалось подключиться к PipeWire")?;

    let mixer = Rc::new(RefCell::new(Mixer::create(&wav_path)?));
    let mic_stream = capture_stream(&core, Source::Mic, mixer.clone())?;
    let sys_stream = capture_stream(&core, Source::SinkMonitor, mixer.clone())?;

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

    let _pidfile = Pidfile::create()?;
    println!(
        "Идёт запись (микрофон + системный звук) в {} — Ctrl+C или `throisma toggle` для остановки.",
        wav_path.display()
    );

    mainloop.run();

    // стримы держат клоны mixer — отпускаем их, чтобы забрать его целиком
    drop((mic_stream, sys_stream));
    Rc::try_unwrap(mixer)
        .map_err(|_| anyhow::anyhow!("mixer всё ещё используется"))?
        .into_inner()
        .finalize()?;

    println!("Готово: {}", wav_path.display());
    Ok(())
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
            if let Err(e) = mixer.borrow_mut().push(source, samples) {
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
