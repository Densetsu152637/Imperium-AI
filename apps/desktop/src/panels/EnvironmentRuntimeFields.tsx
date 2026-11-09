// Where an environment's tools run (this machine, Docker, WSL, or SSH), a Test button that checks
// the runtime can be reached, and the switch for the environment's persistent agent session. A
// small self-contained section any environment create/edit view can host.

import { useEffect, useState } from "react";

import type { EnvironmentInfo, EnvironmentRuntime, RuntimeTestResult } from "@imperium/protocol";

import { Select } from "../components/Select";
import { Switch } from "../components/Switch";

type Kind = EnvironmentRuntime["kind"];

const KINDS: { value: Kind; label: string; description: string }[] = [
  { value: "local", label: "This machine", description: "Under the configured isolation tier" },
  { value: "docker", label: "Docker", description: "A container from an image, or a running container" },
  { value: "wsl", label: "WSL", description: "A Windows Subsystem for Linux distribution" },
  { value: "ssh", label: "SSH", description: "A host from your SSH config, or user@host:port" },
];

/** The fields of a runtime being edited, kept as text whatever the kind. */
type Draft = { kind: Kind; image: string; container: string; distro: string; host: string };

const draftOf = (runtime: EnvironmentRuntime | undefined): Draft => {
  const draft: Draft = { kind: runtime?.kind ?? "local", image: "", container: "", distro: "", host: "" };
  if (runtime?.kind === "docker") return { ...draft, image: runtime.image, container: runtime.container ?? "" };
  if (runtime?.kind === "wsl") return { ...draft, distro: runtime.distro };
  if (runtime?.kind === "ssh") return { ...draft, host: runtime.host };
  return draft;
};

/** The runtime a draft describes, or why it is not complete. */
export function runtimeOf(draft: Draft): EnvironmentRuntime | string {
  switch (draft.kind) {
    case "local":
      return { kind: "local" };
    case "docker":
      if (!draft.image.trim()) return "Name the Docker image.";
      return draft.container.trim()
        ? { kind: "docker", image: draft.image.trim(), container: draft.container.trim() }
        : { kind: "docker", image: draft.image.trim() };
    case "wsl":
      return draft.distro.trim() ? { kind: "wsl", distro: draft.distro.trim() } : "Name the WSL distribution.";
    case "ssh":
      return draft.host.trim() ? { kind: "ssh", host: draft.host.trim() } : "Name the SSH host.";
  }
}

const same = (a: EnvironmentRuntime | undefined, b: EnvironmentRuntime) =>
  JSON.stringify(draftOf(a)) === JSON.stringify(draftOf(b));

export function EnvironmentRuntimeFields({
  env,
  connected,
  onSave,
  onTest,
  onSetPersistentAgent,
}: {
  env: EnvironmentInfo;
  connected: boolean;
  onSave: (runtime: EnvironmentRuntime) => Promise<void>;
  onTest: () => Promise<RuntimeTestResult>;
  onSetPersistentAgent: (on: boolean) => void;
}) {
  const [draft, setDraft] = useState<Draft>(() => draftOf(env.runtime));
  const [result, setResult] = useState<RuntimeTestResult | null>(null);
  const [busy, setBusy] = useState(false);
  // Follow changes made elsewhere (another window, the CLI).
  useEffect(() => setDraft(draftOf(env.runtime)), [env.runtime]);

  const runtime = runtimeOf(draft);
  const changed = typeof runtime !== "string" && !same(env.runtime, runtime);
  const field = (key: keyof Omit<Draft, "kind">, label: string, placeholder: string) => (
    <label className="runtime-field">
      <span>{label}</span>
      <input
        value={draft[key]}
        placeholder={placeholder}
        disabled={!connected}
        onChange={(event) => setDraft({ ...draft, [key]: event.target.value })}
      />
    </label>
  );

  return (
    <section className="environment-runtime" aria-label={`Runtime of ${env.name}`}>
      <div className="runtime-row">
        <span className="runtime-label">Runtime</span>
        <Select
          aria-label="Runtime"
          value={draft.kind}
          disabled={!connected}
          options={KINDS}
          onChange={(kind) => {
            setResult(null);
            setDraft({ ...draft, kind: kind as Kind });
          }}
        />
      </div>
      {draft.kind === "docker" && (
        <>
          {field("image", "Image", "ubuntu:24.04")}
          {field("container", "Running container (optional)", "my-dev-container")}
        </>
      )}
      {draft.kind === "wsl" && field("distro", "Distribution", "Ubuntu")}
      {draft.kind === "ssh" && field("host", "Host", "devbox or me@host:22")}
      {draft.kind !== "local" && (
        <p className="muted runtime-hint">
          {draft.kind === "docker" && !draft.container.trim()
            ? "Each command runs in a fresh container with the folders mounted."
            : "Folders are paths inside the runtime, such as /home/me/project."}
        </p>
      )}
      <div className="environment-actions">
        <button
          className={changed ? "primary" : undefined}
          disabled={!connected || !changed || busy}
          title={typeof runtime === "string" ? runtime : undefined}
          onClick={async () => {
            if (typeof runtime === "string") return;
            setBusy(true);
            setResult(null);
            try {
              await onSave(runtime);
            } finally {
              setBusy(false);
            }
          }}
        >
          Save runtime
        </button>
        <button
          disabled={!connected || changed || busy}
          title={changed ? "Save the runtime first" : "Check that the runtime can be reached"}
          onClick={async () => {
            setBusy(true);
            try {
              setResult(await onTest());
            } catch (error) {
              setResult({ ok: false, message: error instanceof Error ? error.message : String(error) });
            } finally {
              setBusy(false);
            }
          }}
        >
          {busy ? "Testing…" : "Test"}
        </button>
      </div>
      {result && (
        <p className={result.ok ? "runtime-result ok" : "runtime-result error"} role="status">
          {result.message}
        </p>
      )}
      <div className="runtime-row">
        <span className="runtime-label" title="An always-available session that keeps its conversation across days">
          Persistent agent
        </span>
        <Switch
          label={`Persistent agent for ${env.name}`}
          checked={env.persistent_agent ?? false}
          disabled={!connected}
          onChange={onSetPersistentAgent}
        />
      </div>
    </section>
  );
}
