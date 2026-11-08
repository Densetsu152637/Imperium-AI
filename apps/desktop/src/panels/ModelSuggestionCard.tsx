// A model download suggestion in the conversation: what the daemon offers to download and why,
// with Download / Not now / Don't ask again, then the download's progress and, once it is
// ready, a button that switches the session to it. Nothing downloads without a click.

import { useEffect, useState } from "react";

import type { DownloadJob, SessionControlResult, SuggestionRespondResult } from "@imperium/protocol";

import type { DaemonClient } from "../daemon/client";
import type { SuggestionEntry } from "../sessions";

const GIB = 1024 * 1024 * 1024;
const gib = (bytes: number): string => `${(bytes / GIB).toFixed(1)} GiB`;

const FIT: Record<string, string> = {
  gpu: "fits on the GPU",
  partial: "split between GPU and system memory",
  cpu: "runs on the CPU",
};

type Props = {
  entry: SuggestionEntry;
  sessionId: string;
  client: DaemonClient | null;
  /** Switches the session's model; resolves with the daemon's answer, or null when it failed. */
  onSetModel: (model: string) => Promise<SessionControlResult | null>;
};

export function ModelSuggestionCard({ entry, sessionId, client, onSetModel }: Props) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [job, setJob] = useState<DownloadJob | null>(null);
  const [switched, setSwitched] = useState(false);

  // Follow the download this suggestion started.
  useEffect(() => {
    if (!client || !entry.jobId || entry.state !== "downloading") return;
    const off = client.onNotification((notification) => {
      if (notification.method !== "models.download_progress") return;
      const update = notification.params as DownloadJob;
      if (update.job_id === entry.jobId) setJob(update);
    });
    return () => {
      off();
    };
  }, [client, entry.jobId, entry.state]);

  const answer = async (action: "download" | "dismiss" | "never") => {
    if (!client) return;
    setBusy(true);
    setError(null);
    try {
      const result = await client.call<SuggestionRespondResult>("session.suggestion_respond", {
        session_id: sessionId,
        suggestion_id: entry.id,
        action,
      });
      if (result.job) setJob(result.job);
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setBusy(false);
    }
  };

  const switchTo = async () => {
    setBusy(true);
    const result = await onSetModel(entry.model);
    setBusy(false);
    if (result) setSwitched(true);
  };

  const percent = job && job.total_bytes > 0 ? Math.floor((job.done_bytes / job.total_bytes) * 100) : 0;
  const label = `${entry.repo} ${entry.quant}`;
  return (
    <div className="permission suggestion" role="group" aria-label="Model download suggestion">
      <div className="suggestion-body">
        <span>
          Download <strong>{entry.repo}</strong> ({entry.quant}, {gib(entry.sizeBytes)}, {FIT[entry.fit] ?? entry.fit})?
        </span>
        <span className="muted">Suggested because {entry.reason}.</span>
        {entry.state === "downloading" && (
          <>
            <progress max={100} value={percent} aria-label={`${label} progress`} />
            <span className="muted">
              {job ? `${percent}% (${gib(job.done_bytes)} of ${gib(job.total_bytes)})` : "Starting…"}
            </span>
          </>
        )}
        {entry.state === "ready" && <span className="muted">Downloaded — available as {entry.model}.</span>}
        {(entry.state === "failed" || entry.state === "cancelled") && (
          <span className="error" role="alert">
            Download {entry.state}: {entry.message ?? "no reason given"}
          </span>
        )}
        {entry.state === "dismissed" && <span className="muted">Not now.</span>}
        {entry.state === "disabled" && (
          <span className="muted">
            Suggestions are off; turn them on again in Settings (Suggest model downloads).
            {entry.message ? ` ${entry.message}` : ""}
          </span>
        )}
        {error && (
          <span className="error" role="alert">
            {error}
          </span>
        )}
      </div>
      {entry.state === "pending" && (
        <span className="buttons">
          <button className="primary" disabled={busy || !client} onClick={() => answer("download")}>
            Download
          </button>
          <button disabled={busy || !client} onClick={() => answer("dismiss")}>
            Not now
          </button>
          <button disabled={busy || !client} onClick={() => answer("never")}>
            Don't ask again
          </button>
        </span>
      )}
      {entry.state === "ready" && (
        <span className="buttons">
          <button className="primary" disabled={busy || switched} onClick={switchTo}>
            {switched ? "Switched" : "Switch to it"}
          </button>
        </span>
      )}
    </div>
  );
}
