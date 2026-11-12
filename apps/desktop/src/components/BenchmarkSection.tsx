// Settings → Models → Local model benchmark: runs the daemon's benchmark (`models.bench`) on one
// local model or all of them and shows the saved results. It only runs when asked, takes its
// place in the normal model queue as a background call (so it waits while the machine is saturated), and
// can be cancelled while it runs. Numbers come from the daemon; nothing is measured here.

import { useCallback, useEffect, useState } from "react";

import type {
  BenchResult,
  BenchRun,
  LocalModelListResult,
  ModelBenchCancelResult,
  ModelBenchResultsResult,
} from "@imperium/protocol";

import { activeRun, gibibytes, history, latestHere, mergeRun, millis, seconds, tokensPerSecond } from "../bench";
import type { DaemonClient } from "../daemon/client";
import { Row, RowNote } from "./SettingsRow";

const message = (error: unknown): string => (error instanceof Error ? error.message : String(error));

const ALL = "";

function ResultRow({ result }: { result: BenchResult }) {
  return (
    <tr>
      <th scope="row">{result.model_id}</th>
      <td>{seconds(result.load_s)}</td>
      <td>{millis(result.ttft_ms)}</td>
      <td>{tokensPerSecond(result.prompt_tok_s)}</td>
      <td>{tokensPerSecond(result.gen_tok_s, 1)}</td>
      <td>
        {gibibytes(result.peak_ram_bytes)} / {gibibytes(result.peak_vram_bytes)}
      </td>
      <td>{result.date.slice(0, 10)}</td>
    </tr>
  );
}

function ResultTable({ results }: { results: BenchResult[] }) {
  return (
    <table className="bench-table">
      <thead>
        <tr>
          <th scope="col">Model</th>
          <th scope="col">Load</th>
          <th scope="col">First token</th>
          <th scope="col">Prompt</th>
          <th scope="col">Generation</th>
          <th scope="col">Peak RAM / VRAM</th>
          <th scope="col">Date</th>
        </tr>
      </thead>
      <tbody>
        {results.map((result) => (
          <ResultRow key={`${result.run_id}:${result.model_id}`} result={result} />
        ))}
      </tbody>
    </table>
  );
}

export function BenchmarkSection({ client }: { client: DaemonClient }) {
  const [models, setModels] = useState<{ id: string; label: string }[]>([]);
  const [model, setModel] = useState(ALL);
  const [results, setResults] = useState<BenchResult[]>([]);
  const [fingerprints, setFingerprints] = useState<string[]>([]);
  const [runs, setRuns] = useState<BenchRun[]>([]);
  const [warm, setWarm] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const listing = await client.call<ModelBenchResultsResult>("models.bench_results", {});
      setResults(listing.results);
      setFingerprints(listing.hardware_fingerprints);
      setRuns(listing.runs);
    } catch (failure) {
      setError(message(failure));
    }
  }, [client]);

  useEffect(() => {
    void load();
    client
      .call<LocalModelListResult>("models.local")
      .then((listing) =>
        setModels(
          listing.models
            .filter((row) => !row.problem)
            .map((row) => ({ id: row.id, label: row.alias ? `${row.alias} (${row.id})` : row.id })),
        ),
      )
      .catch((failure: unknown) => setError(message(failure)));
    // Follows runs started anywhere (the CLI, another window); the saved results are read again
    // whenever a run ends.
    return client.onNotification((notification) => {
      if (notification.method !== "models.bench_progress") return;
      const run = notification.params as BenchRun;
      setRuns((current) => mergeRun(current, run));
      if (run.state !== "running") void load();
    });
  }, [client, load]);

  const running = activeRun(runs);

  const start = async () => {
    setBusy(true);
    setError(null);
    try {
      const run = await client.call<BenchRun>("models.bench", { model: model === ALL ? null : model, cold: !warm });
      setRuns((current) => mergeRun(current, run));
    } catch (failure) {
      setError(message(failure));
    } finally {
      setBusy(false);
    }
  };

  const cancel = async (run: BenchRun) => {
    setBusy(true);
    try {
      await client.call<ModelBenchCancelResult>("models.bench_cancel", { run_id: run.run_id });
    } catch (failure) {
      setError(message(failure));
    } finally {
      setBusy(false);
    }
  };

  const here = latestHere(results, fingerprints);
  const earlier = history(results, here);
  const failed = runs.flatMap((run) => run.results).filter((result) => result.error);

  return (
    <section className="environment-section" aria-label="Local model benchmark">
      <div className="environment-section-head">
        <span className="environment-label">Local model benchmark</span>
      </div>
      <Row
        label="Run a benchmark"
        description="Loads the model the way any call does, waits while the machine is saturated, and sends a few fixed prompts, measuring load time, time to first token, prompt and generation speed, and peak memory. It runs only when you start it and can be cancelled."
      >
        <select
          className="field"
          aria-label="Model to benchmark"
          value={model}
          disabled={busy || !!running}
          onChange={(event) => setModel(event.target.value)}
        >
          <option value={ALL}>All local models</option>
          {models.map((row) => (
            <option key={row.id} value={row.id}>
              {row.label}
            </option>
          ))}
        </select>
        {running ? (
          <button type="button" disabled={busy} onClick={() => void cancel(running)}>
            Cancel
          </button>
        ) : (
          <button type="button" className="primary" disabled={busy || models.length === 0} onClick={() => void start()}>
            Run benchmark
          </button>
        )}
      </Row>
      <Row sub label="Keep a loaded model loaded" description="Skips unloading an idle model first, so its load time is not measured.">
        <input type="checkbox" aria-label="Keep a loaded model loaded" checked={warm} disabled={!!running} onChange={(event) => setWarm(event.target.checked)} />
      </Row>
      {running && (
        <RowNote>
          <p role="status">
            {running.current ?? "Starting"}
            {running.step ? ` — ${running.step}` : ""} ({running.models_done} of {running.models.length} models done)
          </p>
        </RowNote>
      )}
      {failed.map((result) => (
        <p key={`${result.run_id}:${result.model_id}`} className="error" role="alert">
          {result.model_id}: {result.error}
        </p>
      ))}
      {here.length === 0 && !running && (
        <p className="muted">No results for this machine yet. Run a benchmark to see how fast your models are here.</p>
      )}
      {here.length > 0 && (
        <>
          <p className="muted">Latest on this machine ({here[0].hardware}):</p>
          <ResultTable results={here} />
        </>
      )}
      {earlier.length > 0 && (
        <details>
          <summary>Earlier results and other machines ({earlier.length})</summary>
          <ResultTable results={earlier} />
          <p className="muted">Results are kept per model, machine and date; a different GPU, memory, or runtime build (the llama.cpp release and pack, or vLLM) counts as another machine.</p>
        </details>
      )}
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
    </section>
  );
}
