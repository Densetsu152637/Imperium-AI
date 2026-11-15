// Finding a model-table row from a name that may be its id or its spec (`<endpoint>:<model>`).
// Two orders exist, because callers hold different kinds of name: a configured name is an id
// first (`models.default` may name a row by its id), a name taken from a session or a call is
// a spec first. Pure, so the rules are tested without a window (test/modelSpec.test.ts).

/** The row whose id is `id`; a spec never matches. */
export function rowById<T extends { id: string }>(rows: readonly T[], id: string): T | undefined {
  return rows.find((row) => row.id === id);
}

/** The row `name` stands for when it may be a spec or an id: the row with that spec, else the one with that id. */
export function rowBySpecOrId<T extends { id: string; spec: string }>(rows: readonly T[], name: string): T | undefined {
  return rows.find((row) => row.spec === name) ?? rows.find((row) => row.id === name);
}
