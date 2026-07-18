import { accessSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

const pkgJs = resolve(
  fileURLToPath(new URL(".", import.meta.url)),
  "../pkg/dashplay_wasm.js",
);

try {
  accessSync(pkgJs);
} catch {
  console.error(
    [
      "Missing wasm package at web/pkg/.",
      "Build it first:",
      "",
      "  cd wasm/dashplay-wasm",
      "  export WASI_SDK_PATH=/path/to/wasi-sdk",
      "  AR=llvm-ar wasm-pack build --target web --out-dir web/pkg",
      "",
    ].join("\n"),
  );
  process.exit(1);
}
