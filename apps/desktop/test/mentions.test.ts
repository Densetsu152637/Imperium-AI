// `@` file mentions (src/sessions/mentions.ts, src/sessions/slash.ts, and the chips they add in
// src/sessions/attachments.ts). Run with `pnpm test` (node's test runner).

import assert from "node:assert/strict";
import { test } from "node:test";

import { addMention, canSend, markFailed, markReady, readyAttachments, removeChip } from "../src/sessions/attachments.ts";
import {
  activeMention,
  composeMessage,
  insertMention,
  mentionNotes,
  mentionToken,
  syncMentions,
  withoutMention,
} from "../src/sessions/mentions.ts";
import { SLASH_COMMANDS, builtinOf, builtinParams, parseSlash } from "../src/sessions/slash.ts";

test("an @ that starts a word opens a mention up to the caret", () => {
  assert.deepEqual(activeMention("@", 1), { start: 0, end: 1, query: "" });
  assert.deepEqual(activeMention("look at @src/li", 15), { start: 8, end: 15, query: "src/li" });
  // The caret in the middle of the word: the query is what precedes it, the whole word is replaced.
  assert.deepEqual(activeMention("see @readme now", 8), { start: 4, end: 11, query: "rea" });
});

test("an address, a closed word and a caret before the @ are not mentions", () => {
  assert.equal(activeMention("mail me@example.com", 19), null);
  assert.equal(activeMention("@src/lib.rs and more", 20), null);
  assert.equal(activeMention("no at sign", 10), null);
  assert.equal(activeMention("later @file", 3), null);
  // A line break ends the word as a space does.
  assert.equal(activeMention("@a\nb", 4), null);
});

test("choosing a file puts its path in the message and leaves the caret after it", () => {
  const typed = "explain @li please";
  const active = activeMention(typed, 11);
  assert.ok(active);
  const next = insertMention(typed, active, "src/lib.rs");
  assert.equal(next.text, "explain @src/lib.rs please");
  assert.equal(next.caret, "explain @src/lib.rs ".length);
  // At the end of the text a space is added so the user can keep typing.
  const end = activeMention("@li", 3);
  assert.ok(end);
  assert.deepEqual(insertMention("@li", end, "src/lib.rs"), { text: "@src/lib.rs ", caret: 12 });
});

test("a path with spaces is quoted", () => {
  assert.equal(mentionToken("my notes.md"), '@"my notes.md"');
  assert.equal(mentionToken("a/b.rs"), "@a/b.rs");
});

test("removing a mention takes out its token and one space, and only the whole word", () => {
  assert.equal(withoutMention("explain @src/lib.rs please", "@src/lib.rs"), "explain please");
  assert.equal(withoutMention("@src/lib.rs fix", "@src/lib.rs"), "fix");
  assert.equal(withoutMention("fix @src/lib.rs", "@src/lib.rs"), "fix");
  assert.equal(withoutMention("fix @src/lib.rsx", "@src/lib.rs"), "fix @src/lib.rsx");
  // Sentence punctuation after a token does not make it a different word.
  assert.equal(withoutMention("is it @README.md?", "@README.md"), "is it?");
  assert.equal(withoutMention("fix @src/lib.rs, then", "@src/lib.rs"), "fix, then");
  assert.equal(withoutMention("see @src/lib.rs.bak", "@src/lib.rs"), "see @src/lib.rs.bak");
  assert.equal(withoutMention("nothing here", "@src/lib.rs"), "nothing here");
});

const staged = {
  id: "a1",
  name: "lib.rs",
  kind: "text" as const,
  mime: "text/plain",
  size: 120,
  path: "/work/src/lib.rs",
  copied: false,
  vision: false,
};

