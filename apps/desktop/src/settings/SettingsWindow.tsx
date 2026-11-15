// The Settings window: a sidebar of sections and the selected section's settings, with search
// across all of them. App settings (appearance, chat, notifications, startup, panels) are kept
// in local files by the app; daemon settings (models, runtimes, tools, and the rest) are read
// and written through the daemon's config.describe / config.set into its config file. Both
// persist across versions.

import { FolderPlus, Keyboard, Plug, Search, SlidersHorizontal, Volume2 } from "lucide-react";
import { Fragment, useCallback, useEffect, useMemo, useState, type ReactNode } from "react";
import type { ConfigDescribeResult, ConfigEntry, ConfigSetResult, ModListResult } from "@imperium/protocol";
import { ApiSection } from "../components/ApiSection";
import { BenchmarkSection } from "../components/BenchmarkSection";
import { BudgetsSection } from "../components/BudgetsEditor";
import { RowMenu } from "../components/RowMenu";
import { matches, ModalNavGroup, ModalNavItem, SettingsModal } from "../components/SettingsModal";
import { Row } from "../components/SettingsRow";
import { ShortcutsSection } from "../components/ShortcutsSection";
import { Switch as Toggle } from "../components/Switch";
import type { DaemonClient } from "../daemon/client";
import { type Daemon, DaemonSettings } from "../shared/DaemonControl";
import { addModelFolder } from "../panels/ModelSetup";
import { preview } from "../shared/notify";
import { reservedChords, type AppSettings } from "./settings";
import { shortcutHolding } from "./shortcuts";
import { type Updates, UpdateSettings } from "../shared/UpdateControl";
import { LayoutRows, type LayoutControls } from "./LayoutRows";
import { FIELDS, type Field } from "./fields";
import { CONFIG_SECTIONS, APP_SECTIONS, PERMISSIONS_SECTION, BUDGETS_SECTION, API_SECTION, AGENT_CHANGES_SECTION, TAIL_SECTIONS, SECTION_ICONS, type Section } from "./sections";
import { FieldRow } from "./FieldRow";
import { WorkspacesRootRow } from "./WorkspacesRootRow";
import { PersonalityRow } from "./PersonalityRow";
import { ConfigRow } from "./ConfigRow";
import { ModsSection } from "./ModsSection";
import { AgentChangesSection } from "./AgentChangesSection";
import { PermissionsSection } from "./PermissionsSection";
import { DataSection } from "./DataSection";
import { AboutSection } from "./AboutSection";
import { TestNotificationRow } from "./TestNotificationRow";

