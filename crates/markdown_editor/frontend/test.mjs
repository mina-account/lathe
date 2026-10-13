import { test } from "node:test";
import assert from "node:assert/strict";
import { DocumentBridge, SourceProjection, delta, display, fromDisplay, rebase, separateChanges, withinBudget } from "./application.ts";

test("projection boundaries exclude adjacent normalized source from replacements", () => {
    for (const newline of ["\n","\r\n"]) for (const [source,canonical,target] of [
        ["<https://example.com/target>","https://example.com/target","target"],
        ["<target@example.com>","target@example.com","com"],
        ["first  \nnext","first\nnext","first"],
        ["__target__","**target**","target"],
        ["before &amp; target","before & target","before"],
        ["target\n=======","# target","target"],
    ]) {
        const original=source.replace(/\n/g,newline), projection=new SourceProjection();
        projection.reset(original,canonical);
        assert.equal(projection.edit(canonical.replace(target,"CHANGED")),original.replace(target,"CHANGED"),source);
    }
    const projection=new SourceProjection(); projection.reset("target\n=======","target\n======");
    assert.equal(projection.edit("target\n------"),"target\n------");
});

test("rapid toolbar history commands wait for their own acknowledgment and revision", () => {
    const messages = [];
    const bridge = new DocumentBridge("session", "1", message => messages.push(message), () => {}, () => {});
    bridge.receive({kind:"document",revision:0,text:"a",writable:true,draft:null});
    bridge.input("ab"); bridge.history("undo"); bridge.history("undo"); bridge.history("redo");
    bridge.receive({kind:"accepted",request:1,sequence:1,revision:1,text:"ab"});
    assert.equal(messages.filter(message => ["undo","redo"].includes(message.kind)).length, 1);
    bridge.receive({kind:"history_accepted",request:2,revision:2,text:"a",writable:true});
    assert.deepEqual(messages.at(-1), {kind:"undo",request:3,base_revision:2,session:"session",document:"1"});
    bridge.receive({kind:"history_accepted",request:2,revision:2,text:"a",writable:true});
    assert.equal(messages.at(-1).request, 3);
    bridge.receive({kind:"history_accepted",request:3,revision:2,text:"a",writable:true});
    assert.equal(messages.at(-1).kind, "redo"); assert.equal(messages.at(-1).base_revision, 2);
    bridge.receive({kind:"flush",request:90});
    assert.equal(messages.some(message => message.kind === "flushed"), false);
    bridge.receive({kind:"history_accepted",request:4,revision:3,text:"ab",writable:true});
    assert.equal(messages.at(-1).kind, "flushed");
});

test("UTF-16 changes preserve code points and untouched CRLF source", () => {
    assert.deepEqual(delta("😀a", "😁a"), {start:0,end:2,text:"😁"});
    const source = "---\r\nx: 1\r\n---\r\n[ref]: /a\r\n<script>evil()</script>\r\n```unknown\r\n😀\r\n```";
    assert.equal(fromDisplay(source, display(source)), source);
    assert.equal(fromDisplay(source, display(source).replace("x: 1", "x: 2")), source.replace("x: 1", "x: 2"));
    assert.equal(rebase("a world", "prefix a world", "a WORLD"), "prefix a WORLD");
    assert.throws(() => rebase("world", "other", "WORLD"));
});

test("one pending edit, local queue, exact acknowledgments, native undo, flush", () => {
    const messages = [], shown = [];
    const bridge = new DocumentBridge("session", "1", message => messages.push(message), text => shown.push(text), () => {});
    bridge.receive({kind:"document",revision:0,text:"a",writable:true,draft:null});
    bridge.input("ab"); bridge.input("abc");
    assert.equal(messages.filter(message => message.kind === "edit").length, 1);
    bridge.receive({kind:"accepted",request:99,sequence:1,revision:1,text:"wrong"});
    assert.equal(bridge.pending.request, 1);
    bridge.receive({kind:"flush",request:7});
    assert.equal(messages.some(message => message.kind === "flushed"), false);
    bridge.receive({kind:"accepted",request:1,sequence:1,revision:1,text:"ab"});
    assert.equal(messages.filter(message => message.kind === "edit").length, 2);
    bridge.receive({kind:"accepted",request:2,sequence:3,revision:2,text:"abc"});
    assert.equal(messages.at(-1).kind, "flushed");
    bridge.history("undo"); assert.equal(messages.at(-1).kind, "undo");
    assert.equal(bridge.draft, "abc");
});

