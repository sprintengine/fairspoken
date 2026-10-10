# Installing models from your own server

Fairspoken downloads its speech and text polish models from the internet the
first time they are used. If your network blocks that, put a zip of each model
on a server your users can reach and give the app a link to it. The app
downloads the zip, unpacks it and installs the model as if it had downloaded
it the usual way, with the same checks.

## 1. Build the zip

On a machine with internet access, run the bundle script from this repository
once per model and per app:

```bash
scripts/models/make-model-bundle.sh parakeet-tdt-0.6b-v3 desktop parakeet-tdt-0.6b-v3-desktop.zip
scripts/models/make-model-bundle.sh parakeet-tdt-0.6b-v3 mac     parakeet-tdt-0.6b-v3-mac.zip
```

It needs only `bash`, `curl` and `zip` (or `python3`), and works on macOS and
Linux. The desktop app and the Mac apps run different model files, so they
need different zips:

| Model id | Model | `desktop` zip: Fairspoken for Windows, Linux and macOS, and `transcription-host` | `mac` zip: Fairspoken for Mac and Fairspoken Server |
| --- | --- | --- | --- |
| `parakeet-tdt-0.6b-v3` | Parakeet 0.6B v3 (the default) | Yes | Yes |
| `parakeet-ultra` | Parakeet Ultra | Yes | Yes |
| `parakeet-tdt-0.6b-v2` | Parakeet 0.6B v2 (English) | Yes | Yes |
| `tiny`, `base`, `small`, `medium`, `large-v2`, `large-v3`, `large-v3-turbo` | Whisper | Yes | No |
| `speakoflow-mini`, `qwen3.5-0.8b`, `qwen3.5-2b`, `qwen3.5-4b` | Text polish | Yes | No |

What is in a zip: the model's files at the top level, nothing else. For the
desktop app these are files such as `encoder-model.onnx`, `ggml-base.bin` or
`Qwen3.5-2B-Q4_K_M.gguf`; for the Mac apps, Core ML bundles such as
`Encoder.mlmodelc`, which stay folders, and `parakeet_vocab.json`. The apps
also accept a zip whose files sit in a folder inside it, a `.tar` or `.tar.gz`
instead of a zip, and for a one-file model (Whisper, text polish) the file
itself.

## 2. Put it where your users can reach it

Any of these works:

- A generic repository in Artifactory or Nexus, for example
  `https://artifactory.example.org/artifactory/fairspoken-models/parakeet-tdt-0.6b-v3-desktop.zip`.
- An intranet web server, SharePoint or similar.
- A network share, for example `\\fileserver\models\parakeet-tdt-0.6b-v3-desktop.zip`
  or `/Volumes/Models/parakeet-tdt-0.6b-v3-desktop.zip`.

The apps can't sign in to a server. Use a link that downloads without a
sign-in: anonymous read access, or a link with an access token in it. Links
may have a query string (`?token=…`); the apps never show it or write it to
their logs. Links with a user name and password in them
(`https://user:password@…`) are refused.

## 3. Give the app the link

**Desktop and Mac apps:** on the **Models** screen, choose **Download from
link…** next to the model, paste the link and choose **Download**. The app
remembers the link (it shows the server and path under the model) and uses it
whenever that model is downloaded again. **Use the standard download** forgets
it.

The links are saved in the app's settings file as `modelLinks`, by model id,
so you can also set them there before the app first starts:

```json
"modelLinks": {
  "parakeet-tdt-0.6b-v3": "https://artifactory.example.org/artifactory/fairspoken-models/parakeet-tdt-0.6b-v3-desktop.zip"
}
```

Text polish also needs a small runtime, which the desktop app downloads from
GitHub (`github.com/ggml-org/llama.cpp`) the first time; a link covers the
model file only.

### Transcription hosts

`transcription-host` takes a link per model from, highest first:

1. `--model-link <model-id>=<link>` on the command line, once per model.
   An empty link (`--model-link parakeet-ultra=`) means the usual download.
2. `FAIRSPOKEN_MODEL_LINKS`: `model-id=link` pairs separated by spaces. Write
   a path with spaces in it as a `file://` link with `%20`, or use the config
   file.
3. `modelLinks` in the host config file (`host-config.json`), as above.

```bash
transcription-host --model-link parakeet-tdt-0.6b-v3=https://files.example.org/models/parakeet-tdt-0.6b-v3-desktop.zip
FAIRSPOKEN_MODEL_LINKS="parakeet-tdt-0.6b-v3=/srv/models/parakeet-v3.zip large-v3-turbo=/srv/models/whisper-turbo.zip" transcription-host
```

The host prints each link it will use at startup, and refuses to start when a
model id is unknown or a link is malformed, saying which. Fairspoken Server
reads `modelLinks` from its config file the same way, with the `mac` zips.
