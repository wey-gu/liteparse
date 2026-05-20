#!/usr/bin/env bash
# Generates API reference docs from rustdoc JSON via rustdoc-md and outputs
# a single Starlight-compatible markdown file with frontmatter.
set -euo pipefail

DOCS_DIR="./docs/src/content/docs/liteparse"
TMP_DIR="${DOCS_DIR}/.api-tmp"
OUT_FILE="${DOCS_DIR}/api.md"

# Generate rustdoc JSON and convert to markdown
mkdir -p $TMP_DIR
cd crates/liteparse/
cargo +nightly rustdoc --lib -- -Z unstable-options --output-format json
cd ../../
rustdoc-md --path ./target/doc/liteparse.json --output ${TMP_DIR}/README.md
./scripts/strip-impls-from-api-docs.py

# Prepend Starlight frontmatter to the generated README
cat > "${OUT_FILE}" <<'FRONTMATTER'
---
title: API Reference
description: API reference for the liteparse Rust crate. Types are shared across Node.js, Python, and WASM bindings.
sidebar:
  order: 6
---
FRONTMATTER

cat "${TMP_DIR}/README.md" >> "${OUT_FILE}"

# Clean up temp directory
rm -rf "${TMP_DIR}"

echo "Generated ${OUT_FILE}"
