// The trust view: everything a plugin would run, shown before it is installed, updated or approved
// (docs/plans/plugin-marketplace.md, "The trust prompt shows exactly what runs"). Hooks and servers
// come first, each with its full command line and the names of its variables and headers; then
// where it came from, what else it holds and its content hash. Everything is plain text from the
// daemon's preview. Approving goes through the app's own command and native dialog (approve.ts);
// this view only asks.

import { ArrowLeft, ShieldAlert } from "lucide-react";
import { useEffect, useRef } from "react";
import type { PluginPreview } from "@imperium/protocol";
import { Notice } from "../components/Notice";
import { Spinner } from "../components/LoadingState";
import { plain } from "./model";
import { isUpToDate, needsApproval, runLines, type ApproveAction, type RunDiff } from "./trust";

function Lines({ title, lines }: { title: string; lines: string[] }) {
  if (lines.length === 0) return null;
  return (
    <section className="plugin-detail-group" aria-label={title}>
      <h4>
        {title} ({lines.length})
      </h4>
      <ul className="trust-lines">
        {lines.map((line, index) => (
          <li key={index}>
            <code>{line}</code>
          </li>
        ))}
      </ul>
    </section>
  );
}

function Diff({ diff }: { diff: RunDiff }) {
  if (diff.kind === "unknown")
    return (
      <Notice tone="warn" title="What changed could not be worked out">
        The installed version could not be read for a comparison, so read the whole list below as new.
      </Notice>
    );
  if (diff.kind === "unchanged")
    // The run digest covers command lines, URLs and the names of variables and headers, not the
    // scripts they run or the values of a server's variables, so a changed file is not calm news.
    return diff.otherFilesChanged ? (
      <Notice tone="warn" title="Hooks and servers read the same, but other files changed">
        The command lines, URLs and variable names are the same, but other files changed. That can include the scripts these hooks run and the values
        of the servers' variables and headers.
      </Notice>
    ) : (
      <Notice tone="info" title="Hooks and servers are unchanged">
        Nothing else changed either.
      </Notice>
    );
  if (diff.added.length === 0 && diff.removed.length === 0)
    return (
      <Notice tone="warn" title="What runs is the same, in a different order">
        The same hooks and servers are listed, but their order changed. Other files may have changed too.
      </Notice>
    );
  return (
    <Notice tone="warn" title="What this update changes in what runs">
      {diff.added.length > 0 && (
        <>
          <p className="trust-diff-title">Added or changed</p>
          <ul className="trust-lines">
            {diff.added.map((line) => (
              <li key={line}>
                <code>+ {line}</code>
              </li>
            ))}
          </ul>
        </>
      )}
      {diff.removed.length > 0 && (
        <>
          <p className="trust-diff-title">Removed or replaced</p>
          <ul className="trust-lines">
            {diff.removed.map((line) => (
              <li key={line}>
                <code>- {line}</code>
              </li>
            ))}
          </ul>
        </>
      )}
      <p>Other files may have changed too.</p>
    </Notice>
  );
}

