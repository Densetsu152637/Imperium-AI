// The Browse view: the catalog of servers, plugins and skills.

import { Check, Plus } from "lucide-react";
import type { CatalogServer } from "@imperium/protocol";
import { ConnectorIcon } from "../connectorIcons";
import { matches } from "./common";

export function BrowseView({
  entries,
  query,
  category,
  onCategory,
  added,
  onAdd,
  onCustom,
  loading,
  problems,
  overrideFile,
}: {
  entries: CatalogServer[];
  query: string;
  category: string | null;
  onCategory: (category: string | null) => void;
  added: Set<string>;
  onAdd: (entry: CatalogServer) => void;
  onCustom: () => void;
  loading: boolean;
  problems: string[];
  overrideFile: string | null;
}) {
  const categories = [...new Set(entries.map((entry) => entry.category))].sort();
  const shown = entries.filter(
    (entry) =>
      (!category || entry.category === category) &&
      matches(query, entry.name, entry.description, entry.category, entry.publisher, entry.id),
  );
  if (loading) return <p className="connectors-empty muted">Loading the directory…</p>;
  return (
    <>
      <div className="chips" role="group" aria-label="Categories">
        {[null, ...categories].map((name) => (
          <button key={name ?? "all"} className="chip" aria-pressed={category === name} onClick={() => onCategory(name)}>
            {name ?? "All"}
          </button>
        ))}
      </div>
      {problems.map((problem) => (
        <p key={problem} className="warning">
          {problem}
        </p>
      ))}
      <div className="connector-grid">
        {shown.map((entry) => (
          <article key={entry.id} className="connector-card">
            <div className="connector-card-head">
              <ConnectorIcon icon={entry.icon} />
              <div className="connector-card-title">
                <h3>{entry.name}</h3>
                {entry.publisher && <span className="muted">{entry.publisher}</span>}
              </div>
            </div>
            <p className="connector-card-description">{entry.description}</p>
            <div className="connector-card-foot">
              <span className="tag">{entry.category}</span>
              <span className="tag">{entry.url ? "Remote" : "Local"}</span>
              <span className="spacer" />
              {added.has(entry.id) ? (
                <span className="added" title="Already installed">
                  <Check aria-hidden="true" /> Added
                </span>
              ) : (
                <button className="primary small" onClick={() => onAdd(entry)} aria-label={`Add ${entry.name}`}>
                  <Plus aria-hidden="true" /> Add
                </button>
              )}
            </div>
          </article>
        ))}
        {!query && !category && (
          <article className="connector-card custom">
            <div className="connector-card-head">
              <ConnectorIcon icon="terminal" />
              <div className="connector-card-title">
                <h3>Custom connector</h3>
                <span className="muted">Any MCP server</span>
              </div>
            </div>
            <p className="connector-card-description">Add a server by the command that starts it or the URL it serves on.</p>
            <div className="connector-card-foot">
              <span className="spacer" />
              <button className="small" onClick={onCustom}>
                <Plus aria-hidden="true" /> Add custom
              </button>
            </div>
          </article>
        )}
      </div>
      {shown.length === 0 && <p className="connectors-empty muted">No connectors match “{query}”.</p>}
      {overrideFile && (
        <p className="muted connectors-footnote">
          Add your own entries to the directory in <code>{overrideFile}</code>.
        </p>
      )}
    </>
  );
}

// ---- Installed ----
