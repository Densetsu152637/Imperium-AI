# 09 — Media generation

Text and tool-calling models run through the runtimes in [04](04-models-and-routing.md).
Image, video, and audio models mostly require Python, so they run in a supervised **media
sidecar** or through **ComfyUI**.

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
