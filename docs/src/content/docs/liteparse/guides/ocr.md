---
title: OCR Configuration
description: Configure OCR in LiteParse — built-in Tesseract, or bring your own via HTTP servers.
sidebar:
  order: 2
---

LiteParse uses OCR selectively — only on embedded images or pages where native text extraction didn't find text. This keeps parsing fast while still capturing text from scanned pages and embedded images.

## Built-in Tesseract (default)

Tesseract is bundled with LiteParse and works out of the box. Just run:

```bash
lit parse document.pdf
```

### Language support

Specify the OCR language for better accuracy on non-English documents:

```bash
lit parse document.pdf --ocr-language fra    # French
lit parse document.pdf --ocr-language deu    # German
lit parse document.pdf --ocr-language jpn    # Japanese
```

Tesseract uses [ISO 639-3](https://tesseract-ocr.github.io/tessdoc/Data-Files-in-different-versions.html) language codes (`eng`, `fra`, `deu`, etc.).

### Offline / air-gapped environments

For environments without internet access, point Tesseract at a local directory containing pre-downloaded `.traineddata` files:

```bash
# Via environment variable
export TESSDATA_PREFIX=/path/to/tessdata
lit parse document.pdf --ocr-language eng

# Or via CLI flag
lit parse document.pdf --tessdata-path /path/to/tessdata
```

The `tessdata_path` / `tessdataPath` option is also available in the library APIs.

### Disabling OCR

If you don't need OCR (pure native-text PDFs, or you don't care about images), disable it for faster parsing:

```bash
lit parse document.pdf --no-ocr
```

## HTTP OCR servers

For higher accuracy or GPU-accelerated OCR, you can point LiteParse at an HTTP OCR server. LiteParse ships with ready-to-use examples for popular OCR engines.

### EasyOCR

```bash
# Start the EasyOCR server (requires Python)
git clone https://github.com/run-llama/liteparse.git
cd liteparse/ocr/easyocr
pip install -r requirements.txt
python server.py

# Parse with EasyOCR in another terminal
lit parse document.pdf --ocr-server-url http://localhost:8828/ocr
```

### PaddleOCR

```bash
# Start the PaddleOCR server (requires Python)
git clone https://github.com/run-llama/liteparse.git
cd liteparse/ocr/paddleocr
pip install -r requirements.txt
python server.py

# Parse with PaddleOCR in another terminal
lit parse document.pdf --ocr-server-url http://localhost:8828/ocr
```

### Parallel OCR workers

LiteParse OCRs multiple pages in parallel. By default, it uses one fewer worker than your CPU core count. Override this with:

```bash
lit parse document.pdf --num-workers 8
```

This is useful if you need to slow down OCR requests to an external server or if your OCR engine is GPU-accelerated and can handle more concurrency.

## Custom OCR servers

You can integrate any OCR engine by implementing the LiteParse OCR API. Your server needs a single endpoint:

```
POST /ocr
Content-Type: multipart/form-data
```

**Request fields:**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `file` | binary | Yes | Image file (PNG, JPG, etc.) |
| `language` | string | No | ISO 639-1 language code (default: `en`) |

**Response format:**

```json
{
  "results": [
    {
      "text": "recognized text",
      "bbox": [x1, y1, x2, y2],
      "confidence": 0.95
    }
  ]
}
```

Each result contains:

| Field | Type | Description |
|-------|------|-------------|
| `text` | string | Recognized text |
| `bbox` | `[x1, y1, x2, y2]` | Bounding box in pixels. Origin is top-left, x goes right, y goes down |
| `confidence` | number | Score from 0.0 to 1.0 |

### Testing your server

```bash
# Quick test with curl
curl -X POST http://localhost:8080/ocr \
  -F "file=@test.png" \
  -F "language=en" | jq .

# Use with LiteParse
lit parse document.pdf --ocr-server-url http://localhost:8080/ocr
```

### Common Gotchas

- Return `{"results": []}` if no text is detected
- Bounding boxes must be axis-aligned (`[x1, y1, x2, y2]` where top-left to bottom-right)
- If your engine returns rotated boxes, convert to axis-aligned by taking min/max coordinates
- If your engine doesn't provide confidence scores, return `1.0`
- Results should be in reading order (top-to-bottom, left-to-right)
- Cache OCR models in memory rather than reloading per request

## OCR in the browser (WASM)

The built-in Tesseract and HTTP OCR backends are not available in the WASM build. Instead, you can pass a custom `ocrEngine` object with a `recognize` method. See the [browser usage guide](/liteparse/guides/browser-usage/) for details.

### A note on OCR approaches

These days, its common to apply the term "OCR" to both traditional approaches and newer LLM-based document understanding models.

The LiteParse OCR API is designed specifically for approaches that return text with bounding boxes.

If you are trying to integrate a method that doesn't return bounding boxes, you will have to generate dummy bounding boxes.
