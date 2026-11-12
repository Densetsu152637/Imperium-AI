// The sections of the environment window (pure, so which sections show is tested without a window).

export type EnvironmentSectionId = "general" | "folders" | "agents" | "memory" | "config" | "permissions" | "danger";

export type EnvironmentSection = { id: EnvironmentSectionId; label: string };

const ALL: EnvironmentSection[] = [
  { id: "general", label: "General" },
  { id: "folders", label: "Folders & runtime" },
  { id: "agents", label: "Agents" },
  { id: "memory", label: "Memory & working folder" },
  { id: "config", label: "Configuration file" },
  { id: "permissions", label: "Permissions" },
  { id: "danger", label: "Danger zone" },
];

type Kind = { creating: boolean; builtin: boolean };

/**
 * The sections to show. A new environment has no working folder, configuration file or delete yet;
 * Home (the built-in environment) cannot be deleted.
 */
export function sectionsFor({ creating, builtin }: Kind): EnvironmentSection[] {
  return ALL.filter((section) => {
    if (section.id === "memory" || section.id === "config") return !creating;
    if (section.id === "danger") return !creating && !builtin;
    return true;
  });
}

/** The section to open on: the delete confirmation's when asked for and available, else General. */
export function initialSection(kind: Kind, confirmDelete: boolean): EnvironmentSectionId {
  return confirmDelete && sectionsFor(kind).some((section) => section.id === "danger") ? "danger" : "general";
}

/** Whether a section's pane is hidden. Panes stay mounted, so an agent draft survives a switch. */
export const paneHidden = (selected: EnvironmentSectionId, pane: EnvironmentSectionId): boolean => selected !== pane;

/** What Ctrl+S / Enter saves: the open agent form while one is shown, else the environment. */
export const saveTarget = (agentFormOpen: boolean): "agent" | "environment" => (agentFormOpen ? "agent" : "environment");

/** The footer Save waits while the agent form holds unsaved changes (saving would drop them). */
export const footerSaveBlocked = (agentFormDirty: boolean): boolean => agentFormDirty;

/** What a close request does: ask first when there are unsaved edits, close on the second request. */
export const closeAction = (dirty: boolean, confirming: boolean): "close" | "confirm" =>
  dirty && !confirming ? "confirm" : "close";

/** Whether anything in the window is unsaved. */
export const environmentDirty = (edits: Record<string, boolean>): boolean => Object.values(edits).some(Boolean);

