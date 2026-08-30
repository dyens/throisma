# throisma

Простой рекордер речи/встреч с транскрибацией через whisper.cpp.

Linux-приложение (PipeWire). Захватывает два потока прямо через libpipewire
(`pipewire-rs`): микрофон и системный звук — то, что слышно в колонках/наушниках,
т.е. собеседников во встрече. Потоки сводятся на лету в один WAV сразу в формате
whisper (16 кГц, моно, s16), поэтому остановка мгновенная, а перед транскрибацией
ничего конвертировать не нужно.

Для сборки нужен `pipewire-devel`; в рантайме — только PipeWire и `whisper-cli`.

## Команды

```sh
throisma record        # запись с микрофона, стоп — Ctrl+C
throisma toggle        # старт/стоп записи (удобно для горячей клавиши)
throisma list          # список записей
throisma transcribe    # транскрибировать последнюю запись
throisma transcribe path/to/file.wav -l ru
```

Записи складываются в `~/.local/share/throisma/recordings/`, транскрипты — рядом (`.txt`).

## Горячая клавиша

Собрать и установить бинарь:

```sh
cargo install --path .
```

Затем в настройках DE (GNOME: Settings → Keyboard → Custom Shortcuts) повесить
клавишу на команду `throisma toggle`: первое нажатие начинает запись, второе — останавливает.

## Значок в трее

Пока идёт запись встречи, в трее висит красный кружок со счётчиком времени
(в подсказке — таймер и путь к файлу). Клик по нему останавливает запись,
правая кнопка открывает меню с тем же пунктом. Значок — это
StatusNotifierItem, так что нужен трей, который его показывает: waybar с
модулем `tray`, KDE, GNOME с расширением AppIndicator. Если трея нет, запись
идёт как раньше, а в stderr печатается одна строка о недоступности значка.

## Транскрибация

Нужен `whisper-cli` из whisper.cpp:

```sh
git clone https://github.com/ggml-org/whisper.cpp
cd whisper.cpp && cmake -B build && cmake --build build -j
sudo cp build/bin/whisper-cli /usr/local/bin/
```

И модель (пути можно переопределить флагами `-m` / `--whisper-bin` или переменной `THROISMA_MODEL`):

```sh
mkdir -p ~/.local/share/throisma/models
curl -L -o ~/.local/share/throisma/models/ggml-base.bin \
  https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin
```

Для русской речи лучше взять модель побольше, например `ggml-small.bin` или `ggml-medium.bin`.
