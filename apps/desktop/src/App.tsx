// Connects to the daemon and arranges the default view: Projects and Sessions stacked on the
// left, the session spanning the middle, agents, models, and telemetry stacked on the right.
// The workspace is always shown: while the daemon is away, a banner offers to start or reach it
// and the lists show what was last known, and the layout reopens as the user left it.

import { FolderPlus, Plug, Plus, Settings } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import type {
  ApprovalInfo,
  ApprovalListResult,
  PermissionDecision,
  ResumePolicy,
  SendMode,
  SessionControlParams,
  SessionQueueParams,
  SessionSendParams,
  SessionControlResult,
  SessionCreateResult,
  SessionEventParams,
  LocalModelListResult,
  ModListResult,
  PetListResult,
  PetStateInfo,
  ModelListResult,
  SessionListResult,
  SessionSummary,
  SessionSubscribeResult,
} from "@imperium/protocol";

import { DaemonClient, type DaemonConnection } from "./daemon/client";
import { findDaemon, insideTauri, pickFolder, rememberDevConnection } from "./daemon/connection";
import { Columns, type PanelSpec } from "./layout/Columns";
import { getLayout, resetLayout, updateLayout } from "./layout/state";
import { notify } from "./notify";
import { ProjectsPanel, SessionsPanel } from "./panels/NavigationPanels";
import { SessionPanel } from "./panels/SessionPanel";
import { AgentsPanel } from "./panels/AgentsPanel";
import { ModBus, ModFrame } from "./mods/ModFrame";
import { ApprovalsPanel } from "./panels/ApprovalsPanel";
import { BrowserPanel } from "./panels/BrowserPanel";
import { PetPanel } from "./panels/PetPanel";
import { LocalModelsPanel } from "./panels/LocalModelsPanel";
import { ModelsPanel } from "./panels/ModelsPanel";
import { TelemetryPanel } from "./panels/TelemetryPanel";
import { normalizeProject, useProjectIcons } from "./projectIcons";
import { applyEvent, DEFAULT_RESUME_POLICY, markAnswered, newSession, type SessionState } from "./sessions";
import { applySettings, loadSettings, saveSettings, SettingsContext, type AppSettings } from "./settings";
import { SettingsWindow } from "./SettingsWindow";
import { ConnectorsWindow } from "./ConnectorsWindow";
import { getDoc, setDoc } from "./store";
import { type Daemon, useDaemon } from "./DaemonControl";
import { UpdateControl, useUpdates } from "./UpdateControl";

type Status = { kind: "connecting" } | { kind: "connected"; client: DaemonClient } | { kind: "failed"; message: string };

/** Sessions as last seen, shown until the daemon answers. */
type Cache = { sessions: SessionSummary[]; savedAt: number };

function cachedSessions(): Record<string, SessionState> {
  const cache = getDoc<Cache>("cache");
  return Object.fromEntries(
    (cache?.sessions ?? []).map((summary, index) => [summary.session_id, newSession(summary, index)] as const),
  );
}

