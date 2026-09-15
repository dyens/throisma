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
throisma transcribe --speakers 3   # если число собеседников известно
throisma summary       # итоги последней встречи через Claude Code
throisma summary --focus "технические решения"
```

Записи складываются в `~/.local/share/throisma/recordings/`, транскрипты — рядом
(`.txt`), итоги — `.summary.md`. Транскрипт встречи — диалог со временем реплик:

```
[03:12] Я: Причём purge будет асинхронным.
[03:18] Они 1: А тесты его ждать не будут?
[03:21] Они 2: Нет, достаточно проверить, что задача создана.
```

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

Заметно лучше `ggml-large-v3-turbo.bin` (термины, речь собеседников): если он
лежит в том же каталоге, транскрипция и диктовка берут его сами, иначе —
`ggml-base.bin`.

```sh
curl -L -o ~/.local/share/throisma/models/ggml-large-v3-turbo.bin \
  https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo.bin
```

## Собеседники по голосам

Свой голос отделён каналом записи, а собеседников в системном звуке делит
диаризация (sherpa-onnx: сегментация pyannote + эмбеддинги WeSpeaker) —
«Они 1», «Они 2», … Она включается сама, если скачаны модели:

```sh
cd ~/.local/share/throisma/models
curl -L https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-segmentation-models/sherpa-onnx-pyannote-segmentation-3-0.tar.bz2 | tar xj
curl -LO https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/wespeaker_en_voxceleb_resnet34_LM.onnx
```

Считает её отдельный бинарь `throisma-diarize` (ставится тем же `cargo install`):
в одном процессе с whisper.cpp onnxruntime падает. Диаризация идёт на CPU,
примерно 30 с на 10 минут записи. Голоса, наговорившие меньше 30 с, считаются
шумом кластеризации и сливаются с соседними; если собеседников меньше или
больше, чем получилось, подскажите число флагом `--speakers`.

## Итоги встречи

`throisma summary` отдаёт транскрипт (при отсутствии — сначала транскрибирует)
в Claude Code CLI (`claude -p`, без инструментов) и сохраняет `.summary.md`:
TL;DR, темы со временем, решения, action items, открытые вопросы, риски.
Искажённые распознаванием термины модель восстанавливает по контексту и по
словарю `prompt.txt`. Нужен установленный `claude` с выполненным входом.