test("queued input returning to committed text waits for its own validated acknowledgment", () => {
    const messages = [];
    const bridge = new DocumentBridge("session", "1", message => messages.push(message), () => {}, () => {});
    bridge.receive({kind:"document",revision:0,text:"a",writable:true,draft:null});
    bridge.input("ab"); bridge.input("abc"); bridge.input("ab");
    bridge.receive({kind:"flush",request:7}); bridge.history("undo");
    bridge.receive({kind:"accepted",request:1,sequence:1,revision:1,text:"ab"});
    assert.equal(messages.some(message => message.kind === "flushed" || message.kind === "undo"), false);
    const edit = messages.filter(message => message.kind === "edit").at(-1);
    assert.equal(edit.request, 2); assert.equal(edit.base_revision, 1);
    assert.deepEqual(edit.change, {start:0,end:0,text:""});
    const draft = messages.filter(message => message.kind === "draft").at(-1);
    assert.equal(draft.sequence, edit.sequence); assert.equal(draft.text, "ab");
    bridge.receive({kind:"accepted",request:2,sequence:1,revision:1,text:"ab"});
    assert.equal(messages.some(message => message.kind === "flushed"), false);
    bridge.receive({kind:"accepted",request:edit.request,sequence:edit.sequence,revision:1,text:"ab"});
    assert.equal(messages.at(-1).kind, "undo");
    bridge.receive({kind:"history_accepted",request:messages.at(-1).request,revision:2,text:"a",writable:true});
    assert.equal(messages.at(-1).kind, "flushed");
});

test("cancelled composition acknowledges its no-op draft before flush and history", () => {
    const messages = [];
    const bridge = new DocumentBridge("session", "1", message => messages.push(message), () => {}, () => {});
    bridge.receive({kind:"document",revision:0,text:"a",writable:true,draft:null});
    bridge.composing = true; bridge.input("ab"); bridge.input("a");
    bridge.receive({kind:"flush",request:7}); bridge.history("redo"); bridge.compositionEnd();
    assert.equal(messages.some(message => message.kind === "flushed" || message.kind === "redo"), false);
    const edit = messages.filter(message => message.kind === "edit").at(-1);
    assert.deepEqual(edit.change, {start:0,end:0,text:""}); assert.equal(edit.sequence, 2);
    bridge.receive({kind:"accepted",request:edit.request,sequence:edit.sequence,revision:0,text:"a"});
    assert.equal(messages.at(-1).kind, "redo");
    bridge.receive({kind:"history_accepted",request:messages.at(-1).request,revision:0,text:"a",writable:true});
    assert.equal(messages.at(-1).kind, "flushed");
});

test("separate external edits rebase around local edits with Unicode and source bytes preserved", () => {
    assert.equal(rebase("0123456789", "X12345678Y", "01234L6789"), "X1234L678Y");
    assert.equal(rebase("0123456789", "01234L6789", "X12345678Y"), "X1234L678Y");
    assert.equal(rebase("0123456789", "prefix 012345678 suffix", "01234L6789"), "prefix 01234L678 suffix");
    const base = "😀\r\n0123456789\r\n[ref]: /a";
    assert.equal(rebase(base, base.replace("😀", "😁😁").replace("/a", "/long"), base.replace("5", "L")), "😁😁\r\n01234L6789\r\n[ref]: /long");
    assert.throws(() => rebase("0123456789", "X1234R678Y", "01234L6789"), /overlap/);
    assert.throws(() => rebase("0123456789", "01234X56789", "01234L56789"), /overlap/);
});

test("bounded reconciliation supports sparse changes in an 8 MiB document and retains divergent drafts", () => {
    const base = "0" + "a".repeat(4*1024*1024-1) + "5" + "b".repeat(4*1024*1024-2) + "9";
    const current = "X" + base.slice(1,-1) + "Y";
    assert.equal(rebase(base, current, base.slice(0,4*1024*1024) + "L" + base.slice(4*1024*1024+1)), current.slice(0,4*1024*1024) + "L" + current.slice(4*1024*1024+1));
    const messages = [], errors = [];
    const bridge = new DocumentBridge("session", "1", message => messages.push(message), () => {}, error => errors.push(error));
    bridge.receive({kind:"document",revision:0,text:"a".repeat(256),writable:true,draft:null});
    bridge.composing = true; bridge.input("a".repeat(128) + "L" + "a".repeat(127));
    bridge.receive({kind:"document",revision:1,text:"b".repeat(256),writable:true,draft:null});
    bridge.receive({kind:"flush",request:7}); bridge.compositionEnd();
    assert.equal(bridge.blocked, true); assert.equal(bridge.draft, "a".repeat(128) + "L" + "a".repeat(127));
    assert.match(errors.at(-1), /overlap/); assert.equal(messages.some(message => message.kind === "flushed"), false);
});

