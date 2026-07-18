/**
 * Minimal WASI preview1 stubs for Bento4 / wasi-libc in the browser.
 * wasm-bindgen emits `import … from "wasi_snapshot_preview1"`; map that
 * specifier to this file via the import map in index.html.
 */
export function fd_close() {
  return 0;
}

export function fd_fdstat_get() {
  return 0;
}

export function fd_seek() {
  return 0;
}

export function fd_write() {
  return 0;
}
