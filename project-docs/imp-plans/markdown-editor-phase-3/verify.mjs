import assert from "node:assert/strict";
import {readFile} from "node:fs/promises";
import {createHash} from "node:crypto";

const files = process.argv.slice(2);
if (!files.length) files.push(...["windows","wayland","x11"].map(backend=>new URL(`${backend}-observed.jsonl`,import.meta.url)));
for (const file of files) {
    const entries=(await readFile(file,"utf8")).trim().split(/\r?\n/).map(JSON.parse);
    const matching=event=>entries.filter(entry=>entry.event===event);
    const one=event=>{assert.equal(matching(event).length,1,event);return matching(event)[0];};
    assert.match(one("gpui_backend").handle,/Win32|Wayland|Xlib|Xcb/);
    const runtimes=matching("rich_runtime");
    assert.deepEqual(runtimes.map(entry=>entry.mode),["ir","wysiwyg","sv","ir","preview"]);
    for (const runtime of runtimes) {assert.equal(runtime.version,"3.11.2");assert(runtime.source_preserved);}
    assert(one("visual_to_native").source_same_buffer);
    const modeInput=one("delayed_rich_input_mode"), flushInput=one("delayed_rich_input_flush");
    assert(modeInput.passed); assert(flushInput.passed);
    assert.equal(modeInput.text.replace("# debounced","# visual"),one("visual_to_native").text);
    assert.equal(flushInput.text.replace("# flush-input","# mirrored"),one("disjoint_external_rebase").text);
    assert(one("native_to_visual_to_native").text.startsWith("# mirrored 😀"));
    assert(one("rapid_toolbar_history").passed);
    assert(one("browser_native_undo").text.startsWith("# native 😀"));
    assert(one("browser_native_redo").text.startsWith("# mirrored 😀"));
    assert(one("real_item_flush").passed);
    assert(one("queued_no_op_acknowledged").passed);
    assert(one("cancelled_composition_flush").passed);
    assert(one("disjoint_external_rebase").text.endsWith("X1234L678Y"));
    const pending=one("unacknowledged_draft_dirty"), crashed=one("renderer_crash_retained_draft"), recreated=one("recreated_browser_retained_draft");
    assert.equal(pending.draft,`${pending.native_text} retained`);
    assert.equal(crashed.draft,pending.draft); assert(crashed.error);
    assert.equal(recreated.native_text,pending.native_text); assert.equal(recreated.draft,pending.draft);
    assert(matching("navigation").some(entry=>entry.url==="https://foreign.invalid/" && entry.allowed===false));
    assert.equal(one("retired_surface_released").tracked,1);
    assert.equal(matching("browser_created").length,2); assert.equal(matching("browser_closed").length,2);
    assert.notEqual(matching("browser_created")[0].session,matching("browser_created")[1].session);
    for (const entry of matching("resource")) {assert(entry.url.startsWith("https://lathe-markdown.invalid/")); assert.equal(entry.status,200);}
    assert(matching("resource").some(entry=>entry.url.endsWith("/vendor/vditor.js")));
    assert(matching("resource").some(entry=>entry.url.endsWith("/vendor/purify.js")));
    console.log(`Verified pinned rich modes, source/history/flush/recovery: ${one("gpui_backend").handle}`);
}
const runtime=JSON.parse(await readFile(new URL("runtime.json",import.meta.url),"utf8"));
for (const [file,hash] of Object.entries(runtime.assets)) {
    assert.equal(createHash("sha256").update(await readFile(new URL(`../../../${file}`,import.meta.url))).digest("hex"),hash,file);
}
console.log("Verified locked packaged core asset bytes");
const checks=JSON.parse(await readFile(new URL("checks.json",import.meta.url),"utf8"));
assert.equal(checks.frontendBridge,17);
assert.equal(checks.frontendRealBrowser,36);
assert.equal(checks.isolatedGeneratedAssetReproduction,11);
assert.deepEqual(checks.nativeSmokeExit,{windows:0,wayland:0,x11:0});
assert(checks.reviewPass4Regressions && checks.reviewPass4Verification && checks.reviewPass4SetextCounterCases);
console.log("Verified recorded boundary, empty-input, Viewer-selection and final-asset checks");
