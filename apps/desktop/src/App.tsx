// Connects to the daemon and arranges the default view: Projects and Sessions stacked on the
// left, the session spanning the middle, agents, models, and telemetry stacked on the right.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import type {
  PermissionDecision,
  SessionCreateResult,
  SessionEventParams,
  LocalModelListResult,
  ModListResult,
  ModelListResult,
  SessionListResult,
  SessionSubscribeResult,
} from "@imperium/protocol";

import { DaemonClient, type DaemonConnection } from "./daemon/client";
import { findDaemon, insideTauri, pickFolder, rememberDevConnection } from "./daemon/connection";
import { Columns } from "./layout/Columns";
import { ProjectsPanel, SessionsPanel } from "./panels/NavigationPanels";
import { SessionPanel } from "./panels/SessionPanel";
import { AgentsPanel } from "./panels/AgentsPanel";
import { ModBus, ModFrame } from "./mods/ModFrame";
import { LocalModelsPanel } from "./panels/LocalModelsPanel";
import { ModelsPanel } from "./panels/ModelsPanel";
import { TelemetryPanel } from "./panels/TelemetryPanel";
import { applyEvent, markAnswered, newSession, type SessionState } from "./sessions";
import { applySettings, loadSettings, saveSettings, type DisplaySettings } from "./settings";
import { SettingsMenu } from "./SettingsMenu";

type Status = { kind: "connecting" } | { kind: "connected"; client: DaemonClient } | { kind: "failed"; message: string };

