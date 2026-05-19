---
title: CLI Reference
description: Complete reference for all LiteParse CLI commands and options.
sidebar:
  order: 5
---

LiteParse provides the `lit` CLI with three commands: `parse`, `batch-parse`, and `screenshot`. The CLI is the same whether installed via `npm`, `pip`, or built from Rust source.

## `lit parse`

Parse a single document.

```
lit parse [options] <file>
```

### Arguments

| Argument | Description |
|----------|-------------|
| `file` | Path to the document file, or `-` to read from stdin |

### Options

| Option | Description | Default |
|--------|-------------|---------|
| `-o, --output <file>` | Write output to a file instead of stdout | — |
| `--format <format>` | Output format: `json` or `text` | `text` |
| `--no-ocr` | Disable OCR entirely | — |
| `--ocr-language <lang>` | OCR language code (Tesseract format) | `eng` |
| `--ocr-server-url <url>` | HTTP OCR server URL | — (uses Tesseract) |
| `--tessdata-path <path>` | Path to tessdata directory | — (uses `TESSDATA_PREFIX` env var) |
| `--num-workers <n>` | Pages to OCR in parallel | CPU cores - 1 |
| `--max-pages <n>` | Maximum pages to parse | `1000` |
| `--target-pages <pages>` | Pages to parse (e.g., `"1-5,10"`) | — (all pages) |
| `--dpi <dpi>` | Rendering DPI | `150` |
| `--preserve-small-text` | Keep very small text | — |
| `--password <password>` | Password for encrypted/protected documents | — |
| `-q, --quiet` | Suppress progress output | — |

### Examples

```bash
# Basic text parsing
lit parse report.pdf

# JSON output with bounding boxes
lit parse report.pdf --format json -o report.json

# Parse pages 1-5 only, no OCR
lit parse report.pdf --target-pages "1-5" --no-ocr

# High-DPI rendering with French OCR
lit parse report.pdf --dpi 300 --ocr-language fra

# Use an external OCR server
lit parse report.pdf --ocr-server-url http://localhost:8828/ocr

# Pipe output to another tool
lit parse report.pdf -q | wc -l

# Parse a remote file via stdin
curl -sL https://example.com/report.pdf | lit parse --no-ocr -
```

---

## `lit batch-parse`

Parse multiple documents in a directory.

```
lit batch-parse [options] <input-dir> <output-dir>
```

### Arguments

| Argument | Description |
|----------|-------------|
| `input-dir` | Directory containing documents to parse |
| `output-dir` | Directory for output files |

### Options

| Option | Description | Default |
|--------|-------------|---------|
| `--format <format>` | Output format: `json` or `text` | `text` |
| `--no-ocr` | Disable OCR entirely | — |
| `--ocr-language <lang>` | OCR language code | `eng` |
| `--ocr-server-url <url>` | HTTP OCR server URL | — (uses Tesseract) |
| `--tessdata-path <path>` | Path to tessdata directory | — |
| `--num-workers <n>` | Pages to OCR in parallel | CPU cores - 1 |
| `--max-pages <n>` | Maximum pages per file | `1000` |
| `--dpi <dpi>` | Rendering DPI | `150` |
| `--recursive` | Search subdirectories | — |
| `--extension <ext>` | Only process this extension (e.g., `".pdf"`) | — (all supported) |
| `--password <password>` | Password for encrypted/protected documents (applied to all files) | — |
| `-q, --quiet` | Suppress progress output | — |

### Examples

```bash
# Parse all supported files in a directory
lit batch-parse ./documents ./output

# Recursively parse only PDFs
lit batch-parse ./documents ./output --recursive --extension ".pdf"

# Batch parse with JSON output and no OCR
lit batch-parse ./documents ./output --format json --no-ocr
```

---

## `lit screenshot`

Generate page images from a PDF.

```
lit screenshot [options] <file>
```

### Arguments

| Argument | Description |
|----------|-------------|
| `file` | Path to the PDF file |

### Options

| Option | Description | Default |
|--------|-------------|---------|
| `-o, --output-dir <dir>` | Output directory | `./screenshots` |
| `--target-pages <pages>` | Pages to screenshot (e.g., `"1,3,5"` or `"1-5"`) | — (all pages) |
| `--dpi <dpi>` | Rendering DPI | `150` |
| `--password <password>` | Password for encrypted/protected documents | — |
| `-q, --quiet` | Suppress progress output | — |

### Examples

```bash
# Screenshot all pages
lit screenshot document.pdf -o ./pages

# First 5 pages at high DPI
lit screenshot document.pdf --target-pages "1-5" --dpi 300 -o ./pages

# Specific pages only
lit screenshot document.pdf --target-pages "1,5,10" -o ./pages
```

---

## Global options

These options are available on all commands:

| Option | Description |
|--------|-------------|
| `-h, --help` | Show help for a command |
| `-V, --version` | Show version number |
