import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// Tauri serves the dev build from this fixed port (see src-tauri/tauri.conf.json).
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  build: { target: "es2022", outDir: "dist" },
});
