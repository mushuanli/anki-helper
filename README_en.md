# Anki Helper

A self-contained local web card-making app with a Rust backend and embedded HTML/CSS/JavaScript. The frontend and the default Anki templates live inside the executable, so users do not need to install Rust, Node, Python, edge-tts, or lame.

> [:cn: 中文版本](./README.md)

![Desktop](./anki-helper.png)

Anki card preview:

![Card](./anki.png)

## Running

Download the executable for your system, put it in a writable directory, and launch it. With no arguments it starts the local web app and opens the browser automatically:

```bash
./helper
```

On Windows you can double-click the `.exe`. On Linux/macOS you may need `chmod +x helper` the first time; unsigned macOS release files are subject to the system security policy. The configuration and project list are stored in **the process's current working directory**, so when starting from a terminal you can `cd` to the directory where you want them saved first. For desktop launches the working directory is decided by the system; the page sidebar and the startup output show the directory actually in use.

```bash
./helper serve --port 8765 --no-browser
```

The default port is `0`, which picks a free port automatically. The server only listens on `127.0.0.1`. When the browser is not opened automatically, copy the full address from the startup output (including the session identifier after `#`) and open it. Closing the browser does not stop the app; press `Ctrl+C` in the terminal to exit. Restarting produces a new address. Only one app instance may run per working directory at a time.

## Workflow

1. Open "Models & Services → Service Connections". The page supports Volcengine only: the Ark connection stores one API key shared by text, image recognition, and image generation; Doubao Speech uses a separate speech connection and key. The Ark and Doubao Speech configurations are always present, so there is no need to add connections; changing the address requires re-entering the key. Existing multi-connection configurations are preserved.
2. Under "Model Usage", choose a connection and model for text, image recognition, image generation, and speech. Text defaults to DeepSeek V4.1 Flash, image recognition to Doubao Seed 2.1 Lite, and image generation to Seedream 5.0 Flash. The recommended list does not mean your account already has access; you can enter a custom Model ID / endpoint ID, or maintain the model list in the connection.
3. Create a project; the default name is "英语单词" and the directory is `projects/英语单词`. Changing the name automatically updates the default directory; once you edit the directory manually, that path is kept. You can paste words or load a WordData JSON array. With one word per line you can group them with `Unit 2`; the page does not offer TXT file upload.
4. Photograph the vocabulary list at the back of the textbook and upload JPG, PNG, or WebP photos (up to 6 files, 8 MB each, 12 MB total). After saving the image-recognition settings, click "Recognize and fill word list"; the model extracts the English word, phonetic transcription, Chinese definition, and Unit group, and converts them to JSON. Review the recognition result before creating a project or appending words; recognition does not write to the project directly and does not generate example sentences. Photos are sent to the selected image-recognition service and may incur charges.
5. An existing project directory can be opened directly; it is read from `word_json/*.json`, and initialization from `index.json` is also supported. Words can be appended to a project; existing words with the same name are not overwritten.
6. Click "Fill all missing content". Filling preserves existing content and manual edits field by field, and shows the status of text, audio, and images. Audio and images are off by default. Once audio is enabled it uses Doubao Speech by default, which requires a speech key, resource ID, and voice; on macOS you can choose "System Voice", which calls the built-in `say` to produce WAV without a speech key, and you can enter the name of a voice installed on the system.
7. After filling in the connection parameters, click "Verify this connection" under the corresponding service: Ark generates sample text and Doubao generates sample speech, using the current unsaved key and parameters. Only that connection is verified, and other connections that are not configured do not matter. You can also verify text, image recognition, image generation, and speech individually under "Model Usage". Verification does not save the configuration, and results are invalidated once settings change. The image-recognition test uses a built-in APPLE image; speech can be auditioned and images can be previewed. Success shows the connection name, model, and elapsed time; failure shows the reason. Changing parameters automatically invalidates earlier results. Tests use the current settings and may incur charges. Temporary test media is cleaned up on exit. In the card editor you can regenerate text, audio, or images individually; regeneration warns about replacement and cost, and failed cards can be retried after a task failure.
8. Export the Anki card package. When it finishes, the page shows the full local file path; click "Open containing folder" and in Anki choose "File → Import" to select the `.apkg`. Markdown export is also available. Both Doubao MP3 and system WAV are supported for preview and packaging.

Project data is stored locally. Calling a model sends the corresponding words, prompts, photos, or text to be read aloud; macOS system speech is converted on the machine. Filling skips existing media; after switching the model, voice, or audio mode you must regenerate explicitly to replace existing files. Editing an example sentence or an image prompt invalidates the corresponding media reference, which is rebuilt on the next fill. Generating a single audio track or image does not require a text key, and a failed media generation does not delete existing valid files.

