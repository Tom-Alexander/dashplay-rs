import path from "node:path";
import { fileURLToPath } from "node:url";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

const rootDir = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(rootDir, "../../..");

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      // wasm-bindgen emits bare `wasi_snapshot_preview1` imports for Bento4/libc.
      wasi_snapshot_preview1: path.resolve(rootDir, "src/wasi_snapshot_preview1.ts"),
    },
  },
  server: {
    port: 8080,
    fs: {
      allow: [rootDir, repoRoot],
    },
  },
  optimizeDeps: {
    exclude: ["dashplay-wasm"],
  },
});
