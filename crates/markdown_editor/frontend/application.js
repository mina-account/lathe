

export function delta(before        , after        )                {
    if (before === after) return null;
    const left = Array.from(before), right = Array.from(after);
    let prefix = 0, suffix = 0;
    while (prefix < left.length && prefix < right.length && left[prefix] === right[prefix]) prefix++;
    while (suffix < left.length - prefix && suffix < right.length - prefix && left[left.length - suffix - 1] === right[right.length - suffix - 1]) suffix++;
    return { start: left.slice(0, prefix).join("").length, end: before.length - left.slice(left.length - suffix).join("").length, text: right.slice(prefix, right.length - suffix).join("") };
}
export function apply(text        , change        )         { return text.slice(0, change.start) + change.text + text.slice(change.end); }
function unchangedAnchor(before        , after        )                                                         {
    const find = (parts                                    ) => {
        const candidates = new Map                                                                                  ();
        let offset = 0;
        for (const text of parts(before)) {
            const existing = candidates.get(text);
            if (existing) existing.leftCount++;
            else if (candidates.size < 65536) candidates.set(text, { before: offset, after: 0, leftCount: 1, rightCount: 0 });
            offset += text.length;
        }
        offset = 0;
        for (const text of parts(after)) {
            const candidate = candidates.get(text);
            if (candidate) { candidate.rightCount++; candidate.after = offset; }
            offset += text.length;
        }
        const matches = [];
        for (const [text, candidate] of candidates) if (candidate.leftCount === 1 && candidate.rightCount === 1) matches.push({ before: candidate.before, after: candidate.after, text });
        return matches[Math.floor(matches.length / 2)] ?? null;
    };
    const anchor = find(text => text.match(/[^\n]*\n|[^\n]+$/g) ?? []) ?? find(text => text);
    if (!anchor) return null;
    let leftStart = anchor.before, rightStart = anchor.after, leftEnd = leftStart + anchor.text.length, rightEnd = rightStart + anchor.text.length;
    const previous = (text        , offset        ) => offset - (offset >= 2 && text.charCodeAt(offset - 1) >= 0xdc00 && text.charCodeAt(offset - 1) <= 0xdfff && text.charCodeAt(offset - 2) >= 0xd800 && text.charCodeAt(offset - 2) <= 0xdbff ? 2 : 1);
    while (leftStart > 0 && rightStart > 0) {
        const left = previous(before, leftStart), right = previous(after, rightStart);
        if (before.slice(left, leftStart) !== after.slice(right, rightStart)) break;
        leftStart = left; rightStart = right;
    }
    while (leftEnd < before.length && rightEnd < after.length && before.codePointAt(leftEnd) === after.codePointAt(rightEnd)) {
        const length = before.codePointAt(leftEnd)  > 0xffff ? 2 : 1;
        leftEnd += length; rightEnd += length;
    }
    return { before: leftStart, after: rightStart, text: before.slice(leftStart, leftEnd) };
}
export function separateChanges(before        , after        )           {
    return reconcileChanges(before, after, { remaining: 64 * 1024 * 1024 }, 0);
}
function reconcileChanges(before        , after        , budget                       , depth        )           {
    const enclosing = delta(before, after);
    if (!enclosing) return [];
    if (enclosing.start === enclosing.end || enclosing.text.length === 0) return [enclosing];
    const left = Array.from(before.slice(enclosing.start, enclosing.end)), right = Array.from(enclosing.text);

    let frontier = new Map              ();
    let remaining = Math.min(budget.remaining, 32 * 1024 * 1024);
    // Small character diffs keep fine-grained spans; larger replacements split at unchanged anchors.
    search:
    for (let distance = 0; distance <= 128; distance++) {
        const next = new Map              ();
        for (let diagonal = -distance; diagonal <= distance; diagonal += 2) {
            const deletion = frontier.get(diagonal - 1), insertion = frontier.get(diagonal + 1);
            const canDelete = deletion && deletion.left < left.length, canInsert = insertion && insertion.right < right.length;
            let path      ;
            if (distance === 0) path = { left: 0, right: 0, changes: [] };
            else {
                const remove = canDelete && (!canInsert || deletion.left >= insertion.left);
                const previous = remove ? deletion : canInsert ? insertion : null;
                if (!previous) continue;
                path = { left: previous.left, right: previous.right, changes: previous.changes.map(change => ({ ...change })) };
                const change = { start: path.left, end: path.left + (remove ? 1 : 0), insertedStart: path.right, insertedEnd: path.right + (remove ? 0 : 1) };
                const last = path.changes.at(-1);
                if (last && last.end === change.start && last.insertedEnd === change.insertedStart) { last.end = change.end; last.insertedEnd = change.insertedEnd; }
                else path.changes.push(change);
                path.left = change.end; path.right = change.insertedEnd;
            }
            while (path.left < left.length && path.right < right.length) {
                if (--remaining < 0) break search;
                budget.remaining--;
                if (left[path.left] !== right[path.right]) break;
                path.left++; path.right++;
            }
            if (path.left === left.length && path.right === right.length) {
                let leftIndex = 0, rightIndex = 0, leftOffset = enclosing.start, rightOffset = 0;
                return path.changes.map(change => {
                    while (leftIndex < change.start) leftOffset += left[leftIndex++].length;
                    const start = leftOffset;
                    while (leftIndex < change.end) leftOffset += left[leftIndex++].length;
                    while (rightIndex < change.insertedStart) rightOffset += right[rightIndex++].length;
                    const insertedStart = rightOffset;
                    while (rightIndex < change.insertedEnd) rightOffset += right[rightIndex++].length;
                    return { start, end: leftOffset, text: enclosing.text.slice(insertedStart, rightOffset) };
                });
            }
            next.set(diagonal, path);
        }
        frontier = next;
    }
    const old = before.slice(enclosing.start, enclosing.end), replacement = enclosing.text;
    budget.remaining -= 3 * (old.length + replacement.length);
    if (budget.remaining < 0) throw new Error("Concurrent change reconciliation exceeds its work limit. The visual draft is retained; use Recover Draft.");
    if (depth >= 64) return [enclosing];
    const anchor = unchangedAnchor(old, replacement);
    if (!anchor) return [enclosing];
    const earlier = reconcileChanges(old.slice(0, anchor.before), replacement.slice(0, anchor.after), budget, depth + 1);
    const later = reconcileChanges(old.slice(anchor.before + anchor.text.length), replacement.slice(anchor.after + anchor.text.length), budget, depth + 1);
    return [...earlier.map(change => ({ ...change, start: change.start + enclosing.start, end: change.end + enclosing.start })),
        ...later.map(change => ({ ...change, start: change.start + enclosing.start + anchor.before + anchor.text.length, end: change.end + enclosing.start + anchor.before + anchor.text.length }))];
}
export function rebase(base        , current        , draft        )         {
    const local = delta(base, draft), external = delta(base, current);
    if (!local) return current;
    if (!external) return draft;
    if (external.end < local.start || external.end === local.start && external.start !== local.start) {
        const distance = external.text.length - (external.end - external.start);
        return apply(current, { ...local, start: local.start + distance, end: local.end + distance });
    }
    if (local.end < external.start || local.end === external.start && local.start !== external.start) return apply(current, local);
    const externalChanges = separateChanges(base, current);
    const localChanges = separateChanges(base, draft).map(local => {
        let distance = 0;
        for (const change of externalChanges) {
            if (change.end < local.start || change.end === local.start && change.start !== local.start) distance += change.text.length - (change.end - change.start);
            else if (!(local.end < change.start || local.end === change.start && local.start !== change.start)) throw new Error("Concurrent edits overlap. The visual draft is retained; use Recover Draft.");
        }
        return { ...local, start: local.start + distance, end: local.end + distance };
    });
    for (const change of localChanges.reverse()) current = apply(current, change);
    return current;
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

export class SourceProjection {
    source = ""; canonical = "";
    normalization                                                                = [];
    reset(source        , canonical        ) {
        this.source = source; this.canonical = canonical;
        let distance = 0;
        this.normalization = separateChanges(display(source), canonical).map(change => {
            const canonicalStart = change.start + distance;
            distance += change.text.length - (change.end - change.start);
            return {...change,canonicalStart,canonicalEnd:canonicalStart + change.text.length};
        });
    }
    sourceOffset(offset        , edge                  = "start")         {
        let distance = 0, shown = offset;
        for (const span of this.normalization) {
            if (offset < span.canonicalStart) break;
            if (offset < span.canonicalEnd) {
                shown = edge === "start" ? span.start : span.end;
                return this.rawOffset(shown);
            }
            distance += span.text.length - (span.end - span.start);
        }
        return this.rawOffset(offset - distance);
    }
    rawOffset(offset        )         {
        let raw = 0, shown = 0;
        while (shown < offset && raw < this.source.length) {
            if (this.source[raw] === "\r" && this.source[raw + 1] === "\n") raw++;
            raw++; shown++;
        }
        return raw;
    }
    edit(canonical        )         {
        if (canonical === this.canonical) return this.source;
        const changes = separateChanges(this.canonical, canonical).map(change => {
            const start = this.sourceOffset(change.start);
            const end = change.start === change.end ? start : this.sourceOffset(change.end, "end");
            return {start,end,text:change.text.replace(/\n/g, this.source.includes("\r\n") ? "\r\n" : "\n")};
        });
        for (let index = 1; index < changes.length; index++) {
            if (changes[index].start < changes[index - 1].end) throw new Error("This syntax cannot be mapped safely. Edit its source block instead.");
        }
        let source = this.source;
        for (const change of changes.reverse()) source = apply(source, change);
        this.reset(source, canonical);
        return source;
    }
}

export class DocumentBridge {
    session        ; document        ; revision = 0; sequence = 0; request = 0; acknowledgedSequence = 0;
    base = ""; draft = ""; pending                 = null; composing = false; blocked = false; writable = true;
    flushes           = []; afterFlush                 = [];
    deferredDocument      = null; deferredAccepted      = null; reconciling = false; initialized = false;
    historyQueue                      = []; historyPending                = null;
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
        if (this.pending || this.historyPending || this.composing || this.reconciling || this.blocked) return;
        const difference = delta(this.base, this.draft);
        if (!this.writable && difference) return;
        const change = difference ?? { start: 0, end: 0, text: "" };
        if (this.sequence === this.acknowledgedSequence) { this.finishFlush(); return; }
        this.pending = { request: ++this.request, sequence: this.sequence, before: this.base, after: this.draft };
        this.emit({ kind: "edit", request: this.request, sequence: this.sequence, base_revision: this.revision, change });
    }
    receive(message     ) {
        if (message.kind === "notice") { this.status(message.text); return; }
        if (message.kind === "history_accepted") {
            if (message.request !== this.historyPending) return;
            this.historyPending = null;
            this.receive({kind:"document",revision:message.revision,text:message.text,writable:message.writable,draft:null});
            this.submit(); this.finishFlush(); return;
        }
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
            this.acknowledgedSequence = pending.sequence;
            if (this.draft !== this.base || this.sequence > this.acknowledgedSequence) { this.sequence++; this.emit({ kind: "draft", sequence: this.sequence, base_revision: this.revision, text: this.draft }); }
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
            else if (this.draft !== this.base || this.sequence > this.acknowledgedSequence) { this.sequence++; this.emit({ kind: "draft", sequence: this.sequence, base_revision: this.revision, text: this.draft }); }
            if (!withinBudget(this.base)) this.status("This document exceeds the visual editing limit. Open Source to edit it.");
            this.show(this.draft);
        }
    }
    finishFlush() {
        if (this.pending || this.historyPending || this.composing || this.reconciling || this.blocked || this.draft !== this.base || this.sequence !== this.acknowledgedSequence) return;
        const history = this.historyQueue.shift();
        if (history) {
            this.historyPending = ++this.request;
            this.emit({kind:history,request:this.historyPending,base_revision:this.revision}); return;
        }
        for (const request of this.flushes.splice(0)) this.emit({ kind: "flushed", request, sequence: this.sequence });
        for (const callback of this.afterFlush.splice(0)) callback();
    }
    history(kind                 ) {
        if (!this.writable) return;
        this.historyQueue.push(kind); this.submit(); this.finishFlush();
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
