use crate::notify::Notifier;
use crate::paths;
use anyhow::{Context, Result};
use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;
use pipewire as pw;
use pw::spa;
use pw::types::ObjectType;
use spa::param::audio::{AudioFormat, AudioInfoRaw};
use spa::pod::Pod;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::io::{Seek, Write};
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
    /// Monitor одного из аудиовыходов (индекс дорожки в миксере).
    SinkMonitor(usize),
}

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

/// Сводит микрофон и мониторы всех аудиовыходов в один моно-поток.
///
/// Часы задаёт микрофон: его сэмплы пишутся сразу, к каждому подмешиваются
/// головы очередей мониторов (пустая очередь — тишина). Ждать данных от всех
/// дорожек нельзя: монитор бездействующего (suspended) выхода не производит
/// сэмплов вообще и застопорил бы запись.
struct Mixer<W: Write + Seek> {
    sys: Vec<VecDeque<i16>>,
    writer: hound::WavWriter<W>,
}

/// Мониторы живут на своих часах и могут опережать микрофон — храним не
/// больше двух секунд, лишнее отбрасываем с головы очереди.
const SYS_QUEUE_CAP: usize = 2 * RATE as usize;

fn wav_spec() -> hound::WavSpec {
    hound::WavSpec {
        channels: 1,
        sample_rate: RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    }
}

/// Миксер, пишущий в файл (как в реальной записи).
type FileMixer = Mixer<std::io::BufWriter<std::fs::File>>;

impl FileMixer {
    fn create(path: &Path, n_sinks: usize) -> Result<FileMixer> {
        Ok(Mixer::new(hound::WavWriter::create(path, wav_spec())?, n_sinks))
    }
}

impl<W: Write + Seek> Mixer<W> {
    fn new(writer: hound::WavWriter<W>, n_sinks: usize) -> Mixer<W> {
        Mixer { sys: vec![VecDeque::new(); n_sinks], writer }
    }

    fn push(&mut self, source: Source, samples: impl Iterator<Item = i16>) -> Result<()> {
        match source {
            Source::Mic => {
                for s in samples {
                    let sum = self
                        .sys
                        .iter_mut()
                        .filter_map(VecDeque::pop_front)
                        .fold(s as i32, |acc, x| acc + x as i32);
                    self.writer.write_sample(clamp(sum))?;
                }
            }
            Source::SinkMonitor(i) => {
                let q = &mut self.sys[i];
                q.extend(samples);
                if q.len() > SYS_QUEUE_CAP {
                    q.drain(..q.len() - SYS_QUEUE_CAP);
                }
            }
        }
        Ok(())
    }

