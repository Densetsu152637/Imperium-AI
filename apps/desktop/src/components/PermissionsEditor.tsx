// Editing path grants and permission rules, for Settings → Permissions (global) and the
// environment window (per environment). The editor changes a draft the caller saves: grants open
// paths outside an environment's folders read-only or writable (one-click presets for common
// caches and toolchains, resolved by the daemon for its machine); rules allow, ask, or deny tool
// calls by tool name and a pattern (a command prefix for run_command, a path glob for the file
// tools). Deny wins over ask, and ask over allow; an environment's rule overrides a global rule
// of equal specificity.

import { FolderPlus, Plus, X } from "lucide-react";
import { useState } from "react";

import type { PathGrant, PathMode, PathPreset, PermissionRule, PermissionSet, RuleAction } from "@imperium/protocol";

import { pickFolder } from "../daemon/connection";
import { Select } from "./Select";

const MODES: { value: PathMode; label: string }[] = [
  { value: "read", label: "Read-only" },
  { value: "read_write", label: "Read and write" },
];

const ACTIONS: { value: RuleAction; label: string }[] = [
  { value: "allow", label: "Allow" },
  { value: "ask", label: "Ask" },
  { value: "deny", label: "Deny" },
];

/** Tools a rule commonly names; any tool name (or a `*` wildcard such as `mcp__github__*`) works. */
const TOOLS = ["run_command", "write_file", "edit_file", "read_file", "list_dir", "web_fetch", "*"];

/** The placeholder for a rule's pattern, by tool. */
function patternHint(tool: string): string {
  if (tool === "run_command") return "command prefix, e.g. cargo test";
  if (["read_file", "write_file", "edit_file", "list_dir"].includes(tool)) return "path glob, e.g. src/** or .env";
  return "empty: every call";
}

/** A rule in one line, such as `allow run_command cargo test`. */
export function describeRule(rule: PermissionRule): string {
  return `${rule.action} ${rule.tool}${rule.pattern ? ` ${rule.pattern}` : ""}`;
}

/** Whether two grant paths name the same place, as typed (case and separators aside). */
function samePath(a: string, b: string): boolean {
  const norm = (path: string) => path.trim().replace(/\\/g, "/").replace(/\/+$/, "").toLowerCase();
  return norm(a) === norm(b);
}

