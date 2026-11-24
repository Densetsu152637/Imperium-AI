// Offers from the selected environment's project: marketplaces and plugins its repository suggests.
// The text is the repository's, so it is drawn as plain text and goes nowhere else. An offer adds,
// installs and enables nothing: a marketplace offer only fills the add form for the user to check.

import { useEffect, useRef } from "react";
import { BellOff, Lightbulb, X } from "lucide-react";
import type { SuggestedMarketplace } from "@imperium/protocol";
import { Notice } from "../components/Notice";
import { offerCount, plain, shortPath, type Offers } from "./model";

export function SuggestionsView({
  offers,
  onReview,
  onFind,
  onDismiss,
  onMute,
}: {
  offers: Offers | null;
  onReview: (offer: SuggestedMarketplace) => void;
  onFind: (id: string) => void;
  /** Resolve true when the daemon took it; on false the focus plan is dropped. */
  onDismiss: (key: string) => Promise<boolean>;
  onMute: (folder: string) => Promise<boolean>;
}) {
  const section = useRef<HTMLElement>(null);
  const mutedNote = useRef<HTMLDivElement>(null);
  /** Where focus goes once the control the user pressed is gone: the next row's Dismiss, or the muted note. */
  const after = useRef<{ row: number } | "muted" | null>(null);
  useEffect(() => {
    const want = after.current;
    if (!want) return;
    after.current = null;
    if (want === "muted") return mutedNote.current?.focus();
    const buttons = section.current?.querySelectorAll<HTMLElement>("[data-dismiss]");
    const target = buttons && buttons.length > 0 ? buttons[Math.min(want.row, buttons.length - 1)] : null;
    (target ?? document.querySelector<HTMLElement>("[role=tabpanel]"))?.focus();
  }, [offers]);
  const clearOnFailure = (ok: boolean) => {
    if (!ok) after.current = null;
  };
  if (!offers) return null;
  if (offers.muted)
    return (
      <div ref={mutedNote} tabIndex={-1} className="plugin-muted">
        <Notice tone="info" title="No suggestions from this project">
          Suggestions from this project are turned off.
        </Notice>
      </div>
    );
  if (offerCount(offers) === 0) return null;
  return (
    <section ref={section} className="connectors-section plugin-offers" aria-labelledby="plugin-offers-heading">
      <div className="connectors-section-head">
        <h3 id="plugin-offers-heading">
          <Lightbulb aria-hidden="true" /> Suggested by this project
        </h3>
        {offers.source_folder && (
          <button
            className="small"
            onClick={() => {
              after.current = "muted";
              void onMute(offers.source_folder!).then(clearOnFailure);
            }}
          >
            <BellOff aria-hidden="true" /> No suggestions from this project
          </button>
        )}
      </div>
      <p className="connectors-note">The project's files suggest these. Nothing is added or installed until you do it.</p>
      <ul className="connector-rows">
        {offers.marketplaces.map((offer, index) => (
          <li key={offer.key} className="connector-row">
            <span />
            <div className="connector-row-title">
              <strong>Marketplace</strong>
              <code className="plugin-offer-text">
                {plain(offer.source)}
                {offer.revision ? ` @ ${plain(offer.revision)}` : ""}
              </code>
            </div>
            <div className="connector-row-actions">
              {offer.added ? (
                <span className="connector-row-counts">Already added</span>
              ) : (
                <button className="small" onClick={() => onReview(offer)}>
                  Review
                </button>
              )}
              <button
                className="small"
                data-dismiss
                aria-label={`Dismiss marketplace ${plain(offer.source)}`}
                onClick={() => {
                  after.current = { row: index };
                  void onDismiss(offer.key).then(clearOnFailure);
                }}
              >
                <X aria-hidden="true" /> Dismiss
              </button>
            </div>
            <div className="connector-row-detail">
              <span className="connector-row-source" title={plain(offer.file)}>
                From {shortPath(offer.file)}
              </span>
            </div>
          </li>
        ))}
        {offers.plugins.map((offer, index) => (
          <li key={offer.key} className="connector-row">
            <span />
            <div className="connector-row-title">
              <strong>Plugin</strong>
              <code className="plugin-offer-text">{plain(offer.id)}</code>
            </div>
            <div className="connector-row-actions">
              {offer.installed ? (
                <span className="connector-row-counts">Installed</span>
              ) : (
                <button className="small" onClick={() => onFind(offer.id)}>
                  Find
                </button>
              )}
              <button
                className="small"
                data-dismiss
                aria-label={`Dismiss plugin ${plain(offer.id)}`}
                onClick={() => {
                  after.current = { row: offers.marketplaces.length + index };
                  void onDismiss(offer.key).then(clearOnFailure);
                }}
              >
                <X aria-hidden="true" /> Dismiss
              </button>
            </div>
            <div className="connector-row-detail">
              <span className="connector-row-source" title={plain(offer.file)}>
                From {shortPath(offer.file)}
                {offer.marketplace_added ? "" : " · its marketplace is not added yet"}
              </span>
            </div>
          </li>
        ))}
      </ul>
    </section>
  );
}
