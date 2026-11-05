import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Built files are served by columbus-mcp from ui/ beside the program, under
// a strict Content-Security-Policy (approvals_page.rs, UI_CSP): scripts,
// styles and fonts from the same origin only, so nothing is inlined.
export default defineConfig({
  plugins: [react()],
  base: "/",
  build: {
    outDir: "dist",
    assetsDir: "assets",
    assetsInlineLimit: 0,
    cssCodeSplit: false,
    sourcemap: false,
    modulePreload: { polyfill: false },
  },
});
