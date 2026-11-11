// The model table: every model delegation can use, searchable and sortable. Favourites come
// first and hidden models last, in a collapsed group (modelPrefs.ts).

import { ChevronDown, ChevronUp } from "lucide-react";
import { useMemo, useState } from "react";

import type { ModelListResult, ModelRow } from "@imperium/protocol";

import { arrangeModels, markOf } from "../modelPrefs";
import { useModelMarks } from "../settings";
import { HiddenGroupRow, ModelMarkButtons } from "./ModelMarks";

type SortKey = "id" | "location" | "tier" | "price";

const TIER_ORDER = { basic: 0, standard: 1, strong: 2, frontier: 3 } as const;

const price = (row: ModelRow): number => (row.input_usd_per_mtok ?? 0) + (row.output_usd_per_mtok ?? 0);

function compare(a: ModelRow, b: ModelRow, key: SortKey): number {
  switch (key) {
    case "id":
      return a.id.localeCompare(b.id);
    case "location":
      return a.location.localeCompare(b.location) || a.id.localeCompare(b.id);
    case "tier":
      return TIER_ORDER[a.tier] - TIER_ORDER[b.tier] || a.id.localeCompare(b.id);
    case "price":
      return price(a) - price(b) || a.id.localeCompare(b.id);
  }
}

const formatPrice = (row: ModelRow): string =>
  row.input_usd_per_mtok === null && row.output_usd_per_mtok === null
    ? "free"
    : `$${row.input_usd_per_mtok ?? 0} / $${row.output_usd_per_mtok ?? 0}`;

export function ModelsPanel({ table, onConfigure }: { table: ModelListResult | null; onConfigure: () => void }) {
  const [query, setQuery] = useState("");
  const [sort, setSort] = useState<{ key: SortKey; descending: boolean }>({ key: "id", descending: false });
  const [showHidden, setShowHidden] = useState(false);
  const { marks, toggle } = useModelMarks();

  const rows = useMemo(() => {
    const words = query.toLowerCase().split(/\s+/).filter(Boolean);
    const matching = (table?.models ?? []).filter((row) => {
      const text = [row.id, row.alias ?? "", row.spec, row.location, row.tier, ...row.tags].join(" ").toLowerCase();
      return words.every((word) => text.includes(word));
    });
    const sorted = [...matching].sort((a, b) => compare(a, b, sort.key));
    return arrangeModels(sort.descending ? sorted.reverse() : sorted, (row) => row.spec, marks);
  }, [table, query, sort, marks]);

  if (!table || table.models.length === 0) {
    return (
      <p className="empty">
        No model table yet. Add <code>[[models.catalog]]</code> entries (id, spec, location, tier) to your config.
      </p>
    );
  }

  const header = (key: SortKey, label: string) => (
    <th aria-sort={sort.key === key ? (sort.descending ? "descending" : "ascending") : "none"}>
      <button
        className="sort"
        onClick={() => setSort((current) => ({ key, descending: current.key === key && !current.descending }))}
      >
        {label}
        {sort.key === key &&
          (sort.descending ? <ChevronDown aria-hidden="true" /> : <ChevronUp aria-hidden="true" />)}
      </button>
    </th>
  );

  const modelRow = (row: ModelRow, hidden: boolean) => (
    <tr
      key={row.id}
      className={[row.endpoint_configured && row.enabled ? null : "unusable", hidden ? "model-hidden" : null].filter(Boolean).join(" ") || undefined}
      title={row.alias ? `${row.id} · ${row.spec}` : row.spec}
    >
      <td>
        {row.alias ?? row.id}
        {!row.enabled && <span className="badge off">off</span>}
      </td>
      <td>
        <span className={`badge ${row.location}`}>{row.location}</span>
      </td>
      <td>{row.tier}</td>
      <td>{formatPrice(row)}</td>
      <td>{row.tags.join(", ")}</td>
      <td>
        <ModelMarkButtons name={row.id} mark={markOf(marks, row.spec)} onToggle={(mark) => toggle(row.spec, mark)} />
      </td>
    </tr>
  );

  return (
    <div className="models">
      <div className="models-toolbar">
        <input
          type="search"
          placeholder="Filter by id, model, location, tier, or tag"
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          aria-label="Filter models"
        />
        <span className="muted">
          prefer {table.prefer} · {table.rules} rule{table.rules === 1 ? "" : "s"}
        </span>
        <button onClick={onConfigure}>Configure</button>
      </div>
      <table>
        <thead>
          <tr>
            {header("id", "Id")}
            {header("location", "Where")}
            {header("tier", "Tier")}
            {header("price", "$/Mtok in/out")}
            <th>Tags</th>
            <th>
              <span className="visually-hidden">Favourite or hide</span>
            </th>
          </tr>
        </thead>
        <tbody>
          {[...rows.favourites, ...rows.normal].map((row) => modelRow(row, false))}
          {rows.hidden.length > 0 && (
            <HiddenGroupRow count={rows.hidden.length} open={showHidden} colSpan={6} onToggle={() => setShowHidden((open) => !open)} />
          )}
          {showHidden && rows.hidden.map((row) => modelRow(row, true))}
        </tbody>
      </table>
    </div>
  );
}
