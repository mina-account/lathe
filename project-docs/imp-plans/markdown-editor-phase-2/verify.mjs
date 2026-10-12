import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const files = process.argv.slice(2);
if (!files.length) files.push(...["windows", "wayland", "x11"].map(backend => new URL(`${backend}-observed.jsonl`, import.meta.url)));
for (const file of files) {
    const entries = (await readFile(file, "utf8")).trim().split(/\r?\n/).map(JSON.parse);
    const matching = event => entries.filter(entry => entry.event === event);
    const one = event => { assert.equal(matching(event).length, 1, event); return matching(event)[0]; };
    assert.match(one("gpui_backend").handle, /Win32|Wayland|Xlib|Xcb/);
    assert(one("visual_to_native").source_same_buffer);
    assert(one("native_to_visual_to_native").text.startsWith("# mirrored 😀"));
    assert(one("browser_native_undo").text.startsWith("# native 😀"));
    assert(one("browser_native_redo").text.startsWith("# mirrored 😀"));
    assert(one("real_item_flush").passed);
    const pending = one("unacknowledged_draft_dirty");
    const crashed = one("renderer_crash_retained_draft");
    const recreated = one("recreated_browser_retained_draft");
    assert.equal(pending.draft, `${pending.native_text} retained`);
    assert.equal(crashed.draft, pending.draft);
    assert(crashed.error);
    assert.equal(recreated.native_text, pending.native_text);
    assert.equal(recreated.draft, `${pending.native_text} recreated`);
    assert(matching("navigation").some(entry => entry.url === "https://foreign.invalid/" && entry.allowed === false));
    assert.equal(one("retired_surface_released").tracked, 1);
    assert.equal(matching("browser_created").length, 2);
    assert.equal(matching("browser_closed").length, 2);
    assert.notEqual(matching("browser_created")[0].session, matching("browser_created")[1].session);
    for (const entry of matching("resource")) {
        assert(entry.url.startsWith("https://lathe-markdown.invalid/"));
        assert.equal(entry.status, 200);
    }
    console.log(`Verified real shared-buffer Markdown Item edits/history/flush/crash/recreation/close: ${one("gpui_backend").handle}`);
}