## Local Files

The working directory stores:

```text
config.toml       # Configuration (contains the API key in plain text)
projects.json     # Project names and absolute directory paths
.helper.lock      # Lock file preventing duplicate launches of the same working directory
projects/         # Default project location when no project directory is specified
```

Each project directory stores:

```text
word_json/*.json  # Data for each word card
recite_json/      # Recitation data supported by the original CLI (no recitation editor in the web UI yet)
audio/*.mp3
images/*.png
.anki-deck-id     # Stable deck ID, preserved across repeated exports
<directory name>_Deck.apkg
cards.md
```

Export runs in the background and reports the card processing, database generation, media writing, and finishing stages separately. Media is written into the ZIP in 64 KiB chunks, and progress is based on bytes actually written; MP3 and images use ZIP store mode to avoid recompression. The final file is first written to a temporary file in the same directory and replaces the target only on success, so a failure never overwrites a previously complete card package. Export needs sufficient local disk space; the old package and the temporary new package may coexist.

Media preview and file-save responses are streamed and do not load the whole card package into memory. Downloading is not a required step — the final file is already saved in the project directory. Repeated exports use stable deck and note identifiers; Anki's actual update behavior still depends on the import options.

`config.toml` is created automatically on first launch. See [config.toml.example](config.toml.example). Legacy flat configuration is converted to connection configuration automatically, and after saving, the key is stored only in the corresponding connection. The environment variables `OPENAI_BASEURL`, `OPENAI_API_KEY`, `OPENAI_MODEL`, `ARK_API_KEY`, and `TTS_API_KEY` can provide defaults on first launch or when migrating legacy configuration; the new connection configuration does not automatically restore a cleared key from environment variables. Newly written configuration files have `0600` permissions on Unix. The settings page never echoes a saved key: leaving it blank under the same connection and address keeps it, and you can also clear it explicitly. Settings are split into "Service Connections", "Model Usage", and "Advanced": the connections page manages keys and addresses, the model page switches between text, image recognition, image generation, and speech and tests the current usage's configuration directly; the port and concurrency are in advanced settings. Testing does not require saving first, and after saving the configuration applies to subsequent tasks.

## Building from Source and Releasing

```bash
cargo test --locked
cargo build --release --locked
```

Frontend interaction tests use jsdom and can reuse an existing Node development environment:

```bash
ANKI_JSDOM_PATH=/path/to/node_modules/jsdom node tests/web-ui.cjs
```

The test that actually generates macOS system speech and exports to Anki requires the system speech service:

```bash
cargo test --locked system_audio_generates_and_exports_without_cloud_credentials -- --ignored
```


The build artifact is `target/release/helper` (`helper.exe` on Windows), and only this file needs to be distributed. The app already contains the web assets and default templates; HTTPS uses Rustls, and SQLite and bzip2 are compiled into the binary. For cross-distribution Linux releases, a static musl build is recommended:

```bash
rustup target add x86_64-unknown-linux-musl
cargo build --release --locked --target x86_64-unknown-linux-musl
```

This target requires the musl C compiler to be installed on the system, for example `musl-tools` on Debian/Ubuntu.

The [release workflow](.github/workflows/release.yml) can be triggered manually and produces single-file artifacts for Linux x64 (musl), Windows x64, macOS Intel, and Apple Silicon. Pushing a `v*` tag creates a GitHub Release after builds and tests pass on all four platforms. Verification must be done separately on each target platform; the workflow does not include app signing or macOS notarization.

## Retained CLI

```bash
./helper init /path/to/project --template /path/to/words.txt
./helper pack /path/to/project
./helper pack /path/to/project --template /path/to/custom-ankimodel.json
./helper md /path/to/project --output-dir /path/to/output
./helper pkg2md /path/to/file.apkg --output-dir /path/to/output
```

`pack` uses the embedded template by default, so there is no need to carry a `template/` directory. `init` uses the same configuration and HTTP media service as the web UI. `pkg2md`'s field mapping targets this project's template and does not apply to arbitrary Anki templates.

The model suggestion list is based on the official documentation snapshot of 2026-10-08; actual availability for your account is determined by the settings-page test and the activation status in the console:

- [Ark model announcements](https://docs.volcengine.com/docs/ark/model-release-announcement?lang=zh)
- [Seedream models and parameters](https://docs.volcengine.com/docs/ark/seedream-4-0-5-0?lang=zh)
- [Doubao Speech HTTP V3](https://docs.volcengine.com/docs/DoubaoVoice/unidirectional-streaming-text-to-speech-http?lang=zh)
- [Speech API key usage](https://docs.volcengine.com/docs/DoubaoVoice/APIKeyUsage?lang=zh)
