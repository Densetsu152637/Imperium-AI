// The search box at the top of the Sessions panel and its results: one row per matching session
// with the best excerpt, matches marked. Choosing a result opens the session at the match.

import { Archive, Search, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";

import type { SessionSearchHit } from "@imperium/protocol";

import { highlightSegments, hitKindLabel, searchSessions, type SearchClient } from "../sessionSearch";

/** Wait this long after typing stops before searching. */
const DEBOUNCE_MS = 250;

export function SessionSearch({
  client,
  environmentId,
  onOpen,
  onActiveChange,
}: {
  client: SearchClient | null;
  /** Search this environment only; every session when null. */
  environmentId: string | null;
  onOpen: (sessionId: string, eventIndex: number) => void;
  /** Whether a query is entered, so the panel can show results instead of its list. */
  onActiveChange: (active: boolean) => void;
}) {
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<SessionSearchHit[]>([]);
  const [state, setState] = useState<"idle" | "searching" | "done" | "failed">("idle");
  const [error, setError] = useState<string | null>(null);
  const latest = useRef(0);
  const trimmed = query.trim();

  useEffect(() => onActiveChange(trimmed.length > 0), [trimmed, onActiveChange]);

  useEffect(() => {
    if (!trimmed || !client) {
      setHits([]);
      setState("idle");
      return;
    }
    const attempt = ++latest.current;
    setState("searching");
    const timer = window.setTimeout(() => {
      searchSessions(client, trimmed, { environment_id: environmentId ?? undefined, limit: 30 })
        .then((result) => {
          if (attempt !== latest.current) return;
          setHits(result.hits);
          setState("done");
          setError(null);
        })
        .catch((reason: unknown) => {
          if (attempt !== latest.current) return;
          setHits([]);
          setState("failed");
          setError(reason instanceof Error ? reason.message : String(reason));
        });
    }, DEBOUNCE_MS);
    return () => window.clearTimeout(timer);
  }, [trimmed, client, environmentId]);

  return (
    <div className="session-search">
      <label className="session-search-box">
        <Search aria-hidden="true" />
        <input
          type="search"
          placeholder={environmentId ? "Search this environment's sessions" : "Search sessions"}
          aria-label="Search sessions"
          value={query}
          disabled={!client}
          onChange={(event) => setQuery(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Escape") setQuery("");
          }}
        />
        {query && (
          <button className="icon" aria-label="Clear search" onClick={() => setQuery("")}>
            <X aria-hidden="true" />
          </button>
        )}
      </label>
      {trimmed && (
        <div className="session-search-results" aria-live="polite">
          {state === "failed" && <p className="empty">Search failed: {error}</p>}
          {state === "searching" && hits.length === 0 && <p className="empty">Searching…</p>}
          {state === "done" && hits.length === 0 && <p className="empty">No session mentions that.</p>}
          {hits.length > 0 && (
            <ul className="list" aria-label="Search results">
              {hits.map((hit) => (
                <li key={hit.session_id}>
                  <button className="row-main search-hit" onClick={() => onOpen(hit.session_id, hit.event_index)}>
                    <span className="name">
                      {hit.title || "Untitled session"}
                      {hit.archived && <Archive className="pin-mark" aria-label="Archived" />}
                    </span>
                    <span className="search-snippet">
                      {highlightSegments(hit.snippet, hit.highlights).map((segment, index) =>
                        segment.match ? <mark key={index}>{segment.text}</mark> : <span key={index}>{segment.text}</span>,
                      )}
                    </span>
                    <span className="row-meta">
                      <span className="detail">
                        {hitKindLabel(hit.kind)}
                        {hit.at_ms ? ` · ${new Date(hit.at_ms).toLocaleDateString()}` : ""}
                        {hit.more > 0 ? ` · ${hit.more} more` : ""}
                        {hit.semantic ? " · related" : ""}
                      </span>
                    </span>
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
    </div>
  );
}
