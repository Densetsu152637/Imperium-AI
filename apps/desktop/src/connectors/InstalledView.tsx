// The Installed view: what is installed, its status, and the actions on it. Each row is one
// `--row-h` tall line (icon, name, status, actions) with its detail lines under the name, so the
// list keeps the rhythm of the sidebar's rows.

import { CircleAlert, CircleCheck, KeyRound, LogIn, LogOut, Pencil, Plug, Plus, Puzzle, RefreshCw, Sparkles, Trash, X } from "lucide-react";
import { useState } from "react";
import type { CatalogServer, McpListResult, McpServerInfo, McpStatus, PluginInfo } from "@imperium/protocol";
import { Badge } from "../components/Badge";
import { EmptyState } from "../components/EmptyState";
import { IconButton } from "../components/IconButton";
import { LoadingState, Spinner } from "../components/LoadingState";
import { Menu } from "../components/Menu";
import { Notice } from "../components/Notice";
import { Switch } from "../components/Switch";
import { installedByAgent, sourceLabel } from "../settings/configChanges";
import { ConnectorIcon } from "./connectorIcons";
import { openExternal } from "../shared/openExternal";
import { matches, count } from "./common";

function relative(at: number | null): string {
  if (!at) return "";
  const seconds = Math.round((Date.now() - at) / 1000);
  if (seconds < 60) return "just now";
  if (seconds < 3600) return `${Math.round(seconds / 60)} min ago`;
  if (seconds < 86400) return `${Math.round(seconds / 3600)} h ago`;
  return new Date(at).toLocaleDateString();
}

function StatusPill({ status, busy }: { status: McpStatus; busy: boolean }) {
  if (busy || status.sign_in === "signing_in") {
    return (
      <Badge tone="accent">
        <Spinner /> {busy ? "Connecting…" : "Waiting for sign-in…"}
      </Badge>
    );
  }
  const title = status.checked_at ? `Checked ${relative(status.checked_at)}` : undefined;
  if (status.state !== "disabled" && status.sign_in === "needs_sign_in") {
    return (
      <Badge tone="warn" icon={LogIn} title={title}>
        Needs sign-in
      </Badge>
    );
  }
  switch (status.state) {
    case "connected":
      return (
        <Badge tone="ok" icon={CircleCheck} title={title}>
          Connected · {status.tools.length} {status.tools.length === 1 ? "tool" : "tools"}
        </Badge>
      );
    case "error":
      return (
        <Badge tone="danger" icon={CircleAlert} title={title}>
          Error
        </Badge>
      );
    case "disabled":
      return <Badge title={title}>Off</Badge>;
    default:
      return <Badge title={title}>{status.sign_in === "signed_in" ? "Signed in" : "Not tested"}</Badge>;
  }
}

/** Whether the server carries its own `Authorization` header, which leaves no room for a sign-in. */
function hasAuthorizationHeader(server: McpServerInfo): boolean {
  return Object.keys(server.config.headers).some((name) => name.toLowerCase() === "authorization");
}

