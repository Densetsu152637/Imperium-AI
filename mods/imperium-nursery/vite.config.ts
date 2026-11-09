// Bundles the mod into ONE self-contained ES module (index.js): the app inlines the entry into
// a sandboxed frame, so relative imports cannot resolve there. Only the SDK stays external; the
// frame's import map supplies it.

import { defineConfig } from "vite";

export default defineConfig({
  build: {
    lib: { entry: "src/index.ts", formats: ["es"], fileName: () => "index.js" },
    outDir: ".",
    emptyOutDir: false,
    minify: false,
    rollupOptions: { external: ["@imperium/mod-sdk"] },
  },
});
