// The Plugins window's rules and daemon calls, kept free of React so a mocked client can test them
// (test/pluginsModel.test.ts). Every string a marketplace or a project repository supplies is data:
// it is cleaned here and the views draw it only as text, never as markup.

import type {
  InstalledPlugin,
  MarketplaceInfo,
  MarketplaceRefresh,
  PluginListing,
  PluginsBrowseResult,
  PluginsDismissSuggestionResult,
  PluginsListResult,
  PluginsMarketplaceListResult,
  PluginsMarketplaceRefreshResult,
  PluginsMarketplaceRemoveResult,
  PluginsSuggestionsResult,
} from "@imperium/protocol";

/** The part of the daemon client the window uses, so tests can stand in for it. */
export type Caller = { call<Result>(method: string, params?: unknown): Promise<Result> };

export type Installed = Map<string, InstalledPlugin>;

// Control, bidirectional-override and zero-width characters can hide or reorder text.
// eslint-disable-next-line no-control-regex
const HIDDEN = /[\u0000-\u0008\u000b-\u001f\u007f-\u009f​-‏‪-‮⁦-⁩﻿]/g;

/** Repository- or marketplace-supplied text with control and direction-changing characters removed. */
export function plain(text: string | null | undefined): string {
  return (text ?? "").replace(HIDDEN, "").trim();
}

export const message = (error: unknown) => (error instanceof Error ? error.message : String(error));

/** Every word of the query appears in some text (the Connectors rule). */
export function matches(query: string, ...texts: (string | null | undefined)[]): boolean {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  const haystack = texts.filter(Boolean).join(" ").toLowerCase();
  return words.every((word) => haystack.includes(word));
}

/** The listings the search and the marketplace filter leave, in the daemon's order. */
export function filterListings(plugins: PluginListing[], query: string, marketplace: string | null): PluginListing[] {
  return plugins.filter(
    (plugin) =>
      (!marketplace || plugin.marketplace === marketplace) &&
      matches(query, plugin.name, plugin.id, plugin.description, plugin.category, plugin.marketplace, plugin.author, ...plugin.keywords, ...plugin.tags),
  );
}

/** Installed plugins by `name@marketplace`. */
export function installedById(list: InstalledPlugin[]): Installed {
  return new Map(list.flatMap((plugin) => (plugin.id ? [[plugin.id.toLowerCase(), plugin] as const] : [])));
}

export type ListingState = "available" | "installed" | "update";

/**
 * Whether a listing is installed, and whether its marketplace has moved on since. The daemon has no
 * update flag yet, so a path plugin counts as having an update when the marketplace's commit differs
 * from the one it was installed at, and any plugin does when the last refresh in this window listed
 * it as changed.
 */
export function listingState(
  listing: PluginListing,
  installed: Installed,
  marketplaces: MarketplaceInfo[],
  changed: ReadonlySet<string>,
): ListingState {
  const record = installed.get(listing.id.toLowerCase());
  if (!record) return "available";
  if (changed.has(listing.id.toLowerCase())) return "update";
  const commit = marketplaces.find((m) => m.name === listing.marketplace)?.commit;
  if (listing.source_kind === "path" && commit && record.commit && commit !== record.commit) return "update";
  return "installed";
}

/** The categories the listings use, sorted. */
export const categoriesOf = (plugins: PluginListing[]): string[] =>
  [...new Set(plugins.flatMap((p) => (p.category ? [plain(p.category)] : [])).filter(Boolean))].sort((a, b) => a.localeCompare(b));

const part = (n: number, noun: string) => (n > 0 ? `${n} ${noun}${n === 1 ? "" : "s"}` : null);

/** "2 skills · 1 hook" for a listing, or null when the marketplace does not say what is inside. */
export function componentSummary(listing: PluginListing): string | null {
  const c = listing.components;
  if (!c) return null;
  const parts = [part(c.skills.length, "skill"), part(c.commands.length, "command"), part(c.agents.length, "agent"), part(c.hooks, "hook"), part(c.mcp_servers.length, "server")];
  return parts.filter(Boolean).join(" · ") || "No components";
}

