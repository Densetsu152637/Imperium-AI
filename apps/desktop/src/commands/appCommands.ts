// The app's own commands for the palette: actions, sessions, environments, models, panels, and
// settings sections. App.tsx registers them with its current state and the handlers it already
// uses for the same things, so the palette adds no second way of doing anything.

import {
  AppWindow,
  Archive,
  ArchiveRestore,
  Boxes,
  Cpu,
  Download,
  FolderOpen,
  FolderPlus,
  Gauge,
  Layers,
  MessageSquare,
  MessageSquarePlus,
  Minimize2,
  PanelRightClose,
  PanelTop,
  Pin,
  PinOff,
  Plug,
  Settings,
  SunMoon,
  X,
  type LucideIcon,
} from "lucide-react";

import type { EnvironmentInfo, LocalModelRow, ModelRow } from "@imperium/protocol";

import type { Command } from "./registry";

/** What the palette needs from the app: its state, and the handlers it already has. */
export type AppCommandContext = {
  connected: boolean;
  /** Every session, with its title and environment's name. */
  sessions: { id: string; title: string; environment: string | null; archived: boolean; pinned: boolean }[];
  /** The open session, if any. */
  current: { id: string; title: string; model: string | null; archived: boolean; pinned: boolean } | null;
  environments: EnvironmentInfo[];
  models: ModelRow[];
  localModels: LocalModelRow[];
  /** Panels that can be opened, closed, or detached, with where each is now. */
  panels: { id: string; title: string; state: "docked" | "closed" | "detached"; closable: boolean }[];
  /** Settings sections by id. */
  settingsSections: { id: string; label: string }[];
  theme: "light" | "dark";
  canCheckUpdates: boolean;
  newSession: (environmentId?: string) => void;
  newEnvironment: () => void;
  showEnvironment: (id: string) => void;
  editEnvironment: (id: string) => void;
  selectSession: (id: string) => void;
  setPinned: (id: string, pinned: boolean) => void;
  setArchived: (id: string, archived: boolean) => void;
  compact: (id: string) => void;
  setModel: (spec: string) => void;
  openSettings: (section?: string) => void;
  openConnectors: () => void;
  configureModels: () => void;
  openPanel: (id: string) => void;
  closePanel: (id: string) => void;
  detachPanel: (id: string) => void;
  dockPanel: (id: string) => void;
  toggleTheme: () => void;
  checkUpdates: () => void;
};

/** Icons for the panels that have an obvious one; the rest use a generic panel icon. */
const PANEL_ICONS: Record<string, LucideIcon> = {
  usage: Gauge,
  "local-models": Cpu,
  models: Boxes,
  sessions: MessageSquare,
  projects: FolderOpen,
};

/** The models a session can switch to, by spec: usable table rows, then local models the table does not list. */
function modelChoices(models: ModelRow[], local: LocalModelRow[]): { spec: string; label: string; detail: string }[] {
  const choices = models
    .filter((row) => row.enabled && row.endpoint_configured)
    .map((row) => ({ spec: row.spec, label: row.id, detail: [row.id === row.spec ? null : row.spec, row.location, row.tier].filter(Boolean).join(" · ") }));
  for (const row of local) {
    const spec = `${row.runtime}:${row.id}`;
    if (row.problem || choices.some((choice) => choice.spec === spec)) continue;
    choices.push({ spec, label: row.id, detail: [row.quantization, "local"].filter(Boolean).join(" · ") });
  }
  return choices;
}

