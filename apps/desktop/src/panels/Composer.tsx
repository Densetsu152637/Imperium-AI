// The session composer: the message box with its attachments and push-to-talk.
//
// Files come in by the paperclip button, by pasting, or by dropping them on the composer (in the
// desktop app the window reports dropped paths; in a browser the files themselves are uploaded).
// Each is staged with `session.attach` and shows as a chip until the message is sent or the chip
// removed. Holding the push-to-talk key (Settings → Chat) or the mic button records speech;
// releasing it transcribes into the message box (`voice.transcribe`) without sending, and Esc
// cancels.

import { getCurrentWebview } from "@tauri-apps/api/webview";
import { ArrowUp, ListPlus, Mic, Paperclip, X } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";

import type { Attachment, SendMode, SessionAttachResult, VoiceTranscribeResult } from "@imperium/protocol";

import {
  addUploading,
  baseName,
  canSend,
  markFailed,
  markReady,
  pastedName,
  readBase64,
  readyAttachments,
  removeChip,
  thumbnail,
  type Chip,
} from "../attachments";
import { ComposerChips } from "../components/AttachmentChips";
import type { DaemonClient } from "../daemon/client";
import { insideTauri } from "../daemon/connection";
import { useSettings } from "../settings";
import {
  Recorder,
  formatElapsed,
  insertTranscript,
  isPttPress,
  isPttRelease,
  microphoneError,
  pttStep,
  type PttEvent,
  type PttState,
} from "../voice";

/** The most a browser uploads in one `session.attach` (the daemon connection takes about 16 MiB per message). */
const UPLOAD_LIMIT = 12 * 1024 * 1024;

type Props = {
  sessionId: string;
  busy: boolean;
  hidden: boolean;
  client: DaemonClient | null;
  placeholder: string;
  onSend: (text: string, mode: SendMode, attachments: Attachment[]) => void;
  /** Text to put in the message box (a message branched from, to edit); `nonce` repeats it. */
  prefill?: { text: string; nonce: number } | null;
};

let nextKey = 0;
const chipKey = () => `chip-${++nextKey}`;

const message = (error: unknown) => (error instanceof Error ? error.message : String(error));

