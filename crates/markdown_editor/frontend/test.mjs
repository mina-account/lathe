import { test } from "node:test";
import assert from "node:assert/strict";
import { DocumentBridge, delta, display, fromDisplay, rebase } from "./application.ts";

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