    /// Дописывает несведённые хвосты мониторов и закрывает файл.
    fn finalize(mut self) -> Result<()> {
        while self.sys.iter().any(|q| !q.is_empty()) {
            let sum = self
                .sys
                .iter_mut()
                .filter_map(VecDeque::pop_front)
                .fold(0i32, |acc, x| acc + x as i32);
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

    // приложения могут играть не в дефолтный выход (WirePlumber помнит
    // маршруты per-приложение) — поэтому пишем мониторы всех выходов сразу
    let sink_ids = if cfg.mic_only { Vec::new() } else { audio_sinks(&mainloop, &core)? };

    let mixer = Rc::new(RefCell::new(Mixer::create(&cfg.wav, sink_ids.len())?));
    let mic_stream = capture_stream(&core, Source::Mic, None, mixer.clone())?;
    let sys_streams = sink_ids
        .iter()
        .enumerate()
        .map(|(i, name)| capture_stream(&core, Source::SinkMonitor(i), Some(name), mixer.clone()))
        .collect::<Result<Vec<_>>>()?;

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
    drop((mic_stream, sys_streams));
    Rc::try_unwrap(mixer)
        .map_err(|_| anyhow::anyhow!("mixer всё ещё используется"))?
        .into_inner()
        .finalize()
}

/// node.name всех аудиовыходов (нод с media.class == Audio/Sink) на момент
/// вызова. Появившиеся уже во время записи выходы не захватываются.
fn audio_sinks(mainloop: &pw::main_loop::MainLoopRc, core: &pw::core::CoreRc) -> Result<Vec<String>> {
    let registry = core.get_registry().context("PipeWire registry")?;
    let sinks = Rc::new(RefCell::new(Vec::new()));
    let done = Rc::new(Cell::new(false));
    // sync-roundtrip: когда сервер ответит done, все существующие глобальные
    // объекты уже проехали через global-колбэк
    let pending = core.sync(0).context("PipeWire sync")?;
    let _core_listener = core
        .add_listener_local()
        .done({
            let done = done.clone();
            let mainloop = mainloop.clone();
            move |id, seq| {
                if id == pw::core::PW_ID_CORE && seq == pending {
                    done.set(true);
                    mainloop.quit();
                }
            }
        })
        .register();
    let _registry_listener = registry
        .add_listener_local()
        .global({
            let sinks = sinks.clone();
            move |global| {
                let Some(props) = global.props.as_ref().map(|p| p.as_ref()) else { return };
                if global.type_ == ObjectType::Node
                    && props.get("media.class") == Some("Audio/Sink")
                {
                    if let Some(name) = props.get("node.name") {
                        sinks.borrow_mut().push(name.to_string());
                    }
                }
            }
        })
        .register();
    while !done.get() {
        mainloop.run();
    }
    let ids = sinks.borrow().clone();
    Ok(ids)
}

type StreamHandle<'c> = (pw::stream::StreamBox<'c>, pw::stream::StreamListener<()>);

fn capture_stream<'c>(
    core: &'c pw::core::CoreRc,
    source: Source,
    target: Option<&str>,
    mixer: Rc<RefCell<FileMixer>>,
) -> Result<StreamHandle<'c>> {
    let mut props = pw::properties::properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "Communication",
    };
    let name = match source {
        Source::Mic => "throisma-mic",
        Source::SinkMonitor(_) => {
            props.insert("stream.capture.sink", "true");
            // числовой target в connect() у современного PipeWire не работает
            // (трактуется как object.serial) — таргетим по node.name
            if let Some(target) = target {
                props.insert("target.object", target);
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Прогоняет сценарий через миксер с файлом во временном каталоге и
    /// возвращает записанные сэмплы.
    fn run(name: &str, n_sinks: usize, scenario: impl FnOnce(&mut FileMixer)) -> Vec<i16> {
        let path = std::env::temp_dir()
            .join(format!("throisma-mixer-{name}-{}.wav", std::process::id()));
        let mut mixer = Mixer::create(&path, n_sinks).unwrap();
        scenario(&mut mixer);
        mixer.finalize().unwrap();
        let samples = hound::WavReader::open(&path)
            .unwrap()
            .samples::<i16>()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        std::fs::remove_file(&path).ok();
        samples
    }

    #[test]
    fn mic_only_passthrough() {
        let out = run("mic-only", 0, |m| {
            m.push(Source::Mic, [1i16, -2, 3].into_iter()).unwrap();
        });
        assert_eq!(out, [1, -2, 3]);
    }

    #[test]
    fn mixes_available_sink_samples() {
        let out = run("mix", 1, |m| {
            m.push(Source::SinkMonitor(0), [10i16, 20, 30].into_iter()).unwrap();
            m.push(Source::Mic, [1i16, 2, 3].into_iter()).unwrap();
        });
        assert_eq!(out, [11, 22, 33]);
    }

    #[test]
    fn silent_sink_does_not_stall_mic() {
        // регрессия: suspended-выход не производит сэмплов — микрофон всё
        // равно должен писаться сразу, а не копиться до finalize
        let out = run("silent-sink", 2, |m| {
            m.push(Source::Mic, [1i16, 2, 3].into_iter()).unwrap();
        });
        assert_eq!(out, [1, 2, 3]);
    }

    #[test]
    fn sink_tails_are_summed_on_finalize() {
        let out = run("tails", 2, |m| {
            m.push(Source::SinkMonitor(0), [5i16, 5].into_iter()).unwrap();
            m.push(Source::SinkMonitor(1), [7i16].into_iter()).unwrap();
        });
        assert_eq!(out, [12, 5]);
    }

    #[test]
    fn clamps_overflow() {
        let out = run("clamp", 1, |m| {
            m.push(Source::SinkMonitor(0), [i16::MAX].into_iter()).unwrap();
            m.push(Source::Mic, [i16::MAX].into_iter()).unwrap();
        });
        assert_eq!(out, [i16::MAX]);
    }

    #[test]
    fn sink_queue_is_capped() {
        let out = run("cap", 1, |m| {
            // 3 «старых» сэмпла + полный кап «новых»: старые должны отпасть
            let samples = std::iter::repeat(111i16)
                .take(3)
                .chain(std::iter::repeat(222i16).take(SYS_QUEUE_CAP));
            m.push(Source::SinkMonitor(0), samples).unwrap();
            m.push(Source::Mic, [0i16].into_iter()).unwrap();
        });
        assert_eq!(out[0], 222);
        assert_eq!(out.len(), SYS_QUEUE_CAP); // 1 с микрофоном + хвост
    }
}
