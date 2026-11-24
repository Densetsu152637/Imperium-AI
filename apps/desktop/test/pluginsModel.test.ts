// The Plugins window's rules and daemon calls (plugins/model.ts) against a mocked client.

import assert from "node:assert/strict";
import { test } from "node:test";
import type { InstalledPlugin, MarketplaceInfo, MarketplaceRefresh, PluginListing } from "@imperium/protocol";

import {
  categoriesOf,
  changedIds,
  componentSummary,
  filterListings,
  installedById,
  listingState,
  marketplaceSummary,
  mutedOffers,
  plain,
  pluginsApi,
  refreshSummary,
  shortPath,
  withoutOffer,
  type Caller,
} from "../src/plugins/model.ts";

const listing = (over: Partial<PluginListing>): PluginListing => ({
  id: "fmt@official",
  name: "fmt",
  marketplace: "official",
  description: "Formats code",
  version: null,
  author: null,
  homepage: null,
  license: null,
  category: "tools",
  keywords: [],
  tags: [],
  source_kind: "path",
  source: "plugins/fmt",
  components: null,
  ...over,
});

const record = (over: Partial<InstalledPlugin>): InstalledPlugin => ({
  name: "fmt",
  folder: "fmt",
  id: "fmt@official",
  origin: "marketplace",
  source: null,
  commit: "aaaa",
  approval: null,
  enabled: true,
  decided_by: "user",
  withheld: false,
  reason: null,
  hooks: 0,
  servers: [],
  required_secrets: [],
  missing_secrets: [],
  ...over,
});

const market = (over: Partial<MarketplaceInfo>): MarketplaceInfo => ({
  name: "official",
  source: "github.com/x/y",
  revision: null,
  kind: "github",
  built_in: true,
  enabled: true,
  commit: "aaaa",
  refreshed_unix_ms: null,
  plugin_count: 2,
  problems: [],
  ...over,
});

/** A client that records each call and answers from a table. */
function mock(answers: Record<string, unknown>) {
  const calls: { method: string; params: unknown }[] = [];
  const client: Caller = {
    async call<R>(method: string, params?: unknown) {
      calls.push({ method, params });
      if (!(method in answers)) throw new Error(`unexpected ${method}`);
      return answers[method] as R;
    },
  };
  return { client, calls };
}

test("search words match name, description, category, marketplace and keywords, and the marketplace filter narrows", () => {
  const all = [listing({}), listing({ id: "lint@mine", name: "lint", marketplace: "mine", description: "Finds bugs", category: "quality", keywords: ["static"] })];
  assert.deepEqual(filterListings(all, "bugs", null).map((p) => p.name), ["lint"]);
  assert.deepEqual(filterListings(all, "static quality", null).map((p) => p.name), ["lint"]);
  assert.deepEqual(filterListings(all, "", "official").map((p) => p.name), ["fmt"]);
  assert.deepEqual(filterListings(all, "nothing", null), []);
  assert.deepEqual(categoriesOf(all), ["quality", "tools"]);
});

test("a card from an external source counts what the marketplace declares", () => {
  const external = listing({ source_kind: "github", source: "someone/plugin", components: { skills: ["a", "b"], commands: [], agents: [], hooks: 1, mcp_servers: ["db"] } });
  assert.equal(componentSummary(external), "2 skills · 1 hook · 1 server");
  assert.equal(componentSummary(listing({})), null);
  assert.equal(componentSummary(listing({ components: { skills: [], commands: [], agents: [], hooks: 0, mcp_servers: [] } })), "No components");
});

test("a listing is installed, or has an update when its marketplace moved or a refresh changed it", () => {
  const installed = installedById([record({})]);
  assert.equal(listingState(listing({ id: "other@official" }), installed, [market({})], new Set()), "available");
  assert.equal(listingState(listing({}), installed, [market({})], new Set()), "installed");
  assert.equal(listingState(listing({}), installed, [market({ commit: "bbbb" })], new Set()), "update");
  // An external plugin's commit is its own repository's, so the marketplace's commit says nothing.
  assert.equal(listingState(listing({ source_kind: "github" }), installed, [market({ commit: "bbbb" })], new Set()), "installed");
  assert.equal(listingState(listing({ source_kind: "github" }), installed, [market({})], new Set(["fmt@official"])), "update");
});

test("text from a marketplace or a repository loses control and direction-changing characters and stays text", () => {
  assert.equal(plain("a‮b​c\u0007d"), "abcd");
  assert.equal(plain(null), "");
  // Markup stays characters: the views draw it as text, so there is no HTML to inject.
  assert.equal(plain("<img src=x onerror=alert(1)> **bold**"), "<img src=x onerror=alert(1)> **bold**");
  assert.equal(shortPath("/home/me/project/.imperium/config.toml"), "…/.imperium/config.toml");
});

