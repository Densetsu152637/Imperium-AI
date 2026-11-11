// Push-to-talk: hold a key (or the mic button) to record, release to transcribe into the
// composer (`voice.transcribe`); Esc cancels. The state machine and the shortcut matching are
// pure so they can be tested; `Recorder` wraps the browser's MediaRecorder.

/** The push-to-talk shortcuts Settings offers; "off" turns the key off (the mic button stays). */
export const PTT_KEYS = ["Ctrl+Space", "Alt+Space", "Ctrl+Shift+Space", "F8", "off"] as const;
export type PttKey = (typeof PTT_KEYS)[number];

export type PttState =
  | { phase: "idle" }
  | { phase: "recording"; since: number }
  | { phase: "transcribing" }
  | { phase: "error"; message: string };

export type PttEvent =
  | { type: "press"; at: number }
  | { type: "release"; at: number }
  | { type: "cancel" }
  | { type: "done" }
  | { type: "failed"; message: string }
  | { type: "dismiss" };

/** What the recorder should do after a transition. */
export type PttEffect = "start" | "stop-and-transcribe" | "discard" | null;

/** Recordings shorter than this are taken as a slip and discarded. */
export const MIN_RECORDING_MS = 300;

export function pttStep(state: PttState, event: PttEvent): { state: PttState; effect: PttEffect } {
  switch (event.type) {
    case "press":
      // Pressing again while transcribing does nothing; after an error it starts over.
      if (state.phase === "idle" || state.phase === "error") {
        return { state: { phase: "recording", since: event.at }, effect: "start" };
      }
      return { state, effect: null };
    case "release":
      if (state.phase !== "recording") return { state, effect: null };
      if (event.at - state.since < MIN_RECORDING_MS) return { state: { phase: "idle" }, effect: "discard" };
      return { state: { phase: "transcribing" }, effect: "stop-and-transcribe" };
    case "cancel":
      if (state.phase === "recording") return { state: { phase: "idle" }, effect: "discard" };
      if (state.phase === "error") return { state: { phase: "idle" }, effect: null };
      return { state, effect: null };
    case "done":
      return { state: state.phase === "transcribing" ? { phase: "idle" } : state, effect: null };
    case "failed":
      return { state: { phase: "error", message: event.message }, effect: state.phase === "recording" ? "discard" : null };
    case "dismiss":
      return { state: state.phase === "error" ? { phase: "idle" } : state, effect: null };
  }
}

type KeyLike = Pick<KeyboardEvent, "key" | "code" | "ctrlKey" | "metaKey" | "altKey" | "shiftKey">;

/** Whether a keydown starts push-to-talk under `shortcut` (Ctrl also matches Cmd on macOS). */
export function isPttPress(event: KeyLike, shortcut: PttKey): boolean {
  const ctrl = event.ctrlKey || event.metaKey;
  switch (shortcut) {
    case "Ctrl+Space":
      return event.code === "Space" && ctrl && !event.altKey && !event.shiftKey;
    case "Alt+Space":
      return event.code === "Space" && event.altKey && !ctrl && !event.shiftKey;
    case "Ctrl+Shift+Space":
      return event.code === "Space" && ctrl && event.shiftKey && !event.altKey;
    case "F8":
      return event.key === "F8";
    case "off":
      return false;
  }
}

/** Whether a keyup ends push-to-talk: letting go of the key or of its modifier. */
export function isPttRelease(event: KeyLike, shortcut: PttKey): boolean {
  switch (shortcut) {
    case "Ctrl+Space":
    case "Ctrl+Shift+Space":
      return event.code === "Space" || event.key === "Control" || event.key === "Meta";
    case "Alt+Space":
      return event.code === "Space" || event.key === "Alt";
    case "F8":
      return event.key === "F8";
    case "off":
      return false;
  }
}

/** The composer's text with `spoken` inserted at the caret, separated by spaces as needed. */
export function insertTranscript(draft: string, caret: number, spoken: string): { text: string; caret: number } {
  const words = spoken.trim();
  if (!words) return { text: draft, caret };
  const at = Math.max(0, Math.min(caret, draft.length));
  const before = draft.slice(0, at);
  const after = draft.slice(at);
  const lead = before && !/\s$/.test(before) ? " " : "";
  const trail = after && !/^\s/.test(after) ? " " : "";
  const inserted = `${lead}${words}${trail}`;
  return { text: before + inserted + after, caret: at + inserted.length };
}

export function formatElapsed(ms: number): string {
  const seconds = Math.max(0, Math.floor(ms / 1000));
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
}

/** Why the microphone could not be opened, in words. */
export function microphoneError(error: unknown): string {
  const name = error instanceof DOMException ? error.name : "";
  if (name === "NotAllowedError" || name === "SecurityError") {
    return "Microphone access was refused. Allow Imperium to use the microphone in your system's privacy settings.";
  }
  if (name === "NotFoundError") return "No microphone was found.";
  return error instanceof Error ? error.message : String(error);
}

/** One recording from the default microphone. */
export class Recorder {
  private recorder: MediaRecorder | null = null;
  private stream: MediaStream | null = null;
  private chunks: Blob[] = [];

  /** Opens the microphone and starts recording; rejects when it cannot. */
  async start(): Promise<void> {
    if (!navigator.mediaDevices?.getUserMedia || typeof MediaRecorder === "undefined") {
      throw new Error("This window cannot record audio.");
    }
    this.stream = await navigator.mediaDevices.getUserMedia({ audio: true });
    const type = ["audio/webm;codecs=opus", "audio/webm", "audio/ogg;codecs=opus", "audio/mp4"].find((candidate) =>
      MediaRecorder.isTypeSupported(candidate),
    );
    this.chunks = [];
    this.recorder = new MediaRecorder(this.stream, type ? { mimeType: type } : undefined);
    this.recorder.ondataavailable = (event) => {
      if (event.data.size > 0) this.chunks.push(event.data);
    };
    this.recorder.start();
  }

  /** Stops recording and returns the audio. */
  stop(): Promise<Blob> {
    const recorder = this.recorder;
    if (!recorder) return Promise.reject(new Error("Nothing is being recorded."));
    return new Promise((resolve) => {
      recorder.onstop = () => {
        const blob = new Blob(this.chunks, { type: recorder.mimeType || "audio/webm" });
        this.release();
        resolve(blob);
      };
      recorder.stop();
    });
  }

  /** Stops recording and throws the audio away. */
  discard(): void {
    if (this.recorder && this.recorder.state !== "inactive") {
      this.recorder.onstop = () => this.release();
      this.recorder.stop();
    } else {
      this.release();
    }
  }

  private release(): void {
    this.stream?.getTracks().forEach((track) => track.stop());
    this.stream = null;
    this.recorder = null;
    this.chunks = [];
  }
}
