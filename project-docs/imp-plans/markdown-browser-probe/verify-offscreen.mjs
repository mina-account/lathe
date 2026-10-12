import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const observations = async (path) => (await readFile(path, "utf8")).trim().split(/\r?\n/).map(JSON.parse);
const files = process.argv.slice(2);
if (files.length === 0) files.push(new URL("./offscreen-windows-observed.jsonl", import.meta.url), new URL("./offscreen-wayland-observed.jsonl", import.meta.url));
for (const path of files) {
  const entries = await observations(path);
  const matching = (event) => entries.filter((entry) => entry.event === event);
  assert.equal(matching("runtime").length, 1);
  const runtime = matching("runtime")[0];
  assert.equal(runtime.cef_bindings, "154.5.0+154.0.34");
  assert.equal(runtime.sandbox, false);
  assert.equal(runtime.content, "packaged-static-only");
  const backend = matching("gpui_backend")[0]?.handle;
  assert.match(backend, /^(Win32|Wayland)\(/);
  if (backend.startsWith("Wayland")) {
    assert.equal(runtime.chromium_backend, "headless");
    assert.equal(runtime.display, null);
    assert.equal(matching("scene_readback_unavailable").length, 4);
  } else {
    assert.equal(runtime.chromium_backend, "windows-windowless");
    for (const label of ["initial", "resized", "hidden", "recreated"]) {
      assert(matching("scene_readback").some((entry) => entry.label === label && entry.overlay && entry.clipping));
    }
  }
  for (const session of ["left", "right", "recreated"]) {
    assert.equal(matching("browser_created").filter((entry) => entry.session === session).length, 1);
    assert.equal(matching("browser_closed").filter((entry) => entry.session === session).length, 1);
    for (const kind of ["Hello", "Acknowledged"]) {
      assert(matching("ipc").some((entry) => entry.message.session === session && entry.message.kind === kind && entry.message.request === 1));
    }
    const frames = matching("frame_consumed").filter((entry) => entry.session === session);
    assert(frames.length > 0);
    assert(frames.every((frame) => frame.bytes === frame.width * frame.height * 4 && frame.bytes > 0 && frame.bytes <= 16 * 1024 * 1024));
    const cost = matching("frame_cost").find((entry) => entry.session === session);
    assert(cost?.frames > 0 && cost.copied_bytes > 0 && cost.copy_microseconds > 0 && cost.gpui_paints > 0);
  }
  for (const session of ["left", "right"]) {
    assert(new Set(matching("bounds").filter((entry) => entry.session === session).map((entry) => `${entry.width}:${entry.height}`)).size >= 2);
  }
  assert(matching("resource").filter((entry) => entry.status === 404 && entry.url.endsWith("/missing.bin")).length >= 3);
  assert(matching("navigation").some((entry) => entry.allowed === false));
  assert(matching("navigation").every((entry) => entry.allowed === /^https:\/\/lathe-probe\.invalid\/index\.html\?session=(left|right|recreated)$/.test(entry.url)));
  assert(matching("popup_denied").length > 0);
  assert(matching("visibility").some((entry) => entry.pane === 1 && entry.visible === false));
  assert(matching("visibility").some((entry) => entry.pane === 1 && entry.visible === true));
  for (const [session, text] of [["left", "a"], ["right", "b"]]) {
    assert(matching("ipc").some((entry) => entry.message.kind === "Input" && entry.message.session === session && entry.message.text === text));
  }
  for (const event of ["pointer", "wheel", "key_up"]) {
    assert(matching("ipc").some((entry) => entry.message.kind === "Event" && entry.message.session === "left" && entry.message.event === event));
  }
  const events = matching("ipc").filter((entry) => entry.message.kind === "Event" && entry.message.session === "left").map((entry) => entry.message);
  for (const [key, key_code, caret] of [["Home",36,6], ["End",35,10], ["PageUp",33,0], ["PageDown",34,16], [" ",32,17]]) {
    assert(events.some(({event, details}) => event === "key_up" && details.type === "keyboard" && details.key === key && details.key_code === key_code && details.selection_start === caret && details.selection_end === caret), `Missing browser key/caret result for ${key}`);
  }
  assert(matching("ipc").some((entry) => entry.message.kind === "Input" && entry.message.session === "left" && entry.message.text === "alpha\nbeta\ngamma "));
  for (const [event, name, buttons, detail, alt_key] of [["pointer","pointerdown",1,0,false], ["drag","pointermove",1,0,false], ["pointer_up","pointerup",0,0,false], ["double_click","dblclick",0,2,false], ["wheel","wheel",0,0,true], ["wheel","wheel",1,0,true]]) {
    assert(events.some((entry) => entry.event === event && entry.details.type === "pointer" && entry.details.name === name && entry.details.shift_key === true && entry.details.control_key === false && entry.details.alt_key === alt_key && entry.details.meta_key === false && entry.details.buttons === buttons && entry.details.detail === detail), `Missing browser pointer metadata for ${name}, buttons=${buttons}`);
  }
  assert.deepEqual(matching("smoke_phase").map((entry) => entry.phase), Array.from({length: 17}, (_, index) => index));
  assert.equal(matching("runtime_shutdown").length, 1);
  console.log(`Verified real ${backend.startsWith("Wayland") ? "Wayland/headless-CEF" : "Windows"} offscreen smoke; native IME/accessibility/installer acceptance remains pending.`);
}
