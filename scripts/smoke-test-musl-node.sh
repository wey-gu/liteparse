#!/bin/sh
# Smoke test for the musl Node binding, run inside node:20-alpine.
# Assumes the build artifact has been downloaded into /work/packages/node and
# `npx tsc` has produced dist/.
set -eux

cd /work/packages/node

ls -la *.node *.so 2>/dev/null || true

# Load the .node file directly first, so any dlopen / relocation error surfaces
# verbatim. The native.ts loader silently swallows require() failures and
# reports a generic "Failed to load native module" message, which hides the
# real cause (missing shared lib, unresolved symbol, etc.).
node -e '
  const path = require("node:path");
  const f = path.resolve("./liteparse.linux-x64-musl.node");
  console.log("loading", f);
  require(f);
  console.log("raw .node loaded ok");
'

# Then run the actual smoke check through the public lib entry point.
node -e '
  import("./dist/lib.js").then(async ({ LiteParse }) => {
    const p = new LiteParse({ ocrEnabled: false, quiet: true });
    console.log("Config:", JSON.stringify(p.getConfig()));
    console.log("Native module loaded successfully");
  }).catch(e => { console.error(e); process.exit(1); });
'
