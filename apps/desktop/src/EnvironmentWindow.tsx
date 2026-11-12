// The environment window: create an environment, or edit one. It is the shared modal (Settings,
// Connectors, the model editor): sections on the left (General, Folders & runtime, Agents, Memory &
// working folder, Configuration file, Permissions, Danger zone), the selected one on the right, and
// Cancel / Save under it; Esc or a click on the backdrop closes it. A new environment has no
// working folder, configuration file or delete, so those sections are left out, and Home (the
// built-in environment) has no Danger zone and a fixed runtime. Folder changes and agents of an
// existing environment apply at once (each is its own daemon call); everything else (name, icon,
// colour, pet, runtime, project worktrees, permissions) applies with Save. A new environment is
// created with everything at once, its folders included. For a remote runtime, folders are paths
// typed in, inside it. The Pet picks the default pet for the nursery. The Permissions section
// edits path grants and allow / ask / deny rules on top of the global ones. The Configuration
// file section reports what the environment's own `.imperium/config.toml` changes and what it
// asked for that was refused.

import {
  Bot,
  FileText,
  Folder,
  FolderOpen,
  FolderPlus,
  Search,
  ShieldCheck,
  SlidersHorizontal,
  Trash,
  TriangleAlert,
  X,
  type LucideIcon,
} from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";


import type {
  EnvironmentInfo,
  PathPreset,
  PermissionSet,
  PermissionsResult,
  PetListResult,
  RuntimeTestResult,
  WorkspacesWhereResult,
} from "@imperium/protocol";

import { PermissionsEditor, samePermissions } from "./components/PermissionsEditor";
import { Select } from "./components/Select";
import { ModalNavGroup, ModalNavItem, SettingsModal } from "./components/SettingsModal";
import { Row, RowNote } from "./components/SettingsRow";
import type { DaemonClient } from "./daemon/client";
import { pickFolder } from "./daemon/connection";
import {
  closeAction,
  environmentDirty,
  footerSaveBlocked,
  initialSection,
  paneHidden,
  saveTarget,
  sectionsFor,
  type EnvironmentSectionId,
} from "./environmentSections";
import { searchIcons } from "./icons";
import { EnvironmentAgents, type EnvironmentAgentsHandle } from "./panels/EnvironmentAgents";
import { EnvironmentConfigSection } from "./panels/EnvironmentConfigSection";
import { EnvironmentRuntimeFields, draftOf, isRemote, runtimeOf, sameRuntime } from "./panels/EnvironmentRuntimeFields";
import { GlyphSvg, ICON_COLORS, ProjectIcon, environmentIcon, sameFolder, type ProjectIconChoice } from "./projectIcons";

type Props = {
  /** The environment to edit; null to create one. */
  environment: EnvironmentInfo | null;
  client: DaemonClient | null;
  /** Open with the delete confirmation showing. */
  confirmDelete?: boolean;
  /** An environment was created or changed. */
  onSaved: (env: EnvironmentInfo) => void;
  onDeleted: (id: string) => void;
  onClose: () => void;
  /** Opens a session (one the user started from an agent's suggestion). */
  onOpenSession?: (sessionId: string) => void;
};

const SECTION_ICONS: Record<EnvironmentSectionId, LucideIcon> = {
  general: SlidersHorizontal,
  folders: Folder,
  agents: Bot,
  memory: FileText,
  config: FileText,
  permissions: ShieldCheck,
  danger: TriangleAlert,
};

const message = (error: unknown) => (error instanceof Error ? error.message : String(error));

/** The set as saved: rules without a tool name are dropped. */
const cleaned = (set: PermissionSet): PermissionSet => ({
  grants: set.grants,
  rules: set.rules.filter((rule) => rule.tool.trim()),
});

