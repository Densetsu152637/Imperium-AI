// The Browse view: plugins the marketplaces list, as cards, and one plugin's detail. Everything a
// marketplace supplies is drawn as plain text through `plain`; nothing here installs a plugin.

import { ArrowLeft, Check, CircleArrowUp, Puzzle, SearchX } from "lucide-react";
import { useEffect, useRef } from "react";
import type { InstalledPlugin, MarketplaceInfo, PluginListing } from "@imperium/protocol";
import { Badge } from "../components/Badge";
import { EmptyState } from "../components/EmptyState";
import { LoadingState } from "../components/LoadingState";
import { Notice } from "../components/Notice";
import { categoriesOf, componentSummary, filterListings, installedById, listingState, plain, type ListingState } from "./model";

export function StateBadge({ state }: { state: ListingState }) {
  if (state === "update")
    return (
      <Badge tone="accent" icon={CircleArrowUp} title="The marketplace has changed since this was installed">
        Update available
      </Badge>
    );
  if (state === "installed")
    return (
      <Badge tone="ok" icon={Check}>
        Installed
      </Badge>
    );
  return null;
}

export function BrowseView({
  plugins,
  problems,
  query,
  category,
  onCategory,
  marketplace,
  onMarketplace,
  marketplaces,
  installed,
  changed,
  onOpen,
  onAddMarketplace,
}: {
  plugins: PluginListing[] | null;
  problems: string[];
  query: string;
  category: string | null;
  onCategory: (category: string | null) => void;
  marketplace: string | null;
  onMarketplace: (name: string | null) => void;
  marketplaces: MarketplaceInfo[];
  installed: InstalledPlugin[];
  changed: ReadonlySet<string>;
  onOpen: (id: string) => void;
  onAddMarketplace: () => void;
}) {
  if (!plugins) return <LoadingState label="Reading the marketplaces…" />;
  const byId = installedById(installed);
  const inMarketplace = plugins.filter((p) => !marketplace || p.marketplace === marketplace);
  const categories = categoriesOf(inMarketplace);
  const shown = filterListings(inMarketplace, query, null).filter((p) => !category || plain(p.category) === category);
  const enabled = marketplaces.filter((m) => m.enabled);
  return (
    <>
      {enabled.length > 1 && (
        <div className="chips" role="group" aria-label="Marketplaces">
          {[null, ...enabled.map((m) => m.name)].map((name) => (
            <button key={name ?? "all"} className="chip" aria-pressed={marketplace === name} onClick={() => onMarketplace(name)}>
              {name ? plain(name) : "All marketplaces"}
            </button>
          ))}
        </div>
      )}
      {categories.length > 0 && (
        <div className="chips" role="group" aria-label="Categories">
          {[null, ...categories].map((name) => (
            <button key={name ?? "all"} className="chip" aria-pressed={category === name} onClick={() => onCategory(name)}>
              {name ?? "All"}
            </button>
          ))}
        </div>
      )}
      {problems.map((problem) => (
        <Notice key={problem} tone="warn">
          {plain(problem)}
        </Notice>
      ))}
      {shown.length > 0 && (
        <div className="connector-grid">
          {shown.map((plugin) => {
            const summary = componentSummary(plugin);
            return (
              <article key={plugin.id} className="card interactive connector-card" onClick={() => onOpen(plugin.id)}>
                <div className="connector-card-head">
                  <span className="connector-icon" aria-hidden="true">
                    <Puzzle />
                  </span>
                  <div className="connector-card-title">
                    <h3>
                      <button
                        className="plugin-card-open"
                        data-plugin-id={plugin.id}
                        onClick={(event) => {
                          event.stopPropagation();
                          onOpen(plugin.id);
                        }}
                      >
                        {plain(plugin.name)}
                      </button>
                    </h3>
                    <span className="connector-card-publisher">{plain(plugin.marketplace)}</span>
                  </div>
                </div>
                <p className="connector-card-description plugin-clamp">{plain(plugin.description) || "No description."}</p>
                <div className="connector-card-foot plugin-card-foot">
                  {plugin.category && <Badge>{plain(plugin.category)}</Badge>}
                  {summary && <span className="connector-row-counts">{summary}</span>}
                  <span className="connector-card-gap" />
                  <StateBadge state={listingState(plugin, byId, marketplaces, changed)} />
                </div>
              </article>
            );
          })}
        </div>
      )}
      {shown.length === 0 && (
        <EmptyState
          icon={SearchX}
          title={plugins.length === 0 ? "No plugins to show" : query ? `No plugins match “${query}”` : "Nothing here"}
          body={plugins.length === 0 ? "Add a marketplace to see the plugins it lists." : "Try another word, or pick All."}
          action={
            plugins.length === 0 ? (
              <button className="primary" onClick={onAddMarketplace}>
                Add a marketplace
              </button>
            ) : undefined
          }
        />
      )}
    </>
  );
}

