#!/usr/bin/env node

const fs = require("node:fs");
const http = require("node:http");
const https = require("node:https");
const path = require("node:path");
const puppeteer = require("puppeteer-core");

function usage() {
  console.error("Usage: node examples/pptr.js <session-json-path> [target-url]");
  process.exit(1);
}

function loadSession(sessionPath) {
  const absolute = path.resolve(sessionPath);
  const raw = fs.readFileSync(absolute, "utf8");
  return JSON.parse(raw);
}

function requestJSON(endpoint, headers) {
  return new Promise((resolve, reject) => {
    const parsed = new URL(endpoint);
    const client = parsed.protocol === "https:" ? https : http;
    const req = client.request(
      {
        hostname: parsed.hostname,
        port: parsed.port,
        path: `${parsed.pathname}${parsed.search}`,
        method: "GET",
        headers,
      },
      (res) => {
        let body = "";
        res.on("data", (chunk) => {
          body += chunk;
        });
        res.on("end", () => {
          if (res.statusCode !== 200) {
            reject(new Error(`unexpected status ${res.statusCode}`));
            return;
          }
          try {
            resolve(JSON.parse(body));
          } catch (err) {
            reject(err);
          }
        });
      },
    );
    req.on("error", reject);
    req.end();
  });
}

async function resolveWebSocketURL(session, headers) {
  if (session.browser_ws_url) {
    return session.browser_ws_url;
  }
  if (!session.browse_url) {
    throw new Error("session JSON must include browser_ws_url or browse_url");
  }

  const base = new URL(session.browse_url);
  const payload = await requestJSON(
    `${session.browse_url.replace(/\/$/, "")}/json/version`,
    headers,
  );
  if (!payload.webSocketDebuggerUrl) {
    throw new Error("webSocketDebuggerUrl missing from /json/version response");
  }
  const upstream = new URL(payload.webSocketDebuggerUrl);
  upstream.protocol = base.protocol === "https:" ? "wss:" : "ws:";
  upstream.host = base.host;
  return upstream.toString();
}

async function main() {
  const [, , sessionPath, targetArg] = process.argv;
  if (!sessionPath) {
    usage();
  }

  const session = loadSession(sessionPath);
  const headers = session.connect_headers || {
    Host: "localhost",
    ...(session.token ? { Authorization: `Bearer ${session.token}` } : {}),
  };
  const wsURL = await resolveWebSocketURL(session, headers);

  const browser = await puppeteer.connect({
    browserWSEndpoint: wsURL,
    headers,
    ignoreHTTPSErrors: true,
  });

  const pages = await browser.pages();
  const page = pages[0] || (await browser.newPage());
  const target = targetArg || session.dev_server_url || "https://example.com";
  await page.goto(target, { waitUntil: "domcontentloaded" });
  console.log(`title=${await page.title()}`);
  await page.screenshot({ path: "puppeteer.png", fullPage: true });
  await browser.close();
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
