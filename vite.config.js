import { fileURLToPath } from 'node:url';
import { defineConfig } from 'vite';

/**
 * Host UI build for Toolbox 0.2.0.
 *
 * `ui/` stays the source root (the pages were written there and the Rust side
 * refers to `panel.html` / `main.html` by name), the build goes to `ui/dist`, and
 * `src-tauri/tauri.conf.json` points `frontendDist` at `../ui/dist`.
 *
 * Notes that are easy to get wrong (all three were measured during the S0 spike):
 *   * `outDir` is resolved **relative to `root`**, not to this file — so `'dist'`
 *     means `ui/dist`. `'../dist'` would land in the repository root `dist/`,
 *     which is where `scripts/make-portable.ps1` writes its artifacts.
 *   * `base: './'` keeps asset URLs relative. Both `'/'` and `'./'` work for the
 *     host (the panel is loaded from the origin root), but relative survives a
 *     future sub-path and costs nothing.
 *   * No `devUrl` on the Tauri side: with Tauri's default features the crate is
 *     compiled in "dev" mode, and `devUrl` makes it embed an empty asset map —
 *     a plain `cargo build --release` would then ship an exe that tries to reach
 *     the dev server.
 */
const at = (p) => fileURLToPath(new URL(p, import.meta.url));

export default defineConfig({
  root: 'ui',
  base: './',
  publicDir: false,
  clearScreen: false,
  envPrefix: ['VITE_', 'TAURI_'],
  build: {
    outDir: 'dist', // -> ui/dist (relative to `root`)
    emptyOutDir: true,
    target: 'chrome105', // WebView2 floor documented for Tauri 2 on Windows
    rollupOptions: {
      input: {
        panel: at('./ui/panel.html'),
        main: at('./ui/main.html'),
      },
    },
  },
  server: {
    host: '127.0.0.1',
    port: 5173,
    strictPort: true,
  },
});