test("large disjoint replacements use unchanged anchors in both change sets", () => {
    const base = "A".repeat(200) + "0123456789" + "B".repeat(200);
    const sides = "C".repeat(200) + "0123456789" + "D".repeat(200);
    const middle = base.replace("5", "L"), result = sides.replace("5", "L");
    assert.equal(rebase(base, sides, middle), result);
    assert.equal(rebase(base, middle, sides), result);
    assert.throws(() => rebase(base, sides.replace("5", "R"), middle), /overlap/);
    const paragraph = "the unchanged paragraph contains ordinary repeated letters\n";
    const lines = "first ".repeat(200) + "\n" + paragraph + "last ".repeat(200);
    const newLines = "external first ".repeat(200) + "\n" + paragraph + "external last ".repeat(200);
    assert.equal(rebase(lines, newLines, lines.replace("ordinary", "local")), newLines.replace("ordinary", "local"));
});

test("cancelled composition settles after a read-only projection without permitting edits", () => {
    const messages = [];
    const bridge = new DocumentBridge("session", "1", message => messages.push(message), () => {}, () => {});
    bridge.receive({kind:"document",revision:0,text:"a",writable:true,draft:null});
    bridge.composing = true; bridge.input("ab"); bridge.input("a");
    bridge.receive({kind:"document",revision:0,text:"a",writable:false,draft:null});
    bridge.receive({kind:"flush",request:7}); bridge.compositionEnd();
    const edit = messages.filter(message => message.kind === "edit").at(-1);
    assert(edit, "read-only no-op input must receive a validated acknowledgment");
    assert.deepEqual(edit.change, {start:0,end:0,text:""}); assert.equal(bridge.writable, false);
    bridge.receive({kind:"accepted",request:edit.request,sequence:edit.sequence,revision:0,text:"a"});
    assert.equal(messages.at(-1).kind, "flushed");
    bridge.input("ab"); assert.equal(messages.at(-1).kind, "draft");
    assert.equal(messages.filter(message => message.kind === "edit").length, 1);
});

test("separate Unicode changes reconstruct their target without touching matching spans", () => {
    const texts = ["", "a", "b", "ab", "ba", "aba", "bab", "😀a😀", "a\r\nb"];
    for (const before of texts) for (const after of texts) {
        let reconstructed = before;
        for (const change of separateChanges(before, after).reverse()) reconstructed = reconstructed.slice(0,change.start) + change.text + reconstructed.slice(change.end);
        assert.equal(reconstructed, after);
    }
});

test("composition and conflict retain drafts and suppress host replacement", () => {
    const messages = [], shown = [], errors = [];
    const bridge = new DocumentBridge("session", "1", message => messages.push(message), text => shown.push(text), text => errors.push(text));
    bridge.receive({kind:"document",revision:0,text:"base",writable:true,draft:null});
    bridge.composing = true; bridge.input("draft");
    bridge.receive({kind:"document",revision:1,text:"other",writable:true,draft:null});
    assert.deepEqual(shown, ["base"]);
    assert.equal(messages.some(message => message.kind === "edit"), false);
    bridge.compositionEnd();
    bridge.receive({kind:"conflict",request:1,revision:1,text:"other",draft:"draft",reason:"overlap"});
    bridge.receive({kind:"flush",request:1});
    assert.equal(bridge.draft, "draft");
    assert.equal(messages.some(message => message.kind === "flushed"), false);
    assert.match(errors.at(-1), /retained/);
});

test("programmatic/no-op input sends no draft and queued rebase gets a new sequence", () => {
    const messages = [];
    const bridge = new DocumentBridge("session", "1", message => messages.push(message), () => {}, () => {});
    bridge.receive({kind:"document",revision:0,text:"a",writable:true,draft:null});
    bridge.input("a");
    assert.equal(messages.length, 0);
    bridge.input("ab"); bridge.input("abc");
    bridge.receive({kind:"accepted",request:1,sequence:1,revision:1,text:"prefix ab"});
    const drafts = messages.filter(message => message.kind === "draft");
    assert.equal(drafts.at(-1).text, "prefix abc");
    assert.equal(drafts.at(-1).sequence, 3);
});


