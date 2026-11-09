// Connects to the daemon and arranges the default view: Environments and Sessions stacked on the
// left, the session spanning the middle, agents, models, and telemetry stacked on the right.
// The workspace is always shown: while the daemon is away, a banner offers to start or reach it
// and the lists show what was last known, and the layout reopens as the user left it.
// Any panel can be detached into a window of its own, which runs this same component with
// `?panel=<id>`: it connects to the daemon itself but shows only that panel. The main window
// owns the layout and the list of detached panels; see panelWindows.ts.

import { Plug, Plus, Settings } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import type {
  EnvironmentInfo,
  EnvironmentListResult,
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
import { findDaemon, insideTauri, rememberDevConnection } from "./daemon/connection";
import { Columns, type ColumnSpec, type PanelSpec } from "./layout/Columns";
import { PanelWindow } from "./layout/PanelWindow";
import { getLayout, resetLayout, updateLayout, type DetachedPanel } from "./layout/state";
import { notify } from "./notify";
import {
  EnvironmentsPanel,
  SessionsPanel,
  environmentOf,
  type EnvironmentActions,
  type SessionSort,
} from "./panels/NavigationPanels";
import { SessionPanel } from "./panels/SessionPanel";
import { AgentsPanel } from "./panels/AgentsPanel";
import { ModBus, ModFrame } from "./mods/ModFrame";
import { ApprovalsPanel } from "./panels/ApprovalsPanel";
import { BrowserPanel } from "./panels/BrowserPanel";
import { PetPanel } from "./panels/PetPanel";
import { LocalModelsPanel } from "./panels/LocalModelsPanel";
import { ModelsPanel } from "./panels/ModelsPanel";
import { TelemetryPanel } from "./panels/TelemetryPanel";
import { environmentIcon, projectName, sameFolder, useProjectIcons, type ProjectIcons } from "./projectIcons";
import { applyEvent, DEFAULT_RESUME_POLICY, markAnswered, newSession, snapshotSessions, type SessionState } from "./sessions";
import { applySettings, loadSettings, modEnabled, saveSettings, SettingsContext, type AppSettings } from "./settings";
import { SettingsWindow } from "./SettingsWindow";
import { emitPanelEvent, detachedPanelId, listenPanelEvents, openPanelWindow } from "./panelWindows";
import { ConnectorsWindow } from "./ConnectorsWindow";
import { EnvironmentWindow } from "./EnvironmentWindow";
import { Select } from "./components/Select";
import { getDoc, setDoc, subscribeDoc } from "./store";
import { type Daemon, useDaemon } from "./DaemonControl";
import { UpdateControl, useUpdates } from "./UpdateControl";

type Status = { kind: "connecting" } | { kind: "connected"; client: DaemonClient } | { kind: "failed"; message: string };

/** Sessions and environments as last seen, shown until the daemon answers. */
type Cache = { sessions: SessionSummary[]; environments?: EnvironmentInfo[]; savedAt: number };

function cachedSessions(): Record<string, SessionState> {
  const cache = getDoc<Cache>("cache");
  return Object.fromEntries(
    (cache?.sessions ?? []).map((summary, index) => [summary.session_id, newSession(summary, index)] as const),
  );
}

const cachedEnvironments = (): EnvironmentInfo[] => getDoc<Cache>("cache")?.environments ?? [];

let migrating: Promise<EnvironmentInfo[]> | null = null;

/** Lists the environments. The first time, each project folder known from before environments
 * (opened here, or used by a session) becomes an environment of its own, keeping its icon. */
function migrateProjects(client: DaemonClient, sessionFolders: string[], icons: ProjectIcons): Promise<EnvironmentInfo[]> {
  const list = async () => (await client.call<EnvironmentListResult>("environments.list")).environments;
  if (getLayout().environmentsMigrated) return list();
  // React's development double-mount connects twice; migrate once.
  migrating ??= (async () => {
    const envs = await list();
    const layout = getLayout();
    const folders = [...layout.folders, ...(layout.project ? [layout.project] : []), ...sessionFolders];
    for (const folder of folders) {
      if (envs.some((env) => env.folders[0] && sameFolder(env.folders[0], folder))) continue;
      const choice = icons.iconFor(folder);
      try {
        envs.push(
          await client.call<EnvironmentInfo>("environments.create", {
            name: projectName(folder),
            icon: choice.icon,
            color: choice.color,
            folders: [folder],
          }),
        );
      } catch (error) {
        // A folder that no longer exists is left behind.
        console.warn(`could not make ${folder} an environment`, error);
      }
    }
    updateLayout({ environmentsMigrated: true });
    return envs;
  })().finally(() => {
    migrating = null;
  });
  return migrating;
}

export function App() {
  const [settings, setSettings] = useState<AppSettings>(loadSettings);
  const saved = useMemo(() => getLayout(), []);
  // The panel this window shows alone; null in the main window.
  const detachedId = useMemo(detachedPanelId, []);
  const isDetached = detachedId !== null;
  // A detached window follows the main window's selection, so it always restores it.
  const restore = settings.restoreSession || isDetached;
  const [status, setStatus] = useState<Status>({ kind: "connecting" });
  const [sessions, setSessions] = useState<Record<string, SessionState>>(cachedSessions);
  const [environments, setEnvironments] = useState<EnvironmentInfo[]>(cachedEnvironments);
  // The environment new sessions start in, and whether the Sessions list shows only its sessions.
  const [envId, setEnvId] = useState<string | null>(restore ? saved.environment : null);
  const [envView, setEnvView] = useState(restore && saved.projectView);
  /** The environment window: which environment it edits (null id: a new one). */
  const [envWindow, setEnvWindow] = useState<{ id: string | null; confirmDelete?: boolean } | null>(null);
  const [sessionSort, setSessionSort] = useState<SessionSort>(saved.sessionSort);
  /** The model "New session" asks for; empty for the configured default. */
  const [newModel, setNewModel] = useState("");
  const [newWorktree, setNewWorktree] = useState(false);
  const [sessionError, setSessionError] = useState<string | null>(null);
  const [selected, setSelected] = useState<string | null>(restore ? saved.session : null);
  const [hidden, setHidden] = useState<string[]>(saved.hidden);
  const [detached, setDetached] = useState<Record<string, DetachedPanel>>(saved.detached);
  // Panels whose windows this session has opened, so each opens once.
  const opened = useRef(new Set<string>());
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
  // Icons chosen per folder before environments, carried over when folders become environments.
  const folderIcons = useProjectIcons();
  const folderIconsRef = useRef(folderIcons);
  folderIconsRef.current = folderIcons;

  useEffect(() => applySettings(settings), [settings]);

  // Settings changed in another window apply here too.
  useEffect(() => subscribeDoc("settings", () => setSettings(loadSettings())), []);

  // A detached window follows the main window's selected environment and session.
  useEffect(() => {
    if (!isDetached) return;
    return subscribeDoc("layout", () => {
      const next = getLayout();
      setEnvId(next.environment);
      setEnvView(next.projectView);
      setSelected(next.session);
    });
  }, [isDetached]);

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
      const target = connection ?? (await findDaemon(!isDetached && loadSettings().autoStartDaemon));
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
        if (notification.method === "environments.changed") {
          setEnvironments((notification.params as EnvironmentListResult).environments);
          return;
        }
        if (notification.method === "session.deleted") {
          const { session_id } = notification.params as { session_id: string };
          known.current.delete(session_id);
          setSessions((all) => {
            const { [session_id]: _gone, ...rest } = all;
            return rest;
          });
          return;
        }
        if (notification.method === "session.updated") {
          const summary = notification.params as SessionSummary;
          setSessions((all) =>
            all[summary.session_id] ? { ...all, [summary.session_id]: { ...all[summary.session_id], summary } } : all,
          );
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
        if (attempt === generation.current) setStatus({ kind: "failed", message: "Lost the connection to imperium-core." });
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
      const envs = await migrateProjects(
        client,
        restored.map(([, s]) => s.summary.project_dir),
        folderIconsRef.current,
      );
      if (attempt !== generation.current) return;
      setEnvironments(envs);
      // Keep the selected environment if it still exists; else the one the saved project folder
      // became, the first restored session's, or the first there is.
      setEnvId((selected) => {
        if (selected && envs.some((env) => env.id === selected)) return selected;
        const layout = getLayout();
        const byFolder = (folder: string | null | undefined) =>
          folder ? envs.find((env) => env.folders[0] && sameFolder(env.folders[0], folder))?.id : undefined;
        const firstSession = restored[0]?.[1];
        return (
          (loadSettings().restoreSession ? byFolder(layout.project) : undefined) ??
          (firstSession ? environmentOf(firstSession, envs)?.id : undefined) ??
          envs[0]?.id ??
          null
        );
      });
      setStatus({ kind: "connected", client });
    } catch (error) {
      if (attempt === generation.current) {
        setStatus({ kind: "failed", message: error instanceof Error ? error.message : String(error) });
      }
    }
  }, [modBus, isDetached]);
  const daemon = useDaemon(() => void connect(), status.kind === "connected", !isDetached);

  // Remember the selection, closed panels, and detached windows. Only the main window writes
  // the layout.
  useEffect(() => {
    if (isDetached) return;
    updateLayout({ environment: envId, session: selected, projectView: envView, hidden, detached, sessionSort });
  }, [isDetached, envId, selected, envView, hidden, detached, sessionSort]);

  // Remember the sessions as last seen, for the next time the daemon is away.
  useEffect(() => {
    if (status.kind !== "connected" || isDetached) return;
    const summaries = Object.values(sessions).map((s) => s.summary);
    setDoc<Cache>("cache", { sessions: summaries, environments, savedAt: Date.now() });
  }, [sessions, environments, status.kind, isDetached]);

  // Ctrl+, (Cmd+, on macOS) opens Settings, as in most desktop apps.
  useEffect(() => {
    if (isDetached) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "," && (event.ctrlKey || event.metaKey)) {
        event.preventDefault();
        setShowSettings((open) => !open);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [isDetached]);

  useEffect(() => {
    void connect();
    return () => {
      generation.current++;
      current.current?.close();
      current.current = null;
    };
  }, [connect]);

  const allSessions = useMemo(() => Object.values(sessions), [sessions]);
  const session = selected ? (sessions[selected] ?? null) : null;
  const client = status.kind === "connected" ? status.client : null;
  const modHost = useMemo(
    () => ({ sessions: snapshotSessions(allSessions), selected, client }),
    [allSessions, selected, client],
  );
  const environment = environments.find((env) => env.id === envId) ?? null;
  // The session header shows a session's icon by its folder: that of its environment.
  const icons = useMemo<ProjectIcons>(
    () => ({
      iconFor: (path) => {
        const env = environments.find((e) => e.folders[0] && sameFolder(e.folders[0], path));
        return env ? environmentIcon(env) : environmentIcon({ id: path, icon: null, color: null });
      },
      setIcon: () => {},
    }),
    [environments],
  );

  const message = (error: unknown) => (error instanceof Error ? error.message : String(error));
  const report = (error: unknown) => window.alert(message(error));

  const respondToApproval = (id: number, approve: boolean) => {
    client
      ?.call("approvals.respond", { id, approve })
      .then(() => setApprovals((all) => all.filter((a) => a.id !== id)), report);
  };

  const refreshLocalModels = () => {
    client?.call<LocalModelListResult>("models.local").then(setLocalModels, report);
  };

  /** Puts a changed environment in the list (the daemon's notice may arrive later). */
  const replaceEnvironment = (env: EnvironmentInfo) =>
    setEnvironments((all) => (all.some((e) => e.id === env.id) ? all.map((e) => (e.id === env.id ? env : e)) : [...all, env]));

  const envActions: EnvironmentActions = {
    onEdit: (id) => setEnvWindow({ id }),
    onSetPinned: (env, pinned) => {
      client?.call<EnvironmentInfo>("environments.update", { id: env.id, pinned }).then(replaceEnvironment, report);
    },
    onDelete: (id) => setEnvWindow({ id, confirmDelete: true }),
  };

  const setSessionPinned = (sessionId: string, pinned: boolean) => {
    control(sessionId, { action: "set_pinned", pinned }).then(
      () =>
        setSessions((all) =>
          all[sessionId] ? { ...all, [sessionId]: { ...all[sessionId], summary: { ...all[sessionId].summary, pinned } } } : all,
        ),
      report,
    );
  };

  /** Archives a session, or resumes an archived one (which this window then follows afresh). */
  const setSessionArchived = (sessionId: string, archived: boolean) => {
    if (!archived) known.current.delete(sessionId);
    control(sessionId, { action: archived ? "archive" : "unarchive" }).catch(report);
  };

  const showEnvironment = (id: string) => {
    setEnvId(id);
    setEnvView(true);
  };

  const startSession = async () => {
    if (!client || !environment) return;
    setSessionError(null);
    try {
      const created = await client.call<SessionCreateResult>("session.create", {
        environment_id: environment.id,
        ...(newModel ? { model: newModel } : {}),
        ...(newWorktree ? { worktree: true } : {}),
      });
      known.current.add(created.session_id);
      // The rest of the summary arrives with the daemon's session.created notice.
      const summary: SessionSummary = {
        session_id: created.session_id,
        project_dir: created.folders[0] ?? environment.folders[0],
        environment_id: created.environment_id,
        folders: created.folders,
        model: created.model,
        isolation: created.isolation,
        title: "",
        status: "idle",
        status_detail: null,
        resume_policy: DEFAULT_RESUME_POLICY,
        persistent: false,
        archived: false,
        archived_at: null,
        worktrees: created.worktrees,
        pinned: false,
      };
      setSessions((all) => ({ ...all, [created.session_id]: newSession(summary, Date.now()) }));
      setSelected(created.session_id);
    } catch (error) {
      // Shown beside the button rather than in a dialog, which some webviews do not show.
      setSessionError(message(error));
    }
  };

  const newSessionBlocker = !client
    ? "Not connected to imperium-core"
    : !environment
      ? "Select or create an environment first"
      : environment.folders.length === 0
        ? `Add a folder to ${environment.name} first`
        : null;

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
    if (!client) return Promise.reject(new Error("Not connected to imperium-core."));
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
    // New sessions start in the environment the user is working in.
    const picked = sessions[sessionId];
    const env = picked ? environmentOf(picked, environments) : null;
    if (env) setEnvId(env.id);
  };

  const updateSettings = (next: AppSettings) => {
    setSettings(next);
    saveSettings(next);
  };

  // Sound and a title badge when an agent needs approval or a session finishes.
  const approvalCount = useRef(0);
  useEffect(() => {
    if (!isDetached && approvals.length > approvalCount.current) notify("approval", settings);
    approvalCount.current = approvals.length;
  }, [approvals.length, settings, isDetached]);
  const running = useRef(new Set<string>());
  useEffect(() => {
    if (isDetached) return;
    for (const s of Object.values(sessions)) {
      const busy = s.summary.status === "working";
      if (!busy && running.current.has(s.summary.session_id)) notify("finished", settings);
      if (busy) running.current.add(s.summary.session_id);
      else running.current.delete(s.summary.session_id);
    }
  }, [sessions, settings, isDetached]);

  // Panels from enabled mods, by the side they dock on (the right unless they ask for the left).
  const modPanels = (side: "left" | "right"): PanelSpec[] =>
    (mods?.mods ?? []).filter((mod) => modEnabled(settings, mod)).flatMap((mod) =>
      mod.panels
        .filter((panel) => (panel.slot === "sidebar.left" ? "left" : "right") === side)
        .map((panel) => ({
          id: `mod:${mod.id}:${panel.id}`,
          title: panel.title,
          body: <ModFrame mod={mod} panelId={panel.id} bus={modBus} host={modHost} />,
        })),
    );

  // Every panel the app has, in the order they are laid out. The main window docks the ones not
  // closed or detached; a detached window shows just its own.
  const leftPanels: PanelSpec[] = [
    {
      id: "projects",
      title: "Environments",
      actions: (
        <button onClick={() => setEnvWindow({ id: null })} disabled={!client}>
          <Plus aria-hidden="true" /> New environment
        </button>
      ),
      body: (
        <EnvironmentsPanel
          environments={environments}
          selected={envId}
          connected={!!client}
          onSelect={(next) => {
            showEnvironment(next);
            setSelected(null);
          }}
          actions={envActions}
        />
      ),
    },
    {
      id: "sessions",
      title: "Sessions",
      actions: (
        <span className="new-session">
          <Select
            aria-label="Model for new sessions"
            value={newModel}
            onChange={setNewModel}
            options={[
              { value: "", label: "Default model" },
              ...(modelTable?.models ?? []).map((row) => ({
                value: row.id,
                label: row.id,
                disabled: !row.endpoint_configured,
                description: row.endpoint_configured ? undefined : "Endpoint not configured",
              })),
            ]}
          />
          <label className="new-session-worktree" title="Start on a new git worktree and branch">
            <input type="checkbox" checked={newWorktree} onChange={(event) => setNewWorktree(event.target.checked)} />
            Start in a worktree
          </label>
          <button
            onClick={startSession}
            disabled={newSessionBlocker !== null}
            title={newSessionBlocker ?? `New session in ${environment?.name}`}
          >
            <Plus aria-hidden="true" /> New session
          </button>
        </span>
      ),
      body: (
        <SessionsPanel
          sessions={allSessions}
          environments={environments}
          scope={envView ? environment : null}
          selected={selected}
          error={sessionError}
          sort={sessionSort}
          onSort={setSessionSort}
          onSelect={selectSession}
          onSetPinned={setSessionPinned}
          onSetArchived={setSessionArchived}
          onShowEnvironment={showEnvironment}
          onShowAll={() => setEnvView(false)}
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
    ...modPanels("left"),
  ];
  const approvalsPanel: PanelSpec = {
    id: "approvals",
    title: `Approvals (${approvals.length})`,
    body: <ApprovalsPanel approvals={approvals} onRespond={respondToApproval} />,
  };
  const sessionPanel: PanelSpec = {
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
        onUnarchive={session ? () => setSessionArchived(session.summary.session_id, false) : undefined}
      />
    ),
  };
  const rightPanels: PanelSpec[] = [
    { id: "agents", title: "Agents", body: <AgentsPanel session={session} /> },
    { id: "models", title: "Models", body: <ModelsPanel table={modelTable} /> },
    {
      id: "local-models",
      title: "Local models",
      body: <LocalModelsPanel listing={localModels} onRefresh={refreshLocalModels} client={client} />,
    },
    { id: "telemetry", title: "Telemetry", body: <TelemetryPanel session={session} /> },
    { id: "browser", title: "Browser", body: <BrowserPanel session={session} /> },
    ...modPanels("right"),
  ];
  // Approvals appear in the main window only while something waits, but a detached one stays.
  const everyPanel = [...leftPanels, approvalsPanel, sessionPanel, ...rightPanels];
  const findPanel = (id: string) => everyPanel.find((panel) => panel.id === id);

  const closePanel = (id: string) => setHidden((all) => (all.includes(id) ? all : [...all, id]));
  const togglePanel = (id: string, open: boolean) =>
    setHidden((all) => (open ? all.filter((panel) => panel !== id) : all.includes(id) ? all : [...all, id]));
  /** Leaves out closed and detached panels and lets the rest be closed or detached. */
  const visible = (panels: PanelSpec[]) =>
    panels
      .filter((panel) => !hidden.includes(panel.id) && !detached[panel.id])
      .map((panel) => ({ ...panel, onClose: () => closePanel(panel.id), onDetach: () => detach(panel.id) }));

  /** Moves a panel to its own window (the effect above opens it). */
  const detach = (id: string) => setDetached((all) => (all[id] ? all : { ...all, [id]: { pinned: false } }));
  const forget = (id: string) => {
    opened.current.delete(id);
    setDetached(({ [id]: _gone, ...rest }) => rest);
  };
  /** Docks a panel from the main window, which also closes its window. */
  const dock = (id: string) => {
    emitPanelEvent({ kind: "close", id });
    forget(id);
  };

  // Open the windows of detached panels: at startup, once their panel exists, and on detaching.
  useEffect(() => {
    if (isDetached) return;
    for (const id of Object.keys(detached)) {
      const spec = findPanel(id);
      if (opened.current.has(id) || !spec) continue;
      opened.current.add(id);
      void openPanelWindow(id, spec.title, detached[id], () => forget(id));
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [detached, mods]);

  // What the user does in a panel window: pin it, move it, dock it.
  useEffect(() => {
    if (isDetached) return;
    return listenPanelEvents((event) => {
      if (event.kind === "dock") forget(event.id);
      else if (event.kind === "pinned" || event.kind === "bounds") {
        setDetached((all) => {
          const current = all[event.id];
          if (!current) return all;
          return { ...all, [event.id]: event.kind === "pinned" ? { ...current, pinned: event.pinned } : { ...current, bounds: event.bounds } };
        });
      }
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  if (detachedId !== null) {
    return (
      <SettingsContext.Provider value={settings}>
        <div className="app app-panel">
          {status.kind !== "connected" && <ConnectionBanner status={status} onConnect={connect} daemon={null} />}
          <PanelWindow id={detachedId} panel={findPanel(detachedId) ?? null} />
        </div>
      </SettingsContext.Provider>
    );
  }

  const columns: ColumnSpec[] = [
    { id: "left", mode: "stack", panels: visible([...leftPanels, ...(approvals.length > 0 ? [approvalsPanel] : [])]) },
    ...(detached[sessionPanel.id]
      ? []
      : [{ id: "main", mode: "span" as const, panel: { ...sessionPanel, onDetach: () => detach(sessionPanel.id) } }]),
    { id: "right", mode: "stack", panels: visible(rightPanels) },
  ];

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
          mods={mods}
          panels={PANELS}
          hidden={hidden}
          detached={Object.keys(detached)}
          onTogglePanel={togglePanel}
          onDock={dock}
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
      {envWindow && (envWindow.id === null || environments.some((env) => env.id === envWindow.id)) && (
        <EnvironmentWindow
          key={envWindow.id ?? "new"}
          environment={envWindow.id ? (environments.find((env) => env.id === envWindow.id) ?? null) : null}
          client={client}
          confirmDelete={envWindow.confirmDelete}
          onSaved={(env) => {
            const isNew = !environments.some((e) => e.id === env.id);
            replaceEnvironment(env);
            if (isNew) showEnvironment(env.id);
          }}
          onDeleted={(id) => {
            setEnvironments((all) => all.filter((env) => env.id !== id));
            setEnvId((current) => (current === id ? null : current));
          }}
          onClose={() => setEnvWindow(null)}
        />
      )}
      {status.kind !== "connected" && <ConnectionBanner status={status} onConnect={connect} daemon={daemon} />}
      {(
        <main className="workspace">
          <Columns initial={columns.length === 3 ? [0.2, 0.55, 0.25] : undefined} columns={columns} />
        </main>
      )}
    </div>
    </SettingsContext.Provider>
  );
}

/** Panels the user can close and reopen from Settings → Panels. */
const PANELS: { id: string; title: string }[] = [
  { id: "projects", title: "Environments" },
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
        <span className="daemon-dot" aria-hidden="true" /> Connecting to imperium-core…
      </div>
    );
  }
  return (
    <div className="connection-banner" data-state="offline" role="status">
      <span className="daemon-dot" aria-hidden="true" />
      <span className="connection-message">
        <strong>imperium-core is not connected.</strong> {status.kind === "failed" ? status.message : ""} Showing what was
        last known.
      </span>
      {daemon?.error && <span className="error">{daemon.error}</span>}
      {daemon?.status.can_start && !daemon.status.running && (
        <button className="primary" onClick={() => daemon.run("start")} disabled={daemon.busy !== null}>
          {daemon.busy ? "Starting…" : "Start imperium-core"}
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
