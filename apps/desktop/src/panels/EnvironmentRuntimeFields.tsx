// Where an environment's tools run (this machine, Docker, WSL, or SSH), a Test button that checks
// the saved runtime can be reached, and the project-worktrees switch. Controlled: the host (the environment window) keeps the values and saves them.

import { useState } from "react";

import type { EnvironmentRuntime, RuntimeTestResult } from "@imperium/protocol";

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
export type RuntimeDraft = { kind: Kind; image: string; container: string; distro: string; host: string };

export const draftOf = (runtime: EnvironmentRuntime | undefined): RuntimeDraft => {
  const draft: RuntimeDraft = { kind: runtime?.kind ?? "local", image: "", container: "", distro: "", host: "" };
  if (runtime?.kind === "docker") return { ...draft, image: runtime.image, container: runtime.container ?? "" };
  if (runtime?.kind === "wsl") return { ...draft, distro: runtime.distro };
  if (runtime?.kind === "ssh") return { ...draft, host: runtime.host };
  return draft;
};

/** The runtime a draft describes, or why it is not complete. */
export function runtimeOf(draft: RuntimeDraft): EnvironmentRuntime | string {
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

/** Whether a draft's folders are paths inside the runtime rather than on this machine. */
export const isRemote = (draft: RuntimeDraft) =>
  draft.kind === "wsl" || draft.kind === "ssh" || (draft.kind === "docker" && draft.container.trim() !== "");

/** Whether two runtimes are the same. */
export const sameRuntime = (a: EnvironmentRuntime | undefined, b: RuntimeDraft) =>
  JSON.stringify(draftOf(a)) === JSON.stringify(b);

export function EnvironmentRuntimeFields({
  draft,
  onChange,
  projectWorktrees,
  onProjectWorktrees,
  disabled,
  testBlocked,
  onTest,
}: {
  draft: RuntimeDraft;
  onChange: (draft: RuntimeDraft) => void;
  /** Whether sessions also get the git worktrees of the folders' repositories. */
  projectWorktrees: boolean;
  onProjectWorktrees: (on: boolean) => void;
  disabled?: boolean;
  /** Why Test cannot run now (unsaved changes, no environment yet), if it cannot. */
  testBlocked?: string | null;
  onTest: () => Promise<RuntimeTestResult>;
}) {
  const [result, setResult] = useState<RuntimeTestResult | null>(null);
  const [testing, setTesting] = useState(false);
  const field = (key: keyof Omit<RuntimeDraft, "kind">, label: string, placeholder: string) => (
    <label className="runtime-field">
      <span>{label}</span>
      <input
        value={draft[key]}
        placeholder={placeholder}
        disabled={disabled}
        onChange={(event) => {
          setResult(null);
          onChange({ ...draft, [key]: event.target.value });
        }}
      />
    </label>
  );

  return (
    <section className="environment-section environment-runtime" aria-label="Runtime">
      <div className="environment-section-head">
        <span className="environment-label">Runtime</span>
        <Select
          aria-label="Runtime"
          value={draft.kind}
          disabled={disabled}
          options={KINDS}
          onChange={(kind) => {
            setResult(null);
            onChange({ ...draft, kind: kind as Kind });
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
          {isRemote(draft)
            ? "Folders are paths inside the runtime, such as /home/me/project."
            : "Each command runs in a fresh container with the folders mounted."}
        </p>
      )}
      <div className="runtime-row">
        <button
          type="button"
          disabled={disabled || testing || !!testBlocked}
          title={testBlocked ?? "Check that the runtime can be reached"}
          onClick={async () => {
            setTesting(true);
            try {
              setResult(await onTest());
            } catch (error) {
              setResult({ ok: false, message: error instanceof Error ? error.message : String(error) });
            } finally {
              setTesting(false);
            }
          }}
        >
          {testing ? "Testing…" : "Test"}
        </button>
        {result && (
          <span className={result.ok ? "runtime-result ok" : "runtime-result error"} role="status">
            {result.message}
          </span>
        )}
      </div>
      <div className="runtime-row">
        <span
          className="environment-label"
          title="Sessions can also use the git worktrees of the folders' repositories (the ones git worktree list shows, including new ones), as they use the folders. Local folders only; a session started in its own worktree keeps to it."
        >
          Project worktrees
        </span>
        <Switch
          label="Project worktrees"
          checked={projectWorktrees}
          disabled={disabled || isRemote(draft)}
          onChange={onProjectWorktrees}
        />
      </div>
    </section>
  );
}
