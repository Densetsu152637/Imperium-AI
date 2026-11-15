// Finding an environment by its id, which the lists, panels and windows all do.

import type { EnvironmentInfo } from "@imperium/protocol";

/** The environment with this id, or null: no id, or one the daemon does not list. */
export function environmentById(environments: readonly EnvironmentInfo[], id: string | null | undefined): EnvironmentInfo | null {
  return id ? (environments.find((env) => env.id === id) ?? null) : null;
}
