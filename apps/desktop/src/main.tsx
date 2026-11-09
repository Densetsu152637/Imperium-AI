import { LucideProvider } from "lucide-react";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { App } from "./App";
import { loadStore } from "./store";
import "./styles.css";

// Settings and layout come from local files; load them before the first paint so the window
// opens as it was left.
void loadStore().finally(() =>
  createRoot(document.getElementById("root")!).render(
    <StrictMode>
      {/* One stroke for every icon; their size follows the text around them (styles.css). */}
      <LucideProvider strokeWidth={1.75}>
        <App />
      </LucideProvider>
    </StrictMode>,
  ),
);