export function App() {
  const [status, setStatus] = useState<Status>({ kind: "connecting" });
  const [sessions, setSessions] = useState<Record<string, SessionState>>({});
  const [extraProjects, setExtraProjects] = useState<string[]>([]);
  const [project, setProject] = useState<string | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [settings, setSettings] = useState<DisplaySettings>(loadSettings);
  const [showSettings, setShowSettings] = useState(false);
  const [modelTable, setModelTable] = useState<ModelListResult | null>(null);
  const [localModels, setLocalModels] = useState<LocalModelListResult | null>(null);
  const [mods, setMods] = useState<ModListResult | null>(null);
  const modBus = useMemo(() => new ModBus(), []);

  useEffect(() => applySettings(settings), [settings]);

  // Each connection attempt gets a generation; a superseded attempt (React's development
  // double-mount, a retry) closes its own client instead of adding a second listener.
  const generation = useRef(0);
  const current = useRef<DaemonClient | null>(null);

  const connect = useCallback(async (connection?: DaemonConnection) => {
    const attempt = ++generation.current;
    current.current?.close();
    current.current = null;
    setStatus({ kind: "connecting" });
    try {
      const target = connection ?? (await findDaemon());
      if (!target) {
        setStatus({ kind: "failed", message: "Enter the daemon's URL and token to connect." });
        return;
      }
      const client = await DaemonClient.connect(target);
      if (attempt !== generation.current) {
        client.close();
        return;
      }
      current.current = client;
      if (connection) rememberDevConnection(connection);
      // Events that arrive while sessions are being restored wait here, then apply in order.
      let pending: SessionEventParams[] | null = [];
      const apply = ({ session_id, agent_id, event }: SessionEventParams) =>
        setSessions((all) =>
          all[session_id] ? { ...all, [session_id]: applyEvent(all[session_id], { agent_id, event }) } : all,
        );
      client.onNotification((notification) => {
        if (notification.method !== "session.event") return;
        const params = notification.params as SessionEventParams;
        modBus.emit(params);
        if (pending) pending.push(params);
        else apply(params);
      });
      client.onClose(() => {
        if (attempt === generation.current) setStatus({ kind: "failed", message: "Lost the connection to imperiumd." });
      });
      const { sessions: running } = await client.call<SessionListResult>("session.list");
      const restored = await Promise.all(
        running.map(async (summary) => {
          const { history } = await client.call<SessionSubscribeResult>("session.subscribe", {
            session_id: summary.session_id,
          });
          return [summary.session_id, history.reduce(applyEvent, newSession(summary))] as const;
        }),
      );
      setSessions(Object.fromEntries(restored));
      setModelTable(await client.call<ModelListResult>("models.list"));
      setLocalModels(await client.call<LocalModelListResult>("models.local"));
      setMods(await client.call<ModListResult>("mods.list"));
      const queued = pending;
      pending = null;
      queued.forEach(apply);
      setProject((selected) => selected ?? restored[0]?.[1].summary.project_dir ?? null);
      setStatus({ kind: "connected", client });
    } catch (error) {
      if (attempt === generation.current) {
        setStatus({ kind: "failed", message: error instanceof Error ? error.message : String(error) });
      }
    }
  }, [modBus]);

  useEffect(() => {
    void connect();
    return () => {
      generation.current++;
      current.current?.close();
      current.current = null;
    };
  }, [connect]);

  const projects = useMemo(
    () => [...new Set([...Object.values(sessions).map((s) => s.summary.project_dir), ...extraProjects])].sort(),
    [sessions, extraProjects],
  );
  const projectSessions = Object.values(sessions).filter((s) => s.summary.project_dir === project);
  const session = selected ? (sessions[selected] ?? null) : null;
  const client = status.kind === "connected" ? status.client : null;

  const report = (error: unknown) =>
    window.alert(error instanceof Error ? error.message : String(error));

  const refreshLocalModels = () => {
    client?.call<LocalModelListResult>("models.local").then(setLocalModels, report);
  };

  const openFolder = async () => {
    const folder = await pickFolder();
    if (!folder) return;
    setExtraProjects((all) => [...all, folder]);
    setProject(folder);
  };

  const startSession = async () => {
    if (!client || !project) return;
    try {
      const created = await client.call<SessionCreateResult>("session.create", { project_dir: project });
      const summary = { session_id: created.session_id, project_dir: project, model: created.model };
      setSessions((all) => ({ ...all, [created.session_id]: newSession(summary) }));
      setSelected(created.session_id);
    } catch (error) {
      report(error);
    }
  };

  const send = (text: string) => {
    if (client && selected) client.call("session.send", { session_id: selected, text }).catch(report);
  };

  const respond = (requestId: number, decision: PermissionDecision) => {
    if (!client || !selected) return;
    setSessions((all) => ({ ...all, [selected]: markAnswered(all[selected], requestId) }));
    client.call("session.respond", { session_id: selected, request_id: requestId, decision }).catch(report);
  };

  const updateSettings = (next: DisplaySettings) => {
    setSettings(next);
    saveSettings(next);
  };

  return (
    <div className="app">
      <header className="titlebar">
        <strong>Imperium</strong>
        <span className="muted">
          {status.kind === "connected"
            ? `${status.client.daemon?.daemon.name} ${status.client.daemon?.daemon.version}`
            : status.kind === "connecting"
              ? "Connecting…"
              : "Disconnected"}
        </span>
        <span className="spacer" />
        <button onClick={() => setShowSettings((open) => !open)} aria-expanded={showSettings}>
          Display
        </button>
      </header>
      {showSettings && <SettingsMenu settings={settings} onChange={updateSettings} onClose={() => setShowSettings(false)} />}
      {status.kind === "failed" ? (
        <ConnectScreen message={status.message} onConnect={connect} />
      ) : (
        <main className="workspace">
          <Columns
            initial={[0.2, 0.55, 0.25]}
            columns={[
              {
                id: "left",
                mode: "stack",
                panels: [
                  {
                    id: "projects",
                    title: "Projects",
                    actions: <button onClick={openFolder}>Open folder</button>,
                    body: (
                      <ProjectsPanel
                        projects={projects}
                        selected={project}
                        onSelect={(next) => {
                          setProject(next);
                          setSelected(null);
                        }}
                      />
                    ),
                  },
                  {
                    id: "sessions",
                    title: "Sessions",
                    actions: (
                      <button onClick={startSession} disabled={!project || !client}>
                        New session
                      </button>
                    ),
                    body: <SessionsPanel sessions={projectSessions} selected={selected} onSelect={setSelected} />,
                  },
                ],
              },
              {
                id: "main",
                mode: "span",
                panel: {
                  id: "session",
                  title: "Session",
                  body: <SessionPanel session={session} onSend={send} onRespond={respond} />,
                },
              },
              {
                id: "right",
                mode: "stack",
                panels: [
                  { id: "agents", title: "Agents", body: <AgentsPanel session={session} /> },
                  { id: "models", title: "Models", body: <ModelsPanel table={modelTable} /> },
                  {
                    id: "local-models",
                    title: "Local models",
                    body: <LocalModelsPanel listing={localModels} onRefresh={refreshLocalModels} />,
                  },
                  { id: "telemetry", title: "Telemetry", body: <TelemetryPanel session={session} /> },
                  ...(mods?.mods ?? []).flatMap((mod) =>
                    mod.panels.map((panel) => ({
                      id: `mod:${mod.id}:${panel.id}`,
                      title: panel.title,
                      body: <ModFrame mod={mod} panelId={panel.id} bus={modBus} />,
                    })),
                  ),
                ],
              },
            ]}
          />
        </main>
      )}
    </div>
  );
}

function ConnectScreen({ message, onConnect }: { message: string; onConnect: (connection?: DaemonConnection) => void }) {
  const [url, setUrl] = useState("");
  const [token, setToken] = useState("");
  return (
    <div className="connect">
      <h1>Not connected to imperiumd</h1>
      <p>{message}</p>
      <p className="muted">
        Start the daemon with <code>imperiumd</code>
        {insideTauri() ? ", then retry." : ". In a browser, paste the url and token from its daemon.json."}
      </p>
      {!insideTauri() && (
        <form
          onSubmit={(event) => {
            event.preventDefault();
            onConnect({ url: url.trim(), token: token.trim() });
          }}
        >
          <input placeholder="ws://127.0.0.1:port/rpc" value={url} onChange={(e) => setUrl(e.target.value)} />
          <input placeholder="token" type="password" value={token} onChange={(e) => setToken(e.target.value)} />
          <button type="submit">Connect</button>
        </form>
      )}
      {insideTauri() && <button onClick={() => onConnect()}>Retry</button>}
    </div>
  );
}
