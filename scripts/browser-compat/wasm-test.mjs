/**
 * Playwright test that loads the LiteParse WASM module in a real browser
 * and verifies it can parse a PDF end-to-end.
 *
 * Usage: node scripts/browser-compat/wasm-test.mjs
 *
 * Requires: playwright (npx playwright install chromium)
 * Expects:  packages/wasm/pkg/ to contain the built WASM files
 *           demo/docs/apple-10k-2024.pdf to exist
 */

import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { resolve, extname } from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright";

const ROOT = resolve(fileURLToPath(import.meta.url), "../../..");

const MIME_TYPES = {
  ".html": "text/html",
  ".js": "application/javascript",
  ".wasm": "application/wasm",
  ".pdf": "application/pdf",
  ".json": "application/json",
};

function startServer() {
  return new Promise((resolvePromise) => {
    const server = createServer(async (req, res) => {
      const url = new URL(req.url, "http://localhost");
      const filePath = resolve(ROOT, "." + url.pathname);

      // Basic security: don't serve outside ROOT
      if (!filePath.startsWith(ROOT)) {
        res.writeHead(403);
        res.end("Forbidden");
        return;
      }

      try {
        const data = await readFile(filePath);
        const ext = extname(filePath);
        res.writeHead(200, {
          "Content-Type": MIME_TYPES[ext] || "application/octet-stream",
          "Cross-Origin-Opener-Policy": "same-origin",
          "Cross-Origin-Embedder-Policy": "require-corp",
        });
        res.end(data);
      } catch {
        res.writeHead(404);
        res.end("Not found");
      }
    });

    server.listen(0, "127.0.0.1", () => {
      const port = server.address().port;
      resolvePromise({ server, port });
    });
  });
}

async function main() {
  const { server, port } = await startServer();
  const baseUrl = `http://127.0.0.1:${port}`;
  console.log(`Static server listening on ${baseUrl}`);

  let browser;
  try {
    browser = await chromium.launch();
    const page = await browser.newPage();

    // Collect console errors
    const errors = [];
    page.on("pageerror", (err) => errors.push(err.message));

    console.log("Navigating to test page...");
    await page.goto(`${baseUrl}/scripts/browser-compat/wasm-test.html`);

    // Wait for either #result or #error to appear (up to 120s for large PDFs)
    await page.waitForSelector("#result[style*='block'], #error[style*='block']", {
      timeout: 120_000,
    });

    const errorText = await page.locator("#error").textContent();
    if (errorText) {
      console.error(`FAIL: ${errorText}`);
      if (errors.length) console.error("Console errors:", errors);
      process.exit(1);
    }

    const resultText = await page.locator("#result").textContent();
    const pages = await page.locator("#result").getAttribute("data-pages");
    const textLength = await page.locator("#result").getAttribute("data-text-length");

    console.log(`PASS: ${resultText}`);
    console.log(`  Pages: ${pages}`);
    console.log(`  Text length: ${textLength}`);

    if (errors.length) {
      console.warn("Browser console errors (non-fatal):", errors);
    }
  } finally {
    if (browser) await browser.close();
    server.close();
  }
}

main().catch((err) => {
  console.error("Test runner failed:", err);
  process.exit(1);
});
