// The environment window: create an environment, or edit one — its name, icon and colour, and
// folders — and delete it. A centred sheet over the workspace like Settings; Escape or a click on
// the backdrop closes it. Folder changes to an existing environment apply at once (each is its
// own daemon call); name, icon and colour apply with Save. A new environment is created with
// everything at once, its folders included. The Runtime section says where the environment's
// tools run (for a remote runtime, folders are paths typed in, inside it) and turns its
// persistent agent on or off; both apply with Save. The Pet field picks the environment's
// default pet for the nursery (its sessions' pets are this kind unless the nursery says otherwise).
// The Permissions section edits the environment's path grants and allow / ask / deny rules, on top
// of the global ones; they apply with Save.

import { FolderPlus, Search, Trash, X } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";

import type {
  EnvironmentInfo,
  PathPreset,
  PermissionSet,
  PermissionsResult,
  PetListResult,
  RuntimeTestResult,
} from "@imperium/protocol";

import { PermissionsEditor, samePermissions } from "./components/PermissionsEditor";
import { Select } from "./components/Select";
import type { DaemonClient } from "./daemon/client";
import { pickFolder } from "./daemon/connection";
import { searchIcons } from "./icons";
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
};

const message = (error: unknown) => (error instanceof Error ? error.message : String(error));

/** The set as saved: rules without a tool name are dropped. */
const cleaned = (set: PermissionSet): PermissionSet => ({
  grants: set.grants,
  rules: set.rules.filter((rule) => rule.tool.trim()),
});

