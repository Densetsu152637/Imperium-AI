// The Connectors window: MCP servers, plugins, and skills, as a directory to browse and a list
// of what is installed. Servers are read and written through the daemon's mcp.* methods, which
// keep them in the user's config.toml; secret values go to the system keychain and are never
// shown again. The catalogue under Browse comes from the daemon (catalog/connectors.json, plus
// the user's own connectors.json), not from this file.

import { Plug, Search, X } from "lucide-react";
import { useCallback, useEffect, useId, useMemo, useRef, useState, type ReactNode } from "react";
import type { CatalogServer, ConfigSetResult, McpCatalogResult, McpListResult, McpLoginResult, McpLogoutResult, McpRemoveResult, McpSaveResult, McpServerInfo, McpStatus, PluginInfo } from "@imperium/protocol";
import { Dialog } from "../components/Dialog";
import { IconButton } from "../components/IconButton";
import { Notice } from "../components/Notice";
import { Tabs, tabPanelProps } from "../components/Tabs";
import { EmptyState } from "../components/EmptyState";
import type { DaemonClient } from "../daemon/client";
import { openExternal } from "../shared/openExternal";
import { message } from "./common";
import { BrowseView } from "./BrowseView";
import { InstalledView } from "./InstalledView";
import { AddForm } from "./AddForm";
import { CustomForm } from "./CustomForm";

type Tab = "installed" | "browse";

type View =
  | { kind: "list" }
  | { kind: "add"; entry: CatalogServer }
  | { kind: "custom"; editing: McpServerInfo | null };

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
  // The address of a sign-in under way, kept on screen so it can be reopened or copied.
  const [signInAddress, setSignInAddress] = useState<{ name: string; url: string } | null>(null);
  const idBase = useId();
  const bodyRef = useRef<HTMLDivElement>(null);

  // A new view (a form, the other tab) starts at its top, not where the last one was scrolled to.
  useEffect(() => {
    bodyRef.current?.parentElement?.scrollTo({ top: 0 });
  }, [tab, view.kind]);

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

  // While a sign-in waits for the browser, look again every couple of seconds.
  const signingIn = installed?.servers.some((s) => s.status.sign_in === "signing_in") ?? false;
  useEffect(() => {
    if (!signingIn) return;
    const timer = window.setInterval(() => void refresh(), 2000);
    return () => window.clearInterval(timer);
  }, [signingIn, refresh]);

  // Escape steps back from a form; from the list the dialog's own Escape closes the window.
  const onKeyDown = (event: KeyboardEvent) => {
    if (event.key === "Escape" && view.kind !== "list") {
      event.preventDefault();
      setView({ kind: "list" });
    }
  };

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

  /** Starts an OAuth sign-in and opens the address in the user's own browser. */
  const signIn = async (name: string) => {
    if (!client) return;
    try {
      const { authorization_url } = await client.call<McpLoginResult>("mcp.login", { name });
      setSignInAddress({ name, url: authorization_url });
      await refresh();
      await openExternal(authorization_url);
    } catch (failure) {
      setError(message(failure));
    }
  };

  const signOut = async (name: string) => {
    if (!client) return;
    try {
      await client.call<McpLogoutResult>("mcp.logout", { name });
      setSignInAddress((current) => (current?.name === name ? null : current));
      await refresh();
    } catch (failure) {
      setError(message(failure));
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
    body = <EmptyState icon={Plug} title="Not connected" body="Connect to imperium-core to manage connectors." />;
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
        onSignIn={signIn}
        onSignOut={signOut}
        signInAddress={signInAddress}
        onDismissSignIn={() => setSignInAddress(null)}
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

  const toolbar = (
    <div className="connectors-toolbar">
      <Tabs
        label="Connector views"
        idBase={idBase}
        value={tab}
        onChange={(id) => {
          setTab(id as Tab);
          setView({ kind: "list" });
        }}
        items={[
          { id: "installed", label: "Installed", badge: installedCount ?? undefined },
          { id: "browse", label: "Browse" },
        ]}
      />
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
    </div>
  );

  return (
    <Dialog
      label="Connectors"
      title={
        <span className="connectors-title">
          <Plug aria-hidden="true" /> Connectors
        </span>
      }
      onClose={onClose}
      onKeyDown={onKeyDown}
      notice={
        <>
          {restartNeeded && <Notice tone="info">Restart imperium-core for plugin changes to apply to new sessions.</Notice>}
          {error && (
            <Notice tone="danger" action={<IconButton icon={X} label="Dismiss" size="sm" onClick={() => setError(null)} />}>
              {error}
            </Notice>
          )}
          {toolbar}
        </>
      }
    >
      <div ref={bodyRef} className="connectors-body" {...tabPanelProps(idBase, tab)}>
        {body}
      </div>
    </Dialog>
  );
}
