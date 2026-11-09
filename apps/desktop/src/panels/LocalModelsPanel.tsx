// Local models: GGUF files in the model folders, the hardware they are planned against, which
// are loaded, and downloads from Hugging Face.

import { useEffect, useState, type FormEvent } from "react";
import type {
  DownloadJob,
  HfFilesResult,
  LocalModelListResult,
  LocalModelRow,
  ModelDownloadsResult,
} from "@imperium/protocol";
import type { DaemonClient } from "../daemon/client";

const GIB = 1024 ** 3;

const gib = (bytes: number): string => `${(bytes / GIB).toFixed(1)} GiB`;

function fit(row: LocalModelRow): string {
  const speed = row.tokens_per_s != null ? ` · ${row.tokens_per_s.toFixed(1)} tok/s` : "";
  return `${placement(row)}${speed}`;
}

function placement(row: LocalModelRow): string {
  if (row.loaded) {
    const queue = row.queued > 0 ? ` · ${row.queued} waiting` : "";
    const lane = row.cpu_lane ? "CPU lane · " : "";
    return `${lane}loaded · ${row.slots_in_use}/${row.slots} busy${queue} · pool ${row.pool_reserved_tokens}/${row.pool_tokens}`;
  }
  if (row.cpu_lane && row.placement === "cpu") return "CPU lane (RAM only)";
  switch (row.placement) {
    case "gpu":
      return "fits on GPU";
    case "partial":
      return `partial · ${row.gpu_layers ?? 0} GPU layers`;
    case "cpu":
      return "fits on CPU";
    case "none":
      return "does not fit";
  }
}

/** Whether a download would fit, in words: the hint never relies on colour. */
function fitHint(fit: HfFilesResult["variants"][number]["fit"]): string {
  switch (fit) {
    case "gpu":
      return "fits on GPU";
    case "partial":
      return "fits split across GPU and RAM";
    case "cpu":
      return "fits in RAM (CPU)";
    case "none":
      return "does not fit";
  }
}

const message = (error: unknown): string => (error instanceof Error ? error.message : String(error));

