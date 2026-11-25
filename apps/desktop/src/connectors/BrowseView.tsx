// The Browse view: the catalog of servers, plugins and skills.

import { Check, Plus, SearchX } from "lucide-react";
import type { CatalogServer } from "@imperium/protocol";
import { Badge } from "../components/Badge";
import { EmptyState } from "../components/EmptyState";
import { LoadingState } from "../components/LoadingState";
import { Notice } from "../components/Notice";
import { ConnectorIcon } from "./connectorIcons";
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
  if (loading) return <LoadingState label="Loading the directory…" />;
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
        <Notice key={problem} tone="warn" className="connectors-notice">
          {problem}
        </Notice>
      ))}
      <div className="connector-grid">
        {shown.map((entry) => (
          <article key={entry.id} className={`card connector-card${added.has(entry.id) ? "" : " interactive"}`} onClick={added.has(entry.id) ? undefined : () => onAdd(entry)}>
            <div className="connector-card-head">
              <ConnectorIcon icon={entry.icon} />
              <div className="connector-card-title">
                <h3>{entry.name}</h3>
                {entry.publisher && <span className="connector-card-publisher">{entry.publisher}</span>}
              </div>
            </div>
            <p className="connector-card-description">{entry.description}</p>
            <div className="connector-card-foot">
              <Badge>{entry.category}</Badge>
              <Badge>{entry.url ? "Remote" : "Local"}</Badge>
              <span className="connector-card-gap" />
              {added.has(entry.id) ? (
                <Badge tone="ok" icon={Check} title="Already installed">
                  Added
                </Badge>
              ) : (
                <button
                  className="primary small"
                  onClick={(event) => {
                    event.stopPropagation();
                    onAdd(entry);
                  }}
                  aria-label={`Add ${entry.name}`}
                >
                  <Plus aria-hidden="true" /> Add
                </button>
              )}
            </div>
          </article>
        ))}
        {!query && !category && (
          <article className="card interactive connector-card custom" onClick={onCustom}>
            <div className="connector-card-head">
              <ConnectorIcon icon="terminal" />
              <div className="connector-card-title">
                <h3>Custom connector</h3>
                <span className="connector-card-publisher">Any MCP server</span>
              </div>
            </div>
            <p className="connector-card-description">Add a server by the command that starts it or the URL it serves on.</p>
            <div className="connector-card-foot">
              <span className="connector-card-gap" />
              <button
                className="small"
                onClick={(event) => {
                  event.stopPropagation();
                  onCustom();
                }}
              >
                <Plus aria-hidden="true" /> Add custom
              </button>
            </div>
          </article>
        )}
      </div>
      {shown.length === 0 && (
        <EmptyState icon={SearchX} title={query ? `No connectors match “${query}”` : "Nothing in this category"} body="Try another word, or pick All." />
      )}
      {overrideFile && (
        <p className="muted connectors-footnote">
          Add your own entries to the directory in <code>{overrideFile}</code>.
        </p>
      )}
    </>
  );
}

// ---- Installed ----
