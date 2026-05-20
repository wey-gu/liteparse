---
title: Multi-Format Support
description: Parse Word documents, spreadsheets, presentations, and images with LiteParse.
sidebar:
  order: 3
---

LiteParse automatically converts non-PDF formats to PDF before parsing. This lets you use the same parsing pipeline for Office documents, images, and more.

## Supported formats

### Office documents (via LibreOffice)

| Category | Extensions |
|----------|-----------|
| Word | `.doc`, `.docx`, `.docm`, `.odt`, `.rtf`, `.pages` |
| PowerPoint | `.ppt`, `.pptx`, `.pptm`, `.odp`, `.key` |
| Spreadsheets | `.xls`, `.xlsx`, `.xlsm`, `.ods`, `.csv`, `.tsv`, `.numbers` |

### Images (via ImageMagick)

`.jpg`, `.jpeg`, `.png`, `.gif`, `.bmp`, `.tiff`, `.webp`, `.svg`

Images are converted to PDF and then parsed with OCR to extract text.

## Installing dependencies

Format conversion uses standard system tools. Install the ones you need:

### LibreOffice (for Office documents)

```bash
# macOS
brew install --cask libreoffice

# Ubuntu/Debian
apt-get install libreoffice

# Windows
choco install libreoffice-fresh
```

> On Windows, you may need to add the LibreOffice CLI directory (typically `C:\Program Files\LibreOffice\program`) to your PATH and restart.

### ImageMagick (for images)

```bash
# macOS
brew install imagemagick

# Ubuntu/Debian
apt-get install imagemagick

# Windows
choco install imagemagick.app
```

## Usage

Once the dependencies are installed, just pass any supported file to `lit parse`:

```bash
lit parse report.docx
lit parse slides.pptx --format json
lit parse spreadsheet.xlsx -o output.txt
lit parse scan.png
```

Batch mode also handles mixed formats:

```bash
lit batch-parse ./documents ./output --recursive
```

## How it works

1. LiteParse detects the file extension
2. If it's not a PDF, it converts to PDF using the appropriate tool (LibreOffice or ImageMagick)
3. The resulting PDF is parsed normally
4. Temporary conversion files are cleaned up automatically

If the required conversion tool isn't installed, LiteParse will return an error explaining which dependency is needed.
