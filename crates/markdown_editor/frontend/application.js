

export function delta(before        , after        )                {
    if (before === after) return null;
    const left = Array.from(before), right = Array.from(after);
    let prefix = 0, suffix = 0;
    while (prefix < left.length && prefix < right.length && left[prefix] === right[prefix]) prefix++;
    while (suffix < left.length - prefix && suffix < right.length - prefix && left[left.length - suffix - 1] === right[right.length - suffix - 1]) suffix++;
    return { start: left.slice(0, prefix).join("").length, end: before.length - left.slice(left.length - suffix).join("").length, text: right.slice(prefix, right.length - suffix).join("") };
}
export function apply(text        , change        )         { return text.slice(0, change.start) + change.text + text.slice(change.end); }
export function rebase(base        , current        , draft        )         {
    const local = delta(base, draft), external = delta(base, current);
    if (!local) return current;
    if (!external) return draft;
    if (external.end < local.start || external.end === local.start && external.start !== local.start) {
        const distance = external.text.length - (external.end - external.start);
        return apply(current, { ...local, start: local.start + distance, end: local.end + distance });
    }
    if (local.end < external.start || local.end === external.start && local.start !== external.start) return apply(current, local);
    throw new Error("Concurrent edits overlap. The visual draft is retained; use Recover Draft.");
}
export function display(text        )         { return text.replace(/\r\n|\r/g, "\n"); }
export function transformOffset(offset        , change               )         {
    if (!change || offset < change.start) return offset;
    if (offset >= change.end) return offset + change.text.length - (change.end - change.start);
    return change.start + change.text.length;
}
const maximumDocumentBytes = 8 * 1024 * 1024;
export function withinBudget(text        )          {
    const encoder = new TextEncoder();
    return encoder.encode(text).length <= maximumDocumentBytes && encoder.encode(JSON.stringify(text)).length <= maximumDocumentBytes;
}
export function fromDisplay(original        , changed        )         {
    const change = delta(display(original), changed);
    if (!change) return original;
    const rawOffset = (offset        )         => {
        let raw = 0, shown = 0;
        while (shown < offset && raw < original.length) {
            if (original[raw] === "\r" && original[raw + 1] === "\n") raw++;
            raw++; shown++;
        }
        return raw;
    };
    return apply(original, { start: rawOffset(change.start), end: rawOffset(change.end), text: change.text.replace(/\n/g, original.includes("\r\n") ? "\r\n" : "\n") });
}

export class DocumentBridge {
    session        ; document        ; revision = 0; sequence = 0; request = 0;
    base = ""; draft = ""; pending                 = null; composing = false; blocked = false; writable = true;
    flushes           = []; afterFlush                 = [];
    deferredDocument      = null; deferredAccepted      = null; reconciling = false; initialized = false;
    send                                            ;
    show                        ;
    status                        ;
    constructor(session        , document        , send                                            , show                        , status                        ) {
        this.session = session; this.document = document; this.send = send; this.show = show; this.status = status;
    }
    emit(message                         ) { this.send({ ...message, session: this.session, document: this.document }); }
    input(text        ) {
        if (text === this.draft) { this.finishFlush(); return; }
        this.draft = text; this.sequence++;
        this.emit({ kind: "draft", sequence: this.sequence, base_revision: this.revision, text });
        if (!withinBudget(text)) {
            this.blocked = true; this.status("Input exceeds the visual document limit and is retained for Recover Draft."); this.show(this.draft); return;
        }
        this.submit();
    }
    submit() {
        if (this.pending || this.composing || this.reconciling || this.blocked || !this.writable) return;
        const change = delta(this.base, this.draft);
        if (!change) { this.finishFlush(); return; }
        this.pending = { request: ++this.request, sequence: this.sequence, before: this.base, after: this.draft };
        this.emit({ kind: "edit", request: this.request, sequence: this.sequence, base_revision: this.revision, change });
    }
    receive(message     ) {
        if (this.composing && (message.kind === "accepted" || message.kind === "document") || this.pending && message.kind === "document") {
            if (message.kind === "document") {
                if (!this.deferredDocument || message.revision >= this.deferredDocument.revision) this.deferredDocument = message;
            } else if (this.pending && message.request === this.pending.request && message.sequence === this.pending.sequence) {
                this.deferredAccepted = message;
            }
            return;
        }
        if (message.kind === "flush") { this.flushes.push(message.request); this.submit(); this.finishFlush(); return; }
        if (message.kind === "conflict") {
            this.pending = null; this.blocked = true;
            this.status(message.reason + " — visual draft retained; use Recover Draft."); return;
        }
        if (message.kind === "accepted") {
            if (!this.pending || this.pending.request !== message.request || this.pending.sequence !== message.sequence) return;
            const pending = this.pending;
            this.pending = null;
            try { this.draft = rebase(pending.after, message.text, this.draft); }
            catch (error) { this.blocked = true; this.status(String(error)); return; }
            this.base = message.text; this.revision = message.revision;
            if (this.draft !== this.base) { this.sequence++; this.emit({ kind: "draft", sequence: this.sequence, base_revision: this.revision, text: this.draft }); }
            if (!this.composing) this.show(this.draft);
            this.reconcile(); this.submit(); this.finishFlush(); return;
        }
        if (message.kind === "document") {
            if (message.revision < this.revision) return;
            this.writable = message.writable;
            if (this.blocked) return;
            const recovered = !this.initialized && message.draft !== null && message.draft !== undefined;
            try { this.draft = recovered ? message.draft : rebase(this.base, message.text, this.draft); }
            catch (error) { this.blocked = true; this.status(String(error)); return; }
            this.base = message.text; this.revision = message.revision; this.initialized = true;
            if (recovered) { this.blocked = true; this.status("Recovered visual draft. Use Recover Draft to preserve or resolve it."); }
            else if (this.draft !== this.base) { this.sequence++; this.emit({ kind: "draft", sequence: this.sequence, base_revision: this.revision, text: this.draft }); }
            if (!withinBudget(this.base)) this.status("This document exceeds the visual editing limit. Open Source to edit it.");
            this.show(this.draft);
        }
    }
    finishFlush() {
        if (this.pending || this.composing || this.reconciling || this.blocked || this.draft !== this.base) return;
        for (const request of this.flushes.splice(0)) this.emit({ kind: "flushed", request, sequence: this.sequence });
        for (const callback of this.afterFlush.splice(0)) callback();
    }
    history(kind                 ) {
        this.afterFlush.push(() => this.emit({ kind, request: ++this.request, base_revision: this.revision })); this.submit(); this.finishFlush();
    }
    reconcile() {
        if (this.composing || this.reconciling) return;
        this.reconciling = true;
        const accepted = this.deferredAccepted, document = this.deferredDocument;
        this.deferredAccepted = null; this.deferredDocument = null;
        if (accepted) this.receive(accepted);
        if (document && document.revision >= this.revision) this.receive(document);
        this.reconciling = false;
    }
    compositionEnd() { this.composing = false; this.reconcile(); this.submit(); this.finishFlush(); }
}

