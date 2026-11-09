// Resources: live CPU, RAM, GPU and VRAM usage, the local models loaded (in use or cached), and the
// hosted endpoints sessions use. The daemon samples only while a client subscribes, so this panel
// subscribes while it is mounted and the window is visible.

import { Cpu, Globe, Gpu, MemoryStick, PowerOff, Server } from "lucide-react";
import { useEffect, useState, type ComponentType, type ReactNode } from "react";
import type { ResourceSnapshot } from "@imperium/protocol";
import type { DaemonClient } from "../daemon/client";

/** Samples kept for the sparklines: two minutes at one every two seconds. */
const HISTORY = 60;
const GIB = 1024 ** 3;

const gib = (bytes: number): string => `${(bytes / GIB).toFixed(1)} GiB`;
const share = (used: number, total: number): number => (total > 0 ? (used / total) * 100 : 0);
const message = (error: unknown): string => (error instanceof Error ? error.message : String(error));

function countdown(ms: number): string {
  const s = Math.round(ms / 1000);
  return s >= 60 ? `${Math.floor(s / 60)}m ${String(s % 60).padStart(2, "0")}s` : `${s}s`;
}

type History = Record<string, number[]>;

/** Appends this snapshot's percentages to each series, keeping the last {@link HISTORY}. */
function extend(history: History, snapshot: ResourceSnapshot): History {
  const next: History = { ...history };
  const push = (key: string, value: number) => {
    next[key] = [...(history[key] ?? []), value].slice(-HISTORY);
  };
  push("cpu", snapshot.cpu.total_percent);
  push("ram", share(snapshot.memory.ram_used_bytes, snapshot.memory.ram_total_bytes));
  for (const gpu of snapshot.gpus) {
    push(`gpu${gpu.index}`, gpu.utilization_percent ?? 0);
    push(`vram${gpu.index}`, share(gpu.vram_used_bytes, gpu.vram_total_bytes));
  }
  return next;
}