export function TrustView({
  preview,
  action,
  diff,
  blocked,
  busy,
  onApprove,
  onWithoutApproval,
  onUpdate,
  onBack,
}: {
  preview: PluginPreview;
  action: ApproveAction;
  /** For an update: how it changes what runs. */
  diff: RunDiff | null;
  /** Why this platform cannot approve, in place of the Approve button. */
  blocked: string | null;
  busy: boolean;
  onApprove: () => void;
  /** Install or update and leave the plugin withheld. */
  onWithoutApproval: () => void;
  /** From *approve*, when the installed copy is not what the marketplace offers now. */
  onUpdate: () => void;
  onBack: () => void;
}) {
  const back = useRef<HTMLButtonElement>(null);
  useEffect(() => back.current?.focus(), []);
  const { hooks, servers } = runLines(preview);
  const approvable = needsApproval(preview);
  const upToDate = action === "update" && isUpToDate(preview);
  const stale = action === "approve" && !isUpToDate(preview);
  const facts: [string, string | null][] = [
    ["Plugin", plain(preview.id)],
    ["Version", plain(preview.version) || null],
    ["Marketplace", `${plain(preview.marketplace_source)}${preview.marketplace_commit ? ` at ${plain(preview.marketplace_commit)}` : ""}`],
    ["Plugin source", `${plain(preview.source_kind)}: ${plain(preview.source)}${preview.commit ? ` at ${plain(preview.commit)}` : ""}`],
    ["Content hash (SHA-256)", plain(preview.sha256)],
  ];
  const claims: [string, string | null][] = [
    ["Author", plain(preview.author) || null],
    ["Homepage", plain(preview.homepage) || null],
    ["License", plain(preview.license) || null],
  ];
  const title = { install: "Install", update: "Update", approve: "Approve" }[action];
  return (
    <div className="connector-form plugin-detail trust-view">
      <button ref={back} type="button" className="ghost back" onClick={onBack}>
        <ArrowLeft aria-hidden="true" /> Back
      </button>
      <div className="connector-form-head">
        <span className="connector-icon large" aria-hidden="true">
          <ShieldAlert />
        </span>
        <div>
          <h3>
            {title} {plain(preview.name)}
          </h3>
          <div className="connector-form-subtitle">{plain(preview.id)}</div>
        </div>
      </div>
      {approvable && (
        <Notice tone="warn" title="Approving lets this plugin run what is listed below">
          It runs on your computer with your permissions, outside Imperium's tool sandbox. This list shows what the plugin declares. Any script it
          runs can still fetch and run more code.
        </Notice>
      )}
      {preview.source_kind === "folder" && (
        <Notice tone="info" title="This folder did not come from a marketplace">
          The list below is read from the folder as it is now. Approval is for exactly these files: if any file changes afterwards, the plugin is
          withheld again until it is approved again.
        </Notice>
      )}
      {diff && <Diff diff={diff} />}
      {preview.launcher && (
        <Notice tone="warn" title="Starts a launcher">
          Something below (npx, uvx, an unpinned image or a version of latest) downloads and runs more code when it starts. That code is not covered by the hash.
        </Notice>
      )}
      <Lines title="Hooks" lines={hooks} />
      <Lines title="MCP servers" lines={servers} />
      {!approvable && <p className="connectors-note">This plugin has no hooks or servers, so there is nothing to approve. It adds only skills, commands or agents.</p>}
      <dl className="plugin-facts">
        {facts.map(([label, value]) =>
          value ? (
            <div key={label}>
              <dt>{label}</dt>
              <dd>{value}</dd>
            </div>
          ) : null,
        )}
      </dl>
      {claims.some(([, value]) => value) && (
        <dl className="plugin-facts" aria-label="Claims by the plugin, not verified">
          {claims.map(([label, value]) =>
            value ? (
              <div key={label}>
                <dt>{label} (the plugin's own claim, unverified)</dt>
                <dd>{value}</dd>
              </div>
            ) : null,
          )}
        </dl>
      )}
      <Lines title="Skills" lines={preview.skills.map(plain)} />
      <Lines title="Commands" lines={preview.commands.map((c) => `${plain(c)}${preview.commands_with_shell.includes(c) ? "  (has shell lines or allowed-tools; Imperium does not run these)" : ""}`)} />
      <Lines title="Agents" lines={preview.agents.map(plain)} />
      {preview.notes.map((note) => (
        <Notice key={note} tone="info">
          {plain(note)}
        </Notice>
      ))}
      {upToDate && <Notice tone="ok">Already up to date. There is nothing to update.</Notice>}
      {stale && (
        <Notice tone="warn" title="The installed copy differs from the marketplace">
          Approval is for what the marketplace offers now. Update first, then approve what that update brings.
        </Notice>
      )}
      <div className="trust-actions">
        {!upToDate && action !== "approve" && approvable && !blocked && (
          <button className="primary" disabled={busy} onClick={onApprove}>
            {busy && <Spinner />} {action === "install" ? "Approve and install" : "Approve update"}
          </button>
        )}
        {action === "approve" && approvable && !stale && !blocked && (
          <button className="primary" disabled={busy} onClick={onApprove}>
            {busy && <Spinner />} Approve
          </button>
        )}
        {stale && (
          <button className="primary" disabled={busy} onClick={onUpdate}>
            Review the update
          </button>
        )}
        {!upToDate && action === "install" && (
          <button disabled={busy} onClick={onWithoutApproval}>
            {approvable ? "Install without approving" : "Install"}
          </button>
        )}
        {!upToDate && action === "update" && (
          <button disabled={busy} onClick={onWithoutApproval}>
            {approvable ? "Update without approving" : "Update"}
          </button>
        )}
        <button disabled={busy} onClick={onBack}>
          Cancel
        </button>
      </div>
      {blocked && approvable && !upToDate && (
        <Notice tone="warn" title="Approval is not available here">
          {plain(blocked)}
        </Notice>
      )}
      {approvable && !blocked && action !== "approve" && !upToDate && (
        <p className="connectors-note">Installing without approving leaves the plugin withheld: it does not load until it is approved.</p>
      )}
    </div>
  );
}
