use anyhow::{Context, Result};
use pipewire as pw;
use pw::spa;
use spa::param::audio::{AudioFormat, AudioInfoRaw};
use spa::pod::Pod;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Пишем сразу в формате whisper: 16 кГц, моно, s16 — ресемплит сам PipeWire.
const RATE: u32 = 16_000;

fn pidfile() -> PathBuf {
    dirs::runtime_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("throisma.pid")
}

/// Если запись уже идёт — остановить её, иначе начать новую.
pub fn toggle() -> Result<()> {
    let pidfile = pidfile();
    if let Ok(pid) = std::fs::read_to_string(&pidfile) {
        let pid = pid.trim().to_string();
        if PathBuf::from(format!("/proc/{pid}")).exists() {
            Command::new("kill").arg(&pid).status()?;
            println!("Запись остановлена (pid {pid}).");
            return Ok(());
        }
        // процесс умер, а pid-файл остался
        let _ = std::fs::remove_file(&pidfile);
    }
    record()
}

/// Копит сэмплы обеих дорожек и пишет их сумму в WAV по мере поступления.
struct Mixer {
    mic: VecDeque<i16>,
    sys: VecDeque<i16>,
    writer: hound::WavWriter<std::io::BufWriter<std::fs::File>>,
}

impl Mixer {
    fn push(&mut self, from_mic: bool, samples: &[i16]) -> Result<()> {
        if from_mic {
            self.mic.extend(samples);
        } else {
            self.sys.extend(samples);
        }
        while !self.mic.is_empty() && !self.sys.is_empty() {
            let a = self.mic.pop_front().unwrap();
            let b = self.sys.pop_front().unwrap();
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
    let final_path = crate::recordings_dir()?
        .join(chrono::Local::now().format("%Y-%m-%d_%H-%M-%S.wav").to_string());

    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None).context("PipeWire main loop")?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None).context("не удалось подключиться к PipeWire")?;

    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mixer = Rc::new(RefCell::new(Mixer {
        mic: VecDeque::new(),
        sys: VecDeque::new(),
        writer: hound::WavWriter::create(&final_path, spec)?,
    }));

    // микрофон — дефолтный источник; собеседники — monitor дефолтного выхода
    let _mic = capture_stream(&core, "throisma-mic", false, mixer.clone())?;
    let _sys = capture_stream(&core, "throisma-sys", true, mixer.clone())?;

    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    ctrlc::set_handler(move || stop2.store(true, Ordering::SeqCst))?;

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

    let pidfile = pidfile();
    std::fs::write(&pidfile, std::process::id().to_string())?;

    println!(
        "Идёт запись (микрофон + системный звук) в {} — Ctrl+C или `throisma toggle` для остановки.",
        final_path.display()
    );

    mainloop.run();

    drop((_mic, _sys));
    let _ = std::fs::remove_file(&pidfile);
    Rc::try_unwrap(mixer)
        .map_err(|_| anyhow::anyhow!("mixer всё ещё используется"))?
        .into_inner()
        .finalize()?;

    println!("Готово: {}", final_path.display());
    Ok(())
}

type StreamHandle<'c> = (pw::stream::StreamBox<'c>, pw::stream::StreamListener<()>);

fn capture_stream<'c>(
    core: &'c pw::core::CoreRc,
    name: &str,
    capture_sink: bool,
    mixer: Rc<RefCell<Mixer>>,
) -> Result<StreamHandle<'c>> {
    let mut props = pw::properties::properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "Communication",
    };
    if capture_sink {
        props.insert("stream.capture.sink", "true");
    }

    let stream = pw::stream::StreamBox::new(core, name, props)?;
    let listener = stream
        .add_local_listener::<()>()
        .process(move |stream, _| {
            let Some(mut buffer) = stream.dequeue_buffer() else { return };
            let datas = buffer.datas_mut();
            let Some(data) = datas.first_mut() else { return };
            let n = data.chunk().size() as usize;
            let Some(bytes) = data.data() else { return };
            let samples: Vec<i16> = bytes[..n]
                .chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]]))
                .collect();
            if let Err(e) = mixer.borrow_mut().push(!capture_sink, &samples) {
                eprintln!("ошибка записи в WAV: {e}");
            }
        })
        .register()?;

    let mut info = AudioInfoRaw::new();
    info.set_format(AudioFormat::S16LE);
    info.set_rate(RATE);
    info.set_channels(1);
    let pod_bytes = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(spa::pod::Object {
            type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
            id: spa::param::ParamType::EnumFormat.as_raw(),
            properties: info.into(),
        }),
    )
    .expect("сериализация формата")
    .0
    .into_inner();
    let mut params = [Pod::from_bytes(&pod_bytes).unwrap()];

    stream.connect(
        spa::utils::Direction::Input,
        None,
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut params,
    )?;
    Ok((stream, listener))
}