export function ResourcesPanel({ client }: { client: DaemonClient | null }) {
  const [snapshot, setSnapshot] = useState<ResourceSnapshot | null>(null);
  const [history, setHistory] = useState<History>({});
  const [error, setError] = useState<string | null>(null);
  const [unloading, setUnloading] = useState<string | null>(null);

  useEffect(() => {
    if (!client) return;
    const take = (next: ResourceSnapshot) => {
      setSnapshot(next);
      setHistory((all) => extend(all, next));
      setError(null);
    };
    const stopListening = client.onNotification((notification) => {
      if (notification.method === "system.resources_changed") take(notification.params as ResourceSnapshot);
    });
    const subscribe = (on: boolean) =>
      client.call("system.resources_subscribe", { on }).catch((reason: unknown) => {
        if (on) setError(message(reason));
      });
    // A hidden window (minimised, or behind another tab) needs no samples.
    const follow = () => void subscribe(document.visibilityState === "visible");
    client.call<ResourceSnapshot>("system.resources").then(take, (reason: unknown) => setError(message(reason)));
    follow();
    document.addEventListener("visibilitychange", follow);
    return () => {
      stopListening();
      document.removeEventListener("visibilitychange", follow);
      void subscribe(false);
    };
  }, [client]);

  const unload = (model: string) => {
    if (!client) return;
    setUnloading(model);
    client
      .call("models.unload", { model })
      .catch((reason: unknown) => setError(message(reason)))
      .finally(() => setUnloading(null));
  };

  if (!client) return <p className="empty">Resources appear once imperium-core is connected.</p>;
  if (!snapshot) return <p className="empty">{error ?? "Reading resources…"}</p>;
  const { cpu, memory } = snapshot;
  const daemon = snapshot.processes.find((process) => process.role === "daemon");
  return (
    <div className="resources">
      {error && <p className="warning">{error}</p>}
      <section className="meters" aria-label="Machine usage">
        <Meter
          icon={Cpu}
          label="CPU"
          percent={cpu.total_percent}
          detail={`${cpu.per_core.length} cores`}
          title={cpu.per_core.map((core, i) => `core ${i}: ${core.toFixed(0)}%`).join("\n")}
          history={history.cpu}
        />
        <Meter
          icon={MemoryStick}
          label="RAM"
          percent={share(memory.ram_used_bytes, memory.ram_total_bytes)}
          detail={`${gib(memory.ram_used_bytes)} / ${gib(memory.ram_total_bytes)}`}
          title={`Swap ${gib(memory.swap_used_bytes)} / ${gib(memory.swap_total_bytes)}`}
          history={history.ram}
        />
        {snapshot.gpus.map((gpu) => (
          <div className="gpu" key={gpu.index}>
            <Meter
              icon={Gpu}
              label={`GPU ${gpu.index}`}
              percent={gpu.utilization_percent}
              detail={gpu.temperature_c === null ? gpu.name : `${gpu.name} · ${gpu.temperature_c.toFixed(0)} °C`}
              history={history[`gpu${gpu.index}`]}
            />
            <Meter
              label="VRAM"
              percent={share(gpu.vram_used_bytes, gpu.vram_total_bytes)}
              detail={`${gib(gpu.vram_used_bytes)} / ${gib(gpu.vram_total_bytes)}`}
              history={history[`vram${gpu.index}`]}
            />
          </div>
        ))}
        {snapshot.gpus.length === 0 && <p className="muted">No GPU reported (NVIDIA GPUs are read through their driver).</p>}
      </section>

      <Section icon={Server} title="Local models">
        {snapshot.local_models.length === 0 ? (
          <p className="empty">No local model is loaded.</p>
        ) : (
          <table>
            <thead>
              <tr>
                <th>Model</th>
                <th>Placement</th>
                <th>Memory</th>
                <th>State</th>
                <th>Sessions</th>
                <th aria-label="Actions" />
              </tr>
            </thead>
            <tbody>
              {snapshot.local_models.map((model) => {
                const process = model.pid === null ? undefined : snapshot.processes.find((p) => p.pid === model.pid);
                return (
                  <tr
                    key={model.model}
                    title={
                      process
                        ? `pid ${process.pid} · CPU ${process.cpu_percent.toFixed(1)}% · RAM ${gib(process.ram_bytes)}` +
                          (process.vram_bytes === null ? "" : ` · VRAM ${gib(process.vram_bytes)}`)
                        : undefined
                    }
                  >
                    <td>{`${model.runtime}:${model.model}`}</td>
                    <td>
                      {model.cpu_pinned
                        ? "CPU lane"
                        : model.placement === "partial"
                          ? `partial · ${model.gpu_layers} layers`
                          : model.placement}
                      <span className="muted" title="Tokens one session may use · KV pool reserved by running calls / its size">
                        {" "}
                        · ctx {model.context} · pool {model.pool_reserved_tokens}/{model.pool_tokens}
                      </span>
                    </td>
                    <td>
                      {model.vram_bytes > 0 && `${gib(model.vram_bytes)} VRAM`}
                      {model.vram_bytes > 0 && model.ram_bytes > 0 && " · "}
                      {model.ram_bytes > 0 && `${gib(model.ram_bytes)} RAM`}
                    </td>
                    <td>
                      {model.slots_in_use > 0 ? (
                        <span className="badge running">
                          {model.slots_in_use}/{model.slots} busy{model.queued > 0 ? ` · ${model.queued} queued` : ""}
                        </span>
                      ) : model.queued > 0 || model.claimed ? (
                        <span className="badge running" title="An agent is mid-turn on it, so it stays loaded">
                          {model.queued > 0 ? `${model.queued} queued` : "held by a turn"}
                        </span>
                      ) : (
                        <span className="badge">
                          idle{model.unload_in_ms === null ? "" : ` · unloads in ${countdown(model.unload_in_ms)}`}
                        </span>
                      )}
                    </td>
                    <td title={model.sessions.join("\n")}>{model.sessions.length}</td>
                    <td>
                      {model.can_unload && (
                        <button
                          className="ghost icon-only"
                          onClick={() => unload(model.model)}
                          disabled={unloading === model.model}
                          title={`Unload ${model.model} now`}
                          aria-label={`Unload ${model.model}`}
                        >
                          <PowerOff aria-hidden="true" />
                        </button>
                      )}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        )}
      </Section>

      <Section icon={Globe} title="Endpoints · last 15 min">
        {snapshot.endpoints.length === 0 ? (
          <p className="empty">No session uses a hosted endpoint.</p>
        ) : (
          <table>
            <thead>
              <tr>
                <th>Model</th>
                <th>Sessions</th>
                <th>Requests</th>
                <th>Tokens</th>
                <th>Latency</th>
                <th>Errors</th>
              </tr>
            </thead>
            <tbody>
              {snapshot.endpoints.map((endpoint) => (
                <tr key={endpoint.spec} title={endpoint.host ? `${endpoint.endpoint} · ${endpoint.host}` : endpoint.endpoint}>
                  <td>{endpoint.spec}</td>
                  <td title={endpoint.sessions.join("\n")}>{endpoint.sessions.length}</td>
                  <td>{endpoint.requests}</td>
                  <td>{(endpoint.prompt_tokens + endpoint.completion_tokens).toLocaleString()}</td>
                  <td>{endpoint.last_latency_ms === null ? "—" : `${(endpoint.last_latency_ms / 1000).toFixed(1)} s`}</td>
                  <td title={endpoint.last_error ?? undefined} className={endpoint.errors > 0 ? "errors" : undefined}>
                    {endpoint.errors}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </Section>

      {daemon && (
        <p className="muted daemon-usage">
          imperium-core · CPU {daemon.cpu_percent.toFixed(1)}% · RAM {gib(daemon.ram_bytes)}
        </p>
      )}
    </div>
  );
}

function Section({ icon: Icon, title, children }: { icon: ComponentType<{ "aria-hidden"?: boolean }>; title: string; children: ReactNode }) {
  return (
    <section className="resource-section">
      <h3>
        <Icon aria-hidden={true} /> {title}
      </h3>
      {children}
    </section>
  );
}

/** One usage figure: a bar with its percentage and a sparkline of recent samples. */
function Meter({
  icon: Icon,
  label,
  percent,
  detail,
  title,
  history = [],
}: {
  icon?: ComponentType<{ "aria-hidden"?: boolean }>;
  label: string;
  percent: number | null;
  detail: string;
  title?: string;
  history?: number[];
}) {
  const value = percent === null ? null : Math.max(0, Math.min(100, percent));
  return (
    <div className="meter" title={title}>
      <span className="meter-label">
        {Icon && <Icon aria-hidden={true} />}
        {label}
      </span>
      <span
        className="meter-bar"
        role="meter"
        aria-label={label}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={value ?? undefined}
        data-level={value === null ? undefined : value >= 90 ? "high" : value >= 70 ? "busy" : undefined}
      >
        <span style={{ width: `${value ?? 0}%` }} />
      </span>
      <span className="meter-value">{value === null ? "—" : `${value.toFixed(0)}%`}</span>
      <Sparkline values={history} />
      <span className="meter-detail muted">{detail}</span>
    </div>
  );
}

function Sparkline({ values }: { values: number[] }) {
  if (values.length < 2) return <svg className="sparkline" aria-hidden="true" />;
  const step = 100 / (HISTORY - 1);
  const start = HISTORY - values.length;
  const points = values.map((v, i) => `${((start + i) * step).toFixed(1)},${(20 - (Math.min(100, v) / 100) * 20).toFixed(1)}`);
  return (
    <svg className="sparkline" viewBox="0 0 100 20" preserveAspectRatio="none" aria-hidden="true">
      <polyline points={points.join(" ")} vectorEffect="non-scaling-stroke" />
    </svg>
  );
}
