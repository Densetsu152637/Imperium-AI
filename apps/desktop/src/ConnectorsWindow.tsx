// The Connectors window: MCP servers, plugins, and skills, as a directory to browse and a list
// of what is installed. Servers are read and written through the daemon's mcp.* methods, which
// keep them in the user's config.toml; secret values go to the system keychain and are never
// shown again. The catalogue under Browse comes from the daemon (catalog/connectors.json, plus
// the user's own connectors.json), not from this file.

import {
  ArrowLeft,
  CircleAlert,
  Check,
  KeyRound,
  LoaderCircle,
  Pencil,
  Plug,
  Plus,
  Puzzle,
  RefreshCw,
  Search,
  Sparkles,
  Trash,
  X,
} from "lucide-react";
import { useCallback, useEffect, useId, useMemo, useState, type FormEvent, type ReactNode } from "react";
import { createPortal } from "react-dom";

import type {
  CatalogField,
  CatalogServer,
  ConfigSetResult,
  McpCatalogResult,
  McpListResult,
  McpRemoveResult,
  McpSaveResult,
  McpServerConfig,
  McpServerInfo,
  McpStatus,
  PluginInfo,
} from "@imperium/protocol";

import { RowMenu } from "./components/RowMenu";
import { Select } from "./components/Select";
import { Switch } from "./components/Switch";
import { ConnectorIcon } from "./connectorIcons";
import type { DaemonClient } from "./daemon/client";

type Tab = "installed" | "browse";

type View =
  | { kind: "list" }
  | { kind: "add"; entry: CatalogServer }
  | { kind: "custom"; editing: McpServerInfo | null };

const message = (error: unknown) => (error instanceof Error ? error.message : String(error));

function matches(query: string, ...texts: (string | null | undefined)[]): boolean {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  const haystack = texts.filter(Boolean).join(" ").toLowerCase();
  return words.every((word) => haystack.includes(word));
}

