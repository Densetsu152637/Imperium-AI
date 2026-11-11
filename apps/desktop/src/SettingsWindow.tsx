// The Settings window: a sidebar of sections and the selected section's settings, with search
// across all of them. App settings (appearance, chat, notifications, startup, panels) are kept
// in local files by the app; daemon settings (models, runtimes, tools, and the rest) are read
// and written through the daemon's config.describe / config.set into its config file. Both
// persist across versions.

import {
  Archive,
  Bell,
  Bot,
  Brain,
  Clock,
  Cpu,
  Download,
  Gauge,
  Globe,
  HardDrive,
  Image,
  Info,
  Keyboard,
  LayoutGrid,
  MemoryStick,
  MessageSquare,
  Network,
  Palette,
  PawPrint,
  Plug,
  Puzzle,
  Route,
  Search,
  Server,
  Shield,
  ShieldCheck,
  SlidersHorizontal,
  Smartphone,
  Boxes,
  GitPullRequest,
  Workflow,
  type LucideIcon,
} from "lucide-react";
import { useEffect, useMemo, useState, type CSSProperties, type ReactNode } from "react";

import type {
  ConfigDescribeResult,
  ConfigEntry,
  ConfigSetResult,
  ModListResult,
  PermissionSet,
  PermissionsResult,
} from "@imperium/protocol";

import { BudgetsSection } from "./components/BudgetsEditor";
import { PermissionsEditor, samePermissions } from "./components/PermissionsEditor";
import { Select } from "./components/Select";
import { matches, ModalNavGroup, ModalNavItem, SettingsModal } from "./components/SettingsModal";
import { Row } from "./components/SettingsRow";
import { Switch as Toggle } from "./components/Switch";
import type { DaemonClient } from "./daemon/client";
import { type Daemon, DaemonSettings } from "./DaemonControl";
import { preview, testNotification } from "./notify";
import { ACCENTS, DEFAULT_SETTINGS, BUNDLED_ON_BY_DEFAULT, modEnabled, type AppSettings } from "./settings";
import { storeFolder } from "./store";
import { type Updates, UpdateSettings } from "./UpdateControl";

type Props = {
  settings: AppSettings;
  onChange: (settings: AppSettings) => void;
  updates: Updates | null;
  daemon: Daemon | null;
  client: DaemonClient | null;
  mods: ModListResult | null;
  panels: { id: string; title: string }[];
  hidden: string[];
  /** Panels that are in windows of their own. */
  detached: string[];
  onTogglePanel: (id: string, open: boolean) => void;
  onDock: (id: string) => void;
  /** Opens the Connectors window (MCP servers, plugins, skills). */
  onOpenConnectors: () => void;
  /** Opens the model configuration dialog (local models and model-table rows). */
  onOpenModels: () => void;
  onResetLayout: () => void;
  onClose: () => void;
  /** A section to show, as asked for (by the command palette); each new request switches to it. */
  request?: { section: string } | null;
};

// ---- App settings, described as data so they render and search the same way ----

type Control =
  | { type: "toggle" }
  | { type: "select"; options: { value: string; label: string }[] }
  | { type: "swatches"; options: { value: string; label: string; color: string }[] }
  | { type: "slider"; min: number; max: number; step: number; format: (value: number) => string };

type Field = { key: keyof AppSettings; section: string; label: string; description: string; control: Control };

const percent = (value: number) => `${Math.round(value * 100)}%`;
const choices = (entries: [string, string][]) => entries.map(([value, label]) => ({ value, label }));

