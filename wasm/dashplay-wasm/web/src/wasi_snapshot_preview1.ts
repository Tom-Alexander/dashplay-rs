/**
 * Minimal WASI preview1 stubs for Bento4 / wasi-libc in the browser.
 * wasm-bindgen emits `import … from "wasi_snapshot_preview1"`; Vite resolves
 * that specifier via the alias in vite.config.ts.
 */
export function fd_close(): number {
  return 0;
}

export function fd_fdstat_get(): number {
  return 0;
}

export function fd_seek(): number {
  return 0;
}

export function fd_write(): number {
  return 0;
}