export function ConnectorsWindow({
  client,
  onClose,
  initialTab = "installed",
}: {
  client: DaemonClient | null;
  onClose: () => void;
  initialTab?: Tab;
}) {
  const [tab, setTab] = useState<Tab>(initialTab);
  const [view, setView] = useState<View>({ kind: "list" });
  const [query, setQuery] = useState("");
  const [category, setCategory] = useState<string | null>(null);
  const [installed, setInstalled] = useState<McpListResult | null>(null);
  const [catalog, setCatalog] = useState<McpCatalogResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [testing, setTesting] = useState<Set<string>>(new Set());
  const [restartNeeded, setRestartNeeded] = useState(false);
  const titleId = useId();

  const refresh = useCallback(async () => {
    if (!client) return;
    try {
      setInstalled(await client.call<McpListResult>("mcp.list"));
    } catch (failure) {
      setError(message(failure));
    }
  }, [client]);

  useEffect(() => {
    if (!client) return;
    void refresh();
    client.call<McpCatalogResult>("mcp.catalog").then(setCatalog, (failure: unknown) => setError(message(failure)));
  }, [client, refresh]);

  // Escape steps back from a form, then closes the window.
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      if (view.kind !== "list") setView({ kind: "list" });
      else onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose, view.kind]);

  const replaceServer = (server: McpServerInfo) =>
    setInstalled((current) =>
      current && {
        ...current,
        servers: [...current.servers.filter((s) => s.name !== server.name), server].sort((a, b) => a.name.localeCompare(b.name)),
      },
    );

  const test = async (name: string) => {
    if (!client) return;
    setTesting((all) => new Set(all).add(name));
    try {
      const status = await client.call<McpStatus>("mcp.test", { name });
      setInstalled(
        (current) =>
          current && { ...current, servers: current.servers.map((s) => (s.name === name ? { ...s, status } : s)) },
      );
    } catch (failure) {
      setError(message(failure));
    } finally {
      setTesting((all) => {
        const next = new Set(all);
        next.delete(name);
        return next;
      });
    }
  };

  const setEnabled = async (name: string, enabled: boolean) => {
    if (!client) return;
    try {
      replaceServer((await client.call<McpSaveResult>("mcp.set_enabled", { name, enabled })).server);
    } catch (failure) {
      setError(message(failure));
    }
  };

  const remove = async (name: string, forgetSecrets: boolean) => {
    if (!client) return;
    try {
      await client.call<McpRemoveResult>("mcp.remove", { name, forget_secrets: forgetSecrets });
      setInstalled((current) => current && { ...current, servers: current.servers.filter((s) => s.name !== name) });
    } catch (failure) {
      setError(message(failure));
    }
  };

  const setPluginEnabled = async (plugin: PluginInfo, enabled: boolean) => {
    if (!client || !installed) return;
    const disabled = installed.plugins.filter((p) => (p.name === plugin.name ? !enabled : !p.enabled)).map((p) => p.name);
    try {
      const result = await client.call<ConfigSetResult>("config.set", {
        key: "extensions.disabled_plugins",
        value: disabled.length > 0 ? disabled : null,
      });
      if (result.restart) setRestartNeeded(true);
      setInstalled(
        (current) =>
          current && { ...current, plugins: current.plugins.map((p) => (p.name === plugin.name ? { ...p, enabled } : p)) },
      );
    } catch (failure) {
      setError(message(failure));
    }
  };

  /** After a save: show it as installed and check that it starts. */
  const saved = (server: McpServerInfo) => {
    replaceServer(server);
    setView({ kind: "list" });
    setTab("installed");
    if (server.config.enabled) void test(server.name);
  };

  const entries = catalog?.entries ?? [];
  const byId = useMemo(() => new Map(entries.map((entry) => [entry.id, entry])), [entries]);
  const added = useMemo(
    () => new Set((installed?.servers ?? []).flatMap((server) => (server.config.catalog ? [server.config.catalog] : []))),
    [installed],
  );

  let body: ReactNode;
  if (!client) {
    body = <p className="connectors-empty muted">Connect to imperium-core to manage connectors.</p>;
  } else if (view.kind === "add") {
    body = (
      <AddForm
        entry={view.entry}
        client={client}
        taken={new Set(installed?.servers.map((s) => s.name))}
        onBack={() => setView({ kind: "list" })}
        onSaved={saved}
      />
    );
  } else if (view.kind === "custom") {
    body = (
      <CustomForm
        editing={view.editing}
        client={client}
        taken={new Set(installed?.servers.map((s) => s.name))}
        onBack={() => setView({ kind: "list" })}
        onSaved={saved}
      />
    );
  } else if (tab === "browse") {
    body = (
      <BrowseView
        entries={entries}
        query={query}
        category={category}
        onCategory={setCategory}
        added={added}
        onAdd={(entry) => setView({ kind: "add", entry })}
        onCustom={() => setView({ kind: "custom", editing: null })}
        loading={!catalog}
        problems={catalog?.problems ?? []}
        overrideFile={catalog?.override_file ?? null}
      />
    );
  } else {
    body = (
      <InstalledView
        installed={installed}
        query={query}
        byId={byId}
        testing={testing}
        onTest={test}
        onEnable={setEnabled}
        onRemove={remove}
        onConfigure={(server) => setView({ kind: "custom", editing: server })}
        onBrowse={() => setTab("browse")}
        onCustom={() => setView({ kind: "custom", editing: null })}
        onPluginEnable={setPluginEnabled}
      />
    );
  }

  const installedCount = installed ? installed.servers.length + installed.plugins.length : null;

  return createPortal(
    <div className="settings-backdrop" onMouseDown={(event) => event.target === event.currentTarget && onClose()}>
      <div className="connectors-window" role="dialog" aria-modal="true" aria-labelledby={titleId}>
        <header className="connectors-header">
          <h2 id={titleId}>
            <Plug aria-hidden="true" /> Connectors
          </h2>
          <div className="tabs" role="tablist" aria-label="Connector views">
            {(["installed", "browse"] as const).map((id) => (
              <button
                key={id}
                role="tab"
                aria-selected={tab === id && view.kind === "list"}
                className={tab === id ? "tab active" : "tab"}
                onClick={() => {
                  setTab(id);
                  setView({ kind: "list" });
                }}
              >
                {id === "installed" ? "Installed" : "Browse"}
                {id === "installed" && installedCount !== null && <span className="tab-count">{installedCount}</span>}
              </button>
            ))}
          </div>
          <label className="search-field">
            <Search aria-hidden="true" />
            <input
              type="search"
              placeholder={tab === "browse" ? "Search the directory" : "Search installed"}
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              aria-label="Search connectors"
              autoFocus
            />
          </label>
          <button className="ghost icon-only" onClick={onClose} aria-label="Close connectors" title="Close">
            <X />
          </button>
        </header>
        {restartNeeded && (
          <div className="settings-notice">Restart imperium-core for plugin changes to apply to new sessions.</div>
        )}
        {error && (
          <p className="error connectors-error" role="alert">
            <CircleAlert aria-hidden="true" /> {error}
            <button className="ghost icon-only" onClick={() => setError(null)} aria-label="Dismiss">
              <X />
            </button>
          </p>
        )}
        <div className="connectors-body">{body}</div>
      </div>
    </div>,
    document.body,
  );
}

