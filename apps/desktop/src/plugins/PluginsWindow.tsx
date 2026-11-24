// The Plugins window: plugin marketplaces to browse, the marketplaces themselves, what is
// installed, and offers from the selected environment's project. A separate window from
// Connectors, which has its own Installed and Browse for MCP servers. It talks to the daemon's
// plugins.* methods and installs nothing: the install flow comes later.

import { Puzzle, Search, X } from "lucide-react";
import { useEffect, useId, useMemo, useRef, useState } from "react";
import type { MarketplaceInfo, PluginsBrowseResult, PluginsListResult, SuggestedMarketplace } from "@imperium/protocol";
import { Dialog } from "../components/Dialog";
import { EmptyState } from "../components/EmptyState";
import { IconButton } from "../components/IconButton";
import { Notice } from "../components/Notice";
import { Tabs, tabPanelProps } from "../components/Tabs";
import type { DaemonClient } from "../daemon/client";
import { BrowseView, DetailView } from "./BrowseView";
import { InstalledView } from "./InstalledView";
import { MarketplacesView } from "./MarketplacesView";
import { SuggestionsView } from "./SuggestionsView";
import {
  EMPTY_DRAFT,
  addSequence,
  findQuery,
  installedById,
  listingState,
  message,
  mutedOffers,
  plain,
  pluginsApi,
  refreshSequence,
  reload,
  removeSequence,
  reviewDraft,
  withoutOffer,
  type Draft,
  type Offers,
  type Reloaded,
} from "./model";

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
  const [draft, setDraft] = useState<Draft>(EMPTY_DRAFT);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const idBase = useId();
  const bodyRef = useRef<HTMLDivElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  /** The card to give focus back to when the detail closes. */
  const returnTo = useRef<string | null>(null);
  /** Bumped whenever the connection or environment changes, so an answer to an older request is dropped. */
  const generation = useRef(0);
  const api = useMemo(() => (client ? pluginsApi(client, environment) : null), [client, environment]);

  // The search box takes focus once, when the window opens (not on every tab change).
  useEffect(() => {
    if (tab !== "marketplaces") searchRef.current?.focus();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    bodyRef.current?.parentElement?.scrollTo({ top: 0 });
  }, [tab, detail]);

  // Closing the detail gives focus back to the card that opened it.
  useEffect(() => {
    if (detail !== null || !returnTo.current) return;
    const id = returnTo.current;
    returnTo.current = null;
    bodyRef.current?.querySelector<HTMLElement>(`[data-plugin-id="${CSS.escape(id)}"]`)?.focus();
  }, [detail]);

  const applyReloaded = (data: Reloaded) => {
    setMarketplaces(data.marketplaces);
    setBrowse(data.browse);
    if (data.browseError) setError(data.browseError);
  };

  // Each part loads on its own, so a failure in one leaves the others usable (and ends its spinner).
  useEffect(() => {
    if (!api) return;
    const token = ++generation.current;
    const live = () => generation.current === token;
    setInstalled(null);
    setOffers(null);
    const failed = (failure: unknown) => {
      if (live()) setError(message(failure));
    };
    api.marketplaces().then(
      (list) => live() && setMarketplaces(list),
      (failure: unknown) => {
        if (live()) setMarketplaces([]);
        failed(failure);
      },
    );
    api.browse().then(
      (result) => live() && setBrowse(result),
      (failure: unknown) => {
        if (live()) setBrowse({ plugins: [], problems: [] });
        failed(failure);
      },
    );
    api.installed().then(
      (result) => live() && setInstalled(result),
      (failure: unknown) => {
        if (live()) setInstalled({ plugins: [], problems: [] });
        failed(failure);
      },
    );
    api.suggestions().then((result) => live() && setOffers(result), failed);
    return () => {
      generation.current++;
    };
  }, [api]);

  /** Runs one action; its result is applied only if the connection and environment are still the same. */
  const act = async <T,>(what: string, run: () => Promise<T>, apply: (value: T) => void): Promise<boolean> => {
    const token = generation.current;
    setBusy(what);
    setError(null);
    try {
      const value = await run();
      if (generation.current === token) apply(value);
      return true;
    } catch (failure) {
      if (generation.current === token) setError(message(failure));
      return false;
    } finally {
      setBusy(null);
    }
  };

  const add = () =>
    act(
      "add",
      () => addSequence(api!, draft),
      (result) => {
        setDraft(EMPTY_DRAFT);
        setNotes(result.notes);
        applyReloaded(result);
        setOffers(result.offers);
      },
    );

  const refresh = (name?: string) =>
    act(
      name ?? "*",
      () => refreshSequence(api!, name),
      (result) => {
        setNotes(result.notes);
        setChanged((all) => new Set([...all, ...result.changed]));
        applyReloaded(result);
      },
    );

  const remove = (name: string) =>
    act(
      name,
      () => removeSequence(api!, name),
      (result) => {
        setNotes(result.notes);
        if (marketplace === name) setMarketplace(null);
        applyReloaded(result);
      },
    );

  // Turning the built-in marketplace back on is adding its source again.
  const turnOn = (m: MarketplaceInfo) =>
    act(
      m.name,
      async () => {
        await api!.addMarketplace(m.source, m.revision ?? undefined);
        return reload(api!);
      },
      (result) => {
        setNotes([`Turned on ${plain(m.name)}.`]);
        applyReloaded(result);
      },
    );

  const dismiss = (key: string) =>
    act(
      key,
      () => api!.dismissOffer(key),
      () => setOffers((current) => current && withoutOffer(current, key)),
    );

  const mute = (folder: string) =>
    act(
      "mute",
      () => api!.muteProject(folder),
      () => setOffers((current) => current && mutedOffers(current)),
    );

  const review = (offer: SuggestedMarketplace) => {
    setDraft(reviewDraft(offer));
    setTab("marketplaces");
  };

  const find = (id: string) => {
    setQuery(findQuery(id));
    setMarketplace(null);
    setCategory(null);
    setDetail(null);
  };

  const open = (id: string) => {
    returnTo.current = id;
    setDetail(id);
  };

  const plugin = detail ? browse?.plugins.find((p) => p.id === detail) : undefined;

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
    body = (
      <DetailView
        plugin={plugin}
        state={listingState(plugin, installedById(installed?.plugins ?? []), marketplaces ?? [], changed)}
        onBack={() => setDetail(null)}
      />
    );
  } else if (tab === "browse") {
    body = (
      <>
        <SuggestionsView offers={offers} onReview={review} onFind={find} onDismiss={dismiss} onMute={mute} />
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
          onOpen={open}
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
        onTurnOn={(m) => void turnOn(m)}
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
          returnTo.current = null;
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
      {/* Always mounted, so arrowing between tabs never moves focus into it; hidden on Marketplaces, which has no search. */}
      <label className="search-field" hidden={tab === "marketplaces"}>
        <Search aria-hidden="true" />
        <input
          ref={searchRef}
          type="search"
          placeholder={tab === "installed" ? "Search installed" : "Search plugins"}
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          aria-label="Search plugins"
        />
      </label>
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