function List({ title, items }: { title: string; items: string[] }) {
  if (items.length === 0) return null;
  return (
    <div className="plugin-detail-group">
      <h4>{title}</h4>
      <ul className="plugin-detail-list">
        {items.map((item) => (
          <li key={item}>
            <code>{plain(item)}</code>
          </li>
        ))}
      </ul>
    </div>
  );
}

export function DetailView({
  plugin,
  state,
  onBack,
  onInstall,
  onUpdate,
}: {
  plugin: PluginListing;
  state: ListingState;
  onBack: () => void;
  /** Opens the trust view to install. */
  onInstall: () => void;
  /** Opens the trust view to update. */
  onUpdate: () => void;
}) {
  const c = plugin.components;
  const back = useRef<HTMLButtonElement>(null);
  // The card that opened this is gone from the page, so focus starts on Back.
  useEffect(() => back.current?.focus(), []);
  const facts: [string, string | null][] = [
    ["Marketplace", plain(plugin.marketplace)],
    ["Version", plain(plugin.version) || null],
    ["Author", plain(plugin.author) || null],
    ["Category", plain(plugin.category) || null],
    ["License", plain(plugin.license) || null],
    ["Homepage", plain(plugin.homepage) || null],
    ["Source", plugin.source_kind === "unsupported" ? "A source type this version cannot read" : `${plain(plugin.source_kind)}: ${plain(plugin.source)}`],
  ];
  return (
    <div className="connector-form plugin-detail">
      <button ref={back} type="button" className="ghost back" onClick={onBack}>
        <ArrowLeft aria-hidden="true" /> Back
      </button>
      <div className="connector-form-head">
        <span className="connector-icon large" aria-hidden="true">
          <Puzzle />
        </span>
        <div>
          <h3>{plain(plugin.name)}</h3>
          <div className="connector-form-subtitle">{plain(plugin.id)}</div>
        </div>
        <StateBadge state={state} />
      </div>
      <p className="plugin-detail-description">{plain(plugin.description) || "No description."}</p>
      <dl className="plugin-facts">
        {facts.map(([label, value]) =>
          value ? (
            <div key={label}>
              <dt>{label}</dt>
              <dd>{value}</dd>
            </div>
          ) : null,
        )}
      </dl>
      {c ? (
        <>
          <List title="Skills" items={c.skills} />
          <List title="Commands" items={c.commands} />
          <List title="Agents" items={c.agents} />
          <List title="MCP servers" items={c.mcp_servers} />
          {c.hooks > 0 && <p className="connectors-note">{c.hooks === 1 ? "1 hook that runs a command." : `${c.hooks} hooks that run commands.`}</p>}
        </>
      ) : (
        <p className="connectors-note">The marketplace does not say what this plugin contains.</p>
      )}
      {plugin.source_kind === "unsupported" ? (
        <Notice tone="info">This version cannot read this plugin's source, so it cannot be installed.</Notice>
      ) : (
        <div className="trust-actions">
          {state === "available" ? (
            <button className="primary" onClick={onInstall}>
              Install…
            </button>
          ) : (
            <button className="primary" onClick={onUpdate}>
              Review update…
            </button>
          )}
        </div>
      )}
    </div>
  );
}
