// A model download suggestion in the conversation: what the daemon offers to download and why,
// with Download / Not now / Don't ask again, then the download's progress and, once it is
// ready, a button that switches the session to it. Nothing downloads without a click.

import { X } from "lucide-react";
import { useEffect, useState } from "react";

import type { DownloadJob, SessionControlResult, SuggestionRespondResult } from "@imperium/protocol";

import { IconButton } from "../components/IconButton";
import { Notice } from "../components/Notice";
import type { DaemonClient } from "../daemon/client";
import type { SuggestionEntry } from "../sessions/sessions";

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
    <section className="card model-suggestion" role="group" aria-label="Model download suggestion">
      <div className="house-models-head">
        <h3>Download {entry.repo}?</h3>
        {entry.state === "pending" && (
          <IconButton icon={X} label="Not now" disabled={busy || !client} onClick={() => answer("dismiss")} />
        )}
      </div>
      <table className="table compact model-suggestion-table">
        <thead>
          <tr>
            <th>Model</th>
            <th>Quant</th>
            <th className="num">Size</th>
            <th>Fit</th>
          </tr>
        </thead>
        <tbody>
          <tr>
            <td>
              <strong>{entry.repo}</strong>
            </td>
            <td>{entry.quant}</td>
            <td className="num">{gib(entry.sizeBytes)}</td>
            <td>{FIT[entry.fit] ?? entry.fit}</td>
          </tr>
        </tbody>
      </table>
      <p className="muted">Suggested because {entry.reason}.</p>
      {entry.state === "downloading" && (
        <>
          <progress max={100} value={percent} aria-label={`${label} progress`} />
          <p className="muted">{job ? `${percent}% (${gib(job.done_bytes)} of ${gib(job.total_bytes)})` : "Starting…"}</p>
        </>
      )}
      {entry.state === "ready" && <p className="muted">Downloaded — available as {entry.model}.</p>}
      {(entry.state === "failed" || entry.state === "cancelled") && (
        <Notice tone="danger">
          Download {entry.state}: {entry.message ?? "no reason given"}
        </Notice>
      )}
      {entry.state === "dismissed" && <p className="muted">Not now.</p>}
      {entry.state === "disabled" && (
        <p className="muted">
          Suggestions are off; turn them on again in Settings (Suggest model downloads).
          {entry.message ? ` ${entry.message}` : ""}
        </p>
      )}
      {error && <Notice tone="danger">{error}</Notice>}
      {entry.state === "pending" && (
        <div className="model-suggestion-actions">
          <button className="primary" disabled={busy || !client} onClick={() => answer("download")}>
            Download
          </button>
          <button className="ghost" disabled={busy || !client} onClick={() => answer("never")}>
            Don't ask again
          </button>
        </div>
      )}
      {entry.state === "ready" && (
        <div className="model-suggestion-actions">
          <button className="primary" disabled={busy || switched} onClick={switchTo}>
            {switched ? "Switched" : "Switch to it"}
          </button>
        </div>
      )}
    </section>
  );
}
