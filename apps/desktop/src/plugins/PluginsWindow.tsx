// The Plugins window: plugin marketplaces to browse, the marketplaces themselves, what is
// installed, and offers from the selected environment's project. A separate window from
// Connectors, which has its own Installed and Browse for MCP servers. It talks to the daemon's
// plugins.* methods and installs nothing: the install flow comes later.

import { Puzzle, Search, X } from "lucide-react";
import { useCallback, useEffect, useId, useMemo, useRef, useState } from "react";
import type { MarketplaceInfo, PluginListing, PluginsBrowseResult, PluginsListResult, SuggestedMarketplace } from "@imperium/protocol";
import { Dialog } from "../components/Dialog";
import { EmptyState } from "../components/EmptyState";
import { IconButton } from "../components/IconButton";
import { Notice } from "../components/Notice";
import { Tabs, tabPanelProps } from "../components/Tabs";
import type { DaemonClient } from "../daemon/client";
import { BrowseView, DetailView } from "./BrowseView";
import { InstalledView } from "./InstalledView";
import { MarketplacesView, type Draft } from "./MarketplacesView";
import { SuggestionsView } from "./SuggestionsView";
import { changedIds, installedById, listingState, message, mutedOffers, pluginsApi, refreshSummary, withoutOffer, type Offers } from "./model";

export type PluginsTab = "installed" | "browse" | "marketplaces";

