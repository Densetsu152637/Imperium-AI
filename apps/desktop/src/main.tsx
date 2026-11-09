import { LucideProvider } from "lucide-react";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { App } from "./App";
import { applySettings, loadSettings, resolvedTheme } from "./settings";
import { loadStore } from "./store";
import "./styles.css";

// Follow the operating system's theme until the saved settings are read, so a light window
// does not flash dark while they load.
document.documentElement.dataset.theme = resolvedTheme("system");

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
