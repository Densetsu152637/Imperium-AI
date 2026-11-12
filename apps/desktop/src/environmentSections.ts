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

/**
 * The controls each section holds, by accessible name or visible text, and the source file that
 * renders them. Every control of the old single page is listed once.
 */
export const SECTION_CONTROLS: Record<EnvironmentSectionId, { file: string; controls: string[] }> = {
  general: { file: "EnvironmentWindow.tsx", controls: ["Environment name", "Search icons", "aria-label=\"Icon\"", "aria-label=\"Colour\"", "aria-label=\"Pet\""] },
  folders: {
    file: "EnvironmentWindow.tsx",
    controls: ["Add folder", "Folder inside the runtime", "Add path", "Remove ${folder}", "<EnvironmentRuntimeFields"],
  },
  agents: { file: "panels/EnvironmentAgents.tsx", controls: ["Add agent", "Edit ${agent.name}", "Delete ${agent.name}", "<AgentInitiative", "Local files only", "MCP servers", "Plugins", "Save agent"] },
  memory: { file: "EnvironmentWindow.tsx", controls: ["Open Memory.md", "Open folder"] },
  config: { file: "panels/EnvironmentConfigSection.tsx", controls: ["Reload", "Applied", "Refused"] },
  permissions: { file: "EnvironmentWindow.tsx", controls: ["<PermissionsEditor"] },
  danger: { file: "EnvironmentWindow.tsx", controls: ["Delete environment", "Keep"] },
};