export function PluginsWindow({
  client,
  environment,
  onClose,
  initialTab = "browse",
}: {
  client: DaemonClient | null;
  /** The selected environment's id: whose installed plugins and project offers to show. */
  environment: string | null;
  onClose: () => void;
  initialTab?: PluginsTab;
}) {
  const [tab, setTab] = useState<PluginsTab>(initialTab);
  const [detail, setDetail] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [category, setCategory] = useState<string | null>(null);
  const [marketplace, setMarketplace] = useState<string | null>(null);
  const [marketplaces, setMarketplaces] = useState<MarketplaceInfo[] | null>(null);
  const [browse, setBrowse] = useState<PluginsBrowseResult | null>(null);
  const [installed, setInstalled] = useState<PluginsListResult | null>(null);
  const [offers, setOffers] = useState<Offers | null>(null);
  const [changed, setChanged] = useState<ReadonlySet<string>>(new Set());
  const [notes, setNotes] = useState<string[]>([]);
  const [draft, setDraft] = useState<Draft>({ source: "", revision: "" });
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const idBase = useId();
  const bodyRef = useRef<HTMLDivElement>(null);
  const api = useMemo(() => (client ? pluginsApi(client, environment) : null), [client, environment]);

  useEffect(() => {
    bodyRef.current?.parentElement?.scrollTo({ top: 0 });
  }, [tab, detail]);

  const fail = (failure: unknown) => setError(message(failure));

  const loadBrowse = useCallback(async () => {
    if (!api) return;
    try {
      setBrowse(await api.browse());
    } catch (failure) {
      setBrowse({ plugins: [], problems: [] });
      setError(message(failure));
    }
  }, [api]);

  // Each part loads on its own, so a failure in one leaves the others usable.
  useEffect(() => {
    if (!api) return;
    api.marketplaces().then(setMarketplaces, fail);
    void loadBrowse();
    api.installed().then(setInstalled, fail);
    api.suggestions().then(setOffers, fail);
  }, [api, loadBrowse]);

  const reloadMarketplaces = async () => {
    if (!api) return;
    setMarketplaces(await api.marketplaces());
    await loadBrowse();
  };

  const act = async (what: string, run: () => Promise<void>) => {
    setBusy(what);
    setError(null);
    try {
      await run();
    } catch (failure) {
      fail(failure);
    } finally {
      setBusy(null);
    }
  };

  const add = () =>
    act("add", async () => {
      const added = await api!.addMarketplace(draft.source, draft.revision);
      setDraft({ source: "", revision: "" });
      setNotes([`Added ${added.name}.`]);
      await reloadMarketplaces();
      // An offer for the source just added is now added.
      setOffers(await api!.suggestions());
    });

  const refresh = (name?: string) =>
    act(name ?? "*", async () => {
      const refreshed = await api!.refresh(name);
      setNotes(refreshed.map(refreshSummary));
      setChanged((all) => new Set([...all, ...changedIds(refreshed)]));
      await reloadMarketplaces();
    });

  const remove = (name: string) =>
    act(name, async () => {
      const result = await api!.remove(name);
      setNotes([result.disabled ? `Turned off ${name}.` : `Removed ${name}.`]);
      if (marketplace === name) setMarketplace(null);
      await reloadMarketplaces();
    });

  const dismiss = (key: string) =>
    act(key, async () => {
      await api!.dismissOffer(key);
      setOffers((current) => current && withoutOffer(current, key));
    });

  const mute = (folder: string) =>
    act("mute", async () => {
      await api!.muteProject(folder);
      setOffers((current) => current && mutedOffers(current));
    });

  const review = (offer: SuggestedMarketplace) => {
    setDraft({ source: offer.source, revision: offer.revision ?? "" });
    setTab("marketplaces");
  };

  const find = (id: string) => {
    setQuery(id.split("@")[0] ?? id);
    setMarketplace(null);
    setCategory(null);
    setDetail(null);
  };

  const plugin: PluginListing | undefined = detail ? browse?.plugins.find((p) => p.id === detail) : undefined;

  const onKeyDown = (event: KeyboardEvent) => {
    if (event.key === "Escape" && detail) {
      event.preventDefault();
      setDetail(null);
    }
  };

  let body;
  if (!client) {
    body = <EmptyState icon={Puzzle} title="Not connected" body="Connect to imperium-core to manage plugins." />;
  } else if (tab === "browse" && plugin) {
    body = <DetailView plugin={plugin} state={listingState(plugin, installedById(installed?.plugins ?? []), marketplaces ?? [], changed)} onBack={() => setDetail(null)} />;
  } else if (tab === "browse") {
    body = (
      <>
        <SuggestionsView offers={offers} onReview={review} onFind={find} onDismiss={(key) => void dismiss(key)} onMute={(folder) => void mute(folder)} />
        <BrowseView
          plugins={browse?.plugins ?? null}
          problems={browse?.problems ?? []}
          query={query}
          category={category}
          onCategory={setCategory}
          marketplace={marketplace}
          onMarketplace={setMarketplace}
          marketplaces={marketplaces ?? []}
          installed={installed?.plugins ?? []}
          changed={changed}
          onOpen={setDetail}
          onAddMarketplace={() => setTab("marketplaces")}
        />
      </>
    );
  } else if (tab === "marketplaces") {
    body = (
      <MarketplacesView
        marketplaces={marketplaces}
        busy={busy}
        draft={draft}
        onDraft={setDraft}
        onAdd={() => void add()}
        onRefresh={(name) => void refresh(name)}
        onRemove={(name) => void remove(name)}
        notes={notes}
      />
    );
  } else {
    body = <InstalledView result={installed} query={query} onBrowse={() => setTab("browse")} />;
  }

  const toolbar = (
    <div className="connectors-toolbar">
      <Tabs
        label="Plugin views"
        idBase={idBase}
        value={tab}
        onChange={(id) => {
          setTab(id as PluginsTab);
          setDetail(null);
          setNotes([]);
        }}
        items={[
          { id: "installed", label: "Installed", badge: installed?.plugins.length },
          { id: "browse", label: "Browse" },
          { id: "marketplaces", label: "Marketplaces", badge: marketplaces?.length },
        ]}
      />
      {tab !== "marketplaces" && (
        <label className="search-field">
          <Search aria-hidden="true" />
          <input
            type="search"
            placeholder={tab === "browse" ? "Search plugins" : "Search installed"}
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            aria-label="Search plugins"
            autoFocus
          />
        </label>
      )}
    </div>
  );

  return (
    <Dialog
      label="Plugins"
      title={
        <span className="connectors-title">
          <Puzzle aria-hidden="true" /> Plugins
        </span>
      }
      onClose={onClose}
      onKeyDown={onKeyDown}
      notice={
        <>
          {error && (
            <Notice tone="danger" action={<IconButton icon={X} label="Dismiss" size="sm" onClick={() => setError(null)} />}>
              {error}
            </Notice>
          )}
          {toolbar}
        </>
      }
    >
      <div ref={bodyRef} className="connectors-body" {...tabPanelProps(idBase, tab)}>
        {body}
      </div>
    </Dialog>
  );
}
