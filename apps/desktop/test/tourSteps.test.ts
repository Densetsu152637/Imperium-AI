// The first-run tour's steps and persistence (src/tourSteps.ts). Run with `pnpm test` (node's test runner).

import assert from "node:assert/strict";
import { test } from "node:test";

import {
  autoStartReady,
  availableSteps,
  cardPosition,
  migrateTour,
  nextStep,
  previousStep,
  recordOutcome,
  settleStep,
  shouldAutoStart,
  TOUR_STEPS,
  type TourState,
  type TourStep,
} from "../src/tourSteps.ts";

const steps: TourStep[] = [
  { id: "a", title: "A", body: "a" },
  { id: "b", title: "B", body: "b", target: "#b" },
  { id: "c", title: "C", body: "c", target: "#c" },
  { id: "d", title: "D", body: "d" },
];
const present = (...selectors: string[]) => (selector: string) => selectors.includes(selector);
const ids = (list: TourStep[]) => list.map((step) => step.id);

test("the shipped steps have unique ids and text, and every target is a selector string", () => {
  assert.equal(new Set(TOUR_STEPS.map((step) => step.id)).size, TOUR_STEPS.length);
  for (const step of TOUR_STEPS) {
    assert.ok(step.title && step.body, step.id);
    assert.ok(step.target === undefined || step.target.length > 0, step.id);
  }
  // The main areas the roadmap names are all covered.
  for (const id of ["environments", "sessions", "composer", "layout", "palette", "settings", "approvals", "memory"]) {
    assert.ok(TOUR_STEPS.some((step) => step.id === id), id);
  }
});

test("steps run in order, and Next past the last one ends the tour", () => {
  const all = present("#b", "#c");
  assert.equal(nextStep(steps, "a", all)?.id, "b");
  assert.equal(nextStep(steps, "b", all)?.id, "c");
  assert.equal(nextStep(steps, "c", all)?.id, "d");
  assert.equal(nextStep(steps, "d", all), null);
  assert.equal(previousStep(steps, "d", all)?.id, "c");
  assert.equal(previousStep(steps, "a", all), null);
});

test("steps whose target is missing are skipped, going on and back", () => {
  const some = present("#c");
  assert.deepEqual(ids(availableSteps(steps, some)), ["a", "c", "d"]);
  assert.equal(nextStep(steps, "a", some)?.id, "c");
  assert.equal(previousStep(steps, "c", some)?.id, "a");
  const none = present();
  assert.equal(nextStep(steps, "a", none)?.id, "d");
  assert.equal(previousStep(steps, "d", none)?.id, "a");
});

test("a step that loses its target mid-tour moves to the next available one", () => {
  assert.deepEqual(settleStep(steps, "b", present("#b", "#c"))?.index, 1);
  const lost = settleStep(steps, "b", present("#c"));
  assert.equal(lost?.steps[lost.index].id, "c");
  const lastLost = settleStep([...steps.slice(0, 3)], "c", present());
  assert.equal(lastLost?.steps[lastLost.index].id, "a");
  assert.equal(settleStep([steps[1]], "b", present()), null);
});

test("the tour starts by itself only until it has run", () => {
  assert.equal(shouldAutoStart({ tour: null }), true);
  assert.equal(shouldAutoStart({ tour: undefined as never }), true);
  assert.equal(shouldAutoStart(recordOutcome<{ tour: TourState }>({ tour: null }, "finished")), false);
  assert.equal(shouldAutoStart(recordOutcome<{ tour: TourState }>({ tour: null }, "skipped")), false);
});

test("auto-start waits for a connected main window with the interface up and no dialog open", () => {
  const fresh: { tour: TourState } = { tour: null };
  const ready = { mainWindow: true, connected: true, uiReady: true, modalOpen: false };
  assert.equal(autoStartReady(fresh, ready), true);
  assert.equal(autoStartReady(fresh, { ...ready, connected: false }), false);
  assert.equal(autoStartReady(fresh, { ...ready, uiReady: false }), false);
  assert.equal(autoStartReady(fresh, { ...ready, modalOpen: true }), false);
  assert.equal(autoStartReady(fresh, { ...ready, mainWindow: false }), false);
  assert.equal(autoStartReady(recordOutcome(fresh, "skipped"), ready), false);
});

test("a settings document from before the tour marks an existing install, which never auto-starts", () => {
  const now = new Date("2026-01-02T03:04:05.000Z");
  const existing = migrateTour({ theme: "dark", version: 1 }, now);
  assert.deepEqual(existing, { outcome: "existing", at: "2026-01-02T03:04:05.000Z" });
  assert.equal(shouldAutoStart({ tour: existing! }), false);
  // A new install has no document; one that already records the tour (even null) is left alone.
  assert.equal(migrateTour(undefined, now), undefined);
  assert.equal(migrateTour({ tour: null }, now), undefined);
  assert.equal(migrateTour({ tour: { outcome: "finished", at: "x" } }, now), undefined);
});

test("finishing and skipping are remembered with their outcome, and keep other settings", () => {
  const now = new Date("2026-01-02T03:04:05.000Z");
  const before: { tour: TourState; theme: string } = { tour: null, theme: "dark" };
  const finished = recordOutcome(before, "finished", now);
  assert.deepEqual(finished.tour, { outcome: "finished", at: "2026-01-02T03:04:05.000Z" });
  assert.equal(finished.theme, "dark");
  assert.equal(before.tour, null);
  assert.equal(recordOutcome(finished, "skipped", now).tour?.outcome, "skipped");
});

test("the card sits below its element, above it near the bottom, and inside the viewport", () => {
  const viewport = { width: 1000, height: 700 };
  const card = { width: 360, height: 200 };
  const below = cardPosition({ left: 100, top: 50, width: 200, height: 40 }, card, viewport);
  assert.deepEqual(below, { left: 20, top: 102 });
  const above = cardPosition({ left: 500, top: 600, width: 200, height: 40 }, card, viewport);
  assert.equal(above.top, 600 - 12 - 200);
  const edge = cardPosition({ left: 960, top: 10, width: 30, height: 30 }, card, viewport);
  assert.ok(edge.left + card.width <= viewport.width);
  assert.deepEqual(cardPosition(null, card, viewport), { left: 320, top: 500 / 3 });
});

test("one modal check serves the tour, its auto-start and the Esc guard", async () => {
  const { anyModalOpen, MODAL_SELECTOR } = await import("../src/tourSteps.ts");
  const asked: string[] = [];
  const dom = (found: boolean) => ({
    querySelector: (selector: string) => {
      asked.push(selector);
      return found ? ({} as Element) : null;
    },
  });
  assert.equal(anyModalOpen(dom(true)), true);
  assert.equal(anyModalOpen(dom(false)), false);
  assert.deepEqual(asked, [MODAL_SELECTOR, MODAL_SELECTOR]);
  // Sheets, native dialogs and confirmations all count; the tour's own card does not.
  for (const part of ['[aria-modal="true"]:not(.tour-card)', "dialog[open]", '[role="alertdialog"]']) {
    assert.ok(MODAL_SELECTOR.includes(part), part);
  }
});
