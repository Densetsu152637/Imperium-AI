// The Preview panel's URL rule and the takeover state (src/preview.ts). Run with `pnpm test`.

import assert from "node:assert/strict";
import { test } from "node:test";

import { applyPreviewEvent, browserAddress, clickFraction, keyAction, previewUrl } from "../src/preview.ts";

test("loopback http(s) addresses are previewed", () => {
  for (const input of [
    "http://localhost:5173",
    "http://LOCALHOST:3000/app?x=1#top",
    "http://127.0.0.1:8080/",
    "https://localhost:8443",
    "  http://localhost/  ",
  ]) {
    assert.equal(previewUrl(input).ok, true, input);
  }
  assert.deepEqual(previewUrl("http://localhost:5173"), { ok: true, url: "http://localhost:5173/" });
});

test("remote hosts, look-alikes, and credentials are refused", () => {
  for (const input of [
    "https://example.com",
    "http://localhost.evil.com:80",
    "http://127.0.0.1.evil.com/",
    "http://evil.com/?u=http://localhost",
    "http://localhost@evil.com/",
    "http://127.0.0.1:80@evil.com/",
    "http://user:pw@localhost:3000/",
    "http://192.168.1.5:3000",
    "http://0.0.0.0:3000",
    "http://127.0.0.2:3000",
    "http://[::1]:9000/index.html", // the window's CSP cannot name an IPv6 literal
    "file:///etc/passwd",
    "javascript:alert(1)",
    "ftp://localhost/",
    "localhost:3000",
    "//localhost:3000",
    "",
    "not a url",
  ]) {
    assert.equal(previewUrl(input).ok, false, input);
  }
  // A backslash is a slash for http URLs, so this host is evil.com.
  assert.equal(previewUrl("http://evil.com\\@localhost/").ok, false);
  const refused = previewUrl("https://example.com");
  assert.equal(refused.ok === false && refused.reason.includes("localhost"), true);
});

test("the app's own origin is never framed", () => {
  assert.equal(previewUrl("http://localhost:1420/", "http://localhost:1420").ok, false);
  assert.equal(previewUrl("http://localhost:1420/settings?x=1", "http://localhost:1420").ok, false);
  assert.equal(previewUrl("http://localhost:5173/", "http://localhost:1420").ok, true);
  assert.equal(previewUrl("http://127.0.0.1:1420/", "http://localhost:1420").ok, true);
  assert.equal(previewUrl("http://localhost:1420/", null).ok, true);
});

test("takeover and preview events update the session's state", () => {
  let state = applyPreviewEvent({ driver: "agent", preview: null }, { type: "turn_started" });
  assert.deepEqual(state, { driver: "agent", preview: null });
  state = applyPreviewEvent(state, { type: "browser_control", user: true });
  assert.equal(state.driver, "user");
  state = applyPreviewEvent(state, { type: "preview_changed", url: "http://localhost:5173/", command: "npm run dev" });
  assert.deepEqual(state.preview, { url: "http://localhost:5173/", command: "npm run dev" });
  assert.equal(state.driver, "user");
  state = applyPreviewEvent(state, { type: "browser_control", user: false });
  assert.equal(state.driver, "agent");
  state = applyPreviewEvent(state, { type: "preview_changed", url: null, command: null });
  assert.equal(state.preview, null);
});

test("a click on the screenshot becomes fractions of the page, kept inside it", () => {
  const rect = { left: 100, top: 50, width: 400, height: 200 };
  assert.deepEqual(clickFraction(rect, 300, 150), { x: 0.5, y: 0.5 });
  assert.deepEqual(clickFraction(rect, 0, 900), { x: 0, y: 1 });
  assert.equal(clickFraction({ ...rect, width: 0 }, 1, 1), null);
});

test("keys are typed, pressed, or ignored", () => {
  const key = (value: string, mods: object = {}) => ({ key: value, ctrlKey: false, metaKey: false, altKey: false, ...mods });
  assert.deepEqual(keyAction(key("a")), { type: "type", text: "a" });
  assert.deepEqual(keyAction(key(" ")), { type: "type", text: " " });
  assert.deepEqual(keyAction(key("é")), { type: "type", text: "é" });
  assert.deepEqual(keyAction(key("Enter")), { type: "key", key: "Enter" });
  assert.equal(keyAction(key("Shift")), null);
  assert.equal(keyAction(key("F5")), null);
  assert.equal(keyAction(key("c", { ctrlKey: true })), null);
});

test("the Browser panel's address bar accepts http(s) addresses only", () => {
  assert.equal(browserAddress("https://example.com/a"), "https://example.com/a");
  assert.equal(browserAddress("example.com"), "https://example.com/");
  assert.equal(browserAddress("http://localhost:3000"), "http://localhost:3000/");
  assert.equal(browserAddress("file:///etc/passwd"), null);
  assert.equal(browserAddress("javascript:alert(1)"), null);
  assert.equal(browserAddress("two words"), null);
  assert.equal(browserAddress(""), null);
});