export function PermissionsEditor({
  value,
  onChange,
  presets,
  remote = false,
  disabled = false,
}: {
  value: PermissionSet;
  onChange: (next: PermissionSet) => void;
  /** One-click grants for common caches; empty hides them. */
  presets: PathPreset[];
  /** The environment's tools run inside a container, WSL, or over SSH: grants are paths there. */
  remote?: boolean;
  disabled?: boolean;
}) {
  const [typedPath, setTypedPath] = useState("");
  const [typedMode, setTypedMode] = useState<PathMode>("read");

  const setGrants = (grants: PathGrant[]) => onChange({ ...value, grants });
  const setRules = (rules: PermissionRule[]) => onChange({ ...value, rules });
  const addGrant = (path: string, mode: PathMode) => {
    const trimmed = path.trim();
    if (!trimmed || value.grants.some((grant) => samePath(grant.path, trimmed))) return;
    setGrants([...value.grants, { path: trimmed, mode }]);
  };
  const updateRule = (index: number, change: Partial<PermissionRule>) =>
    setRules(value.rules.map((rule, i) => (i === index ? { ...rule, ...change } : rule)));

  return (
    <div className="permissions-editor">
      <section className="environment-section" aria-label="Paths outside the folders">
        <div className="environment-section-head">
          <span className="environment-label">Paths outside the folders</span>
          {!remote && (
            <button
              type="button"
              disabled={disabled}
              onClick={() =>
                void pickFolder("Grant a folder outside the environment").then((folder) => folder && addGrant(folder, "read"))
              }
            >
              <FolderPlus aria-hidden="true" /> Add folder
            </button>
          )}
        </div>
        <p className="muted permissions-hint">
          {remote
            ? "Absolute paths inside the runtime. Read-only paths can be read and listed but not written or edited by the file tools; commands in the runtime are not limited by them."
            : "Global package caches and toolchains the tools may use. Read-only paths refuse writes and edits; the OS sandbox and container tiers apply the same modes to commands (a Docker container mounts them, read-only ones with :ro)."}
        </p>
        {!remote && presets.length > 0 && (
          <div className="permission-presets" role="group" aria-label="Presets">
            {presets.map((preset) => {
              const added = value.grants.some((grant) => samePath(grant.path, preset.path));
              return (
                <button
                  key={preset.id}
                  type="button"
                  className="chip"
                  aria-pressed={added}
                  disabled={disabled || added}
                  title={`${preset.path} · ${preset.mode === "read" ? "read-only" : "read and write"}${preset.exists ? "" : " · not found on this machine"}`}
                  onClick={() => addGrant(preset.path, preset.mode)}
                >
                  {added ? "✓ " : "+ "}
                  {preset.label}
                  {!preset.exists && <span className="muted"> (not found)</span>}
                </button>
              );
            })}
          </div>
        )}
        <ul className="environment-folders permission-list" aria-label="Granted paths">
          {value.grants.map((grant, index) => (
            <li key={`${grant.path}-${index}`}>
              <span className="folder-path" title={grant.path}>
                {grant.path}
              </span>
              <Select
                aria-label={`Access to ${grant.path}`}
                value={grant.mode}
                options={MODES}
                disabled={disabled}
                onChange={(mode) =>
                  setGrants(value.grants.map((g, i) => (i === index ? { ...g, mode: mode as PathMode } : g)))
                }
              />
              <button
                type="button"
                className="ghost icon-only"
                aria-label={`Remove ${grant.path}`}
                title="Remove this grant (the folder itself is untouched)"
                disabled={disabled}
                onClick={() => setGrants(value.grants.filter((_, i) => i !== index))}
              >
                <X />
              </button>
            </li>
          ))}
          {value.grants.length === 0 && <li className="muted">No paths outside the folders.</li>}
        </ul>
        <div className="runtime-row">
          <input
            aria-label="Path to grant"
            placeholder={remote ? "/home/me/.cargo" : "~/.cargo or an absolute path"}
            value={typedPath}
            disabled={disabled}
            onChange={(event) => setTypedPath(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") {
                event.preventDefault();
                addGrant(typedPath, typedMode);
                setTypedPath("");
              }
            }}
          />
          <Select
            aria-label="Access for the new path"
            value={typedMode}
            options={MODES}
            disabled={disabled}
            onChange={(mode) => setTypedMode(mode as PathMode)}
          />
          <button
            type="button"
            disabled={disabled || !typedPath.trim()}
            onClick={() => {
              addGrant(typedPath, typedMode);
              setTypedPath("");
            }}
          >
            <Plus aria-hidden="true" /> Add path
          </button>
        </div>
      </section>

      <section className="environment-section" aria-label="Rules">
        <div className="environment-section-head">
          <span className="environment-label">Rules</span>
          <button
            type="button"
            disabled={disabled}
            onClick={() => setRules([...value.rules, { tool: "run_command", pattern: "", action: "ask" }])}
          >
            <Plus aria-hidden="true" /> Add rule
          </button>
        </div>
        <p className="muted permissions-hint">
          Allow, ask, or deny tool calls. Deny wins over ask, and ask over allow; a rule of an environment overrides a global rule
          just as specific. A command rule matches the start of each command in a line; a path glob without / matches the file name
          anywhere, ** crosses folders.
        </p>
        <datalist id="permission-tools">
          {TOOLS.map((tool) => (
            <option key={tool} value={tool} />
          ))}
        </datalist>
        <ul className="environment-folders permission-list" aria-label="Rules">
          {value.rules.map((rule, index) => (
            <li key={index} className="permission-rule">
              <Select
                aria-label="Action"
                value={rule.action}
                options={ACTIONS}
                disabled={disabled}
                onChange={(action) => updateRule(index, { action: action as RuleAction })}
              />
              <input
                className="field permission-tool"
                aria-label="Tool"
                list="permission-tools"
                spellCheck={false}
                value={rule.tool}
                disabled={disabled}
                onChange={(event) => updateRule(index, { tool: event.target.value })}
              />
              <input
                className="field permission-pattern"
                aria-label="Pattern"
                spellCheck={false}
                placeholder={patternHint(rule.tool)}
                value={rule.pattern}
                disabled={disabled}
                onChange={(event) => updateRule(index, { pattern: event.target.value })}
              />
              <button
                type="button"
                className="ghost icon-only"
                aria-label={`Remove rule ${describeRule(rule)}`}
                title="Remove this rule"
                disabled={disabled}
                onClick={() => setRules(value.rules.filter((_, i) => i !== index))}
              >
                <X />
              </button>
            </li>
          ))}
          {value.rules.length === 0 && (
            <li className="muted">No rules: reads run, and writes, edits, and commands ask.</li>
          )}
        </ul>
      </section>
    </div>
  );
}

/** Whether two sets are the same, for enabling Save. */
export function samePermissions(a: PermissionSet, b: PermissionSet): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}