/** What a marketplace row says about its freshness and size. */
export function marketplaceSummary(info: MarketplaceInfo): string {
  const parts = [
    info.plugin_count === null ? null : part(info.plugin_count, "plugin") ?? "No plugins",
    info.commit ? `commit ${info.commit.slice(0, 8)}` : null,
    info.refreshed_unix_ms ? `refreshed ${new Date(info.refreshed_unix_ms).toLocaleString()}` : "never refreshed",
  ];
  return parts.filter(Boolean).join(" · ");
}

/** One line for what a refresh found. */
export function refreshSummary(refresh: MarketplaceRefresh): string {
  if (refresh.error) return `${refresh.name}: could not refresh. ${plain(refresh.error)}`;
  const changes = [part(refresh.added.length, "plugin") && `${refresh.added.length} added`, refresh.removed.length > 0 ? `${refresh.removed.length} removed` : null, refresh.changed.length > 0 ? `${refresh.changed.length} changed` : null];
  const found = changes.filter(Boolean).join(", ");
  return `${refresh.name}: ${found || "already up to date"}.`;
}

/** The ids (`name@marketplace`, lower case) a set of refreshes reports as changed. */
export function changedIds(refreshes: MarketplaceRefresh[]): Set<string> {
  return new Set(refreshes.flatMap((r) => r.changed.map((name) => `${name}@${r.name}`.toLowerCase())));
}

/** The folder a suggestion file sits in, as its last two parts, so a long path stays readable. */
export function shortPath(path: string): string {
  const parts = plain(path).split(/[\\/]+/).filter(Boolean);
  return parts.length > 2 ? `…/${parts.slice(-2).join("/")}` : plain(path);
}

export type Offers = Pick<PluginsSuggestionsResult, "source_folder" | "trusted" | "muted" | "marketplaces" | "plugins">;

/** Whether any offer is left to show. */
export const offerCount = (offers: Offers | null): number => (offers ? offers.marketplaces.length + offers.plugins.length : 0);

/** The daemon calls the window makes. None installs, enables or approves anything. */
export function pluginsApi(client: Caller, environment: string | null) {
  return {
    marketplaces: async (): Promise<MarketplaceInfo[]> =>
      (await client.call<PluginsMarketplaceListResult>("plugins.marketplace_list")).marketplaces,
    browse: (marketplace?: string): Promise<PluginsBrowseResult> =>
      client.call<PluginsBrowseResult>("plugins.browse", marketplace ? { marketplace } : {}),
    installed: (): Promise<PluginsListResult> =>
      client.call<PluginsListResult>("plugins.list", environment ? { environment } : {}),
    suggestions: (): Promise<PluginsSuggestionsResult | null> =>
      environment ? client.call<PluginsSuggestionsResult>("plugins.suggestions", { environment }) : Promise.resolve(null),
    addMarketplace: (source: string, revision?: string): Promise<MarketplaceInfo> =>
      client.call<MarketplaceInfo>("plugins.marketplace_add", revision ? { source: source.trim(), revision: revision.trim() } : { source: source.trim() }),
    refresh: async (name?: string): Promise<MarketplaceRefresh[]> =>
      (await client.call<PluginsMarketplaceRefreshResult>("plugins.marketplace_refresh", name ? { name } : {})).refreshed,
    remove: (name: string): Promise<PluginsMarketplaceRemoveResult> =>
      client.call<PluginsMarketplaceRemoveResult>("plugins.marketplace_remove", { name }),
    dismissOffer: async (key: string): Promise<PluginsDismissSuggestionResult> =>
      client.call<PluginsDismissSuggestionResult>("plugins.dismiss_suggestion", { environment: required(environment), key }),
    muteProject: async (sourceFolder: string): Promise<PluginsDismissSuggestionResult> =>
      client.call<PluginsDismissSuggestionResult>("plugins.dismiss_suggestion", { environment: required(environment), source_folder: sourceFolder }),
  };
}

function required(environment: string | null): string {
  if (!environment) throw new Error("Choose an environment first.");
  return environment;
}

/** Removes a dismissed offer from the list, or every offer of a muted project. */
export function withoutOffer(offers: Offers, key: string): Offers {
  return { ...offers, marketplaces: offers.marketplaces.filter((o) => o.key !== key), plugins: offers.plugins.filter((o) => o.key !== key) };
}

export const mutedOffers = (offers: Offers): Offers => ({ ...offers, muted: true, marketplaces: [], plugins: [] });