test("marketplaces: adding, refreshing and removing call the daemon with exactly what was asked", async () => {
  const refreshed: MarketplaceRefresh = { name: "mine", commit_before: "a", commit_after: "b", added: ["x"], removed: [], changed: ["fmt"], problems: [], error: null };
  const { client, calls } = mock({
    "plugins.marketplace_add": market({ name: "mine", built_in: false }),
    "plugins.marketplace_refresh": { refreshed: [refreshed] },
    "plugins.marketplace_remove": { name: "mine", disabled: false },
    "plugins.marketplace_list": { marketplaces: [market({})] },
  });
  const api = pluginsApi(client, "env1");
  assert.equal((await api.addMarketplace(" owner/repo ", " main ")).name, "mine");
  assert.deepEqual(calls.at(-1), { method: "plugins.marketplace_add", params: { source: "owner/repo", revision: "main" } });
  await api.addMarketplace("owner/repo", "");
  assert.deepEqual(calls.at(-1)?.params, { source: "owner/repo" });
  const result = await api.refresh("mine");
  assert.deepEqual(calls.at(-1)?.params, { name: "mine" });
  assert.equal(refreshSummary(result[0]!), "mine: 1 added, 1 changed.");
  assert.deepEqual([...changedIds(result)], ["fmt@mine"]);
  await api.refresh();
  assert.deepEqual(calls.at(-1)?.params, {});
  assert.equal((await api.remove("mine")).disabled, false);
  assert.deepEqual(calls.at(-1), { method: "plugins.marketplace_remove", params: { name: "mine" } });
  assert.equal((await api.marketplaces()).length, 1);
});

test("a failed refresh and a quiet one read as one line each", () => {
  const base: MarketplaceRefresh = { name: "mine", commit_before: null, commit_after: null, added: [], removed: [], changed: [], problems: [], error: null };
  assert.equal(refreshSummary(base), "mine: already up to date.");
  assert.equal(refreshSummary({ ...base, error: "offline" }), "mine: could not refresh. offline");
  assert.match(marketplaceSummary(market({ commit: "0123456789abc", refreshed_unix_ms: null })), /2 plugins · commit 01234567 · never refreshed/);
});

test("suggestions are read for the environment, and dismissing sends the key or the project folder", async () => {
  const offers = {
    source_folder: "/p",
    trusted: true,
    muted: false,
    marketplaces: [{ key: "marketplace:a", source: "a/b", revision: null, file: "/p/.imperium/config.toml", added: false }],
    plugins: [{ key: "plugin:x@a", id: "x@a", file: "/p/.imperium/config.toml", marketplace_added: false, installed: false }],
  };
  const { client, calls } = mock({ "plugins.suggestions": offers, "plugins.dismiss_suggestion": { dismissed: true, muted: false } });
  const api = pluginsApi(client, "env1");
  assert.deepEqual(await api.suggestions(), offers);
  assert.deepEqual(calls.at(-1), { method: "plugins.suggestions", params: { environment: "env1" } });
  await api.dismissOffer("plugin:x@a");
  assert.deepEqual(calls.at(-1)?.params, { environment: "env1", key: "plugin:x@a" });
  await api.muteProject("/p");
  assert.deepEqual(calls.at(-1)?.params, { environment: "env1", source_folder: "/p" });
  assert.equal(withoutOffer(offers, "plugin:x@a").plugins.length, 0);
  assert.equal(withoutOffer(offers, "plugin:x@a").marketplaces.length, 1);
  const muted = mutedOffers(offers);
  assert.ok(muted.muted && muted.marketplaces.length === 0 && muted.plugins.length === 0);
});

test("with no environment there are no offers to read and none to dismiss", async () => {
  const { client, calls } = mock({ "plugins.list": { plugins: [], problems: [] } });
  const api = pluginsApi(client, null);
  assert.equal(await api.suggestions(), null);
  await assert.rejects(api.dismissOffer("k"), /environment/i);
  await api.installed();
  assert.deepEqual(calls, [{ method: "plugins.list", params: {} }]);
});

test("the window calls no method that installs, approves, enables or touches secrets", async () => {
  const { client, calls } = mock({
    "plugins.marketplace_list": { marketplaces: [] },
    "plugins.browse": { plugins: [], problems: [] },
    "plugins.list": { plugins: [], problems: [] },
    "plugins.suggestions": { source_folder: null, trusted: false, muted: false, marketplaces: [], plugins: [] },
  });
  const api = pluginsApi(client, "e");
  await Promise.all([api.marketplaces(), api.browse(), api.installed(), api.suggestions()]);
  for (const { method } of calls) assert.doesNotMatch(method, /install|approve|set_enabled|secret|update|uninstall/);
});
