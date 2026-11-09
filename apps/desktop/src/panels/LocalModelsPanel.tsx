// Local models: the model folders (added and removed here), the llama.cpp runtime and whether it
// sees the GPU, the models found with one-click Use / Test / Settings, and downloads from Hugging
// Face. With nothing set up it shows the first-run choices instead.

import { Check, Download, FlaskConical, FolderPlus, RefreshCw, Search, SlidersHorizontal, Trash2, X } from "lucide-react";
import { useEffect, useRef, useState, type FormEvent, type RefObject } from "react";
import type {
  DownloadJob,
  HfFilesResult,
  LocalModelListResult,
  LocalModelRow,
  ModelDownloadsResult,
  ModelTestResult,
  ModelUseResult,
  RuntimeInstallJob,
  RuntimeStatusResult,
} from "@imperium/protocol";
import { Select } from "../components/Select";
import type { DaemonClient } from "../daemon/client";
import { pickFile } from "../daemon/connection";
import { addModelFolder, ModelSetup } from "./ModelSetup";
import { localDescription, localSpec } from "./ModelSelect";

const GIB = 1024 ** 3;
const MIB = 1024 ** 2;

const gib = (bytes: number): string => `${(bytes / GIB).toFixed(1)} GiB`;

function placement(row: LocalModelRow): string {
  if (row.problem) return "cannot run here";
  if (row.loaded) {
    const queue = row.queued > 0 ? ` · ${row.queued} waiting` : "";
    const lane = row.cpu_lane ? "CPU lane · " : "";
    return `${lane}loaded · ${row.slots_in_use}/${row.slots} busy${queue}`;
  }
  if (row.cpu_lane && row.placement === "cpu") return "CPU lane (RAM only)";
  switch (row.placement) {
    case "gpu":
      return "fits on GPU";
    case "partial":
      return `GPU + RAM · ${row.gpu_layers ?? 0} layers on GPU`;
    case "cpu":
      return "fits in RAM (CPU)";
    case "none":
      return "too large for this machine";
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

/** A context length in words: 131072 → "128K". */
function context(tokens: number | null): string {
  if (!tokens) return "?";
  return tokens >= 1024 ? `${Math.round(tokens / 1024)}K` : String(tokens);
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
  /** Opens the model settings dialog, on one model when `id` is given. */
  onConfigure: (id?: string) => void;
}) {
  const hfInput = useRef<HTMLInputElement>(null);
  const showDownload = () => {
    hfInput.current?.scrollIntoView({ behavior: "smooth", block: "center" });
    hfInput.current?.focus();
  };
  const download = <HfDownload client={client} onDone={onRefresh} inputRef={hfInput} />;
  if (!listing || !listing.enabled) {
    return (
      <div className="models local-models">
        <ModelSetup client={client} onChanged={onRefresh} onDownload={showDownload} />
        {client && <RuntimeCard client={client} />}
        {download}
      </div>
    );
  }
  const { hardware } = listing;
  return (
    <div className="models local-models">
      <div className="models-toolbar">
        {hardware && (
          <span className="muted">
            {hardware.gpus.map((gpu) => `${gpu.name} ${gib(gpu.vram_free_bytes)} free`).join(" · ") || "no GPU"} · RAM{" "}
            {gib(hardware.ram_available_bytes)} free{hardware.overridden ? " (overridden)" : ""}
          </span>
        )}
        <button onClick={onRefresh} title="Scan the model folders again">
          <RefreshCw aria-hidden="true" /> Rescan
        </button>
        <button onClick={() => onConfigure()}>
          <SlidersHorizontal aria-hidden="true" /> Configure
        </button>
      </div>
      <FolderList client={client} listing={listing} onChanged={onRefresh} />
      {client && <RuntimeCard client={client} />}
      <ModelTable listing={listing} client={client} onChanged={onRefresh} onConfigure={onConfigure} />
      {listing.skipped.length > 0 && (
        <details className="skipped">
          <summary className="muted">
            Left out {listing.skipped.length} item{listing.skipped.length === 1 ? "" : "s"} that are not usable chat models
          </summary>
          <ul>
            {listing.skipped.map((s) => (
              <li key={s.path}>
                <code title={s.path}>{s.path}</code> — {s.reason}
              </li>
            ))}
          </ul>
        </details>
      )}
      {download}
    </div>
  );
}

/** The model folders, each removable, and the button that adds one. */
function FolderList({
  client,
  listing,
  onChanged,
}: {
  client: DaemonClient | null;
  listing: LocalModelListResult;
  onChanged: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const run = async (work: () => Promise<unknown>) => {
    setBusy(true);
    setError(null);
    try {
      await work();
      onChanged();
    } catch (failure) {
      setError(message(failure));
    } finally {
      setBusy(false);
    }
  };
  const extra = listing.folders.filter((folder) => !listing.configured_folders.includes(folder));
  return (
    <section className="folder-list" aria-labelledby="model-folders-title">
      <div className="section-head">
        <h3 id="model-folders-title">Model folders</h3>
        <button onClick={() => client && run(() => addModelFolder(client))} disabled={!client || busy}>
          <FolderPlus aria-hidden="true" /> Add folder
        </button>
      </div>
      <ul>
        {listing.configured_folders.map((folder) => (
          <li key={folder}>
            <code title={folder}>{folder}</code>
            <button
              className="ghost icon"
              onClick={() => client && run(() => client.call("models.remove_folder", { path: folder }))}
              disabled={!client || busy}
              aria-label={`Stop scanning ${folder}`}
              title="Stop scanning this folder (nothing in it is deleted)"
            >
              <Trash2 aria-hidden="true" />
            </button>
          </li>
        ))}
        {extra.map((folder) => (
          <li key={folder} className="muted">
            <code title={folder}>{folder}</code> <span>downloads</span>
          </li>
        ))}
      </ul>
      {busy && (
        <p className="muted" role="status">
          Scanning…
        </p>
      )}
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
    </section>
  );
}

/** The llama.cpp runtime: whether it runs and sees the GPU, and how to fix it when not. */
function RuntimeCard({ client }: { client: DaemonClient }) {
  const [status, setStatus] = useState<RuntimeStatusResult | null>(null);
  const [checking, setChecking] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [install, setInstall] = useState<RuntimeInstallJob | null>(null);

  const check = async (work?: () => Promise<RuntimeStatusResult>) => {
    setChecking(true);
    setError(null);
    try {
      const next = await (work ? work() : client.call<RuntimeStatusResult>("runtime.status"));
      setStatus(next);
      if (next.install) setInstall(next.install);
    } catch (failure) {
      setError(message(failure));
    } finally {
      setChecking(false);
    }
  };

  useEffect(() => {
    void check();
    const off = client.onNotification((notification) => {
      if (notification.method !== "runtime.install_progress") return;
      const job = notification.params as RuntimeInstallJob;
      setInstall(job);
      if (job.state !== "running") void check();
    });
    return () => {
      off();
    };
    // Checking again follows the client; `check` is a fresh closure each render.
  }, [client]);

  const useServer = (path: string | null) =>
    check(() => client.call<RuntimeStatusResult>("runtime.set_server", { path }));
  const choose = async () => {
    const path = await pickFile("Choose llama-server");
    if (path) await useServer(path);
  };
  const startInstall = () => {
    client.call<RuntimeInstallJob>("runtime.install").then(setInstall, (failure) => setError(message(failure)));
  };

  const ready = status?.runs && !status.problem;
  const installing = install?.state === "running";
  const percent = install && install.total_bytes > 0 ? Math.floor((install.done_bytes / install.total_bytes) * 100) : 0;
  return (
    <section className={`runtime-card${ready ? " ready" : status ? " attention" : ""}`} aria-labelledby="runtime-title">
      <div className="section-head">
        <h3 id="runtime-title">llama.cpp runtime</h3>
        {status && (
          <span className={`badge ${ready ? "finished" : "failed"}`}>{ready ? "ready" : status.runs ? "check" : "not ready"}</span>
        )}
        <button className="ghost" onClick={() => void check()} disabled={checking} title="Run llama-server again to check">
          <RefreshCw aria-hidden="true" /> Check
        </button>
      </div>
      {!status && checking && (
        <p className="muted" role="status">
          Checking llama-server…
        </p>
      )}
      {status && (
        <>
          <p className="runtime-path">
            {status.source === "custom" ? "Your llama-server" : `Runtime pack ${status.pack}`}:{" "}
            <code title={status.server_path}>{status.server_path}</code>
          </p>
          {status.version && <p className="muted">{status.version}</p>}
          <p>{status.summary}</p>
          {status.problem && (
            <p className="warning" role="alert">
              {status.problem}
            </p>
          )}
          {status.candidates.length > 0 && !ready && (
            <ul className="candidates" aria-label="llama-server found on this computer">
              {status.candidates.map((candidate) => (
                <li key={candidate}>
                  <span className="muted">Found</span> <code title={candidate}>{candidate}</code>
                  <button onClick={() => void useServer(candidate)} disabled={checking}>
                    Use this
                  </button>
                </li>
              ))}
            </ul>
          )}
          <div className="models-toolbar">
            <button onClick={() => void choose()} disabled={checking}>
              Choose llama-server…
            </button>
            {status.source === "custom" && (
              <button onClick={() => void useServer(null)} disabled={checking}>
                Use the runtime pack instead
              </button>
            )}
            {!ready && (
              <button className={status.source === "pack" && !status.installed ? "primary" : undefined} onClick={startInstall} disabled={installing}>
                <Download aria-hidden="true" /> {status.installed && status.source === "pack" ? "Reinstall" : "Install"} runtime pack (
                {status.suggested_pack})
              </button>
            )}
          </div>
        </>
      )}
      {install && (install.state === "running" || install.state === "failed") && (
        <div className="runtime-install">
          <progress max={100} value={percent} aria-label={`Installing ${install.pack}`} />
          <p className={install.state === "failed" ? "error" : "muted"} role={install.state === "failed" ? "alert" : "status"}>
            {install.state === "failed"
              ? `Installing ${install.pack} failed — ${install.message ?? "no reason given"}`
              : `Installing ${install.pack} — ${percent}%${install.total_bytes > 0 ? ` of ${(install.total_bytes / MIB).toFixed(0)} MiB` : ""}`}
          </p>
        </div>
      )}
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
    </section>
  );
}

/** The models found, each with Use (make it the default), Test, and Settings. */
function ModelTable({
  listing,
  client,
  onChanged,
  onConfigure,
}: {
  listing: LocalModelListResult;
  client: DaemonClient | null;
  onChanged: () => void;
  onConfigure: (id?: string) => void;
}) {
  const [testing, setTesting] = useState<string | null>(null);
  const [results, setResults] = useState<Record<string, ModelTestResult | { error: string }>>({});
  const [error, setError] = useState<string | null>(null);

  if (listing.models.length === 0) {
    return <p className="empty">No models in {listing.folders.join(", ")} yet. GGUF files and Hugging Face folders are recognised.</p>;
  }
  const use = (row: LocalModelRow) => {
    setError(null);
    client
      ?.call<ModelUseResult>("models.use", { model: localSpec(row) })
      .then(onChanged, (failure) => setError(message(failure)));
  };
  const test = async (row: LocalModelRow) => {
    if (!client) return;
    setTesting(row.id);
    setResults((all) => {
      const { [row.id]: _, ...rest } = all;
      return rest;
    });
    try {
      const result = await client.call<ModelTestResult>("models.test", { model: localSpec(row) });
      setResults((all) => ({ ...all, [row.id]: result }));
      onChanged();
    } catch (failure) {
      setResults((all) => ({ ...all, [row.id]: { error: message(failure) } }));
    } finally {
      setTesting(null);
    }
  };
  return (
    <>
      <table className="local-model-table">
        <thead>
          <tr>
            <th>Model</th>
            <th>Quant</th>
            <th>Size</th>
            <th>Context</th>
            <th>On this machine</th>
            <th>
              <span className="visually-hidden">Actions</span>
            </th>
          </tr>
        </thead>
        <tbody>
          {listing.models.map((row) => {
            const spec = localSpec(row);
            const isDefault = listing.default_model === spec;
            const result = results[row.id];
            const unusable = row.problem != null || (row.placement === "none" && !row.loaded);
            return (
              <ModelRows
                key={row.id}
                row={row}
                isDefault={isDefault}
                unusable={unusable}
                testing={testing === row.id}
                busy={testing !== null}
                result={result}
                onUse={() => use(row)}
                onTest={() => void test(row)}
                onSettings={() => onConfigure(row.id)}
              />
            );
          })}
        </tbody>
      </table>
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
    </>
  );
}

function ModelRows({
  row,
  isDefault,
  unusable,
  testing,
  busy,
  result,
  onUse,
  onTest,
  onSettings,
}: {
  row: LocalModelRow;
  isDefault: boolean;
  unusable: boolean;
  testing: boolean;
  busy: boolean;
  result: ModelTestResult | { error: string } | undefined;
  onUse: () => void;
  onTest: () => void;
  onSettings: () => void;
}) {
  const fit = `${placement(row)}${row.tokens_per_s != null ? ` · ${row.tokens_per_s.toFixed(1)} tok/s` : ""}`;
  return (
    <>
      <tr className={unusable ? "unusable" : undefined} title={row.path}>
        <td className="model-name">
          <span className="model-id">{row.id}</span>
          <span className="model-meta">
            {row.repo && <span className="muted">{row.repo}</span>}
            {row.runtime === "vllm" && <span className="badge">vLLM</span>}
            {row.mmproj && (
              <span className="badge" title={row.mmproj}>
                vision
              </span>
            )}
            {isDefault && <span className="badge local">default</span>}
            <span className="muted fit-inline">{fit}</span>
          </span>
        </td>
        <td>{row.quantization ?? (row.runtime === "vllm" ? "safetensors" : "?")}</td>
        <td>{gib(row.size_bytes)}</td>
        <td title={row.context > 0 && row.context !== row.context_length ? `Trained for ${context(row.context_length)} tokens; each session gets ${context(row.context)}` : undefined}>
          {context(row.context_length)}
          {row.context > 0 && row.context !== row.context_length && <span className="muted"> · {context(row.context)}/session</span>}
        </td>
        <td>
          <span className={`badge ${row.loaded ? "local" : ""}`} title={row.problem ?? `needs ${gib(row.required_bytes)}`}>
            {fit}
          </span>
        </td>
        <td className="model-actions">
          {isDefault ? (
            <span className="muted default-mark">
              <Check aria-hidden="true" /> Default
            </span>
          ) : (
            <button onClick={onUse} disabled={row.problem != null} title={`Use for new sessions: ${localDescription(row)}`}>
              Use
            </button>
          )}
          <button onClick={onTest} disabled={busy || unusable} title="Load it and send one short prompt">
            <FlaskConical aria-hidden="true" /> {testing ? "Testing…" : "Test"}
          </button>
          <button className="ghost icon" onClick={onSettings} aria-label={`Settings for ${row.id}`} title="Context, slots, GPU layers…">
            <SlidersHorizontal aria-hidden="true" />
          </button>
        </td>
      </tr>
      {row.problem && (
        <tr className="model-note">
          <td colSpan={6} className="muted">
            {row.problem}
          </td>
        </tr>
      )}
      {testing && (
        <tr className="model-note">
          <td colSpan={6} className="muted" role="status">
            Loading {row.id} and sending a short prompt…
          </td>
        </tr>
      )}
      {result && (
        <tr className="model-note">
          <td colSpan={6}>
            {"error" in result ? (
              <span className="error" role="alert">
                Test failed — {result.error}
              </span>
            ) : result.ok ? (
              <span role="status">
                <strong>“{result.reply}”</strong>{" "}
                <span className="muted">
                  {result.completion_tokens} tokens in {result.seconds.toFixed(1)} s
                  {result.tokens_per_s != null ? ` · ${result.tokens_per_s.toFixed(1)} tok/s` : ""}
                  {result.load_seconds >= 1 ? ` · loaded in ${result.load_seconds.toFixed(0)} s` : ""}
                </span>
              </span>
            ) : (
              <span className="error" role="alert">
                Test failed — {result.message ?? "no reason given"}
              </span>
            )}
          </td>
        </tr>
      )}
    </>
  );
}

/** The "Download from Hugging Face" form and the progress of downloads. */
function HfDownload({
  client,
  onDone,
  inputRef,
}: {
  client: DaemonClient | null;
  onDone: () => void;
  inputRef: RefObject<HTMLInputElement | null>;
}) {
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
          ref={inputRef}
          value={repo}
          placeholder="owner/name, e.g. Qwen/Qwen3-8B-GGUF"
          spellCheck={false}
          autoComplete="off"
          onChange={(event) => setRepo(event.target.value)}
        />
        <button type="submit" disabled={!client || busy || !repo.trim()}>
          <Search aria-hidden="true" /> List files
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
          <Select
            id="hf-variant"
            value={variant}
            onChange={setVariant}
            options={files.variants.map((option) => ({
              value: option.name,
              label: option.name,
              description: `${gib(option.size_bytes)} · ${fitHint(option.fit)}`,
            }))}
          />
          {files.projectors.length > 0 && (
            <label>
              <input type="checkbox" checked={mmproj} onChange={(event) => setMmproj(event.target.checked)} /> Include
              vision projector
            </label>
          )}
          <button className="primary" onClick={start} disabled={busy || !variant}>
            <Download aria-hidden="true" /> Download
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
            <X aria-hidden="true" /> Cancel
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
