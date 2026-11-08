// The Settings window: a sidebar of sections and the selected section's settings, with search
// across all of them. App settings (appearance, chat, notifications, startup, panels) are kept
// in local files by the app; daemon settings (models, runtimes, tools, and the rest) are read
// and written through the daemon's config.describe / config.set into its config file. Both
// persist across versions.

import {
  Bell,
  Bot,
  Brain,
  Clock,
  Cpu,
  Download,
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
  SlidersHorizontal,
  Smartphone,
  Boxes,
  GitPullRequest,
  Workflow,
  X,
  type LucideIcon,
} from "lucide-react";
import { useEffect, useMemo, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";

import type { ConfigDescribeResult, ConfigEntry, ConfigSetResult } from "@imperium/protocol";

import { Select } from "./components/Select";
import { Switch as Toggle } from "./components/Switch";
import type { DaemonClient } from "./daemon/client";
import { type Daemon, DaemonSettings } from "./DaemonControl";
import { preview } from "./notify";
import { ACCENTS, DEFAULT_SETTINGS, type AppSettings } from "./settings";
import { storeFolder } from "./store";
import { type Updates, UpdateSettings } from "./UpdateControl";

type Props = {
  settings: AppSettings;
  onChange: (settings: AppSettings) => void;
  updates: Updates | null;
  daemon: Daemon | null;
  client: DaemonClient | null;
  panels: { id: string; title: string }[];
  hidden: string[];
  onTogglePanel: (id: string, open: boolean) => void;
  /** Opens the Connectors window (MCP servers, plugins, skills). */
  onOpenConnectors: () => void;
  onResetLayout: () => void;
  onClose: () => void;
};

// ---- App settings, described as data so they render and search the same way ----

type Control =
  | { type: "toggle" }
  | { type: "select"; options: { value: string; label: string }[] }
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
    key: "accent",
    section: "appearance",
    label: "Accent colour",
    description: "Buttons, highlights, and the connection light.",
    control: { type: "select", options: Object.entries(ACCENTS).map(([value, { label }]) => ({ value, label })) },
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
  daemon: "Network",
};

type Section = { id: string; label: string; group: string };

const APP_SECTIONS: Section[] = [
  { id: "general", label: "General", group: "Imperium" },
  { id: "appearance", label: "Appearance", group: "Imperium" },
  { id: "chat", label: "Chat", group: "Imperium" },
  { id: "notifications", label: "Notifications", group: "Imperium" },
  { id: "panels", label: "Panels", group: "Imperium" },
  { id: "shortcuts", label: "Keyboard", group: "Imperium" },
  { id: "daemon", label: "Daemon", group: "Imperium" },
  { id: "updates", label: "Updates", group: "Imperium" },
  { id: "connectors", label: "Connectors", group: "Imperium" },
];
const TAIL_SECTIONS: Section[] = [
  { id: "data", label: "Data and storage", group: "About" },
  { id: "about", label: "About", group: "About" },
];

/** One icon per section, in the same line style as the rest of the app. */
const SECTION_ICONS: Record<string, LucideIcon> = {
  general: SlidersHorizontal,
  appearance: Palette,
  chat: MessageSquare,
  notifications: Bell,
  panels: LayoutGrid,
  shortcuts: Keyboard,
  daemon: Server,
  updates: Download,
  connectors: Plug,
  data: HardDrive,
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
  "config:daemon": Network,
};

const SHORTCUTS: [string, string][] = [
  ["Ctrl+,", "Open or close Settings"],
  ["Esc", "Close Settings"],
  ["Enter / Ctrl+Enter", "Send a message (see Chat)"],
  ["Shift+Enter", "New line in a message"],
];

function matches(query: string, ...texts: string[]): boolean {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  const haystack = texts.join(" ").toLowerCase();
  return words.every((word) => haystack.includes(word));
}

export function SettingsWindow(props: Props) {
  const { settings, onChange, client, onClose } = props;
  const [section, setSection] = useState("general");
  const [query, setQuery] = useState("");
  const [config, setConfig] = useState<ConfigDescribeResult | null>(null);
  const [configError, setConfigError] = useState<string | null>(null);
  const [needsRestart, setNeedsRestart] = useState(false);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

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
  const sections = [...APP_SECTIONS, ...configSections, ...TAIL_SECTIONS];

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
      if (result.overridden) setConfigError(`${key} is set by an environment variable or project file, which wins.`);
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
      return configRows(config.entries.filter((entry) => entry.section === id));
    }
    switch (section) {
      case "panels":
        return (
          <>
            {props.panels.map((panel) => (
              <Row key={panel.id} label={panel.title} description="Shown in the workspace. Close it from its × as well.">
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
          <UpdateSettings updates={props.updates} />
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
              <Row label="Try the sounds" description="Hear each chime.">
                <div className="row-actions">
                  <button onClick={() => preview("approval")}>Approval</button>
                  <button onClick={() => preview("finished")}>Finished</button>
                </div>
              </Row>
            )}
          </>
        );
      }
    }
  };

  const groups = [...new Set(sections.map((s) => s.group))];
  const title = query.trim() ? "Search" : (sections.find((s) => s.id === section)?.label ?? "Settings");

  // Rendered into the body, above the workspace's own stacking layers.
  return createPortal(
    <div className="settings-backdrop" onMouseDown={(event) => event.target === event.currentTarget && onClose()}>
      <div className="settings-window" role="dialog" aria-modal="true" aria-label="Settings">
        <nav className="settings-nav">
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
            <div key={group} className="settings-group">
              <span className="settings-group-label">{group}</span>
              {sections
                .filter((s) => s.group === group)
                .map((s) => (
                  <button
                    key={s.id}
                    className={`settings-nav-item${s.id === section && !query.trim() ? " active" : ""}`}
                    onClick={() => {
                      setSection(s.id);
                      setQuery("");
                    }}
                    aria-current={s.id === section && !query.trim() ? "page" : undefined}
                  >
                    {(() => {
                      const Icon = SECTION_ICONS[s.id] ?? SlidersHorizontal;
                      return <Icon aria-hidden="true" />;
                    })()}
                    {s.label}
                  </button>
                ))}
            </div>
          ))}
        </nav>
        <section className="settings-content">
          <header className="settings-content-header">
            <h2>{title}</h2>
            <button className="ghost icon-only" onClick={onClose} aria-label="Close settings" title="Close (Esc)">
              <X />
            </button>
          </header>
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
          <div className="settings-rows">{body()}</div>
        </section>
      </div>
    </div>,
    document.body,
  );
}

function Row({ label, description, children, badge }: { label: string; description?: string; children: ReactNode; badge?: ReactNode }) {
  return (
    <div className="settings-row">
      <div className="settings-row-text">
        <span className="settings-row-label">
          {label} {badge}
        </span>
        {description && <span className="settings-row-description">{description}</span>}
      </div>
      <div className="settings-row-control">{children}</div>
    </div>
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

/** `wait_for_reset` → "Wait for reset"; values with dots or digits (q8_0, cuda12) stay as they are. */
function humanize(option: string): string {
  if (/[0-9.]/.test(option) || option.length <= 3) return option;
  const words = option.replace(/[_-]+/g, " ");
  return words.charAt(0).toUpperCase() + words.slice(1);
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
  const changed = (Object.keys(DEFAULT_SETTINGS) as (keyof AppSettings)[]).filter((key) => settings[key] !== DEFAULT_SETTINGS[key]);
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
