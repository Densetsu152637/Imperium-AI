import { LucideProvider } from "lucide-react";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { App } from "./App";
import { armSplash, bootTheme, THEME_KEY } from "./boot";
import { applySettings, loadSettings, resolvedTheme } from "./settings";
import { loadStore } from "./store";
import "./styles.css";

// The loading screen in index.html stays until the panels render (App calls splashDone); the
// timer inside armSplash removes it anyway if they never do.
armSplash();

// Paint the last theme until the saved settings are read, so neither theme flashes the other.
// index.html already set it from the same key; this covers a stale or unavailable key.
let remembered: string | null = null;
try {
  remembered = localStorage.getItem(THEME_KEY);
} catch {
  // Unavailable storage: follow the operating system.
}
document.documentElement.dataset.theme = bootTheme(remembered, resolvedTheme("system") === "light");

// Settings and layout come from local files; load them before the first paint so the window
// opens as it was left.
void loadStore().finally(() => {
  applySettings(loadSettings());
  createRoot(document.getElementById("root")!).render(
    <StrictMode>
      {/* One stroke for every icon; their size follows the text around them (styles.css). */}
      <LucideProvider strokeWidth={1.75}>
        <App />
      </LucideProvider>
    </StrictMode>,
  );
});
