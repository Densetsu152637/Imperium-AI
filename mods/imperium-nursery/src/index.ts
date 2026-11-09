// Imperium Nursery: many pets, three nurseries (session, global, mine). Each panel is its own
// frame, so `activate` runs once per panel and branches on `ctx.panelId` (see panels.ts).
// Pet behaviour is ported from Pixel Pets (MIT; see NOTICE).

import { defineMod } from "@imperium/mod-sdk";

import { startPanel } from "./panels.ts";

export default defineMod({
  id: "imperium-nursery",
  permissions: ["sessions:read", "pets:read", "storage"],
  contributes: {
    panels: [
      { id: "session", title: "Session nursery" },
      { id: "global", title: "Global nursery" },
      { id: "mine", title: "My nursery" },
    ],
  },
  activate: startPanel,
});
