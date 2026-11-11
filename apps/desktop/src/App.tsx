// Connects to the daemon and arranges the default view: Environments and Sessions stacked on the
// left, the session spanning the middle, agents, models, and telemetry stacked on the right.
// The workspace is always shown: while the daemon is away, a banner offers to start or reach it
// and the lists show what was last known, and the layout reopens as the user left it.
// Any panel can be detached into a window of its own, which runs this same component with
// `?panel=<id>`: it connects to the daemon itself but shows only that panel. The main window
// owns the layout and the list of detached panels; see panelWindows.ts.

import { Check, Plug, Plus, Settings } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import type {
  Attachment,
  EnvironmentInfo,
  EnvironmentListResult,
  ApprovalInfo,
  ApprovalListResult,
  BudgetAlert,
  BudgetsResult,
  PermissionDecision,
  PermissionRule,
  ResumePolicy,
  SendMode,
  SessionControlParams,
  SessionAgentCancelParams,
  SessionAgentResumeParams,
  SessionSendParams,
  SessionControlResult,
  SessionCreateParams,
  SessionCreateResult,
  SessionEventParams,
  DownloadJob,
  LocalModelListResult,
  RuntimeInstallJob,
  ModListResult,
  ModelConfigResult,
  ModelListResult,
  ModelSetConfigParams,
  SessionListResult,
  SessionSummary,
  SessionSubscribeResult,
} from "@imperium/protocol";