const FIELDS: Field[] = [
  // General
  {
    key: "restoreSession",
    section: "general",
    label: "Reopen where you left off",
    description: "Select the last project and session when Imperium opens.",
    control: { type: "toggle" },
  },
  {
    key: "autoStartDaemon",
    section: "general",
    label: "Start imperium-core with Imperium",
    description: "Start the bundled daemon when the app opens and none is running.",
    control: { type: "toggle" },
  },
  // Appearance
  {
    key: "theme",
    section: "appearance",
    label: "Theme",
    description: "Light or dark surfaces; System follows the operating system.",
    control: { type: "select", options: choices([["system", "System"], ["light", "Light"], ["dark", "Dark"]]) },
  },
  {
    key: "accent",
    section: "appearance",
    label: "Accent colour",
    description: "Buttons, highlights, and the connection light.",
    control: { type: "swatches", options: Object.entries(ACCENTS).map(([value, { label, base }]) => ({ value, label, color: base })) },
  },
  {
    key: "uiScale",
    section: "appearance",
    label: "Interface scale",
    description: "The size of everything outside messages and code.",
    control: { type: "slider", min: 0.5, max: 3, step: 0.05, format: percent },
  },
  {
    key: "proseScale",
    section: "appearance",
    label: "Message text",
    description: "The size of conversation text.",
    control: { type: "slider", min: 0.5, max: 3, step: 0.05, format: percent },
  },
  {
    key: "codeScale",
    section: "appearance",
    label: "Code text",
    description: "The size of code blocks and tool output.",
    control: { type: "slider", min: 0.5, max: 3, step: 0.05, format: percent },
  },
  {
    key: "maxProseCh",
    section: "appearance",
    label: "Line length",
    description: "The widest a line of message text gets, in characters.",
    control: { type: "slider", min: 50, max: 200, step: 5, format: (value) => `${value} ch` },
  },
  {
    key: "density",
    section: "appearance",
    label: "Density",
    description: "Spacing between and inside panels.",
    control: { type: "select", options: choices([["compact", "Compact"], ["comfortable", "Comfortable"], ["spacious", "Spacious"]]) },
  },
  {
    key: "proseFont",
    section: "appearance",
    label: "Message font",
    description: "The typeface for conversation text.",
    control: { type: "select", options: choices([["sans", "Sans-serif"], ["serif", "Serif"], ["mono", "Monospace"]]) },
  },
  {
    key: "reduceMotion",
    section: "appearance",
    label: "Reduce motion",
    description: "Turn off animations and transitions.",
    control: { type: "toggle" },
  },
  // Chat
  {
    key: "sendKey",
    section: "chat",
    label: "Send with",
    description: "Which key sends a message; the other inserts a new line.",
    control: { type: "select", options: choices([["enter", "Enter"], ["mod-enter", "Ctrl+Enter"]]) },
  },
  {
    key: "renderMarkdown",
    section: "chat",
    label: "Render Markdown",
    description: "Show agent replies formatted; off shows the raw text. Each session can still switch.",
    control: { type: "toggle" },
  },
  {
    key: "wrapCode",
    section: "chat",
    label: "Wrap long code lines",
    description: "Wrap code and tool output instead of scrolling sideways.",
    control: { type: "toggle" },
  },
  {
    key: "autoScroll",
    section: "chat",
    label: "Follow new output",
    description: "Scroll to the newest message as the agent writes.",
    control: { type: "toggle" },
  },
  // Notifications
  {
    key: "soundOnApproval",
    section: "notifications",
    label: "Sound when an agent needs approval",
    description: "A short two-note chime.",
    control: { type: "toggle" },
  },
  {
    key: "soundOnFinish",
    section: "notifications",
    label: "Sound when a session finishes",
    description: "A short falling chime when an agent stops working.",
    control: { type: "toggle" },
  },
  {
    key: "titleBadge",
    section: "notifications",
    label: "Mark the window title",
    description: "Put a dot before the title while something needs you and Imperium is in the background.",
    control: { type: "toggle" },
  },
  {
    key: "notifyOnlyInBackground",
    section: "notifications",
    label: "Only when Imperium is in the background",
    description: "Show system notifications only while no Imperium window has focus. Your system's do-not-disturb still applies.",
    control: { type: "toggle" },
  },
  {
    key: "notifyApproval",
    section: "notifications",
    label: "Notify when an agent needs approval",
    description: "A system notification when an agent asks to run a tool, asks you a question, or an action waits for approval.",
    control: { type: "toggle" },
  },
  {
    key: "notifyFinished",
    section: "notifications",
    label: "Notify when a session finishes",
    description: "A system notification when an agent finishes its turn and waits for you.",
    control: { type: "toggle" },
  },
  {
    key: "notifyFailed",
    section: "notifications",
    label: "Notify when a turn fails",
    description: "A system notification when an agent's turn stops on an error.",
    control: { type: "toggle" },
  },
  {
    key: "notifyBudget",
    section: "notifications",
    label: "Notify about budgets and usage",
    description: "A system notification when a budget reaches one of its alert thresholds (Settings → Budgets).",
    control: { type: "toggle" },
  },
  {
    key: "notifyDownload",
    section: "notifications",
    label: "Notify when a model download finishes",
    description: "A system notification when a downloaded model is ready to use.",
    control: { type: "toggle" },
  },
  {
    key: "notifyUpdate",
    section: "notifications",
    label: "Notify when an update is ready",
    description: "A system notification when a new version has downloaded and installs on restart.",
    control: { type: "toggle" },
  },
];