export function Composer({ sessionId, busy, hidden, client, placeholder, onSend, prefill = null }: Props) {
  const settings = useSettings();
  const [draft, setDraft] = useState("");
  const [chips, setChips] = useState<Chip[]>([]);
  const [dragging, setDragging] = useState(false);
  const [ptt, setPtt] = useState<PttState>({ phase: "idle" });
  const [now, setNow] = useState(Date.now());
  const pttRef = useRef(ptt);
  const recorder = useRef(new Recorder());
  const starting = useRef<Promise<void> | null>(null);
  const field = useRef<HTMLTextAreaElement>(null);
  const root = useRef<HTMLDivElement>(null);
  const picker = useRef<HTMLInputElement>(null);

  // A different session starts with an empty composer.
  useEffect(() => {
    setDraft("");
    setChips([]);
  }, [sessionId]);

  // A message branched from comes back for editing.
  useEffect(() => {
    if (!prefill) return;
    setDraft(prefill.text);
    requestAnimationFrame(() => field.current?.focus());
  }, [prefill?.nonce]);

  // ---- Attachments ----

  const stage = useCallback(
    async (key: string, params: { name: string; path?: string; data?: string }, preview?: string) => {
      if (!client) {
        setChips((all) => markFailed(all, key, "Not connected to the daemon"));
        return;
      }
      try {
        const result = await client.call<SessionAttachResult>("session.attach", { session_id: sessionId, ...params });
        const drawn = preview ?? (result.image_data ? await thumbnail(result.image_data) : undefined);
        setChips((all) => markReady(all, key, result, drawn));
      } catch (error) {
        setChips((all) => markFailed(all, key, message(error)));
      }
    },
    [client, sessionId],
  );

  const attachFiles = useCallback(
    (files: File[]) => {
      files.forEach((file, index) => {
        const key = chipKey();
        const name = file.name && file.name !== "image.png" ? file.name : pastedName(file.type || "image/png", index);
        setChips((all) => addUploading(all, key, name));
        if (file.size > UPLOAD_LIMIT) {
          setChips((all) =>
            markFailed(all, key, "Too large to upload from here (12 MB); put it in one of the session's folders and drop it from there."),
          );
          return;
        }
        void (async () => {
          const preview = file.type.startsWith("image/") ? await thumbnail(file) : undefined;
          try {
            const data = await readBase64(file);
            await stage(key, { name, data }, preview);
          } catch (error) {
            setChips((all) => markFailed(all, key, message(error)));
          }
        })();
      });
    },
    [stage],
  );

  const attachPaths = useCallback(
    (paths: string[]) => {
      for (const path of paths) {
        const key = chipKey();
        setChips((all) => addUploading(all, key, baseName(path)));
        void stage(key, { name: baseName(path), path });
      }
    },
    [stage],
  );

  const choose = async () => {
    if (insideTauri()) {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const chosen = await open({ multiple: true, directory: false, title: "Attach files" });
      if (chosen) attachPaths(Array.isArray(chosen) ? chosen : [chosen]);
    } else {
      picker.current?.click();
    }
  };

  // In the desktop app the window reports drops with their paths (the page gets no files).
  useEffect(() => {
    if (!insideTauri() || hidden) return;
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    const inside = (position: { x: number; y: number }) => {
      const box = root.current?.getBoundingClientRect();
      if (!box) return false;
      const x = position.x / window.devicePixelRatio;
      const y = position.y / window.devicePixelRatio;
      return x >= box.left && x <= box.right && y >= box.top && y <= box.bottom;
    };
    void getCurrentWebview()
      .onDragDropEvent(({ payload }) => {
        if (payload.type === "over") setDragging(inside(payload.position));
        else if (payload.type === "drop") {
          setDragging(false);
          if (inside(payload.position)) attachPaths(payload.paths);
        } else if (payload.type === "leave") setDragging(false);
      })
      .then((stop) => {
        if (cancelled) stop();
        else unlisten = stop;
      });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [attachPaths, hidden]);

  // ---- Push-to-talk ----

  const transcribe = useCallback(async () => {
    try {
      await starting.current;
      const audio = await recorder.current.stop();
      if (!client) throw new Error("Not connected to the daemon");
      const result = await client.call<VoiceTranscribeResult>("voice.transcribe", {
        audio: await readBase64(audio),
        mime: audio.type || "audio/webm",
      });
      const caret = field.current?.selectionStart ?? Number.MAX_SAFE_INTEGER;
      setDraft((current) => {
        const next = insertTranscript(current, caret, result.text);
        requestAnimationFrame(() => {
          field.current?.focus();
          field.current?.setSelectionRange(next.caret, next.caret);
        });
        return next.text;
      });
      dispatch({ type: "done" });
    } catch (error) {
      dispatch({ type: "failed", message: message(error) });
    }
    // `dispatch` (declared below) reads only refs, so this need not change with it.
  }, [client]);

  const dispatch = useCallback(
    (event: PttEvent) => {
      const { state, effect } = pttStep(pttRef.current, event);
      pttRef.current = state;
      setPtt(state);
      if (effect === "start") {
        starting.current = recorder.current.start().catch((error) => {
          recorder.current.discard();
          dispatch({ type: "failed", message: microphoneError(error) });
        });
      } else if (effect === "stop-and-transcribe") {
        void transcribe();
      } else if (effect === "discard") {
        void Promise.resolve(starting.current).then(() => recorder.current.discard());
      }
    },
    [transcribe],
  );

  // The key works while focus is in this composer's session panel.
  useEffect(() => {
    if (hidden) return;
    const shortcut = settings.pushToTalkKey;
    const ours = () => {
      const panel = root.current?.closest(".session");
      return !!panel && panel.contains(document.activeElement);
    };
    const down = (event: KeyboardEvent) => {
      if (event.key === "Escape" && (pttRef.current.phase === "recording" || pttRef.current.phase === "error")) {
        event.preventDefault();
        event.stopPropagation();
        dispatch({ type: "cancel" });
        return;
      }
      if (!isPttPress(event, shortcut) || !ours()) return;
      event.preventDefault();
      if (!event.repeat) dispatch({ type: "press", at: Date.now() });
    };
    const up = (event: KeyboardEvent) => {
      if (pttRef.current.phase === "recording" && isPttRelease(event, shortcut)) {
        event.preventDefault();
        dispatch({ type: "release", at: Date.now() });
      }
    };
    window.addEventListener("keydown", down, true);
    window.addEventListener("keyup", up, true);
    return () => {
      window.removeEventListener("keydown", down, true);
      window.removeEventListener("keyup", up, true);
    };
  }, [dispatch, hidden, settings.pushToTalkKey]);

  // The elapsed time ticks while recording.
  useEffect(() => {
    if (ptt.phase !== "recording") return;
    const timer = window.setInterval(() => setNow(Date.now()), 250);
    return () => window.clearInterval(timer);
  }, [ptt.phase]);

  // Leaving the session (or closing the panel) stops any recording.
  useEffect(() => {
    const current = recorder.current;
    return () => current.discard();
  }, []);

  // ---- Sending ----

  const ready = canSend(draft, chips);
  // While the agent works the composer stays open and sending always queues the message for
  // after the turn; a queued message's Steer button moves it into the running turn. The one
  // shortcut that steers directly is Ctrl+Shift+Enter.
  const send = (mode: SendMode = "queue") => {
    if (!canSend(draft, chips)) return;
    onSend(draft.trim(), mode, readyAttachments(chips));
    setDraft("");
    setChips([]);
  };

  const recording = ptt.phase === "recording";
  return (
    <div
      ref={root}
      className={`composer-area${dragging ? " dragging" : ""}`}
      hidden={hidden}
      onDragOver={(event) => {
        if (!event.dataTransfer.types.includes("Files")) return;
        event.preventDefault();
        setDragging(true);
      }}
      onDragLeave={(event) => {
        if (!event.currentTarget.contains(event.relatedTarget as Node | null)) setDragging(false);
      }}
      onDrop={(event) => {
        if (event.dataTransfer.files.length === 0) return;
        event.preventDefault();
        setDragging(false);
        attachFiles([...event.dataTransfer.files]);
      }}
    >
      {ptt.phase !== "idle" && (
        <div className={`ptt-status ${ptt.phase}`} role="status">
          {ptt.phase === "recording" && (
            <>
              <span className="ptt-dot" aria-hidden="true" /> Recording {formatElapsed(now - ptt.since)} · release to
              transcribe, Esc to cancel
            </>
          )}
          {ptt.phase === "transcribing" && "Transcribing…"}
          {ptt.phase === "error" && (
            <>
              <span>{ptt.message}</span>
              <button
                type="button"
                className="ghost icon-only"
                onClick={() => dispatch({ type: "dismiss" })}
                aria-label="Dismiss"
                title="Dismiss"
              >
                <X aria-hidden="true" />
              </button>
            </>
          )}
        </div>
      )}
      <ComposerChips chips={chips} onRemove={(key) => setChips((all) => removeChip(all, key))} />
      <form
        className="composer"
        onSubmit={(event) => {
          event.preventDefault();
          send();
        }}
      >
        <button
          type="button"
          className="ghost icon-only composer-tool"
          onClick={() => void choose()}
          aria-label="Attach files"
          title="Attach files (or paste, or drop them here)"
        >
          <Paperclip aria-hidden="true" />
        </button>
        <input
          ref={picker}
          type="file"
          multiple
          hidden
          onChange={(event) => {
            attachFiles([...(event.target.files ?? [])]);
            event.target.value = "";
          }}
        />
        <textarea
          ref={field}
          value={draft}
          aria-label="Message"
          placeholder={placeholder}
          onChange={(event) => setDraft(event.target.value)}
          onPaste={(event) => {
            const files = [...event.clipboardData.files];
            if (files.length === 0) return;
            event.preventDefault();
            attachFiles(files);
          }}
          onKeyDown={(event) => {
            if (event.key !== "Enter") return;
            const modified = event.ctrlKey || event.metaKey;
            if (modified && event.shiftKey) {
              event.preventDefault();
              send("steer");
              return;
            }
            const sends = settings.sendKey === "enter" ? !event.shiftKey && !modified : modified;
            if (sends) {
              event.preventDefault();
              send();
            }
          }}
          rows={1}
        />
        <button
          type="button"
          className={`ghost icon-only composer-tool mic${recording ? " recording" : ""}`}
          aria-label={recording ? "Recording: release to transcribe" : "Hold to talk"}
          aria-pressed={recording}
          title={
            settings.pushToTalkKey === "off"
              ? "Hold to talk; release to transcribe into the message"
              : `Hold to talk (or hold ${settings.pushToTalkKey}); release to transcribe into the message`
          }
          disabled={ptt.phase === "transcribing"}
          onPointerDown={(event) => {
            event.preventDefault();
            event.currentTarget.setPointerCapture(event.pointerId);
            dispatch({ type: "press", at: Date.now() });
          }}
          onPointerUp={() => dispatch({ type: "release", at: Date.now() })}
          onPointerCancel={() => dispatch({ type: "cancel" })}
          onClick={(event) => {
            // A keyboard click (no pointer) toggles: first starts, second transcribes.
            if (event.detail !== 0) return;
            dispatch(
              pttRef.current.phase === "recording" ? { type: "release", at: Date.now() } : { type: "press", at: Date.now() },
            );
          }}
        >
          <Mic aria-hidden="true" />
        </button>
        <button
          type="submit"
          className="send"
          disabled={!ready}
          aria-label={busy ? "Queue" : "Send"}
          title={busy ? "Queue for after this turn (Enter)" : "Send (Enter)"}
        >
          {busy ? <ListPlus aria-hidden="true" /> : <ArrowUp aria-hidden="true" />}
        </button>
      </form>
    </div>
  );
}