import { DaemonClient, type DaemonConnection } from "./daemon/client";
import { findDaemon, insideTauri, rememberDevConnection } from "./daemon/connection";
import { addColumn, applyDrop, reconcile, removeColumn, sameArrangement, visibleColumns, type Arrangement, type DropTarget } from "./layout/arrangement";
import { Columns, type PanelSpec } from "./layout/Columns";
import { PanelWindow } from "./layout/PanelWindow";
import { getLayout, resetLayout, updateLayout, type DetachedPanel } from "./layout/state";
import { downloadNotice, sessionNotice, updateNotice } from "./attention";
import { budgetNotice, budgetStop } from "./budgets";
import { focusMainWindow, notify, onNoticeOpened } from "./notify";
import {
  EnvironmentsPanel,
  SessionsPanel,
  environmentOf,
  type EnvironmentActions,
  type SessionSort,
} from "./panels/NavigationPanels";
import { SessionPanel } from "./panels/SessionPanel";
import { NewSessionPanel, type NewSessionOptions } from "./panels/NewSessionPanel";
import { AgentsPanel } from "./panels/AgentsPanel";
import { AgentView } from "./panels/AgentView";
import { agentDelivery, finishedMeanwhile, followUpText } from "./agentView";
import { queueParams, type QueueChange } from "./queue";
import { ModBus, ModFrame } from "./mods/ModFrame";
import { ApprovalsPanel } from "./panels/ApprovalsPanel";
import { BrowserPanel } from "./panels/BrowserPanel";
import { LocalModelsPanel } from "./panels/LocalModelsPanel";
import { ModelsPanel } from "./panels/ModelsPanel";
import { TelemetryPanel } from "./panels/TelemetryPanel";
import { ResourcesPanel } from "./panels/ResourcesPanel";
import { UsagePanel } from "./panels/UsagePanel";
import { environmentIcon, projectName, sameFolder, useProjectIcons, type ProjectIcons } from "./projectIcons";
import {
  applyEvent,
  DEFAULT_RESUME_POLICY,
  markAnswered,
  markQuestionsAnswered,
  markSent,
  newSession,
  sessionTitle,
  snapshotSessions,
  updateSent,
  type SessionState,
} from "./sessions";
import {
  applySettings,
  ChangeSettingsContext,
  loadSettings,
  modEnabled,
  resolvedTheme,
  saveSettings,
  SettingsContext,
  type AppSettings,
  type ChangeSettings,
} from "./settings";
import { PANELS_ICON, SETTINGS_SECTIONS, SettingsWindow } from "./SettingsWindow";
import { appCommands } from "./commands/appCommands";
import { CommandPalette, useRegisterCommands } from "./commands/CommandPalette";
import { emitPanelEvent, detachedPanelId, listenPanelEvents, openPanelWindow } from "./panelWindows";
import { ConnectorsWindow } from "./ConnectorsWindow";
import { EnvironmentWindow } from "./EnvironmentWindow";
import { getDoc, setDoc, subscribeDoc } from "./store";
import { type Daemon, useDaemon } from "./DaemonControl";
import { ModelConfigDialog, type ModelDialogTarget } from "./ModelConfigDialog";
import { setModelAliases } from "./modelNames";
import { hiddenSpecs } from "./modelPrefs";
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
  /** Environments with a folder in a git repository: new sessions there may start on worktrees. */
  const [gitEnvironments, setGitEnvironments] = useState<string[]>([]);
  const [selected, setSelected] = useState<string | null>(restore ? saved.session : null);
  /** A subagent whose log the main panel shows in place of its session's transcript. */
  const [agentView, setAgentView] = useState<{ sessionId: string; agentId: string } | null>(null);
  const [hidden, setHidden] = useState<string[]>(saved.hidden);
  const [detached, setDetached] = useState<Record<string, DetachedPanel>>(saved.detached);
  /** Which panels sit in which columns, as the panel editor left them (null: the default). */
  const [arrangement, setArrangement] = useState<Arrangement | null>(saved.arrangement);
  /** The panel editor: panels are moved only while it is on. */
  const [editingLayout, setEditingLayout] = useState(false);
  // Panels whose windows this session has opened, so each opens once.
  const opened = useRef(new Set<string>());
  const [showSettings, setShowSettings] = useState(false);
  /** The Settings section the command palette asked for. */
  const [settingsRequest, setSettingsRequest] = useState<{ section: string } | null>(null);
  const [modelDialog, setModelDialog] = useState<ModelDialogTarget | null>(null);
  const [showConnectors, setShowConnectors] = useState(false);
  const updates = useUpdates();
  const [modelTable, setModelTable] = useState<ModelListResult | null>(null);
  const [localModels, setLocalModels] = useState<LocalModelListResult | null>(null);
  // Every view shows a model by its alias (modelNames.ts).
  useEffect(() => setModelAliases(modelTable?.models ?? [], localModels?.models ?? []), [modelTable, localModels]);
  const [mods, setMods] = useState<ModListResult | null>(null);
  /** llama.cpp being installed (`runtime.install_progress`), named in a waiting agent's status line. */
  const [runtimeInstall, setRuntimeInstall] = useState<RuntimeInstallJob | null>(null);
  const [approvals, setApprovals] = useState<ApprovalInfo[]>([]);
  /** Where each budget stands (`budgets.get`, then `budgets.changed`), for the session's indicator. */
  const [budgets, setBudgets] = useState<BudgetsResult | null>(null);
  const modBus = useMemo(() => new ModBus(), []);
  // The latest settings and sessions, for the daemon's notification handler (set up once per connection).
  const settingsRef = useRef(settings);
  settingsRef.current = settings;
  const sessionsRef = useRef(sessions);
  sessionsRef.current = sessions;
  // Downloads already announced, so repeated progress reports for a finished one stay quiet.
  const announcedDownloads = useRef(new Set<string>());
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
        if (notification.method === "approval.requested") {
          const approval = notification.params as ApprovalInfo;
          if (!isDetached) {
            void notify({ kind: "approval", title: "Approval needed", body: approval.summary }, settingsRef.current);
          }
          setApprovals((all) => [...all.filter((a) => a.id !== approval.id), approval]);
          return;
        }
        if (notification.method === "budgets.changed") {
          setBudgets(notification.params as BudgetsResult);
          return;
        }
        if (notification.method === "budgets.alert") {
          if (!isDetached) void notify(budgetNotice(notification.params as BudgetAlert), settingsRef.current);
          return;
        }
        if (notification.method === "environments.changed") {
          const listing = notification.params as EnvironmentListResult;
          setEnvironments(listing.environments);
          setGitEnvironments(listing.git ?? []);
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
              [summary.session_id]: history.reduce((state, event) => applyEvent(state, event), newSession(summary, Date.now())),
            }));
          }, 500);
          return;
        }
        if (notification.method === "runtime.install_progress") {
          setRuntimeInstall(notification.params as RuntimeInstallJob);
          return;
        }
        if (notification.method === "models.download_progress") {
          const job = notification.params as DownloadJob;
          const notice = downloadNotice(job);
          if (notice && !isDetached && !announcedDownloads.current.has(job.job_id)) {
            announcedDownloads.current.add(job.job_id);
            void notify(notice, settingsRef.current);
          }
          return;
        }
        if (notification.method !== "session.event") return;
        const params = notification.params as SessionEventParams;
        modBus.emit(params);
        const session = sessionsRef.current[params.session_id];
        const notice =
          !isDetached && session
            ? sessionNotice(params.session_id, sessionTitle(session), params.agent_id, params.event)
            : null;
        if (notice) void notify(notice, settingsRef.current);
        if (pending) pending.push(params);
        else apply(params);
      });
      client.onClose(() => {
        if (attempt === generation.current) setStatus({ kind: "failed", message: "Lost the connection to imperium-core." });
      });
      // The models first, so notices in restored histories name models by their aliases.
      const table = await client.call<ModelListResult>("models.list");
      const local = await client.call<LocalModelListResult>("models.local");
      setModelAliases(table.models, local.models);
      const { sessions: running } = await client.call<SessionListResult>("session.list");
      const restored = await Promise.all(
        running.map(async (summary, index) => {
          const { history } = await client.call<SessionSubscribeResult>("session.subscribe", {
            session_id: summary.session_id,
          });
          // Restored sessions keep the daemon's order (later ones first) until they change here.
          return [summary.session_id, history.reduce((state, event) => applyEvent(state, event), newSession(summary, index))] as const;
        }),
      );
      known.current = new Set(restored.map(([id]) => id));
      setSessions(Object.fromEntries(restored));
      setModelTable(table);
      setLocalModels(local);
      setMods(await client.call<ModListResult>("mods.list"));
      setApprovals((await client.call<ApprovalListResult>("approvals.list")).approvals);
      // Older daemons have no budgets.
      client
        .call<BudgetsResult>("budgets.get", {})
        .then((result) => attempt === generation.current && setBudgets(result))
        .catch(() => {});
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
      // Which environments can start on worktrees; older daemons do not say.
      client
        .call<EnvironmentListResult>("environments.list")
        .then((listing) => attempt === generation.current && setGitEnvironments(listing.git ?? []))
        .catch(() => {});
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

  // Remember the selection, the arrangement, closed panels, and detached windows. Only the main
  // window writes the layout.
  useEffect(() => {
    if (isDetached) return;
    updateLayout({ environment: envId, session: selected, projectView: envView, arrangement, hidden, detached, sessionSort });
  }, [isDetached, envId, selected, envView, arrangement, hidden, detached, sessionSort]);

  // Esc leaves the panel editor, unless a dialog is open (it closes that instead).
  useEffect(() => {
    if (!editingLayout) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== "Escape" || event.defaultPrevented || document.querySelector("[aria-modal='true']")) return;
      setEditingLayout(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [editingLayout]);

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
  const openAgent = session && agentView?.sessionId === selected ? agentView.agentId : null;
  const viewedTask = openAgent ? (session?.tasks[openAgent] ?? null) : null;
  const client = status.kind === "connected" ? status.client : null;

  // The daemon keeps a copy of the hidden models (`models.hidden`), so agents' list_models
  // leaves them out too. Only the main window writes it, and only when it differs.
  const hiddenKey = hiddenSpecs(settings.modelMarks).join("\n");
  useEffect(() => {
    if (!client || isDetached) return;
    const hidden = hiddenKey === "" ? [] : hiddenKey.split("\n");
    let live = true;
    client
      .call<ModelConfigResult>("models.config")
      .then((config) => {
        if (!live || [...(config.hidden ?? [])].sort().join("\n") === hiddenKey) return;
        const params: ModelSetConfigParams = { local: [], table: [], hidden, dry_run: false };
        return client.call("models.set_config", params);
      })
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [client, isDetached, hiddenKey]);

  const modHost = useMemo(
    () => ({ sessions: snapshotSessions(allSessions, environments), selected, client }),
    [allSessions, environments, selected, client],
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

  const refreshModels = () => {
    client?.call<ModelListResult>("models.list").then(setModelTable, report);
    refreshLocalModels();
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

  /** Renames a session; `session.updated` brings the title it keeps (generated again when cleared). */
  const renameSession = (sessionId: string, title: string) => {
    if (title)
      setSessions((all) =>
        all[sessionId] ? { ...all, [sessionId]: { ...all[sessionId], summary: { ...all[sessionId].summary, title } } } : all,
      );
    control(sessionId, { action: "rename", title }).catch(report);
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

  /** Creates a session from the new-session composer and sends its first message; rejects with the daemon's reason. */
  const createSession = async ({ environmentId, model, worktree, text }: NewSessionOptions) => {
    const env = environments.find((e) => e.id === environmentId);
    if (!client || !env) throw new Error("Not connected to imperium-core.");
    const params: SessionCreateParams = {
      environment_id: env.id,
      ...(model ? { model } : {}),
      ...(worktree ? { worktree: true } : {}),
    };
    const created = await client.call<SessionCreateResult>("session.create", params);
    known.current.add(created.session_id);
    // The rest of the summary arrives with the daemon's session.created notice.
    const summary: SessionSummary = {
      session_id: created.session_id,
      project_dir: created.folders[0] ?? env.folders[0],
      environment_id: created.environment_id,
      folders: created.folders,
      model: created.model,
      isolation: created.isolation,
      title: "",
      status: "idle",
      status_detail: null,
      resume_policy: DEFAULT_RESUME_POLICY,
      background: false,
      hold: null,
      persistent: false,
      archived: false,
      archived_at: null,
      created_at: Date.now(),
      worktrees: created.worktrees,
      pinned: false,
    };
    setSessions((all) => ({ ...all, [created.session_id]: newSession(summary, Date.now()) }));
    setSelected(created.session_id);
    setEnvId(env.id);
    if (text) {
      const send: SessionSendParams = { session_id: created.session_id, text, mode: "queue" };
      void sendWithBudget(client, send);
    }
  };

  /** Opens the new-session composer in the middle panel. */
  const newSessionView = () => setSelected(null);

  /** Sends a message: it starts a turn, waits in the queue while one runs, or (steer) joins it. */
  const send = (text: string, mode: SendMode, attachments: Attachment[] = []) => {
    if (!client || !selected) return;
    const params: SessionSendParams = {
      session_id: selected,
      text,
      mode,
      ...(attachments.length > 0 ? { attachments } : {}),
    };
    void sendWithBudget(client, params);
  };

  /** Changes one session's state here, if it is listed. */
  const changeSession = (id: string, change: (session: SessionState) => SessionState) =>
    setSessions((all) => (all[id] ? { ...all, [id]: change(all[id]) } : all));

  /**
   * Sends a message. One that starts a turn gets a status line under it until the agent's
   * first output (waits.ts); when a used-up budget asks first, that line asks instead and
   * [`answerBudget`] sends it again confirmed or drops it. A blocking budget is reported.
   */
  const sendWithBudget = async (daemon: DaemonClient, params: SessionSendParams) => {
    const id = params.session_id;
    const current = sessionsRef.current[id];
    // A message for a busy session waits in the queue, which shows it; a new session is idle.
    const starts = params.mode !== "steer" && (!current || !current.busy);
    if (starts) changeSession(id, (session) => markSent(session, params.text));
    try {
      await daemon.call("session.send", params);
    } catch (error) {
      const stop = budgetStop(error);
      if (stop?.action === "ask") {
        changeSession(id, (session) => updateSent(session.sent ? session : markSent(session, params.text), stop.message));
        return;
      }
      changeSession(id, (session) => updateSent(session, undefined));
      if (!stop || stop.action === "block") report(error);
    }
  };

  /** Answers the selected session's budget question: send the waiting message anyway, or drop it. */
  const answerBudget = (confirm: boolean) => {
    const id = selected;
    const sent = id ? sessions[id]?.sent : null;
    if (!client || !id || !sent) return;
    if (!confirm) {
      changeSession(id, (session) => updateSent(session, undefined));
      return;
    }
    changeSession(id, (session) => updateSent(session, null));
    const params: SessionSendParams = { session_id: id, text: sent.text, mode: "queue", confirm_budget: true };
    client.call("session.send", params).catch((error: unknown) => {
      changeSession(id, (session) => updateSent(session, undefined));
      report(error);
    });
  };

  /**
   * Edits, removes, or steers a queued message. Every window, detached panels included, follows
   * the daemon's `queue_changed`, so the change shows in all of them; a refused change (editing a
   * message that is steering, or one already delivered) is reported.
   */
  const changeQueued = (id: string, change: QueueChange) => {
    if (!client || !selected) return;
    client.call("session.queue", queueParams(selected, id, change)).catch(report);
  };

  /** Opens a subagent's log in the main panel (`null`: back to the session's transcript). */
  const showAgent = (agentId: string | null) =>
    setAgentView(agentId && selected ? { sessionId: selected, agentId } : null);

  /**
   * Messages a subagent from its view. While it runs this is `session.send` with its
   * `agent_id`: queued for its next turn, or steering its running turn, as for the root. A
   * finished or paused subagent gets the message as a follow-up (`session.agent_resume`); so
   * does one that finished between the click and the call.
   */
  const sendToAgent = (agentId: string, text: string, mode: SendMode, attachments: Attachment[]) => {
    if (!client || !selected) return;
    const task = sessionsRef.current[selected]?.tasks[agentId];
    if (!task) return;
    const followUp = () => resumeAgent(agentId, followUpText(text, attachments)).catch(() => {});
    const delivery = agentDelivery(task.status, mode);
    if (delivery.kind === "resume") {
      void followUp();
      return;
    }
    const params: SessionSendParams = {
      session_id: selected,
      text,
      mode: delivery.mode,
      agent_id: agentId,
      ...(attachments.length > 0 ? { attachments } : {}),
    };
    client.call("session.send", params).catch((error: unknown) => {
      if (finishedMeanwhile(error)) void followUp();
      else report(error);
    });
  };

  /** Edits, removes, or steers a message waiting for a running subagent. */
  const changeAgentQueued = (agentId: string, id: string, change: QueueChange) => {
    if (!client || !selected) return;
    client.call("session.queue", queueParams(selected, id, change, agentId)).catch(report);
  };

  /** Ends a paused subagent without resuming it; it reports through `task_finished`. */
  const cancelAgent = (agentId: string): Promise<unknown> => {
    if (!client || !selected) return Promise.reject(new Error("Not connected to imperium-core."));
    const params: SessionAgentCancelParams = { session_id: selected, agent_id: agentId };
    return client.call("session.agent_cancel", params).catch((error: unknown) => {
      report(error);
      throw error;
    });
  };

  /** Gives a finished subagent a follow-up; it reports through `task_resumed` and `task_finished`. */
  const resumeAgent = (agentId: string, text: string): Promise<unknown> => {
    if (!client || !selected) return Promise.reject(new Error("Not connected to imperium-core."));
    const params: SessionAgentResumeParams = { session_id: selected, agent_id: agentId, text };
    return client.call("session.agent_resume", params).catch((error: unknown) => {
      report(error);
      throw error;
    });
  };

  const respond = (requestId: number, decision: PermissionDecision) => {
    if (!client || !selected) return;
    setSessions((all) => ({ ...all, [selected]: markAnswered(all[selected], requestId) }));
    client.call("session.respond", { session_id: selected, request_id: requestId, decision }).catch(report);
  };

  /** "Always allow": saves the suggested rule (in the session's environment), then allows the request. */
  const alwaysAllow = (requestId: number, rule: PermissionRule) => {
    if (!client || !selected) return;
    const sessionId = selected;
    client
      .call("permissions.add_rule", { session_id: sessionId, rule })
      .then(() => {
        setSessions((all) => ({ ...all, [sessionId]: markAnswered(all[sessionId], requestId) }));
        return client.call("session.respond", { session_id: sessionId, request_id: requestId, decision: "allow" });
      })
      .catch(report);
  };

  const answer = (requestId: number, answers: string[]) => {
    if (!client || !selected) return;
    setSessions((all) => ({ ...all, [selected]: markQuestionsAnswered(all[selected], requestId) }));
    client.call("session.answer", { session_id: selected, request_id: requestId, answers }).catch(report);
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

  /** Stops all of the selected session's work, or resumes it; the status follows `status_changed`. */
  const stopSession = (stop: boolean) => {
    if (selected) control(selected, { action: stop ? "stop" : "resume" }).catch(report);
  };

  /** Switches the selected session's model without loading it; the next message loads it and
   * compacts the conversation first if it does not fit.
   * Rejects with the daemon's reason, which the model selector shows beside itself. */
  const setModel = async (model: string): Promise<SessionControlResult> => {
    if (!selected) throw new Error("No session is open.");
    const id = selected;
    const result = await control(id, { action: "set_model", model });
    const now = result.model ?? model;
    setSessions((all) => (all[id] ? { ...all, [id]: { ...all[id], summary: { ...all[id].summary, model: now } } } : all));
    return result;
  };

  // The session search hit to scroll to, until another session is chosen.
  const [searchFocus, setSearchFocus] = useState<{ sessionId: string; index: number; nonce: number } | null>(null);
  const selectSession = (sessionId: string) => {
    setSearchFocus(null);
    setSelected(sessionId);
    // New sessions start in the environment the user is working in.
    const picked = sessions[sessionId];
    const env = picked ? environmentOf(picked, environments) : null;
    if (env) setEnvId(env.id);
  };

  const openSearchHit = (sessionId: string, index: number) => {
    selectSession(sessionId);
    setSearchFocus({ sessionId, index, nonce: Date.now() });
  };

  const updateSettings = (next: AppSettings) => {
    setSettings(next);
    saveSettings(next);
  };
  // For panels that change a setting of their own (model favourites and hidden models).
  const changeSettings: ChangeSettings = (change) =>
    setSettings((current) => {
      const next = change(current);
      saveSettings(next);
      return next;
    });

  // Approvals, finished and failed turns, and downloads notify from the daemon's notification
  // handler above; an update notifies once its build has downloaded.
  const announcedUpdate = useRef<string | null>(null);
  const readyVersion = updates?.status.state === "ready" ? updates.status.version : null;
  useEffect(() => {
    if (isDetached || !readyVersion || announcedUpdate.current === readyVersion) return;
    announcedUpdate.current = readyVersion;
    void notify(updateNotice(readyVersion), settingsRef.current);
  }, [readyVersion, isDetached]);

  // Answering a notification brings the main window forward on the session it was about.
  const selectSessionRef = useRef(selectSession);
  selectSessionRef.current = selectSession;
  useEffect(() => {
    if (isDetached) return;
    return onNoticeOpened((notice) => {
      void focusMainWindow();
      if (notice.sessionId && sessionsRef.current[notice.sessionId]) selectSessionRef.current(notice.sessionId);
    });
  }, [isDetached]);

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
        <button
          onClick={newSessionView}
          disabled={!client}
          title={client ? "Start a session: choose its options and write the first message in the middle panel" : "Not connected to imperium-core"}
        >
          <Plus aria-hidden="true" /> New session
        </button>
      ),
      body: (
        <SessionsPanel
          sessions={allSessions}
          environments={environments}
          scope={envView ? environment : null}
          selected={selected}
          sort={sessionSort}
          onSort={setSessionSort}
          onSelect={selectSession}
          onSetPinned={setSessionPinned}
          onSetArchived={setSessionArchived}
          onRename={renameSession}
          onShowEnvironment={showEnvironment}
          onShowAll={() => setEnvView(false)}
          client={client}
          onOpenHit={openSearchHit}
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
    body: session && viewedTask ? (
      <AgentView
        key={`${session.summary.session_id}/${viewedTask.taskId}`}
        session={session}
        task={viewedTask}
        onBack={() => showAgent(null)}
        onSend={(text, mode, attachments) => sendToAgent(viewedTask.taskId, text, mode, attachments)}
        onChangeQueued={(id, change) => changeAgentQueued(viewedTask.taskId, id, change)}
        onRespond={respond}
        onAlwaysAllow={alwaysAllow}
        onAnswer={answer}
        onResume={client ? resumeAgent : undefined}
        onCancel={client ? cancelAgent : undefined}
        onStop={client ? () => stopSession(true) : undefined}
        client={client}
        runtimeInstall={runtimeInstall}
      />
    ) : session ? (
      <SessionPanel
        session={session}
        icons={icons}
        onSend={send}
        onChangeQueued={changeQueued}
        onRespond={respond}
        onAlwaysAllow={alwaysAllow}
        onAnswer={answer}
        onSetResumePolicy={setResumePolicy}
        onResumeNow={resumeNow}
        onStop={stopSession}
        models={modelTable?.models ?? []}
        localModels={localModels?.models ?? []}
        localRuntime={localModels?.runtime ?? null}
        onOpenLocalModels={() => togglePanel("local-models", true)}
        onSetModel={setModel}
        client={client}
        onUnarchive={() => setSessionArchived(session.summary.session_id, false)}
        budgets={budgets?.status ?? []}
        focusEvent={searchFocus?.sessionId === session.summary.session_id ? searchFocus : null}
        runtimeInstall={runtimeInstall}
        onBudget={answerBudget}
        onOpenAgent={showAgent}
      />
    ) : (
      <NewSessionPanel
        environments={environments}
        gitEnvironments={gitEnvironments}
        environmentId={envId}
        onSelectEnvironment={setEnvId}
        models={modelTable?.models ?? []}
        localModels={localModels?.models ?? []}
        defaultModel={localModels ? (localModels.default_model ?? null) : undefined}
        client={client}
        onCreate={createSession}
        onModelsChanged={refreshModels}
        onOpenLocalModels={() => togglePanel("local-models", true)}
      />
    ),
  };
  const rightPanels: PanelSpec[] = [
    {
      id: "agents",
      title: "Agents",
      body: (
        <AgentsPanel
          session={session}
          openAgent={viewedTask ? openAgent : null}
          onOpen={selected ? (agentId) => showAgent(agentId === openAgent ? null : agentId) : undefined}
          onResume={client && selected ? resumeAgent : undefined}
          onCancel={client && selected ? cancelAgent : undefined}
        />
      ),
    },
    { id: "models", title: "Models", body: <ModelsPanel table={modelTable} onConfigure={() => setModelDialog({ tab: "table" })} /> },
    {
      id: "local-models",
      title: "Local models",
      body: (
        <LocalModelsPanel
          listing={localModels}
          onRefresh={refreshLocalModels}
          client={client}
          onConfigure={(id) => setModelDialog({ tab: "local", id })}
        />
      ),
    },
    { id: "telemetry", title: "Telemetry", body: <TelemetryPanel sessions={allSessions} models={modelTable?.models ?? []} /> },
    { id: "resources", title: "Resources", body: <ResourcesPanel client={client} /> },
    { id: "usage", title: "Usage", body: <UsagePanel client={client} /> },
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

  // The command palette's commands, run with the handlers above. A panel window's own copy of
  // the layout state goes stale, so it reads where panels are from the shared layout.
  useRegisterCommands("app", () => {
    const layout = isDetached ? getLayout() : { hidden, detached };
    return appCommands({
      connected: !!client,
      sessions: allSessions.map((s) => ({
        id: s.summary.session_id,
        title: sessionTitle(s),
        environment: environmentOf(s, environments)?.name ?? null,
        archived: s.summary.archived,
        pinned: s.summary.pinned,
      })),
      current: session && {
        id: session.summary.session_id,
        title: sessionTitle(session),
        model: session.summary.model,
        archived: session.summary.archived,
        pinned: session.summary.pinned,
      },
      environments,
      models: modelTable?.models ?? [],
      localModels: localModels?.models ?? [],
      modelMarks: settings.modelMarks,
      panels: [...PANELS.filter((p) => p.id !== "approvals" || approvals.length > 0), { id: "session", title: "Session" }, ...[...leftPanels, ...rightPanels].filter((p) => p.id.startsWith("mod:"))].map((p) => ({
        id: p.id,
        title: p.title,
        state: layout.detached[p.id] ? "detached" : layout.hidden.includes(p.id) ? "closed" : "docked",
        closable: p.id !== "session",
      })),
      settingsSections: SETTINGS_SECTIONS,
      theme: resolvedTheme(settings.theme),
      canCheckUpdates: !!updates,
      newSession: (environmentId) => {
        if (environmentId) setEnvId(environmentId);
        newSessionView();
      },
      newEnvironment: () => setEnvWindow({ id: null }),
      showEnvironment: (id) => {
        showEnvironment(id);
        setSelected(null);
      },
      editEnvironment: envActions.onEdit,
      selectSession,
      setPinned: setSessionPinned,
      setArchived: setSessionArchived,
      compact: (id) => void control(id, { action: "compact" }).catch(report),
      setModel: (spec) => void setModel(spec).catch(report),
      openSettings: (section) => {
        setSettingsRequest(section ? { section } : null);
        setShowSettings(true);
      },
      openConnectors: () => setShowConnectors(true),
      configureModels: () => setModelDialog({ tab: "local" }),
      openPanel: (id) => {
        const spec = findPanel(id);
        if (detached[id] && spec) void openPanelWindow(id, spec.title, detached[id], () => forget(id));
        else togglePanel(id, true);
      },
      closePanel,
      detachPanel: detach,
      dockPanel: dock,
      toggleTheme: () => updateSettings({ ...settings, theme: resolvedTheme(settings.theme) === "dark" ? "light" : "dark" }),
      checkUpdates: () => updates?.check(),
    });
  });

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
      <ChangeSettingsContext.Provider value={changeSettings}>
        <div className="app app-panel">
          {status.kind !== "connected" && <ConnectionBanner status={status} onConnect={connect} daemon={null} />}
          <PanelWindow id={detachedId} panel={findPanel(detachedId) ?? null} />
          <CommandPalette detached />
        </div>
      </ChangeSettingsContext.Provider>
      </SettingsContext.Provider>
    );
  }

  // The default arrangement: environments and sessions on the left, the session in the middle,
  // the rest on the right. The saved one (from the panel editor) wins, with new panels added.
  const defaultArrangement: Arrangement = [
    { id: "left", panels: [...leftPanels.map((panel) => panel.id), approvalsPanel.id] },
    { id: "main", panels: [sessionPanel.id] },
    { id: "right", panels: rightPanels.map((panel) => panel.id) },
  ];
  const layoutColumns = reconcile(arrangement, defaultArrangement);
  const shownPanels = new Map<string, PanelSpec>(
    visible([...leftPanels, ...(approvals.length > 0 ? [approvalsPanel] : []), ...rightPanels]).map((panel) => [panel.id, panel]),
  );
  if (!detached[sessionPanel.id]) {
    shownPanels.set(sessionPanel.id, { ...sessionPanel, span: true, onDetach: () => detach(sessionPanel.id) });
  }
  const columns = visibleColumns(layoutColumns, (id) => shownPanels.get(id), editingLayout);
  /** Keeps an arrangement the editor changed; an unchanged one is not written. */
  const arrange = (next: Arrangement) => {
    if (!sameArrangement(next, layoutColumns)) setArrangement(next);
  };
  const newColumnId = () => `column-${Date.now().toString(36)}`;

  return (
    <SettingsContext.Provider value={settings}>
      <ChangeSettingsContext.Provider value={changeSettings}>
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
          className={`ghost titlebar-button icon-only layout-toggle${editingLayout ? " active" : ""}`}
          onClick={() => setEditingLayout((editing) => !editing)}
          aria-pressed={editingLayout}
          aria-label="Edit layout"
          title={editingLayout ? "Done editing the layout (Esc)" : "Edit layout"}
        >
          <PANELS_ICON aria-hidden="true" />
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
          onOpenModels={() => {
            setShowSettings(false);
            setModelDialog({ tab: "local" });
          }}
          onModelsChanged={refreshModels}
          onResetLayout={() => {
            setArrangement(null);
            resetLayout();
            window.location.reload();
          }}
          onClose={() => setShowSettings(false)}
          request={settingsRequest}
        />
      )}
      <CommandPalette detached={false} />
      {modelDialog && (
        <ModelConfigDialog client={client} target={modelDialog} onClose={() => setModelDialog(null)} onSaved={refreshModels} />
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
      {editingLayout && (
        <div className="layout-editor-bar" role="region" aria-label="Layout editor">
          <span className="muted">Drag a panel by its header to move it; drop it at a column's edge to start a new column.</span>
          <button className="ghost small" onClick={() => arrange(addColumn(layoutColumns, editingLayout, newColumnId))}>
            <Plus aria-hidden="true" /> Column
          </button>
          <button className="primary small" onClick={() => setEditingLayout(false)}>
            <Check aria-hidden="true" /> Done
          </button>
        </div>
      )}
      <main className={`workspace${editingLayout ? " editing-layout" : ""}`}>
        <Columns
          initial={arrangement === null && columns.length === 3 ? [0.2, 0.55, 0.25] : undefined}
          columns={columns}
          editing={editingLayout}
          onDrop={(panel: string, target: DropTarget) => arrange(applyDrop(layoutColumns, panel, target, editingLayout, newColumnId))}
          onRemoveColumn={(column) => arrange(removeColumn(layoutColumns, column, (id) => shownPanels.has(id), editingLayout))}
        />
      </main>
    </div>
    </ChangeSettingsContext.Provider>
      </SettingsContext.Provider>
  );
}

/** Panels the user can close and reopen from Settings → Panels. */
const PANELS: { id: string; title: string }[] = [
  { id: "projects", title: "Environments" },
  { id: "sessions", title: "Sessions" },
  { id: "approvals", title: "Approvals" },
  { id: "agents", title: "Agents" },
  { id: "models", title: "Models" },
  { id: "local-models", title: "Local models" },
  { id: "telemetry", title: "Telemetry" },
  { id: "resources", title: "Resources" },
  { id: "usage", title: "Usage" },
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
