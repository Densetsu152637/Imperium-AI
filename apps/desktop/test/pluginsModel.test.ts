// The Plugins window's rules and daemon calls (plugins/model.ts) against a mocked client.

import assert from "node:assert/strict";
import { test } from "node:test";
import type { InstalledPlugin, MarketplaceInfo, MarketplaceRefresh, PluginListing } from "@imperium/protocol";

import {
  addSequence,
  categoriesOf,
  changedIds,
  componentSummary,
  filterListings,
  findQuery,
  installedById,
  listingState,
  marketplaceSummary,
  message,
  mutedOffers,
  plain,
  pluginsApi,
  refreshSequence,
  refreshSummary,
  removeSequence,
  reviewDraft,
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

test("plain also removes soft hyphens, isolates, invisible operators and line separators", () => {
  for (const c of ["\u00ad", "\u061c", "\u2028", "\u2029", "\u2060", "\u2064", "\u206a", "\u206f", "\u2066", "\ufeff"]) assert.equal(plain(`a${c}b`), "ab");
  assert.equal(plain("a b\tc"), "a b\tc");
});

test("every name and error shown passes through plain, including a refresh's marketplace name", () => {
  const base: MarketplaceRefresh = { name: "ev\u202Eil", commit_before: null, commit_after: null, added: [], removed: [], changed: [], problems: [], error: null };
  assert.equal(refreshSummary(base), "evil: already up to date.");
  assert.equal(refreshSummary({ ...base, error: "bad\u202E!" }), "evil: could not refresh. bad!");
  assert.equal(message(new Error("x\u200By")), "xy");
});

test("review and find use the offer's words cleaned, and add nothing", () => {
  assert.deepEqual(reviewDraft({ source: "a/b\u202E", revision: null }), { source: "a/b", revision: "" });
  assert.equal(findQuery("li\u200Bnt@official"), "lint");
});

test("add, refresh and remove each read the list and the listings again, and name things cleaned", async () => {
  const { client, calls } = mock({
    "plugins.marketplace_add": market({ name: "mi\u202Ene", built_in: false }),
    "plugins.marketplace_refresh": { refreshed: [{ name: "mine", commit_before: null, commit_after: "b", added: [], removed: [], changed: ["fmt"], problems: [], error: null }] },
    "plugins.marketplace_remove": { name: "mine", disabled: true },
    "plugins.marketplace_list": { marketplaces: [market({})] },
    "plugins.browse": { plugins: [], problems: [] },
    "plugins.suggestions": { source_folder: null, trusted: false, muted: false, marketplaces: [], plugins: [] },
  });
  const api = pluginsApi(client, "e");
  const added = await addSequence(api, { source: "a/b", revision: "" });
  assert.deepEqual(added.notes, ["Added mine."]);
  assert.deepEqual(calls.map((c) => c.method), ["plugins.marketplace_add", "plugins.marketplace_list", "plugins.browse", "plugins.suggestions"]);
  const refreshed = await refreshSequence(api, "mine");
  assert.deepEqual([...refreshed.changed], ["fmt@mine"]);
  assert.equal(refreshed.marketplaces.length, 1);
  assert.deepEqual((await removeSequence(api, "mine")).notes, ["Turned off mine."]);
});

test("a failed browse after an add leaves an empty listing and says why", async () => {
  const { client } = mock({ "plugins.marketplace_add": market({}), "plugins.marketplace_list": { marketplaces: [] }, "plugins.suggestions": null });
  const result = await addSequence(pluginsApi(client, "e"), { source: "a/b", revision: "" });
  assert.deepEqual(result.browse, { plugins: [], problems: [] });
  assert.match(result.browseError ?? "", /unexpected plugins.browse/);
});

test("the window calls only the allow-listed methods, and none that counts an approval", async () => {
  const preview = { id: "p@m", sha256: "abc" };
  const { client, calls } = mock({
    "plugins.marketplace_list": { marketplaces: [] },
    "plugins.marketplace_add": market({}),
    "plugins.marketplace_refresh": { refreshed: [] },
    "plugins.marketplace_remove": { name: "x", disabled: false },
    "plugins.browse": { plugins: [], problems: [] },
    "plugins.list": { plugins: [], problems: [] },
    "plugins.suggestions": { source_folder: null, trusted: false, muted: false, marketplaces: [], plugins: [] },
    "plugins.dismiss_suggestion": { dismissed: true, muted: false },
    "plugins.inspect": {},
    "plugins.inspect_folder": {},
    "plugins.install": {},
    "plugins.update": {},
    "plugins.set_enabled": {},
    "plugins.uninstall": { change_id: 1 },
    "config.undo": {},
    "plugins.set_secret": {},
    "plugins.delete_secret": {},
  });
  const api = pluginsApi(client, "e");
  await api.marketplaces();
  await api.browse();
  await api.installed();
  await api.suggestions();
  await api.addMarketplace("a/b");
  await api.refresh();
  await api.remove("x");
  await api.dismissOffer("k");
  await api.muteProject("/p");
  await api.inspect("p@m");
  await api.inspectFolder("my-tools");
  await api.install(preview);
  await api.update(preview);
  await api.setEnabled("p", true);
  await api.uninstall("p", false);
  await api.undo(1);
  await api.setSecret("p", "KEY", "v");
  await api.deleteSecret("p", "KEY");
  assert.equal(Object.keys(api).length, 18, "every function in pluginsApi was invoked above");
  const allowed = new Set([
    "plugins.marketplace_list",
    "plugins.browse",
    "plugins.list",
    "plugins.suggestions",
    "plugins.marketplace_add",
    "plugins.marketplace_refresh",
    "plugins.marketplace_remove",
    "plugins.dismiss_suggestion",
    "plugins.inspect",
    "plugins.inspect_folder",
    "plugins.install",
    "plugins.update",
    "plugins.set_enabled",
    "plugins.uninstall",
    "config.undo",
    "plugins.set_secret",
    "plugins.delete_secret",
  ]);
  assert.deepEqual(new Set(calls.map((c) => c.method)), allowed);
  // Neither of the daemon's two approval methods is among them; the app's own command is the only way.
  for (const { method } of calls) assert.doesNotMatch(method, /approv/);
});

test("scope: an environment is named only when the switch or the list is for it", async () => {
  const { client, calls } = mock({ "plugins.set_enabled": {}, "plugins.list": { plugins: [], problems: [] } });
  const api = pluginsApi(client, "env1");
  await api.setEnabled("p", false);
  assert.deepEqual(calls.at(-1)?.params, { name: "p", enabled: false });
  await api.setEnabled("p", false, "env1");
  assert.deepEqual(calls.at(-1)?.params, { name: "p", enabled: false, environment: "env1" });
  await api.installed(null);
  assert.deepEqual(calls.at(-1)?.params, {});
  await api.installed();
  assert.deepEqual(calls.at(-1)?.params, { environment: "env1" });
});
