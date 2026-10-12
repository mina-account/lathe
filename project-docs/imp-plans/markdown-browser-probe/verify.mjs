import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const path = process.argv[2] ?? new URL("./windows-observed.jsonl", import.meta.url);
const observations = (await readFile(path, "utf8")).trim().split(/\r?\n/).map(JSON.parse);
const matching = (event) => observations.filter((entry) => entry.event === event);
assert.equal(matching("runtime").length, 1);
assert.equal(matching("runtime")[0].wry, "0.57.0");
assert.match(matching("runtime")[0].webview, /^\d+\.\d+\.\d+\.\d+$/);
for (const session of ["left", "right"]) {
  assert(matching("ipc").some((entry) => entry.session === session && entry.message.startsWith("Hello ")));
  assert(matching("ipc").some((entry) => entry.session === session && entry.message.startsWith("Acknowledged ")));
}
for (const path of ["/index.html", "/probe.js", "/probe.css"]) {
  assert.equal(matching("resource").filter((entry) => entry.path === path && entry.status === 200).length, 2);
}
assert.equal(matching("resource").filter((entry) => entry.path === "/missing.bin" && entry.status === 404).length, 2);
assert.equal(matching("new_window_denied").length, 2);
assert.equal(matching("navigation").filter((entry) => entry.allowed === false).length, 2);
assert(matching("navigation").every((entry) => entry.allowed === (entry.url === "http://lathe-probe.localhost/index.html")));
assert(matching("bounds").length >= 2);
assert(new Set(matching("bounds").map((entry) => entry.x)).size >= 2);
assert(matching("bounds").every((entry) => entry.scale > 0 && entry.width > 0 && entry.height > 0));
for (const tick of [20, 30, 40, 50]) {
  assert.equal(matching("lifecycle_api").filter((entry) => entry.tick === tick && entry.success).length, 2);
}
assert.equal(matching("teardown").length, 1);
console.log("Windows packaged-resource/typed-IPC/navigation/API smoke verified; native composition and input remain unverified.");

const failure = JSON.parse(await readFile(new URL("./wry-failure-observed.json", import.meta.url), "utf8"));
assert.equal(failure.scenario, "unavailable-webview2");
assert.match(failure.environment.WEBVIEW2_BROWSER_EXECUTABLE_FOLDER, /missing-webview2$/);
assert.equal(failure.exit_code, 1);
assert.match(failure.stderr, /browser creation failed: WebView2 error/);
assert.equal(failure.stdout.filter((entry) => entry.event === "teardown").length, 0);
assert.equal(failure.stdout.filter((entry) => entry.event === "ipc").length, 0);
console.log("Unavailable WebView2 initialization fails with exit 1 and no successful teardown marker.");
