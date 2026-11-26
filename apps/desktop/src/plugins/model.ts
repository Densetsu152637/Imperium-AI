// The Plugins window's rules and daemon calls, kept free of React so a mocked client can test them
// (test/pluginsModel.test.ts). Every string a marketplace or a project repository supplies is data:
// it is cleaned here and the views draw it only as text, never as markup.

import type {
  InstalledPlugin,
  MarketplaceInfo,
  MarketplaceRefresh,
  PluginListing,
  PluginPreview,
  PluginsInstallResult,
  PluginsSecretResult,
  PluginsSetEnabledResult,
  PluginsUninstallResult,
  PluginsUpdateResult,
  ConfigUndoResult,
  PluginsBrowseResult,
  PluginsDismissSuggestionResult,
  PluginsListResult,
  PluginsMarketplaceListResult,
  PluginsMarketplaceRefreshResult,
  PluginsMarketplaceRemoveResult,
  PluginsSuggestionsResult,
  McpListResult,
  SkillInfo,
} from "@imperium/protocol";

import { message, plain } from "../shared/plainText.ts";

/** The part of the daemon client the window uses, so tests can stand in for it. */
export type Caller = { call<Result>(method: string, params?: unknown): Promise<Result> };

export type Installed = Map<string, InstalledPlugin>;

// The helpers moved to shared/plainText.ts; the Plugins window keeps importing them from here.
export { message, plain };

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
  const name = plain(refresh.name);
  if (refresh.error) return `${name}: could not refresh. ${plain(refresh.error)}`;
  const changes = [part(refresh.added.length, "plugin") && `${refresh.added.length} added`, refresh.removed.length > 0 ? `${refresh.removed.length} removed` : null, refresh.changed.length > 0 ? `${refresh.changed.length} changed` : null];
  const found = changes.filter(Boolean).join(", ");
  return `${name}: ${found || "already up to date"}.`;
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

/**
 * The daemon calls the window makes. Counting an approval is not one of them: that is the app's
 * own `plugin_approve` command (approve.ts), and the webview has no daemon method for it.
 */
export function pluginsApi(client: Caller, environment: string | null) {
  return {
    marketplaces: async (): Promise<MarketplaceInfo[]> =>
      (await client.call<PluginsMarketplaceListResult>("plugins.marketplace_list")).marketplaces,
    browse: (marketplace?: string): Promise<PluginsBrowseResult> =>
      client.call<PluginsBrowseResult>("plugins.browse", marketplace ? { marketplace } : {}),
    /** The installed plugins as the user sees them (`scope` null) or as an environment does; by default the window's own. */
    installed: (scope: string | null = environment): Promise<PluginsListResult> =>
      client.call<PluginsListResult>("plugins.list", scope ? { environment: scope } : {}),
    inspect: (id: string, revision?: string): Promise<PluginPreview> =>
      client.call<PluginPreview>("plugins.inspect", revision ? { id, revision } : { id }),
    /** What an installed folder that did not come from a marketplace would run, read from the folder itself. */
    inspectFolder: (name: string, kind?: "skill"): Promise<PluginPreview> =>
      client.call<PluginPreview>("plugins.inspect_folder", kind ? { name, kind } : { name }),
    /** The skills an agent installed that the user has not approved, which no session loads. */
    withheldSkills: async (): Promise<SkillInfo[]> => (await client.call<McpListResult>("mcp.list")).skills.filter((skill) => !!skill.withheld_reason),
    install: (preview: Pick<PluginPreview, "id" | "sha256">, env?: string | null): Promise<PluginsInstallResult> =>
      client.call<PluginsInstallResult>("plugins.install", env ? { id: preview.id, sha256: preview.sha256, environment: env } : { id: preview.id, sha256: preview.sha256 }),
    update: (preview: Pick<PluginPreview, "id" | "sha256">): Promise<PluginsUpdateResult> =>
      client.call<PluginsUpdateResult>("plugins.update", { name: preview.id, sha256: preview.sha256 }),
    setEnabled: (name: string, enabled: boolean, env?: string | null): Promise<PluginsSetEnabledResult> =>
      client.call<PluginsSetEnabledResult>("plugins.set_enabled", env ? { name, enabled, environment: env } : { name, enabled }),
    uninstall: (name: string, deleteSecrets: boolean): Promise<PluginsUninstallResult> =>
      client.call<PluginsUninstallResult>("plugins.uninstall", { name, delete_secrets: deleteSecrets }),
    undo: (id: number): Promise<ConfigUndoResult> => client.call<ConfigUndoResult>("config.undo", { id }),
    /** Write-only: the value goes to the daemon and is never read back. */
    setSecret: (name: string, variable: string, value: string): Promise<PluginsSecretResult> =>
      client.call<PluginsSecretResult>("plugins.set_secret", { name, variable, value }),
    deleteSecret: (name: string, variable: string): Promise<PluginsSecretResult> =>
      client.call<PluginsSecretResult>("plugins.delete_secret", { name, variable }),
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

/** Uninstalls a plugin, then reads the list again; the change id lets the user undo it. */
export async function uninstallSequence(api: Api, name: string, deleteSecrets: boolean, scope: string | null) {
  const result = await api.uninstall(name, deleteSecrets);
  return { changeId: result.change_id, name: plain(name), installed: await api.installed(scope) };
}

export type Draft = { source: string; revision: string };
export const EMPTY_DRAFT: Draft = { source: "", revision: "" };

/** What *Review* on a marketplace offer puts in the add form: the offer's own words, cleaned, added to nothing. */
export const reviewDraft = (offer: { source: string; revision: string | null }): Draft => ({ source: plain(offer.source), revision: plain(offer.revision) });

/** What *Find* on a plugin offer searches for: the plugin's name, without its marketplace. */
export const findQuery = (id: string): string => plain(id).split("@")[0] ?? "";

export type Api = ReturnType<typeof pluginsApi>;

export type Reloaded = { marketplaces: MarketplaceInfo[]; browse: PluginsBrowseResult; browseError: string | null };

/** The marketplace list and the listings, read again; a failed browse leaves an empty one and says why. */
export async function reload(api: Api): Promise<Reloaded> {
  const marketplaces = await api.marketplaces();
  try {
    return { marketplaces, browse: await api.browse(), browseError: null };
  } catch (failure) {
    return { marketplaces, browse: { plugins: [], problems: [] }, browseError: message(failure) };
  }
}

/** Adds a marketplace, then reads everything that depends on it (the list, the listings, the offers). */
export async function addSequence(api: Api, draft: Draft): Promise<Reloaded & { notes: string[]; offers: PluginsSuggestionsResult | null }> {
  const added = await api.addMarketplace(draft.source, draft.revision);
  return { notes: [`Added ${plain(added.name)}.`], ...(await reload(api)), offers: await api.suggestions() };
}

/** Refreshes one marketplace or all, then reads the list and listings again. */
export async function refreshSequence(api: Api, name?: string): Promise<Reloaded & { notes: string[]; changed: Set<string> }> {
  const refreshed = await api.refresh(name);
  return { notes: refreshed.map(refreshSummary), changed: changedIds(refreshed), ...(await reload(api)) };
}

/** Removes a marketplace (the built-in one is turned off), then reads the list and listings again. */
export async function removeSequence(api: Api, name: string): Promise<Reloaded & { notes: string[] }> {
  const result = await api.remove(name);
  return { notes: [`${result.disabled ? "Turned off" : "Removed"} ${plain(name)}.`], ...(await reload(api)) };
}