export function EnvironmentWindow({
  environment,
  client,
  confirmDelete = false,
  onSaved,
  onDeleted,
  onClose,
  onOpenSession,
}: Props) {
  const creating = environment === null;
  // A new environment's default icon comes from a fresh seed, as an existing one's does from its id.
  const seed = useMemo(() => environment?.id ?? `new-${Date.now()}`, [environment?.id]);
  const [name, setName] = useState(environment?.name ?? "");
  const [choice, setChoice] = useState<ProjectIconChoice>(() =>
    environmentIcon(environment ?? { id: seed, icon: null, color: null }),
  );
  const [query, setQuery] = useState("");
  const [newFolders, setNewFolders] = useState<string[]>([]);
  const [runtime, setRuntime] = useState(() => draftOf(environment?.runtime));
  const [projectWorktrees, setProjectWorktrees] = useState(environment?.project_worktrees ?? true);
  const [typedFolder, setTypedFolder] = useState("");
  const [pet, setPet] = useState(environment?.pet ?? "");
  const [installedPets, setInstalledPets] = useState<{ id: string; name: string }[]>([]);
  const savedPermissions: PermissionSet = environment?.permissions ?? { grants: [], rules: [] };
  const [permissions, setPermissions] = useState<PermissionSet>(savedPermissions);
  const [presets, setPresets] = useState<PathPreset[]>([]);
  const [working, setWorking] = useState<WorkspacesWhereResult["environments"][number] | null>(null);
  const permissionsChanged = !samePermissions(permissions, savedPermissions);
  const runtimeChanged = !creating && !sameRuntime(environment.runtime, runtime);
  const builtin = environment?.builtin === true;
  const [section, setSection] = useState<EnvironmentSectionId>(() =>
    initialSection({ creating, builtin }, confirmDelete),
  );
  const [initialChoice] = useState(choice);
  const [discarding, setDiscarding] = useState(false);
  const [agentForm, setAgentForm] = useState({ open: false, dirty: false });
  const agentsRef = useRef<EnvironmentAgentsHandle>(null);
  const [deleting, setDeleting] = useState(confirmDelete);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const nameInput = useRef<HTMLInputElement>(null);
  const folders = environment?.folders ?? newFolders;
  const icons = useMemo(() => searchIcons(query), [query]);

  useEffect(() => {
    let live = true;
    client
      ?.call<PetListResult>("pet.list")
      .then((list) => live && setInstalledPets(list.pets.map((p) => ({ id: p.id, name: p.display_name || p.id }))))
      .catch(() => {});
    client
      ?.call<PermissionsResult>("permissions.get", {})
      .then((result) => live && setPresets(result.presets))
      .catch(() => {});
    if (environment) {
      client
        ?.call<WorkspacesWhereResult>("workspaces.where", {})
        .then((result) => live && setWorking(result.environments.find((env) => env.environment_id === environment.id) ?? null))
        .catch(() => {});
    }
    return () => {
      live = false;
    };
  }, [client, environment]);

  useEffect(() => {
    if (!confirmDelete) nameInput.current?.focus();
  }, [confirmDelete]);

  /** Runs a daemon call, showing its error in the window. */
  const attempt = async <T,>(work: (client: DaemonClient) => Promise<T>): Promise<T | null> => {
    if (!client) {
      setError("Not connected to imperium-core.");
      return null;
    }
    setBusy(true);
    setError(null);
    try {
      return await work(client);
    } catch (failure) {
      setError(message(failure));
      return null;
    } finally {
      setBusy(false);
    }
  };

  const addFolder = async (typed?: string) => {
    const folder =
      typed ?? (await pickFolder(creating ? "Add folder to the new environment" : `Add folder to ${environment.name}`));
    if (!folder) return;
    if (creating) {
      setNewFolders((all) => (all.some((known) => sameFolder(known, folder)) ? all : [...all, folder]));
      return;
    }
    const env = await attempt((c) => c.call<EnvironmentInfo>("environments.add_folder", { id: environment.id, folder }));
    if (env) onSaved(env);
  };

  const removeFolder = async (folder: string) => {
    if (creating) {
      setNewFolders((all) => all.filter((known) => known !== folder));
      return;
    }
    const env = await attempt((c) =>
      c.call<EnvironmentInfo>("environments.remove_folder", { id: environment.id, folder }),
    );
    if (env) onSaved(env);
  };

  /** Opens the working folder or Memory.md with the computer's own programs. */
  const openWorking = async (target: "folder" | "memory") => {
    if (creating) return;
    const done = await attempt((c) => c.call("workspaces.open", { environment_id: environment.id, target }));
    if (done !== null && target === "memory") setWorking((current) => current && { ...current, memory_exists: true });
  };

  const save = async () => {
    const trimmed = name.trim();
    if (!trimmed) {
      setSection("general");
      setError("An environment needs a name.");
      nameInput.current?.focus();
      return;
    }
    const chosen = runtimeOf(runtime);
    if (typeof chosen === "string") {
      setSection("folders");
      setError(chosen);
      return;
    }
    const env = await attempt((c) =>
      creating
        ? c.call<EnvironmentInfo>("environments.create", {
            name: trimmed,
            icon: choice.icon,
            color: choice.color,
            folders: newFolders,
            runtime: chosen,
            project_worktrees: projectWorktrees,
            ...(pet ? { pet } : {}),
            permissions: cleaned(permissions),
          })
        : c.call<EnvironmentInfo>("environments.update", {
            id: environment.id,
            name: trimmed,
            icon: choice.icon,
            color: choice.color,
            ...(runtimeChanged ? { runtime: chosen } : {}),
            ...(projectWorktrees !== (environment.project_worktrees ?? true)
              ? { project_worktrees: projectWorktrees }
              : {}),
            ...(pet !== (environment.pet ?? "") ? { pet } : {}),
            ...(permissionsChanged ? { permissions: cleaned(permissions) } : {}),
          }),
    );
    if (env) {
      onSaved(env);
      onClose();
    }
  };

  const remove = async () => {
    if (creating) return;
    const done = await attempt((c) => c.call("environments.delete", { id: environment.id }));
    if (done !== null) {
      onDeleted(environment.id);
      onClose();
    }
  };

  /** Saves the environment, when it can be saved now. */
  const saveEnvironment = () => {
    if (!busy && client && name.trim() && !footerSaveBlocked(agentForm.dirty)) void save();
  };

  /** Ctrl+S: the open agent form first, else the environment. */
  const saveNow = () => {
    if (busy || !client) return;
    if (saveTarget(agentForm.open) === "agent") agentsRef.current?.save();
    else saveEnvironment();
  };

  const unsaved = environmentDirty({
    name: name !== (environment?.name ?? ""),
    icon: choice.icon !== initialChoice.icon || choice.color !== initialChoice.color,
    pet: pet !== (environment?.pet ?? ""),
    worktrees: projectWorktrees !== (environment?.project_worktrees ?? true),
    runtime: runtimeChanged || (creating && runtime.kind !== "local"),
    folders: creating && newFolders.length > 0,
    permissions: permissionsChanged,
    agent: agentForm.dirty,
  });

  /** Esc, the backdrop, the X and Cancel: unsaved edits ask first; asking again discards them. */
  const requestClose = (fromKey = false) => {
    const action = closeAction(unsaved, discarding, fromKey);
    if (action === "confirm") setDiscarding(true);
    else if (action === "dismiss") setDiscarding(false);
    else onClose();
  };

  const title = creating ? "New environment" : `Edit ${environment.name}`;
  const sections = sectionsFor({ creating, builtin });
  const current = sections.find((entry) => entry.id === section) ?? sections[0];
  const disabled = busy || !client;

  const nav = (
    <ModalNavGroup label={creating ? "New environment" : environment.name}>
      {sections.map((entry) => (
        <ModalNavItem key={entry.id} icon={SECTION_ICONS[entry.id]} active={entry.id === current.id} onClick={() => setSection(entry.id)}>
          {entry.label}
        </ModalNavItem>
      ))}
    </ModalNavGroup>
  );

  const footer = (
    <>
      {error && (
        <span className="error" role="alert" title={error}>
          {error}
        </span>
      )}
      <button type="button" onClick={() => requestClose()}>
        Cancel
      </button>
      <button
        type="button"
        className="primary"
        onClick={saveEnvironment}
        disabled={busy || !client || !name.trim() || footerSaveBlocked(agentForm.dirty)}
        title={footerSaveBlocked(agentForm.dirty) ? "Save or cancel the agent you are editing first (Agents section)" : undefined}
      >
        {creating ? "Create" : "Save"}
      </button>
    </>
  );

  return (
    <SettingsModal
      label={title}
      title={current.label}
      nav={nav}
      footer={footer}
      notice={
        discarding && (
          <div className="settings-notice" role="alertdialog" aria-label="Discard changes?">
            <span>Discard changes? Unsaved edits to this environment will be lost.</span>
            <span className="row-actions">
              <button type="button" onClick={() => setDiscarding(false)} autoFocus>
                Keep editing
              </button>
              <button type="button" className="danger" onClick={onClose}>
                Discard
              </button>
            </span>
          </div>
        )
      }
      onClose={() => requestClose()}
      onKeyDown={(event) => {
        // Esc is handled here (preventDefault stops the modal's own close): a held key's repeats
        // are ignored, and while the discard question shows, Esc dismisses it.
        if (event.key === "Escape") {
          event.preventDefault();
          if (!event.repeat) requestClose(true);
          return;
        }
        // Ctrl+S (Cmd+S on macOS) saves, like the model editor.
        if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "s") {
          event.preventDefault();
          saveNow();
        }
      }}
    >
      {current.id === "general" && (
        <>
          <Row label="Name" description="Shown in the Environments panel and wherever the environment is picked.">
            <input
              ref={nameInput}
              type="text"
              aria-label="Environment name"
              value={name}
              placeholder="Environment name"
              onChange={(event) => setName(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter") {
                  event.preventDefault();
                  saveEnvironment();
                }
              }}
            />
          </Row>
          <Row label="Icon" description="Drawn beside the environment's name; pick a glyph, then a colour.">
            <span className="environment-preview">
              <ProjectIcon choice={choice} />
            </span>
          </Row>
          <RowNote>
            <div className="environment-icon-picker">
              <label className="search-field environment-icon-search">
                <Search aria-hidden="true" />
                <input
                  type="search"
                  placeholder="Search icons"
                  aria-label="Search icons"
                  value={query}
                  onChange={(event) => setQuery(event.target.value)}
                  onKeyDown={(event) => {
                    // Escape clears the search first, then closes the window.
                    if (event.key === "Escape" && query) {
                      event.stopPropagation();
                      event.nativeEvent.stopImmediatePropagation();
                      setQuery("");
                    }
                  }}
                />
              </label>
              <div className="tile-grid icon-choices" role="radiogroup" aria-label="Icon">
                {icons.map((glyph) => (
                  <button
                    key={glyph}
                    type="button"
                    role="radio"
                    className="tile-choice"
                    title={glyph}
                    aria-label={glyph}
                    aria-checked={glyph === choice.icon}
                    onClick={() => setChoice((value) => ({ ...value, icon: glyph }))}
                  >
                    <GlyphSvg glyph={glyph} />
                  </button>
                ))}
                {icons.length === 0 && <p className="muted tile-grid-empty">No icon matches “{query}”.</p>}
              </div>
            </div>
          </RowNote>
          <Row label="Colour" description="The icon's background." />
          <RowNote>
            <div className="tile-grid" role="radiogroup" aria-label="Colour">
              {ICON_COLORS.map((color, index) => (
                <button
                  key={color}
                  type="button"
                  role="radio"
                  className="tile-choice colour-choice"
                  style={{ background: color }}
                  title={color}
                  aria-label={`Colour ${index + 1} (${color})`}
                  aria-checked={color === choice.color}
                  onClick={() => setChoice((value) => ({ ...value, color }))}
                />
              ))}
            </div>
          </RowNote>
          <Row label="Pet" description="The default pet for this environment's sessions in the nursery, unless the nursery says otherwise.">
            <Select
              aria-label="Pet"
              value={pet}
              disabled={disabled}
              options={[
                { value: "", label: "Nursery default" },
                // A saved pet that is no longer installed stays selectable until changed.
                ...(pet && !installedPets.some((p) => p.id === pet) ? [{ value: pet, label: pet }] : []),
                ...installedPets.map((p) => ({ value: p.id, label: p.name })),
              ]}
              onChange={setPet}
            />
          </Row>
        </>
      )}

      {current.id === "folders" && (
        <>
          <Row
            label="Folders"
            description={
              isRemote(runtime)
                ? "Paths inside the runtime. The first is the working directory."
                : "Sessions can work in these folders; the first is the working directory. Removing one leaves it on disk."
            }
          >
            {!isRemote(runtime) && (
              <button type="button" onClick={() => void addFolder()} disabled={disabled}>
                <FolderPlus aria-hidden="true" /> Add folder
              </button>
            )}
          </Row>
          {isRemote(runtime) && (
            <Row label="Folder path" description="A path inside the runtime, such as /home/me/project." sub>
              <span className="row-actions">
                <input
                  type="text"
                  aria-label="Folder inside the runtime"
                  placeholder="/home/me/project"
                  value={typedFolder}
                  onChange={(event) => setTypedFolder(event.target.value)}
                />
                <button
                  type="button"
                  disabled={disabled || !typedFolder.trim() || runtimeChanged}
                  title={runtimeChanged ? "Save the runtime first" : "Add this path inside the runtime"}
                  onClick={() => {
                    void addFolder(typedFolder.trim());
                    setTypedFolder("");
                  }}
                >
                  <FolderPlus aria-hidden="true" /> Add path
                </button>
              </span>
            </Row>
          )}
          <RowNote>
            <ul className="environment-folders" aria-label="Folders">
              {folders.map((folder, index) => (
                <li key={folder}>
                  <span className="folder-path" title={folder}>
                    {folder}
                  </span>
                  {index === 0 && <span className="folder-badge">working directory</span>}
                  <button
                    type="button"
                    className="ghost icon-only row-tool"
                    aria-label={`Remove ${folder}`}
                    title="Remove folder (the folder itself is untouched)"
                    disabled={disabled}
                    onClick={() => void removeFolder(folder)}
                  >
                    <X />
                  </button>
                </li>
              ))}
              {folders.length === 0 && (
                <li className="muted">
                  {environment?.builtin
                    ? "Home has no project folders: its sessions work in their own scratch folders. Add a folder only to open a project to them."
                    : "No folders yet. Add one to start sessions here; the first is the working directory."}
                </li>
              )}
            </ul>
          </RowNote>
          <EnvironmentRuntimeFields
            draft={runtime}
            onChange={setRuntime}
            projectWorktrees={projectWorktrees}
            onProjectWorktrees={setProjectWorktrees}
            disabled={disabled}
            fixedLocal={builtin}
            testBlocked={creating ? "Create the environment first" : runtimeChanged ? "Save the runtime first" : null}
            onTest={() => {
              if (!client || creating) return Promise.reject(new Error("Not connected to imperium-core."));
              return client.call<RuntimeTestResult>("environments.test_runtime", { id: environment.id });
            }}
          />
        </>
      )}

      {/* Always mounted, hidden from other sections: switching away keeps an open agent form. */}
      <div hidden={paneHidden(current.id, "agents")}>
        <EnvironmentAgents
          ref={agentsRef}
          environment={environment}
          client={client}
          disabled={busy}
          onDraftChange={setAgentForm}
          onOpenSession={(sessionId) => {
            onOpenSession?.(sessionId);
            onClose();
          }}
        />
      </div>

      {current.id === "memory" && !creating && (
        <>
          <Row
            label="Memory.md"
            description="Notes for this environment only: agents read it at the start of every session here and can add to it."
          >
            <button type="button" onClick={() => void openWorking("memory")} disabled={disabled}>
              <FileText aria-hidden="true" /> Open Memory.md
            </button>
          </Row>
          <Row
            label="Working folder"
            description="Each session also gets its own scratch folder below it, deleted when the session is archived."
          >
            <button type="button" onClick={() => void openWorking("folder")} disabled={disabled}>
              <FolderOpen aria-hidden="true" /> Open folder
            </button>
          </Row>
          <RowNote>
            <code className="path working-folder-path" title={working?.dir}>
              {working?.dir ?? "…"}
            </code>
            {working && (
              <span className="muted">
                {working.memory_exists ? "Memory.md exists" : "No Memory.md yet"}; {working.session_folders} session{" "}
                {working.session_folders === 1 ? "folder" : "folders"}. Move all working folders in Settings → Working
                folders.
              </span>
            )}
          </RowNote>
        </>
      )}

      {current.id === "config" && !creating && <EnvironmentConfigSection environment={environment} client={client} />}

      {current.id === "permissions" && (
        <>
          <Row
            label="Permissions"
            description="Added to the global permissions in Settings → Permissions; applied with Save. Rules reach running sessions at their next tool call, paths reach sessions started afterwards."
          />
          <RowNote>
            <PermissionsEditor
              value={permissions}
              onChange={setPermissions}
              presets={presets}
              remote={isRemote(runtime)}
              disabled={disabled}
            />
          </RowNote>
        </>
      )}

      {current.id === "danger" && !creating && !builtin && (
        <Row
          label="Delete environment"
          description="Removes the environment and its settings. Its folders on disk are untouched."
        >
          {deleting ? (
            <span className="environment-delete-confirm" role="group" aria-label="Confirm delete">
              <span>Delete {environment.name}?</span>
              <button type="button" className="danger" onClick={() => void remove()} disabled={busy} autoFocus>
                Delete
              </button>
              <button type="button" onClick={() => setDeleting(false)}>
                Keep
              </button>
            </span>
          ) : (
            <button
              type="button"
              className="danger"
              onClick={() => setDeleting(true)}
              disabled={disabled}
              aria-label="Delete environment"
              title="Delete environment (folders on disk are untouched)"
            >
              <Trash aria-hidden="true" /> Delete…
            </button>
          )}
        </Row>
      )}
    </SettingsModal>
  );
}
