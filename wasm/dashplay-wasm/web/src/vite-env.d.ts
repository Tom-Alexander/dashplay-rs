/// <reference types="vite/client" />

declare module "wasi_snapshot_preview1" {
  export function fd_close(): number;
  export function fd_fdstat_get(): number;
  export function fd_seek(): number;
  export function fd_write(): number;
}
