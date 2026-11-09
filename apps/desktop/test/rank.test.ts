// Command palette ranking (src/commands/rank.ts). Run with `pnpm test` (node's test runner).

import assert from "node:assert/strict";
import { test } from "node:test";

import {
  groupResults,
  matchScore,
  nextGroupStart,
  pushRecent,
  rankCommands,
  RECENT_LIMIT,
  RECENT_SHOWN,
  type Rankable,
} from "../src/commands/rank.ts";

const commands: Rankable[] = [
  { id: "session.new", group: "actions", title: "New session", keywords: ["start", "create"] },
  { id: "settings.open", group: "actions", title: "Open Settings", keywords: ["preferences"] },
  { id: "theme.toggle", group: "actions", title: "Switch to light theme", keywords: ["toggle theme", "dark mode"] },
  { id: "session:a", group: "sessions", title: "Fix the login bug", subtitle: "Imperium-AI" },
  { id: "session:b", group: "sessions", title: "Write release notes", subtitle: "Website" },
  { id: "session:c", group: "archived", title: "Old spike on settings sync", subtitle: "Imperium-AI" },
  { id: "environment:x", group: "environments", title: "Imperium-AI", subtitle: "D:/Git/Imperium-AI" },
  { id: "panel.open:usage", group: "panels", title: "Open Usage", keywords: ["panel"] },
  { id: "settings:budgets", group: "settings", title: "Settings: Budgets", keywords: ["preferences"] },
];

const ids = (list: Rankable[]) => list.map((command) => command.id);

test("a word that matches nothing rules a command out", () => {
  assert.equal(matchScore("zzz", commands[0]), null);
  assert.equal(matchScore("new zzz", commands[0]), null);
  assert.equal(matchScore("", commands[0]), 0);
});

test("a substring beats scattered letters, and a word start beats the middle of a word", () => {
  const command = { id: "x", group: "actions" as const, title: "Open Settings" };
  const substring = matchScore("set", command)!;
  const scattered = matchScore("ots", command)!;
  assert.ok(substring > scattered, `${substring} > ${scattered}`);
  const start = matchScore("set", { ...command, title: "settings" })!;
  const middle = matchScore("set", { ...command, title: "reset" })!;
  assert.ok(start > middle, `${start} > ${middle}`);
});

test("letters in order match fuzzily across words", () => {
  assert.notEqual(matchScore("nwsn", commands[0]), null);
  assert.equal(matchScore("nsw", commands[0]), null);
});

test("the title outranks the same word in a subtitle or keyword", () => {
  const ranked = rankCommands(commands, "imperium", []);
  assert.equal(ranked[0].id, "environment:x");
  assert.deepEqual(ids(ranked).slice(1).sort(), ["session:a", "session:c"]);
});

test("several words must each match, in the title or the other words", () => {
  const ranked = rankCommands(commands, "settings budg", []);
  assert.equal(ranked[0].id, "settings:budgets");
  assert.deepEqual(ids(rankCommands(commands, "login imperium", [])), ["session:a"]);
});

test("an exact title wins", () => {
  assert.equal(rankCommands(commands, "open usage", [])[0].id, "panel.open:usage");
});

test("with nothing typed, recently run commands come first in the order they were run", () => {
  const ranked = rankCommands(commands, "", ["session:b", "settings.open", "gone"]);
  assert.deepEqual(ids(ranked).slice(0, 2), ["session:b", "settings.open"]);
  assert.equal(ranked.length, commands.length);
  // The rest keep their order.
  assert.deepEqual(ids(ranked).slice(2, 4), ["session.new", "theme.toggle"]);
});

test("with a query, a recently run match is lifted over an equal one", () => {
  const twins: Rankable[] = [
    { id: "one", group: "sessions", title: "Refactor parser" },
    { id: "two", group: "sessions", title: "Refactor parser" },
  ];
  assert.deepEqual(ids(rankCommands(twins, "parser", [])), ["one", "two"]);
  assert.deepEqual(ids(rankCommands(twins, "parser", ["two"])), ["two", "one"]);
});

test("a recent command does not outrank a much better match", () => {
  const ranked = rankCommands(commands, "new session", ["session:c"]);
  assert.equal(ranked[0].id, "session.new");
});

test("with nothing typed, a Recent group leads and the others follow in their usual order", () => {
  const recent = ["session:b", "settings:budgets"];
  const groups = groupResults(rankCommands(commands, "", recent), "", recent);
  assert.deepEqual(
    groups.map((group) => group.group),
    // Settings is left out: its only command is in Recent.
    ["recent", "actions", "sessions", "archived", "environments", "panels"],
  );
  assert.deepEqual(ids(groups[0].items), recent);
  // Commands in Recent are not repeated in their own group.
  assert.deepEqual(ids(groups.find((group) => group.group === "sessions")!.items), ["session:a"]);
});

test("only the first few recent commands form the Recent group", () => {
  const many = Array.from({ length: 10 }, (_, i): Rankable => ({ id: `s${i}`, group: "sessions", title: `Session ${i}` }));
  const recent = many.map((command) => command.id).reverse();
  const groups = groupResults(rankCommands(many, "", recent), "", recent);
  assert.equal(groups[0].items.length, RECENT_SHOWN);
  assert.deepEqual(ids(groups[1].items), recent.slice(RECENT_SHOWN));
});

test("with a query, groups follow their best match", () => {
  const groups = groupResults(rankCommands(commands, "settings", []), "settings", []);
  assert.equal(groups[0].items[0].id, groups.flatMap((group) => group.items)[0].id);
  assert.ok(!groups.some((group) => group.group === "recent"));
  const order = groups.map((group) => group.group);
  assert.equal(new Set(order).size, order.length);
});

test("running a command moves it to the front of the recent list, which is bounded", () => {
  assert.deepEqual(pushRecent(["a", "b", "c"], "b"), ["b", "a", "c"]);
  assert.deepEqual(pushRecent([], "a"), ["a"]);
  const full = Array.from({ length: RECENT_LIMIT }, (_, i) => `c${i}`);
  const next = pushRecent(full, "new");
  assert.equal(next.length, RECENT_LIMIT);
  assert.equal(next[0], "new");
  assert.ok(!next.includes(`c${RECENT_LIMIT - 1}`));
});

test("Tab jumps to the next group's first item and wraps; Shift+Tab goes back", () => {
  const sizes = [2, 3, 1];
  assert.equal(nextGroupStart(sizes, 0, 1), 2);
  assert.equal(nextGroupStart(sizes, 3, 1), 5);
  assert.equal(nextGroupStart(sizes, 5, 1), 0);
  assert.equal(nextGroupStart(sizes, 3, -1), 0);
  assert.equal(nextGroupStart(sizes, 0, -1), 5);
  assert.equal(nextGroupStart([], 0, 1), 0);
});