export function App() {
  const [settings, setSettings] = useState<AppSettings>(loadSettings);
  const saved = useMemo(() => getLayout(), []);
  const restore = settings.restoreSession;
  const [status, setStatus] = useState<Status>({ kind: "connecting" });
  const [sessions, setSessions] = useState<Record<string, SessionState>>(cachedSessions);
  const [extraProjects, setExtraProjects] = useState<string[]>(saved.folders);
  // The project new sessions start in, and whether the Sessions list shows only its sessions.
  const [project, setProject] = useState<string | null>(restore ? saved.project : null);
  const [projectView, setProjectView] = useState(restore && saved.projectView);
  const [selected, setSelected] = useState<string | null>(restore ? saved.session : null);
  const [hidden, setHidden] = useState<string[]>(saved.hidden);
  const [showSettings, setShowSettings] = useState(false);
  const [showConnectors, setShowConnectors] = useState(false);
  const updates = useUpdates();
  const [modelTable, setModelTable] = useState<ModelListResult | null>(null);
  const [localModels, setLocalModels] = useState<LocalModelListResult | null>(null);
  const [mods, setMods] = useState<ModListResult | null>(null);
  const [approvals, setApprovals] = useState<ApprovalInfo[]>([]);
  const [pets, setPets] = useState<PetListResult | null>(null);
  const [petState, setPetState] = useState<PetStateInfo | null>(null);
  const modBus = useMemo(() => new ModBus(), []);
  const icons = useProjectIcons();

  useEffect(() => applySettings(settings), [settings]);

  // Each connection attempt gets a generation; a superseded attempt (React's development
  // double-mount, a retry) closes its own client instead of adding a second listener.
  const generation = useRef(0);
  // Sessions this window already follows, so a session.created notice for one it started
  // itself does not subscribe twice.
  const known = useRef(new Set<string>());
  const current = useRef<DaemonClient | null>(null);

  const connect = useCallback(async (connection?: DaemonConnection) => {
    const attempt = ++generation.current;
    current.current?.close();
    current.current = null;
    setStatus({ kind: "connecting" });
    try {
      const target = connection ?? (await findDaemon(loadSettings().autoStartDaemon));
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
          all[session_id]
            ? { ...all, [session_id]: { ...applyEvent(all[session_id], { agent_id, event }), updatedAt: Date.now() } }
            : all,
        );
      client.onNotification((notification) => {
        if (notification.method === "pet.state") {
          setPetState(notification.params as PetStateInfo);
          return;
        }
        if (notification.method === "approval.requested") {
          const approval = notification.params as ApprovalInfo;
          setApprovals((all) => [...all.filter((a) => a.id !== approval.id), approval]);
          return;
        }
        if (notification.method === "session.created") {
          const summary = notification.params as SessionSummary;
          // Give a session this window is creating time to register before following it.
          window.setTimeout(async () => {
            if (attempt !== generation.current) return;
            if (known.current.has(summary.session_id)) {
              // One this window created: take the daemon's fuller summary (title, resume policy).
              setSessions((all) =>
                all[summary.session_id] ? { ...all, [summary.session_id]: { ...all[summary.session_id], summary } } : all,
              );
              return;
            }
            known.current.add(summary.session_id);
            const { history } = await client.call<SessionSubscribeResult>("session.subscribe", {
              session_id: summary.session_id,
            });
            setSessions((all) => ({
              ...all,
              [summary.session_id]: history.reduce(applyEvent, newSession(summary, Date.now())),
            }));
          }, 500);
          return;
        }
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
        running.map(async (summary, index) => {
          const { history } = await client.call<SessionSubscribeResult>("session.subscribe", {
            session_id: summary.session_id,
          });
          // Restored sessions keep the daemon's order (later ones first) until they change here.
          return [summary.session_id, history.reduce(applyEvent, newSession(summary, index))] as const;
        }),
      );
      known.current = new Set(restored.map(([id]) => id));
      setSessions(Object.fromEntries(restored));
      setModelTable(await client.call<ModelListResult>("models.list"));
      setLocalModels(await client.call<LocalModelListResult>("models.local"));
      setMods(await client.call<ModListResult>("mods.list"));
      setApprovals((await client.call<ApprovalListResult>("approvals.list")).approvals);
      const petList = await client.call<PetListResult>("pet.list");
      setPets(petList);
      setPetState((current) => current ?? petList.state);
      const queued = pending;
      pending = null;
      queued.forEach(apply);
      setProject((selected) => {
        const first = restored[0]?.[1].summary.project_dir;
        return selected ?? (first ? normalizeProject(first) : null);
      });
      setStatus({ kind: "connected", client });
    } catch (error) {
      if (attempt === generation.current) {
        setStatus({ kind: "failed", message: error instanceof Error ? error.message : String(error) });
      }
    }
  }, [modBus]);
  const daemon = useDaemon(() => void connect(), status.kind === "connected");

  // Remember the selection, opened folders, and closed panels.
  useEffect(() => {
    updateLayout({ project, session: selected, projectView, folders: extraProjects, hidden });
  }, [project, selected, projectView, extraProjects, hidden]);

  // Remember the sessions as last seen, for the next time the daemon is away.
  useEffect(() => {
    if (status.kind !== "connected") return;
    const summaries = Object.values(sessions).map((s) => s.summary);
    setDoc<Cache>("cache", { sessions: summaries, savedAt: Date.now() });
  }, [sessions, status.kind]);

  // Ctrl+, (Cmd+, on macOS) opens Settings, as in most desktop apps.
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "," && (event.ctrlKey || event.metaKey)) {
        event.preventDefault();
        setShowSettings((open) => !open);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  useEffect(() => {
    void connect();
    return () => {
      generation.current++;
      current.current?.close();
      current.current = null;
    };
  }, [connect]);

  const projects = useMemo(
    () =>
      [
        ...new Set([...Object.values(sessions).map((s) => s.summary.project_dir), ...extraProjects].map(normalizeProject)),
      ].sort(),
    [sessions, extraProjects],
  );
  const allSessions = useMemo(() => Object.values(sessions), [sessions]);
  const session = selected ? (sessions[selected] ?? null) : null;
  const client = status.kind === "connected" ? status.client : null;

  const report = (error: unknown) =>
    window.alert(error instanceof Error ? error.message : String(error));

  const respondToApproval = (id: number, approve: boolean) => {
    client
      ?.call("approvals.respond", { id, approve })
      .then(() => setApprovals((all) => all.filter((a) => a.id !== id)), report);
  };

  const refreshLocalModels = () => {
    client?.call<LocalModelListResult>("models.local").then(setLocalModels, report);
  };

  const openFolder = async () => {
    const folder = await pickFolder();
    if (!folder) return;
    setExtraProjects((all) => [...all, normalizeProject(folder)]);
    showProject(normalizeProject(folder));
  };

  const showProject = (next: string) => {
    setProject(normalizeProject(next));
    setProjectView(true);
  };

  const startSession = async () => {
    if (!client || !project) return;
    try {
      const created = await client.call<SessionCreateResult>("session.create", { project_dir: project });
      known.current.add(created.session_id);
      // The rest of the summary arrives with the daemon's session.created notice.
      const summary: SessionSummary = {
        session_id: created.session_id,
        project_dir: project,
        model: created.model,
        isolation: created.isolation,
        title: "",
        status: "idle",
        status_detail: null,
        resume_policy: DEFAULT_RESUME_POLICY,
      };
      setSessions((all) => ({ ...all, [created.session_id]: newSession(summary, Date.now()) }));
      setSelected(created.session_id);
    } catch (error) {
      report(error);
    }
  };

  /** Sends a message: it starts a turn, waits in the queue while one runs, or (steer) joins it. */
  const send = (text: string, mode: SendMode) => {
    if (!client || !selected) return;
    const params: SessionSendParams = { session_id: selected, text, mode };
    client.call("session.send", params).catch(report);
  };

  /** Edits (with text) or removes a queued message; the queue updates from `queue_changed`. */
  const changeQueued = (id: string, text: string | null) => {
    if (!client || !selected) return;
    const params: SessionQueueParams =
      text === null
        ? { session_id: selected, action: "remove", id }
        : { session_id: selected, action: "edit", id, text };
    client.call("session.queue", params).catch(report);
  };

  const respond = (requestId: number, decision: PermissionDecision) => {
    if (!client || !selected) return;
    setSessions((all) => ({ ...all, [selected]: markAnswered(all[selected], requestId) }));
    client.call("session.respond", { session_id: selected, request_id: requestId, decision }).catch(report);
  };

  const control = (sessionId: string, params: Partial<Omit<SessionControlParams, "session_id">>) => {
    if (!client) return Promise.reject(new Error("Not connected to imperiumd."));
    const full: SessionControlParams = {
      session_id: sessionId,
      action: "set_resume_policy",
      resume_policy: null,
      repo: null,
      pull_request: null,
      model: null,
      ...params,
    };
    return client.call<SessionControlResult>("session.control", full);
  };

  const setResumePolicy = async (policy: ResumePolicy) => {
    if (!selected) return;
    const id = selected;
    try {
      await control(id, { action: "set_resume_policy", resume_policy: policy });
      setSessions((all) =>
        all[id] ? { ...all, [id]: { ...all[id], summary: { ...all[id].summary, resume_policy: policy } } } : all,
      );
    } catch (error) {
      report(error);
    }
  };

  const resumeNow = () => {
    if (selected) control(selected, { action: "resume_now" }).catch(report);
  };

  /** Switches the selected session's model; the daemon compacts the conversation if needed. */
  const setModel = async (model: string): Promise<SessionControlResult | null> => {
    if (!selected) return null;
    const id = selected;
    try {
      const result = await control(id, { action: "set_model", model });
      const now = result.model ?? model;
      setSessions((all) => (all[id] ? { ...all, [id]: { ...all[id], summary: { ...all[id].summary, model: now } } } : all));
      return result;
    } catch (error) {
      report(error);
      return null;
    }
  };

  const selectSession = (sessionId: string) => {
    setSelected(sessionId);
    // New sessions start in the project the user is working in.
    const dir = sessions[sessionId]?.summary.project_dir;
    if (dir) setProject(normalizeProject(dir));
  };

  const updateSettings = (next: AppSettings) => {
    setSettings(next);
    saveSettings(next);
  };

  // Sound and a title badge when an agent needs approval or a session finishes.
  const approvalCount = useRef(0);
  useEffect(() => {
    if (approvals.length > approvalCount.current) notify("approval", settings);
    approvalCount.current = approvals.length;
  }, [approvals.length, settings]);
  const running = useRef(new Set<string>());
  useEffect(() => {
    for (const s of Object.values(sessions)) {
      const busy = s.summary.status === "working";
      if (!busy && running.current.has(s.summary.session_id)) notify("finished", settings);
      if (busy) running.current.add(s.summary.session_id);
      else running.current.delete(s.summary.session_id);
    }
  }, [sessions, settings]);

  const closePanel = (id: string) => setHidden((all) => (all.includes(id) ? all : [...all, id]));
  const togglePanel = (id: string, open: boolean) =>
    setHidden((all) => (open ? all.filter((panel) => panel !== id) : all.includes(id) ? all : [...all, id]));
  /** Leaves out closed panels and lets the rest be closed. */
  const visible = (panels: PanelSpec[]) =>
    panels
      .filter((panel) => !hidden.includes(panel.id))
      .map((panel) => ({ ...panel, onClose: () => closePanel(panel.id) }));

  return (
    <SettingsContext.Provider value={settings}>
    <div className="app">
      <header className="titlebar">
        <strong className="brand">Imperium</strong>
        <span className="muted connection" data-state={status.kind}>
          {status.kind === "connected"
            ? `${status.client.daemon?.daemon.name} ${status.client.daemon?.daemon.version}`
            : status.kind === "connecting"
              ? "Connecting…"
              : "Disconnected"}
        </span>
        <span className="spacer" />
        <UpdateControl updates={updates} onOpen={() => setShowSettings(true)} />
        <button
          className="ghost titlebar-button"
          onClick={() => setShowConnectors((open) => !open)}
          aria-expanded={showConnectors}
          title="MCP servers, plugins, and skills"
        >
          <Plug aria-hidden="true" /> Connectors
        </button>
        <button
          className="ghost titlebar-button"
          onClick={() => setShowSettings((open) => !open)}
          aria-expanded={showSettings}
          title="Settings (Ctrl+,)"
        >
          <Settings aria-hidden="true" /> Settings
        </button>
      </header>
      {showConnectors && <ConnectorsWindow client={client} onClose={() => setShowConnectors(false)} />}
      {showSettings && (
        <SettingsWindow
          settings={settings}
          onChange={updateSettings}
          updates={updates}
          daemon={daemon}
          client={client}
          panels={PANELS}
          hidden={hidden}
          onTogglePanel={togglePanel}
          onOpenConnectors={() => {
            setShowSettings(false);
            setShowConnectors(true);
          }}
          onResetLayout={() => {
            resetLayout();
            window.location.reload();
          }}
          onClose={() => setShowSettings(false)}
        />
      )}
      {status.kind !== "connected" && <ConnectionBanner status={status} onConnect={connect} daemon={daemon} />}
      {(
        <main className="workspace">
          <Columns
            initial={[0.2, 0.55, 0.25]}
            columns={[
              {
                id: "left",
                mode: "stack",
                panels: visible([
                  {
                    id: "projects",
                    title: "Projects",
                    actions: (
                      <button onClick={openFolder}>
                        <FolderPlus aria-hidden="true" /> Open folder
                      </button>
                    ),
                    body: (
                      <ProjectsPanel
                        projects={projects}
                        selected={project}
                        icons={icons}
                        onSelect={(next) => {
                          showProject(next);
                          setSelected(null);
                        }}
                      />
                    ),
                  },
                  {
                    id: "sessions",
                    title: "Sessions",
                    actions: (
                      <button
                        onClick={startSession}
                        disabled={!project || !client}
                        title={project ? `New session in ${project}` : "Select a project first"}
                      >
                        <Plus aria-hidden="true" /> New session
                      </button>
                    ),
                    body: (
                      <SessionsPanel
                        sessions={allSessions}
                        project={projectView ? project : null}
                        selected={selected}
                        icons={icons}
                        onSelect={selectSession}
                        onShowProject={showProject}
                        onShowAll={() => setProjectView(false)}
                      />
                    ),
                  },
                  {
                    id: "pet",
                    title: pets?.pets.find((p) => p.id === pets.selected)?.display_name ?? "Pet",
                    body: (
                      <PetPanel
                        client={client}
                        pet={pets?.pets.find((p) => p.id === pets.selected) ?? null}
                        state={petState}
                      />
                    ),
                  },
                  ...(approvals.length > 0
                    ? [
                        {
                          id: "approvals",
                          title: `Approvals (${approvals.length})`,
                          body: <ApprovalsPanel approvals={approvals} onRespond={respondToApproval} />,
                        },
                      ]
                    : []),
                ]),
              },
              {
                id: "main",
                mode: "span",
                panel: {
                  id: "session",
                  title: "Session",
                  body: (
                    <SessionPanel
                      session={session}
                      icons={icons}
                      onSend={send}
                      onChangeQueued={changeQueued}
                      onRespond={respond}
                      onSetResumePolicy={setResumePolicy}
                      onResumeNow={resumeNow}
                      models={modelTable?.models ?? []}
                      onSetModel={setModel}
                      client={client}
                    />
                  ),
                },
              },
              {
                id: "right",
                mode: "stack",
                panels: visible([
                  { id: "agents", title: "Agents", body: <AgentsPanel session={session} /> },
                  { id: "models", title: "Models", body: <ModelsPanel table={modelTable} /> },
                  {
                    id: "local-models",
                    title: "Local models",
                    body: <LocalModelsPanel listing={localModels} onRefresh={refreshLocalModels} client={client} />,
                  },
                  { id: "telemetry", title: "Telemetry", body: <TelemetryPanel session={session} /> },
                  { id: "browser", title: "Browser", body: <BrowserPanel session={session} /> },
                  ...(mods?.mods ?? []).flatMap((mod) =>
                    mod.panels.map((panel) => ({
                      id: `mod:${mod.id}:${panel.id}`,
                      title: panel.title,
                      body: <ModFrame mod={mod} panelId={panel.id} bus={modBus} />,
                    })),
                  ),
                ]),
              },
            ]}
          />
        </main>
      )}
    </div>
    </SettingsContext.Provider>
  );
}