// ---- Daemon settings sections (config.describe groups by top-level table) ----

const CONFIG_SECTIONS: Record<string, string> = {
  agents: "Agents",
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
  daemon: "Network",
};

type Section = { id: string; label: string; group: string };

const APP_SECTIONS: Section[] = [
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
const PERMISSIONS_SECTION: Section = { id: "permissions", label: "Permissions", group: "Agent and daemon" };
const BUDGETS_SECTION: Section = { id: "budgets", label: "Budgets", group: "Agent and daemon" };
const TAIL_SECTIONS: Section[] = [
  { id: "data", label: "Data and storage", group: "About" },
  { id: "about", label: "About", group: "About" },
];

/** The Panels section's icon, which the main window's panel-editor toggle uses too. */
export const PANELS_ICON: LucideIcon = LayoutGrid;

/** One icon per section, in the same line style as the rest of the app. */
const SECTION_ICONS: Record<string, LucideIcon> = {
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
  about: Info,
  "config:agents": Bot,
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
  ...TAIL_SECTIONS,
];

const SHORTCUTS: [string, string][] = [
  ["Ctrl+K (Cmd+K on macOS)", "Open or close the command palette, in the main window and panel windows"],
  ["↑ / ↓, Enter, Tab, Esc", "In the palette: move, run the command, jump to the next group (Shift+Tab: previous), close"],
  ["Ctrl+,", "Open or close Settings"],
  ["Esc", "Close Settings"],
  ["Enter / Ctrl+Enter", "Send a message (see Chat)"],
  ["Shift+Enter", "New line in a message"],
];

export function SettingsWindow(props: Props) {
  const { settings, onChange, client, onClose } = props;
  const [section, setSection] = useState(props.request?.section ?? "general");
  const [query, setQuery] = useState("");

  useEffect(() => {
    if (!props.request) return;
    setSection(props.request.section);
    setQuery("");
  }, [props.request]);
  const [config, setConfig] = useState<ConfigDescribeResult | null>(null);
  const [configError, setConfigError] = useState<string | null>(null);
  const [needsRestart, setNeedsRestart] = useState(false);

  useEffect(() => {
    if (!client) return;
    client
      .call<ConfigDescribeResult>("config.describe", {})
      .then(setConfig)
      .catch((error: unknown) => setConfigError(String(error instanceof Error ? error.message : error)));
  }, [client]);

  const configSections: Section[] = useMemo(() => {
    const present = new Set((config?.entries ?? []).map((entry) => entry.section));
    return Object.entries(CONFIG_SECTIONS)
      .filter(([id]) => present.has(id) || !config)
      .map(([id, label]) => ({ id: `config:${id}`, label, group: "Agent and daemon" }));
  }, [config]);
  const sections = [...APP_SECTIONS, ...configSections, PERMISSIONS_SECTION, BUDGETS_SECTION, ...TAIL_SECTIONS];

  const set = <K extends keyof AppSettings>(key: K, value: AppSettings[K]) => onChange({ ...settings, [key]: value });

  const setConfigValue = async (key: string, value: unknown) => {
    if (!client) return;
    setConfigError(null);
    try {
      const result = await client.call<ConfigSetResult>("config.set", { key, value });
      setConfig((current) =>
        current && {
          ...current,
          entries: current.entries.map((entry) => (entry.key === key ? result.entry : entry)),
        },
      );
      if (result.restart) setNeedsRestart(true);
      if (result.overridden) setConfigError(`${key} is set by an environment variable, a project file, or the model configuration, which wins.`);
    } catch (error) {
      setConfigError(String(error instanceof Error ? error.message : error));
    }
  };

  const fieldRows = (fields: Field[]) => fields.map((field) => <FieldRow key={field.key} field={field} value={settings[field.key]} onChange={set} />);
  const configRows = (entries: ConfigEntry[]) =>
    entries.map((entry) => <ConfigRow key={entry.key} entry={entry} disabled={!client} onSet={setConfigValue} />);

  const body = (): ReactNode => {
    if (query.trim()) {
      const fields = FIELDS.filter((field) => matches(query, field.label, field.description, field.section));
      const entries = (config?.entries ?? []).filter((entry) =>
        matches(query, entry.label, entry.description, entry.key, CONFIG_SECTIONS[entry.section] ?? entry.section),
      );
      if (fields.length + entries.length === 0) return <p className="muted">No settings match “{query}”.</p>;
      return (
        <>
          {fieldRows(fields)}
          {configRows(entries)}
        </>
      );
    }
    if (section.startsWith("config:")) {
      const id = section.slice("config:".length);
      if (!client) return <p className="muted">Connect to imperium-core to change these settings.</p>;
      if (!config) return <p className="muted">Loading…</p>;
      const rows = configRows(config.entries.filter((entry) => entry.section === id));
      if (id !== "models" && id !== "runtime") return rows;
      return (
        <>
          <Row
            label="Model configuration"
            description="Context, KV pool, slots, cache types and CPU lane of each local model, and the model table's rows. Saved to the app's settings file, which wins over the user config."
          >
            <button className="primary" onClick={props.onOpenModels}>
              Configure models
            </button>
          </Row>
          {rows}
        </>
      );
    }
    switch (section) {
      case "panels":
        return (
          <>
            {props.panels.map((panel) => (
              <Row
                key={panel.id}
                label={panel.title}
                description={
                  props.detached.includes(panel.id)
                    ? "In its own window."
                    : "Shown in the workspace. Close it from its × as well."
                }
              >
                {props.detached.includes(panel.id) && <button onClick={() => props.onDock(panel.id)}>Dock</button>}
                <Toggle
                  checked={!props.hidden.includes(panel.id)}
                  onChange={(open) => props.onTogglePanel(panel.id, open)}
                  label={panel.title}
                />
              </Row>
            ))}
            <Row label="Reset layout" description="Reopen every panel and restore the default sizes.">
              <button onClick={props.onResetLayout}>Reset</button>
            </Row>
          </>
        );
      case "mods":
        return <ModsSection mods={props.mods} settings={settings} onChange={onChange} />;
      case "shortcuts":
        return (
          <table className="shortcuts">
            <tbody>
              {SHORTCUTS.map(([keys, action]) => (
                <tr key={keys}>
                  <td>
                    <kbd>{keys}</kbd>
                  </td>
                  <td>{action}</td>
                </tr>
              ))}
            </tbody>
          </table>
        );
      case "daemon":
        return props.daemon ? (
          <DaemonSettings daemon={props.daemon} />
        ) : (
          <p className="muted">The daemon is managed here only in the desktop app.</p>
        );
      case "updates":
        return props.updates ? (
          <UpdateSettings updates={props.updates} settings={settings} onChange={onChange} />
        ) : (
          <p className="muted">Updates are available in the desktop app.</p>
        );
      case "connectors":
        return (
          <>
            <Row
              label="Connectors"
              description="Browse and add MCP servers such as GitHub, a database, or web search; test, turn off, or remove them; and manage plugins and skills."
            >
              <button className="primary" onClick={props.onOpenConnectors}>
                <Plug aria-hidden="true" /> Open connectors
              </button>
            </Row>
            {config && configRows(config.entries.filter((entry) => entry.section === "mcp" || entry.section === "extensions"))}
          </>
        );
      case "permissions":
        return client ? <PermissionsSection client={client} /> : <p className="muted">Connect to imperium-core to change permissions.</p>;
      case "budgets":
        return client ? <BudgetsSection client={client} /> : <p className="muted">Connect to imperium-core to manage budgets.</p>;
      case "data":
        return <DataSection userFile={config?.user_file ?? null} settings={settings} onChange={onChange} onResetLayout={props.onResetLayout} />;
      case "about":
        return <AboutSection updates={props.updates} client={client} />;
      default: {
        const fields = FIELDS.filter((field) => field.section === section);
        return (
          <>
            {fieldRows(fields)}
            {section === "notifications" && (
              <>
                <Row label="Try the sounds" description="Hear each chime; a failed turn uses the finish sound setting.">
                  <div className="row-actions">
                    <button onClick={() => preview("approval")}>Approval</button>
                    <button onClick={() => preview("finished")}>Finished</button>
                    <button onClick={() => preview("failed")}>Failed</button>
                  </div>
                </Row>
                <TestNotificationRow />
              </>
            )}
          </>
        );
      }
    }
  };

  const groups = [...new Set(sections.map((s) => s.group))];
  const title = query.trim() ? "Search" : (sections.find((s) => s.id === section)?.label ?? "Settings");

  const nav = (
    <>
      <label className="search-field settings-search">
        <Search aria-hidden="true" />
        <input
          type="search"
          placeholder="Search settings"
          aria-label="Search settings"
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          autoFocus
        />
      </label>
      {groups.map((group) => (
        <ModalNavGroup key={group} label={group}>
          {sections
            .filter((s) => s.group === group)
            .map((s) => (
              <ModalNavItem
                key={s.id}
                icon={SECTION_ICONS[s.id] ?? SlidersHorizontal}
                active={s.id === section && !query.trim()}
                onClick={() => {
                  setSection(s.id);
                  setQuery("");
                }}
              >
                {s.label}
              </ModalNavItem>
            ))}
        </ModalNavGroup>
      ))}
    </>
  );

  const notice = (
    <>
      {needsRestart && (
        <div className="settings-notice">
          Restart imperium-core to apply daemon settings.
          {props.daemon?.status.can_start && (
            <button
              className="primary"
              onClick={() => {
                props.daemon?.run("restart");
                setNeedsRestart(false);
              }}
            >
              Restart now
            </button>
          )}
        </div>
      )}
      {configError && <p className="error">{configError}</p>}
    </>
  );

  return (
    <SettingsModal label="Settings" title={title} nav={nav} notice={notice} onClose={onClose}>
      {body()}
    </SettingsModal>
  );
}

function FieldRow({
  field,
  value,
  onChange,
}: {
  field: Field;
  value: AppSettings[keyof AppSettings];
  onChange: <K extends keyof AppSettings>(key: K, value: AppSettings[K]) => void;
}) {
  const { control } = field;
  const changed = value !== DEFAULT_SETTINGS[field.key];
  const badge = changed ? (
    <button className="ghost reset-link" onClick={() => onChange(field.key, DEFAULT_SETTINGS[field.key])} title="Back to the default">
      reset
    </button>
  ) : null;
  return (
    <Row label={field.label} description={field.description} badge={badge}>
      {control.type === "toggle" && (
        <Toggle checked={Boolean(value)} onChange={(next) => onChange(field.key, next as never)} label={field.label} />
      )}
      {control.type === "select" && (
        <Select
          value={String(value)}
          options={control.options}
          onChange={(next) => onChange(field.key, next as never)}
          aria-label={field.label}
        />
      )}
      {control.type === "swatches" && (
        <div className="swatches" role="radiogroup" aria-label={field.label}>
          {control.options.map((option) => (
            <button
              key={option.value}
              type="button"
              role="radio"
              aria-checked={value === option.value}
              aria-label={option.label}
              title={option.label}
              className={`swatch${value === option.value ? " selected" : ""}`}
              style={{ "--swatch": option.color } as CSSProperties}
              onClick={() => onChange(field.key, option.value as never)}
            />
          ))}
        </div>
      )}
      {control.type === "slider" && (
        <label className="slider">
          <input
            type="range"
            min={control.min}
            max={control.max}
            step={control.step}
            value={Number(value)}
            onChange={(event) => onChange(field.key, Number(event.target.value) as never)}
            aria-label={field.label}
          />
          <output>{control.format(Number(value))}</output>
        </label>
      )}
    </Row>
  );
}

const SOURCE_LABEL: Record<ConfigEntry["source"], string> = {
  default: "",
  user: "",
  app: "set in the model configuration",
  project: "set by the project",
  env: "set by an environment variable",
};

/** One daemon setting; text fields save when they lose focus or on Enter. */
function ConfigRow({ entry, disabled, onSet }: { entry: ConfigEntry; disabled: boolean; onSet: (key: string, value: unknown) => void }) {
  const text = (value: unknown) =>
    Array.isArray(value) ? value.join("\n") : value === null || value === undefined ? "" : String(value);
  const [draft, setDraft] = useState(text(entry.value));
  useEffect(() => setDraft(text(entry.value)), [entry.value]);

  const commit = () => {
    if (draft === text(entry.value)) return;
    const trimmed = draft.trim();
    switch (entry.kind) {
      case "integer":
      case "number": {
        if (trimmed === "") return onSet(entry.key, null);
        const number = Number(trimmed);
        if (Number.isNaN(number)) return setDraft(text(entry.value));
        return onSet(entry.key, entry.kind === "integer" ? Math.round(number) : number);
      }
      case "path_list":
      case "string_list":
        return onSet(
          entry.key,
          draft
            .split("\n")
            .map((line) => line.trim())
            .filter(Boolean),
        );
      default:
        return onSet(entry.key, trimmed === "" ? null : trimmed);
    }
  };

  const overridden = SOURCE_LABEL[entry.source];
  const badge = (
    <>
      {overridden && <span className="source-badge">{overridden}</span>}
      {entry.source === "user" && (
        <button className="ghost reset-link" onClick={() => onSet(entry.key, null)} title="Remove from your config file">
          reset
        </button>
      )}
    </>
  );
  const description = `${entry.description} (${entry.key})`;

  let control: ReactNode;
  if (entry.kind === "bool") {
    control = <Toggle checked={entry.value === true} onChange={(next) => onSet(entry.key, next)} label={entry.label} disabled={disabled} />;
  } else if (entry.kind === "enum") {
    control = (
      <Select
        value={text(entry.value)}
        options={entry.options.map((option) => ({
          value: option,
          label: humanize(option),
          description: option === text(entry.default) ? "Default" : undefined,
        }))}
        onChange={(next) => onSet(entry.key, next)}
        disabled={disabled}
        placeholder="—"
        aria-label={entry.label}
      />
    );
  } else if (entry.kind === "path_list" || entry.kind === "string_list") {
    control = (
      <textarea
        className="field"
        value={draft}
        rows={Math.min(Math.max(draft.split("\n").length, 2), 6)}
        placeholder="One per line"
        onChange={(event) => setDraft(event.target.value)}
        onBlur={commit}
        disabled={disabled}
        aria-label={entry.label}
      />
    );
  } else {
    control = (
      <input
        className={`field${entry.kind === "integer" || entry.kind === "number" ? " numeric" : ""}`}
        type={entry.kind === "integer" || entry.kind === "number" ? "number" : "text"}
        spellCheck={false}
        value={draft}
        placeholder={text(entry.default) || (entry.kind === "secret_ref" ? "secret name" : "not set")}
        onChange={(event) => setDraft(event.target.value)}
        onBlur={commit}
        onKeyDown={(event) => event.key === "Enter" && (event.target as HTMLInputElement).blur()}
        disabled={disabled}
        aria-label={entry.label}
      />
    );
  }
  return (
    <Row label={entry.label} description={description} badge={badge}>
      {control}
    </Row>
  );
}

/** Mods the daemon found: installed ones and the nursery are on until turned off, other bundled ones are opt-in. */
function ModsSection({
  mods,
  settings,
  onChange,
}: {
  mods: ModListResult | null;
  settings: AppSettings;
  onChange: (settings: AppSettings) => void;
}) {
  if (!mods) return <p className="muted">Connect to imperiumd to see the mods it found.</p>;
  const toggle = (id: string, on: boolean) => onChange({ ...settings, enabledMods: { ...settings.enabledMods, [id]: on } });
  return (
    <>
      {mods.mods.length === 0 && <p className="muted">No mods are installed. Put one in the mods folder of the Imperium config folder.</p>}
      {mods.mods.map((mod) => {
        const panels = mod.panels.map((panel) => panel.title).join(", ") || "none";
        const permissions = mod.permissions.join(", ") || "none";
        return (
          <Row
            key={mod.id}
            label={`${mod.name}${mod.version ? ` ${mod.version}` : ""}`}
            badge={<span className="badge">{mod.bundled ? "bundled" : "installed"}</span>}
            description={`Panels: ${panels}. Permissions: ${permissions}.${mod.bundled && !BUNDLED_ON_BY_DEFAULT.includes(mod.id) ? " Off until you turn it on." : ""}`}
          >
            <Toggle checked={modEnabled(settings, mod)} onChange={(on) => toggle(mod.id, on)} label={mod.name} />
          </Row>
        );
      })}
      {mods.problems.map((problem) => (
        <p key={problem} className="muted">
          Could not load {problem}
        </p>
      ))}
    </>
  );
}

/** `wait_for_reset` → "Wait for reset"; values with dots or digits (q8_0, cuda12) stay as they are. */
function humanize(option: string): string {
  if (/[0-9.]/.test(option) || option.length <= 3) return option;
  const words = option.replace(/[_-]+/g, " ");
  return words.charAt(0).toUpperCase() + words.slice(1);
}

/** Global path grants and permission rules (`permissions.get` / `permissions.set`), saved to the config file. */
function PermissionsSection({ client }: { client: DaemonClient }) {
  const [saved, setSaved] = useState<PermissionsResult | null>(null);
  const [draft, setDraft] = useState<PermissionSet>({ grants: [], rules: [] });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    client
      .call<PermissionsResult>("permissions.get", {})
      .then((result) => {
        if (!live) return;
        setSaved(result);
        setDraft(result.global);
      })
      .catch((failure: unknown) => live && setError(String(failure instanceof Error ? failure.message : failure)));
    return () => {
      live = false;
    };
  }, [client]);

  if (!saved) return error ? <p className="error">{error}</p> : <p className="muted">Loading…</p>;
  const changed = !samePermissions(draft, saved.global);

  const save = async () => {
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const result = await client.call<PermissionsResult>("permissions.set", {
        grants: draft.grants,
        rules: draft.rules.filter((rule) => rule.tool.trim()),
      });
      setSaved(result);
      setDraft(result.global);
      setNotice("Saved. Rules apply to running sessions from their next tool call; paths apply to sessions started from now on.");
    } catch (failure) {
      setError(String(failure instanceof Error ? failure.message : failure));
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      <Row
        label="Every environment"
        description="Paths and rules here apply to every session, beside its environment's own (edit an environment to add those). Environments in a container, WSL, or over SSH get only their own paths, since these are paths on this computer."
      />
      <PermissionsEditor value={draft} onChange={setDraft} presets={saved.presets} disabled={busy} />
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
      {notice && !changed && <p className="muted">{notice}</p>}
      <div className="permissions-actions">
        <button onClick={() => setDraft(saved.global)} disabled={busy || !changed}>
          Revert
        </button>
        <button className="primary" onClick={() => void save()} disabled={busy || !changed}>
          Save
        </button>
      </div>
    </>
  );
}