if (typeof window !== "undefined") {
    const element = document.getElementById("document")                       ;
    const status = document.getElementById("status")               ;
    const parameters = new URL(location.href).searchParams;
    let projection = "";
    const bridge = new DocumentBridge(parameters.get("session") ?? "", parameters.get("document") ?? "", message => console.log("LATHE_MARKDOWN:" + JSON.stringify(message)), text => {
        const start = element.selectionStart, end = element.selectionEnd, direction = element.selectionDirection, scroll = element.scrollTop;
        const change = delta(element.value, display(text));
        projection = text; element.value = display(text); element.readOnly = !bridge.writable || bridge.blocked;
        element.setSelectionRange(transformOffset(start, change), transformOffset(end, change), direction); element.scrollTop = scroll;
    }, text => { status.textContent = text; });
    (window       ).latheMarkdown = bridge;
    element.addEventListener("input", () => { projection = fromDisplay(projection, element.value); bridge.input(projection); });
    element.addEventListener("compositionstart", () => { bridge.composing = true; });
    element.addEventListener("compositionend", () => bridge.compositionEnd());
    element.addEventListener("select", () => bridge.emit({ kind: "selection", start: element.selectionStart, end: element.selectionEnd }));
    const allowInsertion = (text        )          => {
        const candidate = element.value.slice(0, element.selectionStart) + text + element.value.slice(element.selectionEnd);
        if (withinBudget(fromDisplay(projection, candidate))) return true;
        status.textContent = "This edit exceeds the visual document limit. Use the source editor for larger documents."; return false;
    };
    element.addEventListener("paste", event => {
        const text = event.clipboardData?.getData("text/plain");
        if (text !== undefined && !allowInsertion(text)) event.preventDefault();
    });
    element.addEventListener("drop", event => {
        const text = event.dataTransfer?.getData("text/plain");
        if (text !== undefined && !allowInsertion(text)) event.preventDefault();
    });
    element.addEventListener("beforeinput", event => {
        if (event.inputType === "historyUndo" || event.inputType === "historyRedo") { event.preventDefault(); bridge.history(event.inputType === "historyUndo" ? "undo" : "redo"); }
        else {
            const text = event.data ?? event.dataTransfer?.getData("text/plain") ?? (event.inputType === "insertLineBreak" || event.inputType === "insertParagraph" ? "\n" : null);
            if (text !== null && !allowInsertion(text)) event.preventDefault();
        }
    });
    element.addEventListener("keydown", event => {
        if ((event.ctrlKey || event.metaKey) && (event.key.toLowerCase() === "z" || event.key.toLowerCase() === "y")) {
            event.preventDefault(); bridge.history(event.shiftKey || event.key.toLowerCase() === "y" ? "redo" : "undo");
        }
    });
    bridge.emit({ kind: "ready" });
    status.textContent = "Markdown Editor";
}