// ---- Browse ----

function BrowseView({
  entries,
  query,
  category,
  onCategory,
  added,
  onAdd,
  onCustom,
  loading,
  problems,
  overrideFile,
}: {
  entries: CatalogServer[];
  query: string;
  category: string | null;
  onCategory: (category: string | null) => void;
  added: Set<string>;
  onAdd: (entry: CatalogServer) => void;
  onCustom: () => void;
  loading: boolean;
  problems: string[];
  overrideFile: string | null;
}) {
  const categories = [...new Set(entries.map((entry) => entry.category))].sort();
  const shown = entries.filter(
    (entry) =>
      (!category || entry.category === category) &&
      matches(query, entry.name, entry.description, entry.category, entry.publisher, entry.id),
  );
  if (loading) return <p className="connectors-empty muted">Loading the directory…</p>;
  return (
    <>
      <div className="chips" role="group" aria-label="Categories">
        {[null, ...categories].map((name) => (
          <button key={name ?? "all"} className="chip" aria-pressed={category === name} onClick={() => onCategory(name)}>
            {name ?? "All"}
          </button>
        ))}
      </div>
      {problems.map((problem) => (
        <p key={problem} className="warning">
          {problem}
        </p>
      ))}
      <div className="connector-grid">
        {shown.map((entry) => (
          <article key={entry.id} className="connector-card">
            <div className="connector-card-head">
              <ConnectorIcon icon={entry.icon} />
              <div className="connector-card-title">
                <h3>{entry.name}</h3>
                {entry.publisher && <span className="muted">{entry.publisher}</span>}
              </div>
            </div>
            <p className="connector-card-description">{entry.description}</p>
            <div className="connector-card-foot">
              <span className="tag">{entry.category}</span>
              <span className="tag">{entry.url ? "Remote" : "Local"}</span>
              <span className="spacer" />
              {added.has(entry.id) ? (
                <span className="added" title="Already installed">
                  <Check aria-hidden="true" /> Added
                </span>
              ) : (
                <button className="primary small" onClick={() => onAdd(entry)} aria-label={`Add ${entry.name}`}>
                  <Plus aria-hidden="true" /> Add
                </button>
              )}
            </div>
          </article>
        ))}
        {!query && !category && (
          <article className="connector-card custom">
            <div className="connector-card-head">
              <ConnectorIcon icon="terminal" />
              <div className="connector-card-title">
                <h3>Custom connector</h3>
                <span className="muted">Any MCP server</span>
              </div>
            </div>
            <p className="connector-card-description">Add a server by the command that starts it or the URL it serves on.</p>
            <div className="connector-card-foot">
              <span className="spacer" />
              <button className="small" onClick={onCustom}>
                <Plus aria-hidden="true" /> Add custom
              </button>
            </div>
          </article>
        )}
      </div>
      {shown.length === 0 && <p className="connectors-empty muted">No connectors match “{query}”.</p>}
      {overrideFile && (
        <p className="muted connectors-footnote">
          Add your own entries to the directory in <code>{overrideFile}</code>.
        </p>
      )}
    </>
  );
}

// ---- Installed ----

function relative(at: number | null): string {
  if (!at) return "";
  const seconds = Math.round((Date.now() - at) / 1000);
  if (seconds < 60) return "just now";
  if (seconds < 3600) return `${Math.round(seconds / 60)} min ago`;
  if (seconds < 86400) return `${Math.round(seconds / 3600)} h ago`;
  return new Date(at).toLocaleDateString();
}