type Props = {
  layoutControls: LayoutControls;
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
  /** Closes Settings and starts the first-run tour again. */
  onStartTour: () => void;
  /** The model folders changed: refresh the model lists. */
  onModelsChanged?: () => void;
  onResetLayout: () => void;
  onClose: () => void;
  /** A section to show, as asked for (by the command palette); each new request switches to it. */
  request?: { section: string } | null;
};

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

  const loadConfig = useCallback(() => {
    if (!client) return;
    client
      .call<ConfigDescribeResult>("config.describe", {})
      .then(setConfig)
      .catch((error: unknown) => setConfigError(String(error instanceof Error ? error.message : error)));
  }, [client]);
  useEffect(loadConfig, [loadConfig]);

  /** Picks a folder of models and adds it to `models.folders` (scanned at once). */
  const addFolder = async () => {
    if (!client) return;
    setConfigError(null);
    try {
      if (!(await addModelFolder(client))) return;
      loadConfig();
      props.onModelsChanged?.();
    } catch (error) {
      setConfigError(String(error instanceof Error ? error.message : error));
    }
  };

  const configSections: Section[] = useMemo(() => {
    const present = new Set((config?.entries ?? []).map((entry) => entry.section));
    return Object.entries(CONFIG_SECTIONS)
      .filter(([id]) => present.has(id) || !config)
      .map(([id, label]) => ({ id: `config:${id}`, label, group: "Agent and daemon" }));
  }, [config]);
  const sections = [...APP_SECTIONS, ...configSections, PERMISSIONS_SECTION, BUDGETS_SECTION, API_SECTION, AGENT_CHANGES_SECTION, ...TAIL_SECTIONS];

  // A push-to-talk key that a shortcut binding already uses is refused, so no binding is dropped later.
  const [pttNotice, setPttNotice] = useState<string | null>(null);
  const set = <K extends keyof AppSettings>(key: K, value: AppSettings[K]) => {
    if (key === "pushToTalkKey") {
      const taken = shortcutHolding(settings.shortcuts, reservedChords({ pushToTalkKey: value as AppSettings["pushToTalkKey"] }));
      if (taken) {
        setPttNotice(`${value as string} is used by the ${taken.label} shortcut. Change that shortcut in Keyboard first.`);
        return;
      }
      setPttNotice(null);
    }
    onChange({ ...settings, [key]: value });
  };

  const setConfigValue = async (key: string, value: unknown): Promise<void> => {
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

  const fieldRows = (fields: Field[]) =>
    fields.map((field) => (
      <Fragment key={field.key}>
        <FieldRow field={field} value={settings[field.key]} onChange={set} />
        {field.key === "pushToTalkKey" && pttNotice && (
          <p className="muted" role="status">
            {pttNotice}
          </p>
        )}
      </Fragment>
    ));
  const configRows = (entries: ConfigEntry[]) =>
    entries.map((entry) =>
      entry.key === "policy.personality" && client ? (
        <PersonalityRow key={entry.key} entry={entry} client={client} onSet={setConfigValue} />
      ) : entry.key === "workspaces.root" && client ? (
        <WorkspacesRootRow key={entry.key} entry={entry} client={client} onSet={setConfigValue} />
      ) : (
        <ConfigRow key={entry.key} entry={entry} disabled={!client} onSet={setConfigValue} />
      ),
    );

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
          {id === "models" && (
            <Row label="Add a model folder" description="Choose a folder of models to scan; it is added to Model folders below.">
              <button onClick={() => void addFolder()}>
                <FolderPlus aria-hidden="true" /> Add folder…
              </button>
            </Row>
          )}
          {rows}
          {id === "models" && client && <BenchmarkSection client={client} />}
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
            <LayoutRows controls={props.layoutControls} />
            <Row label="Reset layout" description="Reopen every panel and restore the default sizes.">
              <button onClick={props.onResetLayout}>Reset</button>
            </Row>
          </>
        );
      case "mods":
        return <ModsSection mods={props.mods} settings={settings} onChange={onChange} />;
      case "shortcuts":
        return <ShortcutsSection settings={settings} onChange={onChange} />;
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
      case "api":
        return client ? <ApiSection client={client} /> : <p className="muted">Connect to imperium-core to manage the API.</p>;
      case "agent-changes":
        return client ? (
          <AgentChangesSection client={client} onChanged={props.onModelsChanged} />
        ) : (
          <p className="muted">Connect to imperium-core to see what agents changed.</p>
        );
      case "data":
        return <DataSection userFile={config?.user_file ?? null} settings={settings} onChange={onChange} onResetLayout={props.onResetLayout} />;
      case "about":
        return <AboutSection updates={props.updates} client={client} onStartTour={props.onStartTour} />;
      default: {
        const fields = FIELDS.filter((field) => field.section === section);
        return (
          <>
            {fieldRows(fields)}
            {section === "notifications" && (
              <>
                <Row label="Try the sounds" description="Hear each chime; a failed turn uses the finish sound setting.">
                  <RowMenu
                    label="Play a sound"
                    icon={Volume2}
                    text="Play"
                    items={[
                      { label: "Approval", onSelect: () => preview("approval") },
                      { label: "Finished", onSelect: () => preview("finished") },
                      { label: "Failed", onSelect: () => preview("failed") },
                    ]}
                  />
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
