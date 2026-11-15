// The environments the window knows, the one new sessions start in, and the icons the session
// header takes from them. The daemon connection fills the list in (App.tsx).

import { useMemo, useRef, useState } from "react";

import type { EnvironmentInfo } from "@imperium/protocol";

import { cachedEnvironments } from "../shared/cache";
import { environmentIcon, sameFolder, type ProjectIcons } from "../projectIcons";
import { environmentById } from "./byId";

/** The saved selection, when it is restored: `initialEnvId`, and whether the Sessions list shows only its sessions. */
export function useEnvironments(initialEnvId: string | null, initialEnvView: boolean) {
  const [environments, setEnvironments] = useState<EnvironmentInfo[]>(cachedEnvironments);
  // The environment new sessions start in, and whether the Sessions list shows only its sessions.
  const [envId, setEnvId] = useState<string | null>(initialEnvId);
  /** The selected environment for state updates that run later, so a layout edit lands on the environment it was made in. */
  const envIdRef = useRef(envId);
  envIdRef.current = envId;
  const [envView, setEnvView] = useState(initialEnvView);
  /** Environments with a folder in a git repository: new sessions there may start on worktrees. */
  const [gitEnvironments, setGitEnvironments] = useState<string[]>([]);
  const environment = environmentById(environments, envId);
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
  return {
    environments,
    setEnvironments,
    envId,
    setEnvId,
    envIdRef,
    envView,
    setEnvView,
    gitEnvironments,
    setGitEnvironments,
    environment,
    icons,
  };
}
