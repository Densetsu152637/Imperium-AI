// The Installed view: plugins on this computer, each with its approval, an on/off switch for the
// user or for the selected environment, and Approve, Update, Secrets and Uninstall. Approving and
// updating open the trust view first; nothing is approved from a row.

import { KeyRound, Puzzle, RefreshCw, ShieldCheck, Trash } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import type { InstalledPlugin, PluginsListResult } from "@imperium/protocol";
import { Badge } from "../components/Badge";
import { EmptyState } from "../components/EmptyState";
import { LoadingState } from "../components/LoadingState";
import { Notice } from "../components/Notice";
import { Switch } from "../components/Switch";
import { SecretsForm } from "./SecretsForm";
import { matches, plain } from "./model";
import { kindLabel } from "./trust";

export type Scope = "user" | "environment";

const part = (n: number, noun: string) => (n > 0 ? `${n} ${noun}${n === 1 ? "" : "s"}` : null);
const runsCode = (p: InstalledPlugin) => p.hooks > 0 || p.servers.length > 0;

export function InstalledView({
  result,
  query,
  scope,
  environmentName,
  onScope,
  busy,
  platformReason,
  onToggle,
  onApprove,
  onUpdate,
  onUninstall,
  onSetSecret,
  onDeleteSecret,
  onBrowse,
}: {
  result: PluginsListResult | null;
  query: string;
  scope: Scope;
  /** The selected environment's name, or null when there is none (then there is no scope to choose). */
  environmentName: string | null;
  onScope: (scope: Scope) => void;
  /** The plugin (by name) an action is under way for, or "*". */
  busy: string | null;
  /** Why this platform cannot approve, shown in place of Approve. */
  platformReason: string | null;
  onToggle: (plugin: InstalledPlugin, enabled: boolean) => void;
  onApprove: (plugin: InstalledPlugin) => void;
  onUpdate: (plugin: InstalledPlugin) => void;
  onUninstall: (plugin: InstalledPlugin, deleteSecrets: boolean) => void;
  onSetSecret: (plugin: InstalledPlugin, variable: string, value: string) => Promise<boolean>;
  onDeleteSecret: (plugin: InstalledPlugin, variable: string) => Promise<boolean>;
  onBrowse: () => void;
}) {
  const [confirming, setConfirming] = useState<string | null>(null);
  const [forget, setForget] = useState(true);
  const [secretsOf, setSecretsOf] = useState<string | null>(null);
  const list = useRef<HTMLUListElement>(null);
  // The confirmation opens on Cancel, the safe choice; closing it returns focus to the Uninstall button.
  useEffect(() => {
    if (confirming) list.current?.querySelector<HTMLElement>("[data-cancel]")?.focus();
  }, [confirming]);
  const closeConfirm = () => {
    const name = confirming;
    setConfirming(null);
    if (name) window.setTimeout(() => list.current?.querySelector<HTMLElement>(`[data-uninstall="${CSS.escape(name)}"]`)?.focus());
  };
  if (!result) return <LoadingState />;
  const shown = result.plugins.filter((p) => matches(query, p.name, p.id, p.origin, p.source));
  return (
    <>
      {environmentName && (
        <div className="chips" role="group" aria-label="Turn plugins on or off for">
          <button className="chip" aria-pressed={scope === "user"} onClick={() => onScope("user")}>
            For you
          </button>
          <button className="chip" aria-pressed={scope === "environment"} onClick={() => onScope("environment")}>
            In {plain(environmentName)}
          </button>
        </div>
      )}
      {result.problems.map((problem) => (
        <Notice key={problem} tone="warn">
          {plain(problem)}
        </Notice>
      ))}
      {result.plugins.length === 0 ? (
        <EmptyState
          icon={Puzzle}
          title="No plugins installed"
          body="Plugins add skills, commands, hooks and servers."
          action={
            <button className="primary" onClick={onBrowse}>
              Browse plugins
            </button>
          }
        />
      ) : shown.length === 0 ? (
        <EmptyState title={`No installed plugins match “${query}”`} />
      ) : (
        <ul ref={list} className="connector-rows">
          {shown.map((plugin) => {
            const needsApproval = plugin.withheld && runsCode(plugin);
            return (
              <li key={plugin.folder} className="connector-row" data-enabled={plugin.enabled && !plugin.withheld} data-plugin={plugin.name}>
                <span className="connector-icon small" aria-hidden="true">
                  <Puzzle />
                </span>
                <div className="connector-row-title">
                  <strong>{plain(plugin.name)}</strong>
                  {plugin.withheld && <Badge tone="warn">Withheld</Badge>}
                  {!plugin.enabled && <Badge>Off</Badge>}
                  {runsCode(plugin) &&
                    (plugin.approval ? (
                      <Badge tone="ok" title={kindLabel(plugin.approval)}>
                        {plain(plugin.approval)}
                      </Badge>
                    ) : (
                      <Badge tone="warn">Not approved</Badge>
                    ))}
                  {plugin.missing_secrets.length > 0 && <Badge tone="warn">Needs secrets</Badge>}
                  <span className="connector-row-counts">
                    {[part(plugin.hooks, "hook"), plugin.servers.length > 0 ? `servers: ${plugin.servers.map(plain).join(", ")}` : null].filter(Boolean).join(" · ")}
                  </span>
                </div>
                <div className="connector-row-actions">
                  {needsApproval && plugin.id && !platformReason && (
                    <button className="small" disabled={busy !== null} onClick={() => onApprove(plugin)}>
                      <ShieldCheck aria-hidden="true" /> Approve
                    </button>
                  )}
                  {plugin.id && (
                    <button className="small" disabled={busy !== null} onClick={() => onUpdate(plugin)} aria-label={`Check ${plain(plugin.name)} for an update`}>
                      <RefreshCw aria-hidden="true" /> Update
                    </button>
                  )}
                  {plugin.required_secrets.length > 0 && (
                    <button className="small" aria-expanded={secretsOf === plugin.name} onClick={() => setSecretsOf(secretsOf === plugin.name ? null : plugin.name)}>
                      <KeyRound aria-hidden="true" /> Secrets
                    </button>
                  )}
                  <button className="small danger" disabled={busy !== null} data-uninstall={plugin.name} onClick={() => setConfirming(plugin.name)}>
                    <Trash aria-hidden="true" /> Uninstall
                  </button>
                  <Switch
                    checked={plugin.enabled}
                    disabled={busy !== null}
                    onChange={(enabled) => onToggle(plugin, enabled)}
                    label={`${plain(plugin.name)} on ${scope === "environment" && environmentName ? `in ${plain(environmentName)}` : "for you"}`}
                  />
                </div>
                <div className="connector-row-detail">
                  <span className="connector-row-source">
                    {[plugin.id && plain(plugin.id), plugin.origin && `from ${plain(plugin.origin)}`, plugin.commit && `commit ${plain(plugin.commit).slice(0, 8)}`, !plugin.enabled && `off by ${plain(plugin.decided_by)}`]
                      .filter(Boolean)
                      .join(" · ")}
                  </span>
                  {plugin.reason && <span className="connector-row-source">{plain(plugin.reason)}</span>}
                  {needsApproval && !plugin.id && (
                    <Notice tone="info">This folder did not come from a marketplace, so it cannot be previewed or approved from here yet.</Notice>
                  )}
                  {needsApproval && plugin.id && platformReason && <Notice tone="warn">{plain(platformReason)}</Notice>}
                  {secretsOf === plugin.name && (
                    <SecretsForm
                      plugin={plugin}
                      disabled={busy !== null}
                      onSet={(variable, value) => onSetSecret(plugin, variable, value)}
                      onDelete={(variable) => onDeleteSecret(plugin, variable)}
                    />
                  )}
                  {confirming === plugin.name && (
                    <div className="connector-confirm" role="alertdialog" aria-label={`Uninstall ${plain(plugin.name)}`}>
                      <span>Uninstall {plain(plugin.name)}? You can undo this afterwards.</span>
                      {plugin.required_secrets.length > 0 && (
                        <label>
                          <input type="checkbox" checked={forget} onChange={(event) => setForget(event.target.checked)} /> Also delete its saved secrets
                        </label>
                      )}
                      <button
                        className="danger small"
                        onClick={() => {
                          setConfirming(null);
                          document.querySelector<HTMLElement>("[role=tabpanel]")?.focus();
                          onUninstall(plugin, plugin.required_secrets.length > 0 && forget);
                        }}
                      >
                        Uninstall
                      </button>
                      <button className="small" data-cancel onClick={closeConfirm}>
                        Cancel
                      </button>
                    </div>
                  )}
                </div>
              </li>
            );
          })}
        </ul>
      )}
    </>
  );
}