export function EnvironmentWindow({ environment, client, confirmDelete = false, onSaved, onDeleted, onClose }: Props) {
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
  const [persistentAgent, setPersistentAgent] = useState(environment?.persistent_agent ?? false);
  const [typedFolder, setTypedFolder] = useState("");
  const [pet, setPet] = useState(environment?.pet ?? "");
  const [installedPets, setInstalledPets] = useState<{ id: string; name: string }[]>([]);
  const savedPermissions: PermissionSet = environment?.permissions ?? { grants: [], rules: [] };
  const [permissions, setPermissions] = useState<PermissionSet>(savedPermissions);
  const [presets, setPresets] = useState<PathPreset[]>([]);
  const permissionsChanged = !samePermissions(permissions, savedPermissions);
  const runtimeChanged = !creating && !sameRuntime(environment.runtime, runtime);
  const [deleting, setDeleting] = useState(confirmDelete);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const nameInput = useRef<HTMLInputElement>(null);
  const folders = environment?.folders ?? newFolders;
  const icons = useMemo(() => searchIcons(query), [query]);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

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
    return () => {
      live = false;
    };
  }, [client]);

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

  const save = async () => {
    const trimmed = name.trim();
    if (!trimmed) {
      setError("An environment needs a name.");
      nameInput.current?.focus();
      return;
    }
    const chosen = runtimeOf(runtime);
    if (typeof chosen === "string") {
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
            persistent_agent: persistentAgent,
            ...(pet ? { pet } : {}),
            permissions: cleaned(permissions),
          })
        : c.call<EnvironmentInfo>("environments.update", {
            id: environment.id,
            name: trimmed,
            icon: choice.icon,
            color: choice.color,
            ...(runtimeChanged ? { runtime: chosen } : {}),
            ...(persistentAgent !== (environment.persistent_agent ?? false) ? { persistent_agent: persistentAgent } : {}),
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

  const title = creating ? "New environment" : `Edit ${environment.name}`;

  return createPortal(
    <div className="settings-backdrop" onMouseDown={(event) => event.target === event.currentTarget && onClose()}>
      <div className="environment-window" role="dialog" aria-modal="true" aria-label={title}>
        <header className="settings-content-header">
          <h2>{title}</h2>
          <button className="ghost icon-only" onClick={onClose} aria-label="Close" title="Close (Esc)">
            <X />
          </button>
        </header>
        <form
          className="environment-form"
          onSubmit={(event) => {
            event.preventDefault();
            void save();
          }}
        >
          <div className="environment-identity">
            <span className="environment-preview">
              <ProjectIcon choice={choice} />
            </span>
            <label className="environment-field">
              <span className="environment-label">Name</span>
              <input
                ref={nameInput}
                value={name}
                placeholder="Environment name"
                onChange={(event) => setName(event.target.value)}
              />
            </label>
          </div>

          <section className="environment-section" aria-label="Icon">
            <div className="environment-section-head">
              <span className="environment-label">Icon</span>
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
            </div>
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
                  onClick={() => setChoice((current) => ({ ...current, icon: glyph }))}
                >
                  <GlyphSvg glyph={glyph} />
                </button>
              ))}
              {icons.length === 0 && <p className="muted tile-grid-empty">No icon matches “{query}”.</p>}
            </div>
          </section>

          <section className="environment-section" aria-label="Colour">
            <span className="environment-label">Colour</span>
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
                  onClick={() => setChoice((current) => ({ ...current, color }))}
                />
              ))}
            </div>
          </section>

          <section className="environment-section" aria-label="Pet">
            <div className="environment-section-head">
              <span className="environment-label">Pet</span>
              <Select
                aria-label="Pet"
                value={pet}
                disabled={busy || !client}
                options={[
                  { value: "", label: "Nursery default" },
                  // A saved pet that is no longer installed stays selectable until changed.
                  ...(pet && !installedPets.some((p) => p.id === pet) ? [{ value: pet, label: pet }] : []),
                  ...installedPets.map((p) => ({ value: p.id, label: p.name })),
                ]}
                onChange={setPet}
              />
            </div>
          </section>

          <section className="environment-section" aria-label="Folders">
            <div className="environment-section-head">
              <span className="environment-label">Folders</span>
              {!isRemote(runtime) && (
                <button type="button" onClick={() => void addFolder()} disabled={busy || !client}>
                  <FolderPlus aria-hidden="true" /> Add folder
                </button>
              )}
            </div>
            {isRemote(runtime) && (
              <div className="runtime-row">
                <input
                  aria-label="Folder inside the runtime"
                  placeholder="/home/me/project"
                  value={typedFolder}
                  onChange={(event) => setTypedFolder(event.target.value)}
                />
                <button
                  type="button"
                  disabled={busy || !client || !typedFolder.trim() || runtimeChanged}
                  title={runtimeChanged ? "Save the runtime first" : "Add this path inside the runtime"}
                  onClick={() => {
                    void addFolder(typedFolder.trim());
                    setTypedFolder("");
                  }}
                >
                  <FolderPlus aria-hidden="true" /> Add path
                </button>
              </div>
            )}
            <ul className="environment-folders" aria-label="Folders">
              {folders.map((folder, index) => (
                <li key={folder}>
                  <span className="folder-path" title={folder}>
                    {folder}
                  </span>
                  {index === 0 && <span className="folder-badge">working directory</span>}
                  <button
                    type="button"
                    className="ghost icon-only"
                    aria-label={`Remove ${folder}`}
                    title="Remove folder (the folder itself is untouched)"
                    disabled={busy || !client}
                    onClick={() => void removeFolder(folder)}
                  >
                    <X />
                  </button>
                </li>
              ))}
              {folders.length === 0 && (
                <li className="muted">No folders yet. Add one to start sessions here; the first is the working directory.</li>
              )}
            </ul>
          </section>

          <section className="environment-section" aria-label="Permissions">
            <span className="environment-label">Permissions</span>
            <p className="muted permissions-hint">
              Added to the global permissions in Settings → Permissions; applied with Save. Rules reach running sessions at
              their next tool call, paths reach sessions started afterwards.
            </p>
            <PermissionsEditor
              value={permissions}
              onChange={setPermissions}
              presets={presets}
              remote={isRemote(runtime)}
              disabled={busy || !client}
            />
          </section>

          <EnvironmentRuntimeFields
            draft={runtime}
            onChange={setRuntime}
            persistentAgent={persistentAgent}
            onPersistentAgent={setPersistentAgent}
            disabled={busy || !client}
            testBlocked={
              creating ? "Create the environment first" : runtimeChanged ? "Save the runtime first" : null
            }
            onTest={() => {
              if (!client || creating) return Promise.reject(new Error("Not connected to imperium-core."));
              return client.call<RuntimeTestResult>("environments.test_runtime", { id: environment.id });
            }}
          />

          {error && (
            <p className="error" role="alert">
              {error}
            </p>
          )}

          <footer className="environment-footer">
            {!creating &&
              (deleting ? (
                <span className="environment-delete-confirm" role="group" aria-label="Confirm delete">
                  <span>Delete {environment.name}? Folders on disk are untouched.</span>
                  <button type="button" className="danger" onClick={() => void remove()} disabled={busy} autoFocus>
                    Delete
                  </button>
                  <button type="button" onClick={() => setDeleting(false)}>
                    Keep
                  </button>
                </span>
              ) : (
                <button type="button" className="danger" onClick={() => setDeleting(true)} disabled={busy || !client}>
                  <Trash aria-hidden="true" /> Delete environment
                </button>
              ))}
            <span className="spacer" />
            <button type="button" onClick={onClose}>
              Cancel
            </button>
            <button type="submit" className="primary" disabled={busy || !client || !name.trim()}>
              {creating ? "Create" : "Save"}
            </button>
          </footer>
        </form>
      </div>
    </div>,
    document.body,
  );
}
