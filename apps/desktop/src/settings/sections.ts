// The sections of the Settings window, their order and icons.

import { Archive, Bell, Bot, Brain, Clock, Cpu, Download, Feather, Gauge, Globe, HardDrive, Image, Info, Keyboard, KeyRound, LayoutGrid, MemoryStick, MessageSquare, Network, Palette, PawPrint, Plug, Puzzle, Route, Server, Shield, ShieldCheck, SlidersHorizontal, Smartphone, Boxes, GitPullRequest, History, Workflow, type LucideIcon } from "lucide-react";

export const CONFIG_SECTIONS: Record<string, string> = {
  agents: "Agents",
  policy: "Personality",
  models: "Models",
  routing: "Routing",
  runtime: "Local runtime",
  hardware: "Hardware",
  memory: "Memory",
  workflow: "Workflow",
  web: "Web and browser",
  media: "Media",
  isolation: "Isolation",
  mcp: "MCP",
  extensions: "Extensions",
  github: "GitHub",
  pets: "Pets",
  phone: "Phone",
  resume: "Resuming",
  sessions: "Sessions",
  workspaces: "Working folders",
  daemon: "Network",
};

export type Section = { id: string; label: string; group: string };

export const APP_SECTIONS: Section[] = [
  { id: "general", label: "General", group: "Imperium" },
  { id: "appearance", label: "Appearance", group: "Imperium" },
  { id: "chat", label: "Chat", group: "Imperium" },
  { id: "notifications", label: "Notifications", group: "Imperium" },
  { id: "panels", label: "Panels", group: "Imperium" },
  { id: "mods", label: "Mods", group: "Imperium" },
  { id: "shortcuts", label: "Keyboard", group: "Imperium" },
  { id: "daemon", label: "Daemon", group: "Imperium" },
  { id: "updates", label: "Updates", group: "Imperium" },
  { id: "connectors", label: "Connectors", group: "Imperium" },
];

export const PERMISSIONS_SECTION: Section = { id: "permissions", label: "Permissions", group: "Agent and daemon" };

export const BUDGETS_SECTION: Section = { id: "budgets", label: "Budgets", group: "Agent and daemon" };

export const API_SECTION: Section = { id: "api", label: "API", group: "Agent and daemon" };

export const AGENT_CHANGES_SECTION: Section = { id: "agent-changes", label: "Changes by agents", group: "Agent and daemon" };

export const TAIL_SECTIONS: Section[] = [
  { id: "data", label: "Data and storage", group: "About" },
  { id: "about", label: "About", group: "About" },
];

/** The Panels section's icon, which the main window's panel-editor toggle uses too. */
export const PANELS_ICON: LucideIcon = LayoutGrid;

/** One icon per section, in the same line style as the rest of the app. */
export const SECTION_ICONS: Record<string, LucideIcon> = {
  general: SlidersHorizontal,
  appearance: Palette,
  chat: MessageSquare,
  notifications: Bell,
  panels: PANELS_ICON,
  shortcuts: Keyboard,
  daemon: Server,
  updates: Download,
  connectors: Plug,
  data: HardDrive,
  permissions: ShieldCheck,
  budgets: Gauge,
  api: KeyRound,
  "agent-changes": History,
  about: Info,
  "config:agents": Bot,
  "config:policy": Feather,
  "config:models": Boxes,
  "config:routing": Route,
  "config:runtime": Cpu,
  "config:hardware": MemoryStick,
  "config:memory": Brain,
  "config:workflow": Workflow,
  "config:web": Globe,
  "config:media": Image,
  "config:isolation": Shield,
  "config:mcp": Plug,
  "config:extensions": Puzzle,
  "config:github": GitPullRequest,
  "config:pets": PawPrint,
  "config:phone": Smartphone,
  "config:resume": Clock,
  "config:sessions": Archive,
  "config:daemon": Network,
};

/** Sections by id and label, for the command palette's "Settings: …" commands. */
export const SETTINGS_SECTIONS: { id: string; label: string }[] = [
  ...APP_SECTIONS,
  ...Object.entries(CONFIG_SECTIONS).map(([id, label]) => ({ id: `config:${id}`, label })),
  PERMISSIONS_SECTION,
  BUDGETS_SECTION,
  API_SECTION,
  AGENT_CHANGES_SECTION,
  ...TAIL_SECTIONS,
];
