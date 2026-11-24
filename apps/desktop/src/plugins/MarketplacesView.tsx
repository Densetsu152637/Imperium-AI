// The Marketplaces view: the ones the daemon knows (the official one is built in), with add, refresh
// and remove, and what a marketplace reported as wrong with it.

import { Library, Plus, RefreshCw, Trash } from "lucide-react";
import { useEffect, useId, useRef, useState } from "react";
import type { MarketplaceInfo } from "@imperium/protocol";
import { Badge } from "../components/Badge";
import { EmptyState } from "../components/EmptyState";
import { Field } from "../components/Field";
import { IconButton } from "../components/IconButton";
import { LoadingState, Spinner } from "../components/LoadingState";
import { Notice } from "../components/Notice";
import { marketplaceSummary, plain } from "./model";

export type Draft = { source: string; revision: string };

export function MarketplacesView({
  marketplaces,
  busy,
  draft,
  onDraft,
  onAdd,
  onRefresh,
  onRemove,
  notes,
}: {
  marketplaces: MarketplaceInfo[] | null;
  /** What is under way: "add", "*" (refresh all), or a marketplace's name. */
  busy: string | null;
  draft: Draft;
  onDraft: (draft: Draft) => void;
  onAdd: () => void;
  onRefresh: (name?: string) => void;
  onRemove: (name: string) => void;
  /** What the last refresh reported, as plain lines. */
  notes: string[];
}) {
  const [confirming, setConfirming] = useState<string | null>(null);
  const source = useRef<HTMLInputElement>(null);
  const formId = useId();
  // An offer from a project fills the form and sends the user here; put the cursor on it.
  useEffect(() => {
    if (draft.source) source.current?.focus();
  }, [draft.source]);
  if (!marketplaces) return <LoadingState />;
  return (
    <>
      <form
        className="plugin-add-form"
        aria-labelledby={formId}
        onSubmit={(event) => {
          event.preventDefault();
          if (draft.source.trim() && busy === null) onAdd();
        }}
      >
        <h3 id={formId}>Add a marketplace</h3>
        <div className="form-grid">
          <Field label="Source" hint="A GitHub owner/name, a git address, or a folder.">
            <input ref={source} className="field" value={draft.source} onChange={(e) => onDraft({ ...draft, source: e.target.value })} spellCheck={false} />
          </Field>
          <Field label="Revision" hint="Optional: a branch, tag or commit (git only).">
            <input className="field" value={draft.revision} onChange={(e) => onDraft({ ...draft, revision: e.target.value })} spellCheck={false} />
          </Field>
        </div>
        <div>
          <button type="submit" className="primary" disabled={!draft.source.trim() || busy !== null}>
            {busy === "add" ? <Spinner /> : <Plus aria-hidden="true" />} Add marketplace
          </button>
        </div>
      </form>
      <section className="connectors-section" aria-labelledby={`${formId}-list`}>
        <div className="connectors-section-head">
          <h3 id={`${formId}-list`}>Marketplaces</h3>
          <button className="small" disabled={busy !== null || marketplaces.length === 0} onClick={() => onRefresh()}>
            {busy === "*" ? <Spinner /> : <RefreshCw aria-hidden="true" />} Refresh all
          </button>
        </div>
        {notes.length > 0 && (
          <Notice tone="info" className="connectors-notice">
            {notes.map((line) => (
              <div key={line}>{line}</div>
            ))}
          </Notice>
        )}
        {marketplaces.length === 0 ? (
          <EmptyState icon={Library} title="No marketplaces" body="Add one above to browse the plugins it lists." />
        ) : (
          <ul className="connector-rows">
            {marketplaces.map((m) => (
              <li key={m.name} className="connector-row" data-enabled={m.enabled}>
                <span className="connector-icon small" aria-hidden="true">
                  <Library />
                </span>
                <div className="connector-row-title">
                  <strong>{plain(m.name)}</strong>
                  {m.built_in && <Badge tone="accent">Built in</Badge>}
                  {!m.enabled && <Badge>Off</Badge>}
                  {m.problems.length > 0 && <Badge tone="warn">{m.problems.length === 1 ? "1 problem" : `${m.problems.length} problems`}</Badge>}
                </div>
                <div className="connector-row-actions">
                  <IconButton icon={RefreshCw} label={`Refresh ${plain(m.name)}`} size="sm" disabled={busy !== null || !m.enabled} onClick={() => onRefresh(m.name)} />
                  <IconButton
                    icon={Trash}
                    label={`${m.built_in ? "Turn off" : "Remove"} ${plain(m.name)}`}
                    size="sm"
                    disabled={busy !== null || !m.enabled}
                    onClick={() => setConfirming(m.name)}
                  />
                </div>
                <div className="connector-row-detail">
                  <code className="connector-row-where" title={plain(m.source)}>
                    {plain(m.source)}
                    {m.revision ? ` @ ${plain(m.revision)}` : ""}
                  </code>
                  <span className="connector-row-source">{busy === m.name ? "Refreshing…" : marketplaceSummary(m)}</span>
                  {m.problems.map((problem) => (
                    <Notice key={problem} tone="warn">
                      {plain(problem)}
                    </Notice>
                  ))}
                  {confirming === m.name && (
                    <div className="connector-confirm" role="alertdialog" aria-label={`${m.built_in ? "Turn off" : "Remove"} ${plain(m.name)}`}>
                      <span>
                        {m.built_in ? "Turn off" : "Remove"} {plain(m.name)}? Plugins already installed stay installed.
                      </span>
                      <button
                        className="danger small"
                        onClick={() => {
                          setConfirming(null);
                          onRemove(m.name);
                        }}
                      >
                        {m.built_in ? "Turn off" : "Remove"}
                      </button>
                      <button className="small" onClick={() => setConfirming(null)}>
                        Cancel
                      </button>
                    </div>
                  )}
                </div>
              </li>
            ))}
          </ul>
        )}
      </section>
    </>
  );
}
