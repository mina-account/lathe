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
    const renderers=matching("phase4_renderers");
    assert.equal(renderers.length,5);
    for(const result of renderers) {
        assert.equal(result.math_version,"0.16.9");assert(result.source_preserved);
        assert.equal(result.code_highlighted,true);assert.equal(result.code_theme_loaded,true);
        for(const language of ["mermaid","graphviz","echarts","abc","flowchart","markmap","mindmap","smiles","math"]) assert(result.renderers.some(renderer=>renderer.language===language&&renderer.state==="ready"));
        for(const language of ["markmap","mindmap"]) assert(result.renderers.some(renderer=>renderer.language===language&&/Root/.test(renderer.text)&&/Child/.test(renderer.text)));
        assert(result.renderers.some(renderer=>renderer.language==="plantuml"&&renderer.state==="error"&&/cannot render offline/.test(renderer.text)));
    }
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
    assert(matching("resource").some(entry=>entry.url.includes("/highlight.js/styles/") && entry.url.endsWith(".min.css")));
    console.log(`Verified pinned rich modes, source/history/flush/recovery: ${one("gpui_backend").handle}`);
}
const runtime=JSON.parse(await readFile(new URL("runtime.json",import.meta.url),"utf8"));
for (const [file,hash] of Object.entries(runtime.assets)) {
    assert.equal(createHash("sha256").update(await readFile(new URL(`../../../${file}`,import.meta.url))).digest("hex"),hash,file);
}
console.log("Verified locked packaged core asset bytes");
for(const [file,hash] of Object.entries(runtime.sourceHashesNormalizedLF)) {
    const source=(await readFile(new URL(`../../../crates/markdown_editor/frontend/${file}`,import.meta.url),"utf8")).replaceAll("\r\n","\n");
    assert.equal(createHash("sha256").update(source).digest("hex"),hash,file);
}
for(const [file,hash] of Object.entries(runtime.nativeSourceHashesNormalizedLF)) {
    const source=(await readFile(new URL(`../../../${file}`,import.meta.url),"utf8")).replaceAll("\r\n","\n");
    assert.equal(createHash("sha256").update(source).digest("hex"),hash,file);
}
const checks=JSON.parse(await readFile(new URL("checks.json",import.meta.url),"utf8"));
assert.equal(checks.frontendBridge,17);
assert.equal(checks.frontendRealBrowser,57);
assert.equal(checks.isolatedGeneratedAssetReproduction,347);
assert.deepEqual(checks.nativeSmokeExit,{windows:0,wayland:0,x11:0});
assert.equal(checks.windowsNative,77);assert.equal(checks.linuxNative,76);
assert.equal(checks.reviewFixPass2.findingsFixed,3);assert.equal(checks.reviewFixPass2.nativeAssetGreen,15);
assert.equal(checks.reviewFixPass2.loadedImageHistoryAndFlush,true);
assert.equal(checks.reviewFixPass3.findingsFixed,3);
assert.equal(checks.reviewFixPass3.windowsAssetGreen,20);assert.equal(checks.reviewFixPass3.linuxAssetGreen,19);
assert.equal(checks.reviewFixPass4.findingsFixed,2);
assert.equal(checks.reviewFixPass4.windowsAssetGreen,24);assert.equal(checks.reviewFixPass4.linuxAssetGreen,23);
assert.equal(checks.reviewFixPass4.raceSeeds,20);
assert.equal(checks.reviewFixPass4.nativeCountRejectedBeforeOpen,true);
assert.equal(checks.reviewFixPass4.nativeCollectionReadBytesBounded,true);
assert.equal(checks.reviewFixPass5.findingsFixed,2);
assert.equal(checks.reviewFixPass5.schedulingSeeds,3);
assert.equal(checks.reviewFixPass5.validationsPerScenario,5);
assert.equal(checks.reviewFixPass5.runtimeRegenerationVerified,true);
assert.equal(checks.reviewFixPass6.findingsFixed,1);
assert.equal(checks.reviewFixPass6.focusedBrowserGreen,3);
assert.equal(checks.reviewFixPass6.pointerPathPreserved,true);
assert.equal(checks.reviewFixPass6.hostReplacementPositionBounded,true);
assert.equal(checks.reviewFixPass6.nativeSourceCaretSaveUndo,true);
assert.equal(checks.reviewFixPass7.findingsFixed,1);
assert.equal(checks.reviewFixPass7.focusedBrowserGreen,6);
assert.equal(checks.reviewFixPass7.queuedScrollBeforeEventModes,4);
assert.equal(checks.reviewFixPass7.synchronousPanePositionGuard,true);
assert.equal(checks.reviewFixPass7.delayedReaderScenarios,9);
assert.equal(checks.reviewFixPass7.noLaterReaderScenarios,4);
assert.equal(checks.reviewFixPass7.viewerSpaceRetained,true);
assert.equal(checks.reviewFixPass7.outlineNavigationRetained,true);
assert.equal(checks.reviewFixPass7.savedScrollRetained,true);
assert.equal(checks.reviewFixPass8.findingsFixed,1);
assert.equal(checks.reviewFixPass8.realFilesystemImportScenarios,48);
assert.equal(checks.reviewFixPass8.utf8FilenameBudget,255);
assert.equal(checks.reviewFixPass8.actualByteIdentity,true);
assert.equal(checks.reviewFixPass8.exactSourceUndoAndSave,true);
assert.equal(checks.reviewFixPass9.findingsFixed,1);
assert.equal(checks.reviewFixPass9.focusedNativeGreen.windows,2);
assert.equal(checks.reviewFixPass9.focusedNativeGreen.linux,2);
assert.equal(checks.reviewFixPass9.savedPickerCancelClearsProgress,true);
assert.equal(checks.reviewFixPass9.partialFailureNoticeRetained,true);
assert.equal(checks.reviewFixPass9.overlapAndRevisionNoticeRetained,true);
assert.equal(checks.reviewFixPass9.packagedDomNativeTraceReplay.actualSequences,20);
assert.equal(checks.reviewFixPass9.packagedDomNativeTraceReplay.typedNoticeDisplayed,true);
assert.equal(checks.reviewFixPass9.packagedDomNativeTraceReplay.sourceAndHistoryUnchanged,true);
assert.equal(checks.windowsWorkspaceSave,25);assert.equal(checks.linuxWorkspaceSave,26);
console.log("Verified recorded Phase4 renderer, resource, preservation and final-asset checks");