test("document identities above JavaScript integer precision remain opaque strings", () => {
    const identifier = "9007199254740993";
    const url = new URL(`https://lathe-markdown.invalid/index.html?session=session&document=${identifier}`);
    const messages = [];
    const bridge = new DocumentBridge(url.searchParams.get("session"), url.searchParams.get("document"), message => messages.push(JSON.stringify(message)), () => {}, () => {});
    bridge.emit({kind:"ready"});
    assert.equal(JSON.parse(messages[0]).document, identifier);
    bridge.receive({kind:"document",revision:0,text:"base",writable:true,draft:null});
    bridge.input("base visual");
    assert(messages.every(message => JSON.parse(message).document === identifier));
});

test("acknowledgments and projections wait for composition before rebasing", () => {
    const messages = [], shown = [];
    const bridge = new DocumentBridge("session", "1", message => messages.push(message), text => shown.push(text), () => {});
    bridge.receive({kind:"document",revision:0,text:"a",writable:true,draft:null});
    bridge.input("ab"); bridge.composing = true; bridge.input("abc");
    bridge.receive({kind:"accepted",request:1,sequence:1,revision:2,text:"prefix ab"});
    assert.equal(bridge.base, "a");
    bridge.input("abcd"); bridge.compositionEnd();
    assert.equal(bridge.draft, "prefix abcd");
    assert.equal(shown.at(-1), "prefix abcd");
    assert.deepEqual(messages.filter(message => message.kind === "edit").at(-1).change, {start:9,end:9,text:"cd"});
    const other = new DocumentBridge("session", "2", () => {}, () => {}, () => {});
    other.receive({kind:"document",revision:0,text:"hello",writable:true,draft:null});
    other.composing = true; other.input("hello world");
    other.receive({kind:"document",revision:1,text:"prefix hello",writable:true,draft:null});
    other.compositionEnd();
    assert.equal(other.draft, "prefix hello world");
    assert.equal(other.revision, 1);
    const draft = messages.filter(message => message.kind === "draft").at(-1);
    const edit = messages.filter(message => message.kind === "edit").at(-1);
    assert.equal(draft.sequence, edit.sequence); assert.equal(draft.base_revision, edit.base_revision);
});

test("composition coalesces projections and replays same-revision capability changes", () => {
    const bridge = new DocumentBridge("session", "1", () => {}, () => {}, () => {});
    bridge.receive({kind:"document",revision:0,text:"hello",writable:true,draft:null});
    bridge.composing = true;
    for (let revision=1; revision<=1000; revision++) bridge.receive({kind:"document",revision,text:`prefix ${revision} hello`,writable:true,draft:null});
    bridge.receive({kind:"document",revision:1000,text:"prefix 1000 hello",writable:false,draft:null});
    assert.equal(bridge.deferredDocument.revision, 1000);
    bridge.compositionEnd();
    assert.equal(bridge.base, "prefix 1000 hello"); assert.equal(bridge.writable, false);
    bridge.composing = true;
    bridge.receive({kind:"document",revision:1000,text:bridge.base,writable:true,draft:null});
    bridge.compositionEnd(); assert.equal(bridge.writable, true);
    bridge.receive({kind:"document",revision:999,text:"stale",writable:false,draft:null});
    assert.equal(bridge.base, "prefix 1000 hello"); assert.equal(bridge.writable, true);
});

test("budget counts UTF-8 and JSON escaping and retains noncancelable overflow", () => {
    assert.equal(withinBudget("😀".repeat(3*1024*1024)), false);
    assert.equal(withinBudget("\u0000".repeat(2*1024*1024)), false);
    const messages = [], errors = [];
    const bridge = new DocumentBridge("session", "1", message => messages.push(message), () => {}, text => errors.push(text));
    bridge.receive({kind:"document",revision:0,text:"base",writable:true,draft:null});
    const overflow = "x".repeat(8*1024*1024+1);
    bridge.input(overflow);
    assert.equal(messages.at(-1).kind, "draft"); assert.equal(messages.at(-1).text, overflow);
    assert.equal(bridge.blocked, true); assert.match(errors.at(-1), /retained/);
});
