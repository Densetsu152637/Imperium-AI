// Display names of models. A model may have an alias (`alias` in its model-table row or local
// model settings, set in the model editor) that the interface shows instead of its id, with the
// id kept as secondary or hover text. Ids and specs are unchanged everywhere they are stored or
// sent; only what is shown changes. App.tsx publishes the aliases from `models.list` and
// `models.local`; components read them with `useModelNames`.

import { useSyncExternalStore } from "react";

import type { LocalModelRow, ModelRow } from "@imperium/protocol";

/** Aliases by every name a model goes by. */
export type ModelAliases = ReadonlyMap<string, string>;

/**
 * Aliases by name: a local model's under its id and its `<runtime>:<id>` spec; a table row's
 * under its id and its spec. A table row without an alias of its own takes the alias of the
 * local model it names, and a row's alias names its spec too.
 */
export function aliasMap(table: readonly ModelRow[], local: readonly LocalModelRow[]): ModelAliases {
  const map = new Map<string, string>();
  for (const row of local) {
    if (!row.alias) continue;
    map.set(row.id, row.alias);
    map.set(`${row.runtime}:${row.id}`, row.alias);
  }
  for (const row of table) {
    const alias = row.alias ?? map.get(row.spec);
    if (!alias) continue;
    map.set(row.id, alias);
    map.set(row.spec, alias);
  }
  return map;
}

/** What to show for a model name (an id or a spec): its alias, else the name itself. */
export function modelName(aliases: ModelAliases, name: string): string {
  return aliases.get(name) ?? name;
}

/** Hover text for a model shown by its alias: the real name; undefined when it has no alias. */
export function modelTitle(aliases: ModelAliases, name: string): string | undefined {
  return aliases.has(name) ? name : undefined;
}

let current: ModelAliases = new Map();
const listeners = new Set<() => void>();

/** Publishes the aliases from the latest `models.list` and `models.local`. */
export function setModelAliases(table: readonly ModelRow[], local: readonly LocalModelRow[]): void {
  const next = aliasMap(table, local);
  if (next.size === current.size && [...next].every(([name, alias]) => current.get(name) === alias)) return;
  current = next;
  listeners.forEach((listener) => listener());
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** The aliases now published, for code outside React. */
export const modelAliases = (): ModelAliases => current;

/** The published aliases; the component re-renders when they change. */
export function useModelNames(): ModelAliases {
  return useSyncExternalStore(subscribe, modelAliases, modelAliases);
}