function StatusPill({ status, busy }: { status: McpStatus; busy: boolean }) {
  if (busy) {
    return (
      <span className="status-pill" data-state="testing">
        <LoaderCircle className="spin" aria-hidden="true" /> Connecting…
      </span>
    );
  }
  const text = {
    connected: `Connected · ${status.tools.length} ${status.tools.length === 1 ? "tool" : "tools"}`,
    error: "Error",
    untested: "Not tested",
    disabled: "Off",
  }[status.state];
  return (
    <span className="status-pill" data-state={status.state} title={status.checked_at ? `Checked ${relative(status.checked_at)}` : undefined}>
      <span className="status-dot" aria-hidden="true" /> {text}
    </span>
  );
}

function InstalledView({
  installed,
  query,
  byId,
  testing,
  onTest,
  onEnable,
  onRemove,
  onConfigure,
  onBrowse,
  onCustom,
  onPluginEnable,
}: {
  installed: McpListResult | null;
  query: string;
  byId: Map<string, CatalogServer>;
  testing: Set<string>;
  onTest: (name: string) => void;
  onEnable: (name: string, enabled: boolean) => void;
  onRemove: (name: string, forgetSecrets: boolean) => void;
  onConfigure: (server: McpServerInfo) => void;
  onBrowse: () => void;
  onCustom: () => void;
  onPluginEnable: (plugin: PluginInfo, enabled: boolean) => void;
}) {
  const [confirming, setConfirming] = useState<string | null>(null);
  const [forget, setForget] = useState(true);
  if (!installed) return <p className="connectors-empty muted">Loading…</p>;
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
          <div className="connectors-empty">
            <p className="muted">No MCP servers yet. Connectors give agents tools such as GitHub, a database, or a browser.</p>
            <button className="primary" onClick={onBrowse}>
              Browse connectors
            </button>
          </div>
        ) : (
          <ul className="connector-rows">
            {servers.map((server) => {
              const entry = server.config.catalog ? byId.get(server.config.catalog) : undefined;
              const where = server.config.url ?? [server.config.command, ...server.config.args].join(" ");
              const busy = testing.has(server.name);
              return (
                <li key={server.name} className="connector-row" data-enabled={server.config.enabled}>
                  <ConnectorIcon icon={entry?.icon ?? (server.config.url ? "cloud" : "server")} />
                  <div className="connector-row-main">
                    <div className="connector-row-title">
                      <strong>{entry && entry.id !== server.name ? `${server.name} (${entry.name})` : (entry?.name ?? server.name)}</strong>
                      <StatusPill status={server.status} busy={busy} />
                    </div>
                    <code className="connector-row-where" title={where}>
                      {where}
                    </code>
                    {server.status.error && !busy && <p className="error connector-row-error">{server.status.error}</p>}
                    {server.status.missing_secrets.length > 0 && (
                      <p className="warning connector-row-error">
                        <KeyRound aria-hidden="true" /> Missing from the keychain: {server.status.missing_secrets.join(", ")}.
                        Configure to enter {server.status.missing_secrets.length === 1 ? "it" : "them"} again.
                      </p>
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
                  <div className="connector-row-actions">
                    <RowMenu
                      label={`More for ${server.name}`}
                      items={[
                        {
                          label: "Test connection",
                          icon: RefreshCw,
                          disabled: busy || !server.config.enabled,
                          onSelect: () => onTest(server.name),
                        },
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
                </li>
              );
            })}
          </ul>
        )}
        {installed.servers.length > 0 && (
          <p className="muted connectors-footnote">New sessions use these servers; running sessions keep the ones they started with.</p>
        )}
      </section>

      <section className="connectors-section" aria-labelledby="plugins-heading">
        <div className="connectors-section-head">
          <h3 id="plugins-heading">Plugins</h3>
        </div>
        {installed.plugins.length === 0 ? (
          <p className="muted">
            No plugins. Put Claude Code–style plugin folders in your config folder's <code>plugins</code>, or add folders under
            Settings → Extensions.
          </p>
        ) : (
          <ul className="connector-rows">
            {plugins.map((plugin) => (
              <li key={plugin.root} className="connector-row" data-enabled={plugin.enabled}>
                <span className="connector-icon" aria-hidden="true">
                  <Puzzle />
                </span>
                <div className="connector-row-main">
                  <div className="connector-row-title">
                    <strong>{plugin.name}</strong>
                  </div>
                  <span className="muted connector-row-counts">
                    {[
                      count(plugin.skills, "skill"),
                      count(plugin.commands, "command"),
                      count(plugin.hooks, "hook"),
                      plugin.servers.length > 0 ? `servers: ${plugin.servers.join(", ")}` : null,
                    ]
                      .filter(Boolean)
                      .join(" · ") || "empty"}
                  </span>
                  <code className="connector-row-where" title={plugin.root}>
                    {plugin.root}
                  </code>
                </div>
                <div className="connector-row-actions">
                  <Switch checked={plugin.enabled} onChange={(enabled) => onPluginEnable(plugin, enabled)} label={`${plugin.name} enabled`} />
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
          <p className="muted">
            No skills. Add folders with a <code>SKILL.md</code> to your config folder's <code>skills</code>.
          </p>
        ) : (
          <ul className="skill-list">
            {skills.map((skill) => (
              <li key={`${skill.origin}:${skill.name}`}>
                <Sparkles aria-hidden="true" />
                <div>
                  <strong>{skill.name}</strong> <span className="tag">{skill.origin}</span>
                  <p className="muted">{skill.description}</p>
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
            <p key={problem} className="warning">
              {problem}
            </p>
          ))}
        </section>
      )}
    </>
  );
}

const count = (n: number, noun: string) => (n > 0 ? `${n} ${noun}${n === 1 ? "" : "s"}` : null);

// ---- Forms ----

/** Server names the daemon accepts: lowercase letters, digits, '_' and '-'. */
const NAME_PATTERN = /^[a-z0-9][a-z0-9_-]{0,39}$/;

const suggestName = (base: string, taken: Set<string>) => {
  if (!taken.has(base)) return base;
  for (let n = 2; ; n++) if (!taken.has(`${base}-${n}`)) return `${base}-${n}`;
};

function FormShell({
  title,
  icon,
  subtitle,
  onBack,
  onSubmit,
  submitLabel,
  busy,
  error,
  children,
}: {
  title: string;
  icon?: string | null;
  subtitle?: ReactNode;
  onBack: () => void;
  onSubmit: () => void;
  submitLabel: string;
  busy: boolean;
  error: string | null;
  children: ReactNode;
}) {
  return (
    <form
      className="connector-form"
      onSubmit={(event: FormEvent) => {
        event.preventDefault();
        onSubmit();
      }}
    >
      <button type="button" className="ghost back" onClick={onBack}>
        <ArrowLeft aria-hidden="true" /> Back
      </button>
      <div className="connector-form-head">
        <ConnectorIcon icon={icon} size="large" />
        <div>
          <h3>{title}</h3>
          {subtitle && <div className="muted">{subtitle}</div>}
        </div>
      </div>
      {children}
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
      <div className="connector-form-actions">
        <button type="button" onClick={onBack}>
          Cancel
        </button>
        <button type="submit" className="primary" disabled={busy}>
          {busy && <LoaderCircle className="spin" aria-hidden="true" />} {submitLabel}
        </button>
      </div>
    </form>
  );
}

function FormField({
  label,
  description,
  required,
  children,
  htmlFor,
}: {
  label: string;
  description?: ReactNode;
  required?: boolean;
  children: ReactNode;
  htmlFor: string;
}) {
  return (
    <div className="form-field">
      <label htmlFor={htmlFor}>
        {label}
        {required && <span className="required" aria-hidden="true"> *</span>}
      </label>
      {children}
      {description && <span className="form-field-description">{description}</span>}
    </div>
  );
}

function AddForm({
  entry,
  client,
  taken,
  onBack,
  onSaved,
}: {
  entry: CatalogServer;
  client: DaemonClient;
  taken: Set<string>;
  onBack: () => void;
  onSaved: (server: McpServerInfo) => void;
}) {
  const [name, setName] = useState(() => suggestName(entry.id, taken));
  const [values, setValues] = useState<Record<string, string>>(() =>
    Object.fromEntries(entry.fields.map((field) => [field.id, field.default ?? ""])),
  );
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const base = useId();

  const submit = async () => {
    if (!NAME_PATTERN.test(name)) return setError("Use lowercase letters, digits, '_' or '-' for the name.");
    const missing = entry.fields.find((field) => field.required && !values[field.id]?.trim());
    if (missing) return setError(`${missing.label} is required.`);
    setBusy(true);
    setError(null);
    try {
      const result = await client.call<McpSaveResult>("mcp.install", { catalog_id: entry.id, name, values });
      onSaved(result.server);
    } catch (failure) {
      setError(message(failure));
    } finally {
      setBusy(false);
    }
  };

  // What will run, with the form's values filled in (secrets stay hidden).
  const fill = (text: string) =>
    text.replace(/\{\{\s*([\w-]+)\s*\}\}/g, (_, id: string) => {
      const field = entry.fields.find((f) => f.id === id);
      if (!field) return "";
      if (field.kind === "secret") return "••••";
      return values[id]?.trim().split("\n").join(" ") || `<${field.label.toLowerCase()}>`;
    });
  const runs = entry.url ? `Connects to ${fill(entry.url)}` : `Runs ${fill([entry.command, ...entry.args].join(" "))}`;
  return (
    <FormShell
      title={`Add ${entry.name}`}
      icon={entry.icon}
      subtitle={
        <>
          {entry.description}
          {entry.homepage && (
            <>
              {" "}
              <a href={entry.homepage} target="_blank" rel="noreferrer">
                Learn more
              </a>
            </>
          )}
        </>
      }
      onBack={onBack}
      onSubmit={() => void submit()}
      submitLabel="Add connector"
      busy={busy}
      error={error}
    >
      <FormField label="Name" htmlFor={`${base}-name`} required description="Agents see its tools as mcp__name__tool.">
        <input id={`${base}-name`} className="field" value={name} onChange={(event) => setName(event.target.value.toLowerCase())} spellCheck={false} />
      </FormField>
      {entry.fields.map((field) => (
        <CatalogInput key={field.id} id={`${base}-${field.id}`} field={field} value={values[field.id] ?? ""} onChange={(value) => setValues((all) => ({ ...all, [field.id]: value }))} />
      ))}
      <p className="muted connector-runs">
        <code>{runs}</code>
      </p>
    </FormShell>
  );
}

function CatalogInput({ id, field, value, onChange }: { id: string; field: CatalogField; value: string; onChange: (value: string) => void }) {
  const description =
    field.kind === "secret" ? (
      <>
        <KeyRound aria-hidden="true" /> {field.description ? `${field.description} ` : ""}Saved to your system keychain; it is not
        shown again.
      </>
    ) : field.kind === "path_list" ? (
      `${field.description ? `${field.description} ` : ""}One per line.`
    ) : (
      field.description
    );
  return (
    <FormField label={field.label} htmlFor={id} required={field.required} description={description}>
      {field.kind === "path_list" ? (
        <textarea id={id} className="field" rows={3} value={value} placeholder={field.placeholder ?? undefined} onChange={(event) => onChange(event.target.value)} spellCheck={false} />
      ) : (
        <input
          id={id}
          className="field"
          type={field.kind === "secret" ? "password" : field.kind === "url" ? "url" : "text"}
          autoComplete={field.kind === "secret" ? "new-password" : "off"}
          value={value}
          placeholder={field.placeholder ?? undefined}
          onChange={(event) => onChange(event.target.value)}
          spellCheck={false}
        />
      )}
    </FormField>
  );
}

/** A secret environment variable being edited: an existing secret is kept unless a value is typed. */
type SecretRow = { key: string; value: string; existing: string | null };

const lines = (text: string) =>
  text
    .split("\n")
    .map((line) => line.trim())
    .filter(Boolean);

/** `KEY=value` (or `Name: value` for headers) lines to a map. */
function pairs(text: string, separator: "=" | ":"): Record<string, string> {
  return Object.fromEntries(
    lines(text).flatMap((line) => {
      const at = line.indexOf(separator);
      return at > 0 ? [[line.slice(0, at).trim(), line.slice(at + 1).trim()]] : [];
    }),
  );
}

const unpairs = (map: Record<string, string>, separator: string) =>
  Object.entries(map)
    .map(([key, value]) => `${key}${separator}${value}`)
    .join("\n");

/** A keychain name for one of a server's secrets, within the 64-character limit. */
const secretName = (server: string, key: string) =>
  `mcp.${server}.${key.toLowerCase().replace(/[^a-z0-9.-]+/g, "-").replace(/^-+|-+$/g, "") || "secret"}`.slice(0, 64);

function CustomForm({
  editing,
  client,
  taken,
  onBack,
  onSaved,
}: {
  editing: McpServerInfo | null;
  client: DaemonClient;
  taken: Set<string>;
  onBack: () => void;
  onSaved: (server: McpServerInfo) => void;
}) {
  const config = editing?.config;
  const [name, setName] = useState(editing?.name ?? "");
  const [transport, setTransport] = useState<"stdio" | "http">(config?.url ? "http" : "stdio");
  const [command, setCommand] = useState(config?.command ?? "");
  const [args, setArgs] = useState(config?.args.join("\n") ?? "");
  const [env, setEnv] = useState(config ? unpairs(config.env, "=") : "");
  const [secretRows, setSecretRows] = useState<SecretRow[]>(
    Object.entries(config?.secret_env ?? {}).map(([key, existing]) => ({ key, value: "", existing })),
  );
  const [url, setUrl] = useState(config?.url ?? "");
  const [headers, setHeaders] = useState(config ? unpairs(config.headers, ": ") : "");
  const [bearer, setBearer] = useState("");
  const [clearBearer, setClearBearer] = useState(false);
  const [allow, setAllow] = useState(config?.allow.join("\n") ?? "");
  const [timeout, setTimeoutSeconds] = useState(config?.timeout_s?.toString() ?? "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const base = useId();

  const submit = async () => {
    if (!NAME_PATTERN.test(name)) return setError("Use lowercase letters, digits, '_' or '-' for the name.");
    if (!editing && taken.has(name)) return setError(`A connector named ${name} already exists.`);
    if (transport === "stdio" && !command.trim()) return setError("Enter the command that starts the server.");
    if (transport === "http" && !url.trim()) return setError("Enter the server's URL.");
    const secrets: Record<string, string> = {};
    const secretEnv: Record<string, string> = {};
    if (transport === "stdio") {
      for (const row of secretRows) {
        const key = row.key.trim();
        if (!key) continue;
        if (row.value) {
          const secret = row.existing ?? secretName(name, key);
          secrets[secret] = row.value;
          secretEnv[key] = secret;
        } else if (row.existing) {
          secretEnv[key] = row.existing;
        } else {
          return setError(`Enter a value for ${key}.`);
        }
      }
    }
    let bearerSecret: string | null = null;
    if (transport === "http" && !clearBearer) {
      if (bearer) {
        bearerSecret = config?.bearer_secret ?? secretName(name, "token");
        secrets[bearerSecret] = bearer;
      } else {
        bearerSecret = config?.bearer_secret ?? null;
      }
    }
    const seconds = timeout.trim() ? Number(timeout) : null;
    if (seconds !== null && !(Number.isInteger(seconds) && seconds > 0)) return setError("The timeout is a whole number of seconds.");
    const next: McpServerConfig = {
      command: transport === "stdio" ? command.trim() : null,
      args: transport === "stdio" ? lines(args) : [],
      env: transport === "stdio" ? pairs(env, "=") : {},
      secret_env: secretEnv,
      url: transport === "http" ? url.trim() : null,
      headers: transport === "http" ? pairs(headers, ":") : {},
      bearer_secret: bearerSecret,
      allow: lines(allow),
      enabled: config?.enabled ?? true,
      timeout_s: seconds,
      catalog: config?.catalog ?? null,
    };
    setBusy(true);
    setError(null);
    try {
      const result = await client.call<McpSaveResult>("mcp.save", { name, config: next, secrets, create: !editing });
      onSaved(result.server);
    } catch (failure) {
      setError(message(failure));
    } finally {
      setBusy(false);
    }
  };

  return (
    <FormShell
      title={editing ? `Configure ${editing.name}` : "Custom connector"}
      icon={transport === "http" ? "cloud" : "terminal"}
      subtitle="Saved to [mcp.servers] in your config file. Secrets go to your system keychain."
      onBack={onBack}
      onSubmit={() => void submit()}
      submitLabel={editing ? "Save" : "Add connector"}
      busy={busy}
      error={error}
    >
      <div className="form-grid">
        <FormField label="Name" htmlFor={`${base}-name`} required>
          <input
            id={`${base}-name`}
            className="field"
            value={name}
            disabled={Boolean(editing)}
            onChange={(event) => setName(event.target.value.toLowerCase())}
            placeholder="my-server"
            spellCheck={false}
          />
        </FormField>
        <FormField label="Type" htmlFor={`${base}-type`}>
          <Select
            id={`${base}-type`}
            value={transport}
            onChange={(value) => setTransport(value as "stdio" | "http")}
            options={[
              { value: "stdio", label: "Local command", description: "A process speaking MCP on stdin/stdout" },
              { value: "http", label: "Remote URL", description: "A streamable-HTTP MCP endpoint" },
            ]}
          />
        </FormField>
      </div>
      {transport === "stdio" ? (
        <>
          <FormField label="Command" htmlFor={`${base}-command`} required>
            <input id={`${base}-command`} className="field" value={command} onChange={(event) => setCommand(event.target.value)} placeholder="npx" spellCheck={false} />
          </FormField>
          <FormField label="Arguments" htmlFor={`${base}-args`} description="One per line.">
            <textarea id={`${base}-args`} className="field" rows={3} value={args} onChange={(event) => setArgs(event.target.value)} placeholder={"-y\n@scope/server"} spellCheck={false} />
          </FormField>
          <FormField label="Environment" htmlFor={`${base}-env`} description="KEY=value, one per line. Put tokens under secret variables instead.">
            <textarea id={`${base}-env`} className="field" rows={2} value={env} onChange={(event) => setEnv(event.target.value)} spellCheck={false} />
          </FormField>
          <div className="form-field">
            <span className="form-field-label">Secret variables</span>
            {secretRows.map((row, index) => (
              <div key={index} className="secret-row">
                <input
                  className="field"
                  aria-label="Variable name"
                  placeholder="API_TOKEN"
                  value={row.key}
                  disabled={row.existing !== null}
                  onChange={(event) => setSecretRows((all) => all.map((r, i) => (i === index ? { ...r, key: event.target.value } : r)))}
                  spellCheck={false}
                />
                <input
                  className="field"
                  type="password"
                  autoComplete="new-password"
                  aria-label={`Value of ${row.key || "the variable"}`}
                  placeholder={row.existing ? "Saved · type to replace" : "value"}
                  value={row.value}
                  onChange={(event) => setSecretRows((all) => all.map((r, i) => (i === index ? { ...r, value: event.target.value } : r)))}
                />
                <button
                  type="button"
                  className="ghost icon-only"
                  aria-label={`Remove ${row.key || "variable"}`}
                  onClick={() => setSecretRows((all) => all.filter((_, i) => i !== index))}
                >
                  <X />
                </button>
              </div>
            ))}
            <button type="button" className="small add-secret" onClick={() => setSecretRows((all) => [...all, { key: "", value: "", existing: null }])}>
              <Plus aria-hidden="true" /> Add secret variable
            </button>
            <span className="form-field-description">
              <KeyRound aria-hidden="true" /> Values are saved to your system keychain and never shown again.
            </span>
          </div>
        </>
      ) : (
        <>
          <FormField label="URL" htmlFor={`${base}-url`} required>
            <input id={`${base}-url`} className="field" type="url" value={url} onChange={(event) => setUrl(event.target.value)} placeholder="https://example.com/mcp" spellCheck={false} />
          </FormField>
          <FormField
            label="Bearer token"
            htmlFor={`${base}-bearer`}
            description={
              <>
                <KeyRound aria-hidden="true" /> Sent as Authorization: Bearer. Saved to your system keychain.
              </>
            }
          >
            <div className="secret-row">
              <input
                id={`${base}-bearer`}
                className="field"
                type="password"
                autoComplete="new-password"
                value={bearer}
                disabled={clearBearer}
                placeholder={config?.bearer_secret ? "Saved · type to replace" : "optional"}
                onChange={(event) => setBearer(event.target.value)}
              />
              {config?.bearer_secret && (
                <label className="inline-check">
                  <input type="checkbox" checked={clearBearer} onChange={(event) => setClearBearer(event.target.checked)} /> Don't send
                </label>
              )}
            </div>
          </FormField>
          <FormField label="Headers" htmlFor={`${base}-headers`} description="Name: value, one per line.">
            <textarea id={`${base}-headers`} className="field" rows={2} value={headers} onChange={(event) => setHeaders(event.target.value)} spellCheck={false} />
          </FormField>
        </>
      )}
      <div className="form-grid">
        <FormField label="Tools that run without asking" htmlFor={`${base}-allow`} description="Tool names, one per line, or * for all.">
          <textarea id={`${base}-allow`} className="field" rows={2} value={allow} onChange={(event) => setAllow(event.target.value)} spellCheck={false} />
        </FormField>
        <FormField label="Timeout (seconds)" htmlFor={`${base}-timeout`} description="Per call; 60 when empty.">
          <input id={`${base}-timeout`} className="field" type="number" min={1} value={timeout} onChange={(event) => setTimeoutSeconds(event.target.value)} />
        </FormField>
      </div>
    </FormShell>
  );
}
