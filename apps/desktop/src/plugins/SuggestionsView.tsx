// Offers from the selected environment's project: marketplaces and plugins its repository suggests.
// The text is the repository's, so it is drawn as plain text and goes nowhere else. An offer adds,
// installs and enables nothing: a marketplace offer only fills the add form for the user to check.

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
  onDismiss: (key: string) => void;
  onMute: (folder: string) => void;
}) {
  if (!offers) return null;
  if (offers.muted)
    return (
      <Notice tone="info" title="No suggestions from this project">
        Suggestions from this project are turned off.
      </Notice>
    );
  if (offerCount(offers) === 0) return null;
  return (
    <section className="connectors-section plugin-offers" aria-labelledby="plugin-offers-heading">
      <div className="connectors-section-head">
        <h3 id="plugin-offers-heading">
          <Lightbulb aria-hidden="true" /> Suggested by this project
        </h3>
        {offers.source_folder && (
          <button className="small" onClick={() => onMute(offers.source_folder!)}>
            <BellOff aria-hidden="true" /> No suggestions from this project
          </button>
        )}
      </div>
      <p className="connectors-note">The project's files suggest these. Nothing is added or installed until you do it.</p>
      <ul className="connector-rows">
        {offers.marketplaces.map((offer) => (
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
              <button className="small" aria-label={`Dismiss marketplace ${plain(offer.source)}`} onClick={() => onDismiss(offer.key)}>
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
        {offers.plugins.map((offer) => (
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
              <button className="small" aria-label={`Dismiss plugin ${plain(offer.id)}`} onClick={() => onDismiss(offer.key)}>
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
