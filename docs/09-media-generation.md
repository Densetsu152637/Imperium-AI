# 09 — Media generation

Text and tool-calling models run through the runtimes in [04](04-models-and-routing.md).
Image, video, and audio models mostly require Python, so they run in a supervised **media
sidecar** or through **ComfyUI**.

**Implemented (phase 10, `imperium-media`):** three agent tools, offered when configured.
`media_image` generates through an OpenAI-compatible images API (an `[endpoints]` entry and a
model; base64 or URL responses) or through ComfyUI (`comfyui_url` and an API-format workflow
file whose text contains `{{prompt}}`, and optionally `{{seed}}`, `{{width}}`, `{{height}}`;
queued with `/prompt`, polled through `/history`, downloaded from `/view`). Both implement the
`ImageGenerator` trait and are built from `backend` by `ImageGenerators`, so adding a backend is
an implementation and a `register` call; an unknown `backend` uses the images API.
`media_speak` uses an OpenAI-compatible speech API and `media_transcribe` an OpenAI-compatible
transcription API (LocalAI, whisper.cpp front-ends, or hosted services); transcription reads
only files inside the project. Generated files are written to the project's `media/` folder with a `.json`
provenance file beside each (prompt or text, model, size, time), and the session reports a
`media_created` event. Every media call asks first unless allowed for the session, since it can
cost money or send data to a service. Not yet: the managed Python sidecar, video, editing,
the gallery pane, and scheduling media jobs with the memory planner.

```toml
[media.image]
backend = "comfyui"
comfyui_url = "http://127.0.0.1:8188"
workflow = "D:/ComfyUI/workflows/sdxl_api.json"

[media.speech]
endpoint = "localai"      # an [endpoints] entry
model = "kokoro"
voice = "af_heart"

[media.transcription]
endpoint = "localai"
model = "whisper-large-v3"
```

## Backends

| Modality              | Default backend                         | Alternatives                              |
| --------------------- | --------------------------------------- | ----------------------------------------- |
| Image generation/edit | ComfyUI (managed or user-provided)      | Diffusers in the sidecar; hosted APIs     |
| Video generation      | ComfyUI workflows (e.g. Wan, LTX-Video) | Hosted APIs                               |
| Speech-to-text        | whisper.cpp (native, no Python)         | faster-whisper in the sidecar; hosted     |
| Text-to-speech        | Sidecar (e.g. Kokoro, Orpheus GGUF via llama.cpp) | Hosted                          |
| Music / audio         | Sidecar                                 | Hosted                                    |
| Vision input          | llama.cpp with `mmproj`, vLLM multimodal | Hosted                                   |

## Media sidecar

- A `uv` project in `sidecars/media/`, installed into a managed environment on first use with
  the right PyTorch build for the detected hardware (CUDA, ROCm, MPS, CPU).
- Speaks JSON-RPC over stdio to the daemon; long jobs report progress events.
- Each pipeline declares its memory footprint so the [memory planner](04-models-and-routing.md#hardware-awareness)
  can schedule it alongside text models (for example, unloading an idle LLM before a video job
  when the user allows it).

## ComfyUI integration

- Imperium can install and supervise a ComfyUI instance or connect to an existing one.
- Workflows (ComfyUI JSON) are registered as **media tools** with typed inputs, so agents call
  `media.generate` with parameters rather than editing graphs.
- Users can import any ComfyUI workflow and expose selected inputs.

## Outputs

Generated files are session artefacts with provenance (model, seed, parameters, prompt), shown
in a gallery pane and addressable by agents for follow-up edits.