export function LocalModelsPanel({
  listing,
  onRefresh,
  client,
  onConfigure,
}: {
  listing: LocalModelListResult | null;
  onRefresh: () => void;
  client: DaemonClient | null;
  onConfigure: () => void;
}) {
  const download = <HfDownload client={client} onDone={onRefresh} />;
  if (!listing || !listing.enabled) {
    return (
      <div className="models local-models">
        <p className="empty">
          No model folders yet. Add <code>folders = ["D:/Models"]</code> under <code>[models]</code> in your config,
          then restart the daemon, or download a model below.
        </p>
        {download}
      </div>
    );
  }
  const { hardware, runtime } = listing;
  return (
    <div className="models local-models">
      <div className="models-toolbar">
        {hardware && (
          <span className="muted">
            {hardware.gpus.map((gpu) => `${gpu.name} ${gib(gpu.vram_free_bytes)} free`).join(" · ") || "no GPU"} · RAM{" "}
            {gib(hardware.ram_available_bytes)} free{hardware.overridden ? " (overridden)" : ""}
          </span>
        )}
        <button onClick={onRefresh}>Refresh</button>
        <button onClick={onConfigure}>Configure</button>
      </div>
      {runtime && !runtime.installed && (
        <p className="warning">
          llama.cpp pack <code>{runtime.pack}</code> is not installed. Run <code>imperium runtime install</code>.
        </p>
      )}
      <table>
        <thead>
          <tr>
            <th>Id</th>
            <th>Quant</th>
            <th>Size</th>
            <th>Needs</th>
            <th>Placement</th>
          </tr>
        </thead>
        <tbody>
          {listing.models.map((row) => (
            <tr key={row.id} className={row.placement === "none" && !row.loaded ? "unusable" : undefined} title={row.path}>
              <td>{`${row.runtime}:${row.id}`}</td>
              <td>{row.quantization ?? "?"}</td>
              <td>{gib(row.size_bytes)}</td>
              <td>{gib(row.required_bytes)}</td>
              <td>
                <span className={`badge ${row.loaded ? "local" : ""}`}>{fit(row)}</span>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      {listing.models.length === 0 && <p className="empty">No GGUF models in {listing.folders.join(", ")}.</p>}
      {listing.skipped.length > 0 && (
        <p className="muted">
          Skipped {listing.skipped.length} file{listing.skipped.length === 1 ? "" : "s"}:{" "}
          {listing.skipped.map((s) => `${s.path} (${s.reason})`).join("; ")}
        </p>
      )}
      {download}
    </div>
  );
}

/** The "Download from Hugging Face" form and the progress of downloads. */
function HfDownload({ client, onDone }: { client: DaemonClient | null; onDone: () => void }) {
  const [repo, setRepo] = useState("");
  const [files, setFiles] = useState<HfFilesResult | null>(null);
  const [variant, setVariant] = useState("");
  const [mmproj, setMmproj] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [jobs, setJobs] = useState<DownloadJob[]>([]);

  // Follow every download, including ones started elsewhere (the CLI, another window).
  useEffect(() => {
    if (!client) return;
    client.call<ModelDownloadsResult>("models.downloads").then(({ jobs }) => setJobs(jobs), () => {});
    const off = client.onNotification((notification) => {
      if (notification.method !== "models.download_progress") return;
      const job = notification.params as DownloadJob;
      setJobs((current) => {
        const index = current.findIndex((other) => other.job_id === job.job_id);
        return index < 0 ? [...current, job] : current.map((other, i) => (i === index ? job : other));
      });
      if (job.state === "done") onDone();
    });
    return () => {
      off();
    };
    // onDone is a fresh closure on every render; following the client is what matters.
  }, [client]);

  const list = async (event: FormEvent) => {
    event.preventDefault();
    if (!client || !repo.trim()) return;
    setBusy(true);
    setError(null);
    setFiles(null);
    try {
      const result = await client.call<HfFilesResult>("models.hf_files", { repo: repo.trim() });
      setFiles(result);
      setVariant(result.default ?? result.variants[0]?.name ?? "");
      setMmproj(false);
    } catch (failure) {
      setError(message(failure));
    } finally {
      setBusy(false);
    }
  };

  const start = async () => {
    if (!client || !files || !variant) return;
    setBusy(true);
    setError(null);
    try {
      const job = await client.call<DownloadJob>("models.download", {
        repo: files.repo,
        revision: files.revision,
        quant: variant,
        mmproj,
      });
      setJobs((current) => (current.some((other) => other.job_id === job.job_id) ? current : [...current, job]));
    } catch (failure) {
      setError(message(failure));
    } finally {
      setBusy(false);
    }
  };

  const cancel = (jobId: string) => {
    client?.call("models.download_cancel", { job_id: jobId }).catch((failure) => setError(message(failure)));
  };

  return (
    <section className="hf-download" aria-labelledby="hf-download-title">
      <h3 id="hf-download-title">Download from Hugging Face</h3>
      <form className="models-toolbar" onSubmit={list}>
        <label htmlFor="hf-repo">Repository</label>
        <input
          id="hf-repo"
          value={repo}
          placeholder="owner/name, e.g. Qwen/Qwen3-8B-GGUF"
          spellCheck={false}
          autoComplete="off"
          onChange={(event) => setRepo(event.target.value)}
        />
        <button type="submit" disabled={!client || busy || !repo.trim()}>
          List files
        </button>
      </form>
      {busy && (
        <p className="muted" role="status">
          Working…
        </p>
      )}
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
      {files && files.variants.length === 0 && (
        <p className="empty">{files.repo} has no GGUF or safetensors weights to download.</p>
      )}
      {files && files.variants.length > 0 && (
        <div className="models-toolbar">
          <label htmlFor="hf-variant">Quantisation</label>
          <select id="hf-variant" value={variant} onChange={(event) => setVariant(event.target.value)}>
            {files.variants.map((option) => (
              <option key={`${option.name}:${option.files[0]}`} value={option.name}>
                {`${option.name} · ${gib(option.size_bytes)} · ${fitHint(option.fit)}`}
              </option>
            ))}
          </select>
          {files.projectors.length > 0 && (
            <label>
              <input type="checkbox" checked={mmproj} onChange={(event) => setMmproj(event.target.checked)} /> Include
              vision projector
            </label>
          )}
          <button className="primary" onClick={start} disabled={busy || !variant}>
            Download
          </button>
        </div>
      )}
      {files && files.variants.length > 0 && <p className="muted">Saves to {files.download_dir}</p>}
      {jobs.length > 0 && (
        <ul className="downloads" aria-label="Downloads">
          {jobs.map((job) => (
            <DownloadRow key={job.job_id} job={job} onCancel={() => cancel(job.job_id)} />
          ))}
        </ul>
      )}
    </section>
  );
}

function DownloadRow({ job, onCancel }: { job: DownloadJob; onCancel: () => void }) {
  const percent = job.total_bytes > 0 ? Math.floor((job.done_bytes / job.total_bytes) * 100) : 0;
  let status: string;
  switch (job.state) {
    case "running":
      status = `Downloading ${job.file ?? ""} — ${percent}% (${gib(job.done_bytes)} of ${gib(job.total_bytes)})`;
      break;
    case "done":
      status = `Done — use it as ${job.model}`;
      break;
    case "failed":
      status = `Failed — ${job.message ?? "no reason given"}`;
      break;
    case "cancelled":
      status = "Cancelled — downloading it again resumes";
      break;
  }
  const label = `${job.repo} ${job.variant}`;
  return (
    <li className={`download ${job.state}`}>
      <div className="download-head">
        <span>{label}</span>
        <span className={`badge ${job.state === "done" ? "finished" : job.state}`}>{job.state}</span>
        {job.state === "running" && (
          <button onClick={onCancel} aria-label={`Cancel download of ${label}`}>
            Cancel
          </button>
        )}
      </div>
      <progress max={100} value={job.state === "done" ? 100 : percent} aria-label={`${label} progress`} />
      <p className={job.state === "failed" ? "error" : "muted"} role={job.state === "failed" ? "alert" : undefined}>
        {status}
      </p>
    </li>
  );
}