export function InstalledView({
  installed,
  query,
  byId,
  testing,
  onTest,
  onSignIn,
  onSignOut,
  signInAddress,
  onDismissSignIn,
  onEnable,
  onRemove,
  onConfigure,
  onBrowse,
  onCustom,
  onPluginEnable,
  onManagePlugins,
}: {
  installed: McpListResult | null;
  query: string;
  byId: Map<string, CatalogServer>;
  testing: Set<string>;
  onTest: (name: string) => void;
  onSignIn: (name: string) => void;
  onSignOut: (name: string) => void;
  signInAddress: { name: string; url: string } | null;
  onDismissSignIn: () => void;
  onEnable: (name: string, enabled: boolean) => void;
  onRemove: (name: string, forgetSecrets: boolean) => void;
  onConfigure: (server: McpServerInfo) => void;
  onBrowse: () => void;
  onCustom: () => void;
  onPluginEnable: (plugin: PluginInfo, enabled: boolean) => void;
  onManagePlugins?: () => void;
}) {
  const [confirming, setConfirming] = useState<string | null>(null);
  const [forget, setForget] = useState(true);
  if (!installed) return <LoadingState />;
  const servers = installed.servers.filter((s) =>
    matches(query, s.name, s.config.command, s.config.url, s.config.catalog, byId.get(s.config.catalog ?? "")?.name),
  );
  const plugins = installed.plugins.filter((p) => matches(query, p.name, p.root));
  const skills = installed.skills.filter((s) => matches(query, s.name, s.description, s.origin));

  return (
    <>
      <section className="connectors-section" aria-labelledby="mcp-servers-heading">
        <div className="connectors-section-head">
          <h3 id="mcp-servers-heading">MCP servers</h3>
          <button className="small" onClick={onCustom}>
            <Plus aria-hidden="true" /> Add custom
          </button>
        </div>
        {installed.servers.length === 0 ? (
          <EmptyState
            icon={Plug}
            title="No MCP servers yet"
            body="Connectors give agents tools such as GitHub, a database, or a browser."
            action={
              <button className="primary" onClick={onBrowse}>
                Browse connectors
              </button>
            }
          />
        ) : (
          <ul className="connector-rows">
            {servers.map((server) => {
              const entry = server.config.catalog ? byId.get(server.config.catalog) : undefined;
              const where = server.config.url ?? [server.config.command, ...server.config.args].join(" ");
              const busy = testing.has(server.name);
              return (
                <li key={server.name} className="connector-row" data-enabled={server.config.enabled}>
                  <ConnectorIcon icon={entry?.icon ?? (server.config.url ? "cloud" : "server")} size="small" />
                  <div className="connector-row-title">
                    <strong>{entry && entry.id !== server.name ? `${server.name} (${entry.name})` : (entry?.name ?? server.name)}</strong>
                    <StatusPill status={server.status} busy={busy} />
                  </div>
                  <div className="connector-row-actions">
                    {server.status.sign_in === "needs_sign_in" && server.config.enabled && (
                      <button className="small primary" onClick={() => onSignIn(server.name)}>
                        <LogIn aria-hidden="true" /> Sign in
                      </button>
                    )}
                    <Menu
                      label={`More for ${server.name}`}
                      items={[
                        {
                          label: "Test connection",
                          icon: RefreshCw,
                          disabled: busy || !server.config.enabled,
                          onSelect: () => onTest(server.name),
                        },
                        ...(server.config.url && !server.config.bearer_secret && !hasAuthorizationHeader(server)
                          ? [
                              server.status.sign_in === "signed_in" || server.status.sign_in === "signing_in"
                                ? {
                                    label: server.status.sign_in === "signing_in" ? "Cancel sign-in" : "Sign out",
                                    icon: LogOut,
                                    onSelect: () => onSignOut(server.name),
                                  }
                                : {
                                    label: "Sign in",
                                    icon: LogIn,
                                    disabled: !server.config.enabled,
                                    onSelect: () => onSignIn(server.name),
                                  },
                            ]
                          : []),
                        { label: "Configure", icon: Pencil, onSelect: () => onConfigure(server) },
                        {
                          label: "Remove",
                          icon: Trash,
                          danger: true,
                          onSelect: () => {
                            setForget(true);
                            setConfirming(server.name);
                          },
                        },
                      ]}
                    />
                    <Switch
                      checked={server.config.enabled}
                      onChange={(enabled) => onEnable(server.name, enabled)}
                      label={`${server.name} enabled`}
                    />
                  </div>
                  <div className="connector-row-detail">
                    <code className="connector-row-where" title={where}>
                      {where}
                    </code>
                    {server.status.error && !busy && (
                      <Notice tone="danger" className="connector-row-notice">
                        {server.status.error}
                      </Notice>
                    )}
                    {server.status.sign_in === "signing_in" && signInAddress?.name === server.name && (
                      <Notice
                        tone="info"
                        className="connector-row-notice"
                        action={<IconButton icon={X} label="Hide" size="sm" onClick={onDismissSignIn} />}
                      >
                        <span>Finish signing in in your browser. If it did not open, use this address:</span>
                        <code className="connector-row-where" title={signInAddress.url}>
                          {signInAddress.url}
                        </code>
                        <div className="connector-notice-buttons">
                          <button className="small" onClick={() => void openExternal(signInAddress.url)}>
                            Open again
                          </button>
                          <button className="small" onClick={() => void navigator.clipboard?.writeText(signInAddress.url)}>
                            Copy address
                          </button>
                          <button className="small" onClick={() => onSignOut(server.name)}>
                            Cancel
                          </button>
                        </div>
                      </Notice>
                    )}
                    {server.status.missing_secrets.length > 0 && (
                      <Notice tone="warn" className="connector-row-notice">
                        <KeyRound aria-hidden="true" /> Missing from the keychain: {server.status.missing_secrets.join(", ")}.
                        Configure to enter {server.status.missing_secrets.length === 1 ? "it" : "them"} again.
                      </Notice>
                    )}
                    {server.status.state === "connected" && server.status.tools.length > 0 && (
                      <details className="connector-tools">
                        <summary>Tools</summary>
                        <ul>
                          {server.status.tools.map((tool) => (
                            <li key={tool}>
                              <code>{tool}</code>
                            </li>
                          ))}
                        </ul>
                      </details>
                    )}
                    {confirming === server.name && (
                      <div className="connector-confirm" role="alertdialog" aria-label={`Remove ${server.name}`}>
                        <span>Remove {server.name} from your config?</span>
                        {(server.config.bearer_secret || Object.keys(server.config.secret_env).length > 0) && (
                          <label>
                            <input type="checkbox" checked={forget} onChange={(event) => setForget(event.target.checked)} /> Also
                            delete its saved secrets
                          </label>
                        )}
                        <button
                          className="danger small"
                          onClick={() => {
                            setConfirming(null);
                            onRemove(server.name, forget);
                          }}
                        >
                          Remove
                        </button>
                        <button className="small" onClick={() => setConfirming(null)}>
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
        {installed.servers.length > 0 && (
          <p className="connectors-footnote">New sessions use these servers; running sessions keep the ones they started with.</p>
        )}
      </section>

      <section className="connectors-section" aria-labelledby="plugins-heading">
        <div className="connectors-section-head">
          <h3 id="plugins-heading">Plugins</h3>
          {onManagePlugins && (
            <button className="small" onClick={onManagePlugins}>
              <Puzzle aria-hidden="true" /> Manage plugins
            </button>
          )}
        </div>
        {installed.plugins.length === 0 ? (
          <p className="connectors-note">
            No plugins. Put Claude Code–style plugin folders in your config folder's <code>plugins</code>, or add folders under
            Settings → Extensions.
          </p>
        ) : (
          <ul className="connector-rows">
            {plugins.map((plugin) => (
              <li key={plugin.root} className="connector-row" data-enabled={plugin.enabled}>
                <span className="connector-icon small" aria-hidden="true">
                  <Puzzle />
                </span>
                <div className="connector-row-title">
                  <strong>{plugin.name}</strong>
                  <span className="connector-row-counts">
                    {[
                      count(plugin.skills, "skill"),
                      count(plugin.commands, "command"),
                      count(plugin.hooks, "hook"),
                      plugin.servers.length > 0 ? `servers: ${plugin.servers.join(", ")}` : null,
                    ]
                      .filter(Boolean)
                      .join(" · ") || "empty"}
                  </span>
                </div>
                <div className="connector-row-actions">
                  <Switch checked={plugin.enabled} onChange={(enabled) => onPluginEnable(plugin, enabled)} label={`${plugin.name} enabled`} />
                </div>
                <div className="connector-row-detail">
                  <code className="connector-row-where" title={plugin.root}>
                    {plugin.root}
                  </code>
                  {plugin.installed_from && (
                    <span className="connector-row-source" title={plugin.installed_from.location}>
                      {installedByAgent(plugin) ? "Installed by an agent from " : "Installed from "}
                      {sourceLabel(plugin.installed_from)}
                    </span>
                  )}
                </div>
              </li>
            ))}
          </ul>
        )}
      </section>

      <section className="connectors-section" aria-labelledby="skills-heading">
        <div className="connectors-section-head">
          <h3 id="skills-heading">Skills</h3>
        </div>
        {installed.skills.length === 0 ? (
          <p className="connectors-note">
            No skills. Add folders with a <code>SKILL.md</code> to your config folder's <code>skills</code>.
          </p>
        ) : (
          <ul className="skill-list">
            {skills.map((skill) => (
              <li key={`${skill.origin}:${skill.name}`}>
                <Sparkles aria-hidden="true" />
                <div>
                  <strong>{skill.name}</strong> <Badge>{skill.origin}</Badge>
                  <p className="skill-description">{skill.description}</p>
                  {skill.installed_from && (
                    <p className="connector-row-source" title={skill.installed_from.location}>
                      {installedByAgent(skill) ? "Installed by an agent from " : "Installed from "}
                      {sourceLabel(skill.installed_from)}
                    </p>
                  )}
                </div>
              </li>
            ))}
          </ul>
        )}
      </section>

      {installed.problems.length > 0 && (
        <section className="connectors-section">
          <h3>Problems</h3>
          {installed.problems.map((problem) => (
            <Notice key={problem} tone="warn" className="connectors-notice">
              {problem}
            </Notice>
          ))}
        </section>
      )}
    </>
  );
}