/** Panels the user can close and reopen from Settings → Panels. */
const PANELS: { id: string; title: string }[] = [
  { id: "projects", title: "Projects" },
  { id: "sessions", title: "Sessions" },
  { id: "pet", title: "Pet" },
  { id: "approvals", title: "Approvals" },
  { id: "agents", title: "Agents" },
  { id: "models", title: "Models" },
  { id: "local-models", title: "Local models" },
  { id: "telemetry", title: "Telemetry" },
  { id: "browser", title: "Browser" },
];

/** A slim bar above the workspace while the daemon is away: why, and how to reach it. */
function ConnectionBanner({
  status,
  onConnect,
  daemon,
}: {
  status: Status;
  onConnect: (connection?: DaemonConnection) => void;
  daemon: Daemon | null;
}) {
  const [url, setUrl] = useState("");
  const [token, setToken] = useState("");
  if (status.kind === "connecting") {
    return (
      <div className="connection-banner" data-state="connecting" role="status">
        <span className="daemon-dot" aria-hidden="true" /> Connecting to imperiumd…
      </div>
    );
  }
  return (
    <div className="connection-banner" data-state="offline" role="status">
      <span className="daemon-dot" aria-hidden="true" />
      <span className="connection-message">
        <strong>imperiumd is not connected.</strong> {status.kind === "failed" ? status.message : ""} Showing what was
        last known.
      </span>
      {daemon?.error && <span className="error">{daemon.error}</span>}
      {daemon?.status.can_start && !daemon.status.running && (
        <button className="primary" onClick={() => daemon.run("start")} disabled={daemon.busy !== null}>
          {daemon.busy ? "Starting…" : "Start imperiumd"}
        </button>
      )}
      {insideTauri() ? (
        <button onClick={() => onConnect()}>Retry</button>
      ) : (
        <form
          className="connection-form"
          onSubmit={(event) => {
            event.preventDefault();
            onConnect({ url: url.trim(), token: token.trim() });
          }}
        >
          <input aria-label="Daemon URL" placeholder="ws://127.0.0.1:port/rpc" value={url} onChange={(e) => setUrl(e.target.value)} />
          <input aria-label="Token" placeholder="token" type="password" value={token} onChange={(e) => setToken(e.target.value)} />
          <button type="submit" className="primary">
            Connect
          </button>
        </form>
      )}
    </div>
  );
}