test("a mentioned file uploads, then sends as an attachment; deleting its token drops the chip", () => {
  let chips = addMention([], "k1", "@src/lib.rs", "src/lib.rs");
  assert.equal(chips[0].state, "uploading");
  assert.equal(canSend("look at @src/lib.rs", chips), false, "wait for the upload");
  chips = markReady(chips, "k1", { attachment: staged, vision: false });
  assert.equal(chips[0].mention, "@src/lib.rs", "the token survives staging");
  assert.deepEqual(
    readyAttachments(chips).map((attachment) => attachment.path),
    ["/work/src/lib.rs"],
  );
  assert.equal(syncMentions(chips, "look at @src/lib.rs").length, 1);
  assert.equal(syncMentions(chips, "look at it").length, 0);
  // Chips that were not mentions stay whatever the text says.
  const pasted = { key: "p", name: "shot.png", state: "ready" as const };
  assert.equal(syncMentions([pasted, ...chips], "plain").length, 1);
});

test("a refused mention fails its chip and sends nothing", () => {
  let chips = addMention([], "k1", "@../secret.txt", "../secret.txt");
  chips = markFailed(chips, "k1", "../secret.txt is outside the session's folders");
  assert.equal(canSend("", chips), false);
  assert.deepEqual(readyAttachments(chips), []);
});

test("a mention followed by punctuation keeps its chip", () => {
  const chips = markReady(addMention([], "k1", "@README.md", "README.md"), "k1", { attachment: staged, vision: false });
  for (const text of ["what is @README.md?", "see @README.md, then", "ok @README.md.", "@README.md!!"]) {
    assert.equal(syncMentions(chips, text).length, 1, text);
  }
  assert.equal(syncMentions(chips, "what is @README.mdx?").length, 0);
});

test("mentioning the same path twice adds one chip", () => {
  const once = addMention([], "k1", "@a.rs", "a.rs");
  assert.equal(addMention(once, "k2", "@a.rs", "a.rs").length, 1);
});

test("a directory is a ready chip that is named in the message, never attached", () => {
  const chips = addMention([], "k1", "@docs", "docs", "/work/docs");
  assert.equal(chips[0].state, "ready");
  assert.deepEqual(readyAttachments(chips), []);
  assert.equal(canSend("", chips), true, "a folder alone can be sent");
  assert.equal(mentionNotes(chips), "\n\n(@docs is the folder /work/docs)");
  assert.equal(composeMessage("  what is in @docs?  ", chips), "what is in @docs?\n\n(@docs is the folder /work/docs)");
  assert.equal(composeMessage("hello", []), "hello");
  assert.equal(removeChip(chips, "k1").length, 0);
});

test("/init is a command, with the user's guidance after it", () => {
  assert.deepEqual(
    SLASH_COMMANDS.map((command) => command.name),
    ["init", "code-review", "security-review"],
  );
  assert.deepEqual(parseSlash("/init"), { command: "init", rest: "" });
  assert.deepEqual(parseSlash("  /init  focus on the build and tests "), { command: "init", rest: "focus on the build and tests" });
  assert.equal(parseSlash("/initialise"), null);
  assert.equal(parseSlash("please /init"), null);
  assert.equal(parseSlash("/unknown"), null);
  assert.equal(parseSlash("init"), null);
});

test("the review commands take an optional effort and target", () => {
  assert.deepEqual(parseSlash("/code-review"), { command: "code-review", rest: "" });
  assert.deepEqual(parseSlash("/code-review 123"), { command: "code-review", rest: "123" });
  assert.deepEqual(parseSlash(" /code-review  high  feat/x "), { command: "code-review", rest: "high  feat/x" });
  assert.deepEqual(parseSlash("/security-review"), { command: "security-review", rest: "" });
  assert.deepEqual(parseSlash("/security-review max src/auth"), { command: "security-review", rest: "max src/auth" });
  assert.equal(parseSlash("/code-reviewer"), null);
  assert.equal(parseSlash("/security-reviews"), null);
  assert.equal(parseSlash("/code_review"), null);
  for (const command of SLASH_COMMANDS) assert.ok(command.description && command.argumentHint);
});

test("a command line becomes one session.builtin request", () => {
  assert.deepEqual(builtinParams("s1", builtinOf("code-review"), "123"), {
    session_id: "s1",
    command: "code_review",
    args: "123",
  });
  assert.deepEqual(builtinParams("s1", builtinOf("security-review"), ""), { session_id: "s1", command: "security_review" });
  assert.deepEqual(builtinParams("s1", builtinOf("init"), ""), { session_id: "s1", command: "init" });
});