export function appCommands(app: AppCommandContext): Command[] {
  const commands: Command[] = [];
  const add = (command: Command) => commands.push(command);
  const { current } = app;

  // ---- Actions ----
  if (app.connected) {
    add({ id: "session.new", group: "actions", title: "New session", keywords: ["start", "create", "chat"], icon: MessageSquarePlus, run: () => app.newSession() });
    for (const env of app.environments) {
      add({
        id: `session.new:${env.id}`,
        group: "actions",
        title: `New session in ${env.name}`,
        keywords: ["start", "create", "environment"],
        icon: MessageSquarePlus,
        run: () => app.newSession(env.id),
      });
    }
    add({ id: "environment.new", group: "actions", title: "New environment", keywords: ["create", "project", "folder"], icon: FolderPlus, run: app.newEnvironment });
  }
  if (current && app.connected) {
    const { id } = current;
    if (!current.archived) {
      add({
        id: "session.compact",
        group: "actions",
        title: "Compact conversation",
        subtitle: current.title,
        keywords: ["summarise", "summarize", "context", "shrink"],
        icon: Minimize2,
        run: () => app.compact(id),
      });
    }
    add({
      id: current.archived ? "session.unarchive" : "session.archive",
      group: "actions",
      title: current.archived ? "Unarchive session" : "Archive session",
      subtitle: current.title,
      keywords: current.archived ? ["restore", "resume"] : ["hide", "close"],
      icon: current.archived ? ArchiveRestore : Archive,
      run: () => app.setArchived(id, !current.archived),
    });
    add({
      id: current.pinned ? "session.unpin" : "session.pin",
      group: "actions",
      title: current.pinned ? "Unpin session" : "Pin session",
      subtitle: current.title,
      icon: current.pinned ? PinOff : Pin,
      run: () => app.setPinned(id, !current.pinned),
    });
  }
  add({ id: "settings.open", group: "actions", title: "Open Settings", keywords: ["preferences", "options"], icon: Settings, shortcut: "Mod+,", run: () => app.openSettings() });
  add({ id: "connectors.open", group: "actions", title: "Open Connectors", keywords: ["mcp", "plugins", "skills", "servers"], icon: Plug, run: app.openConnectors });
  add({ id: "models.configure", group: "actions", title: "Configure models", keywords: ["local models", "model table", "endpoints"], icon: Boxes, run: app.configureModels });
  add({
    id: "theme.toggle",
    group: "actions",
    title: app.theme === "dark" ? "Switch to light theme" : "Switch to dark theme",
    keywords: ["toggle theme", "appearance", "dark mode", "light mode"],
    icon: SunMoon,
    stayInWindow: true,
    run: app.toggleTheme,
  });
  if (app.canCheckUpdates) {
    add({ id: "updates.check", group: "actions", title: "Check for updates", keywords: ["upgrade", "version"], icon: Download, run: app.checkUpdates });
  }

  // ---- Sessions: the open ones, then the archived ones ----
  for (const session of app.sessions) {
    add({
      id: `session:${session.id}`,
      group: session.archived ? "archived" : "sessions",
      title: session.title,
      subtitle: session.environment ?? undefined,
      keywords: session.pinned ? ["pinned"] : undefined,
      icon: session.archived ? Archive : MessageSquare,
      run: () => app.selectSession(session.id),
    });
  }

  // ---- Environments ----
  for (const env of app.environments) {
    add({
      id: `environment:${env.id}`,
      group: "environments",
      title: env.name,
      subtitle: env.folders.join(", "),
      icon: FolderOpen,
      run: () => app.showEnvironment(env.id),
    });
    add({
      id: `environment.edit:${env.id}`,
      group: "environments",
      title: `Edit environment ${env.name}`,
      keywords: ["settings", "configure", "folders", "permissions"],
      icon: Settings,
      run: () => app.editEnvironment(env.id),
    });
  }

  // ---- Models: switch the open session's model ----
  if (current && app.connected && !current.archived) {
    for (const choice of modelChoices(app.models, app.localModels)) {
      if (choice.spec === current.model || choice.label === current.model) continue;
      add({
        id: `model:${choice.spec}`,
        group: "models",
        title: `Use ${choice.label}`,
        subtitle: choice.detail,
        keywords: ["switch model", choice.spec],
        icon: Boxes,
        run: () => app.setModel(choice.spec),
      });
    }
  }

  // ---- Panels ----
  for (const panel of app.panels) {
    const icon = PANEL_ICONS[panel.id] ?? PanelTop;
    const keywords = ["panel", "show", "view"];
    if (panel.state === "closed") {
      add({ id: `panel.open:${panel.id}`, group: "panels", title: `Open ${panel.title}`, keywords, icon, run: () => app.openPanel(panel.id) });
      continue;
    }
    if (panel.state === "detached") {
      add({ id: `panel.open:${panel.id}`, group: "panels", title: `Show ${panel.title} window`, keywords: [...keywords, "open", "focus"], icon, run: () => app.openPanel(panel.id) });
      add({ id: `panel.dock:${panel.id}`, group: "panels", title: `Dock ${panel.title}`, keywords: [...keywords, "attach"], icon: PanelRightClose, run: () => app.dockPanel(panel.id) });
      continue;
    }
    if (panel.closable) {
      add({ id: `panel.close:${panel.id}`, group: "panels", title: `Close ${panel.title}`, keywords: [...keywords, "hide"], icon: X, run: () => app.closePanel(panel.id) });
    }
    add({ id: `panel.detach:${panel.id}`, group: "panels", title: `Detach ${panel.title}`, keywords: [...keywords, "window", "pop out"], icon: AppWindow, run: () => app.detachPanel(panel.id) });
  }

  // ---- Settings sections ----
  for (const section of app.settingsSections) {
    add({
      id: `settings:${section.id}`,
      group: "settings",
      title: `Settings: ${section.label}`,
      keywords: ["preferences", "open"],
      icon: section.id === "budgets" ? Gauge : Layers,
      run: () => app.openSettings(section.id),
    });
  }

  return commands;
}