function DataSection({
  userFile,
  settings,
  onChange,
  onResetLayout,
}: {
  userFile: string | null;
  settings: AppSettings;
  onChange: (settings: AppSettings) => void;
  onResetLayout: () => void;
}) {
  const [folder, setFolder] = useState<string | null>(null);
  useEffect(() => {
    storeFolder().then(setFolder, () => setFolder(null));
  }, []);
  const changed = (Object.keys(DEFAULT_SETTINGS) as (keyof AppSettings)[]).filter(
    (key) => JSON.stringify(settings[key]) !== JSON.stringify(DEFAULT_SETTINGS[key]),
  );
  return (
    <>
      <Row label="App settings and layout" description="Kept on this computer, outside the install folder, so they survive updates and reinstalls.">
        <code className="path">{folder ?? "…"}</code>
      </Row>
      <Row label="Daemon settings" description="Imperium's config file; every setting under Agent and daemon is saved here.">
        <code className="path">{userFile ?? "connect to imperium-core to see it"}</code>
      </Row>
      <Row label="Reset app settings" description={`Return the ${changed.length} changed app settings to their defaults. Daemon settings are not affected.`}>
        <button onClick={() => onChange(DEFAULT_SETTINGS)} disabled={changed.length === 0}>
          Reset
        </button>
      </Row>
      <Row label="Reset layout" description="Reopen every panel and restore the default sizes.">
        <button onClick={onResetLayout}>Reset</button>
      </Row>
    </>
  );
}

function AboutSection({ updates, client }: { updates: Updates | null; client: DaemonClient | null }) {
  const daemon = client?.daemon;
  return (
    <>
      <Row label="Imperium" description="The desktop app.">
        <span>{updates ? `v${updates.status.current_version} · ${updates.status.channel}` : "browser preview"}</span>
      </Row>
      <Row label="imperium-core" description="The daemon this window is connected to.">
        <span>{daemon ? `${daemon.daemon.version} · protocol ${daemon.protocol.major}.${daemon.protocol.minor}` : "not connected"}</span>
      </Row>
      <Row label="Source" description="Code, releases, and issues.">
        <span>github.com/Densetsu152637/Imperium-AI</span>
      </Row>
    </>
  );
}

/** Sends a sample system notification, asking for permission first where the system wants it. */
function TestNotificationRow() {
  const [result, setResult] = useState<string | null>(null);
  const send = async () => {
    setResult(null);
    const shown = await testNotification();
    if (!shown) setResult("Notifications are blocked. Allow them for Imperium in your system or browser settings.");
  };
  return (
    <Row label="Try a notification" description={result ?? "Show a sample system notification now."}>
      <button onClick={() => void send()}>Send a test</button>
    </Row>
  );
}
