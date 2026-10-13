import { DocumentBridge, SourceProjection, display, fromDisplay, separateChanges, transformOffset, withinBudget } from "./application.js";

declare const Vditor: any;
declare const Lute: any;
declare const DOMPurify: any;
type Mode = "ir" | "wysiwyg" | "sv" | "preview";
type Preferences = {mode:Mode;outline:boolean;wrap:boolean;scroll:number};
const defaults = (): Preferences => ({mode:"ir",outline:false,wrap:true,scroll:0});
const applicationControls = new Map([...document.querySelectorAll<HTMLElement>("[id]")].map(element=>[element.id,element]));
const get = (id: string) => applicationControls.get(id)!;
const status = get("status");
const container = get("document"), viewer = get("viewer"), outline = get("outline");
const parameters = new URL(location.href).searchParams;
let editor: any = null, generation = 0, projecting = false, ready = false, preferences = defaults();
let latest = "", sourceEditor: HTMLTextAreaElement | null = null;
let lastSerialized = "";
let sourceEditorSpan: {start:number;end:number} | null = null;
type Intent = {start:number;end:number;previous:string|null;next:string|null;previousOccurrence?:number;nextOccurrence?:number;sourceView:boolean};
let intent: Intent | null = null;
const projection = new SourceProjection();
const forbiddenTags = ["script","style","iframe","object","embed","svg","math","template","form","input","button","select","textarea","audio","video","source","base","meta","link"];
DOMPurify.addHook("uponSanitizeAttribute", (_node:Element, attribute:any) => {
    if (attribute.attrName === "class") attribute.attrValue = attribute.attrValue.replace(/\blanguage-(?:math|mermaid|graphviz|echarts|abc|flowchart|plantuml|markmap|mindmap|smiles)\b/g,"language-text");
});
DOMPurify.addHook("afterSanitizeAttributes", (node:Element, _data:any, configuration:any) => {
    if (node.tagName !== "INPUT") return;
    if (node.getAttribute("type")?.toLowerCase() !== "checkbox") node.remove();
    else if (!configuration.ALLOW_DATA_ATTR) node.setAttribute("disabled","");
});
export function sanitize(html: string, editing = false): string {
    return DOMPurify.sanitize(html, {
        FORBID_TAGS: forbiddenTags.filter(tag => tag !== "input"),
        FORBID_ATTR:["style","srcset","formaction","autofocus","name"],
        ADD_TAGS:editing ? ["wbr"] : [], ALLOW_DATA_ATTR:editing,
        ALLOWED_URI_REGEXP:/^(?:https?:|mailto:|#|[^a-zA-Z:\\/]|[a-zA-Z][^:]*$)/i,
    });
}

// Lute returns HTML before Vditor assigns it to live DOM. Sanitize at that boundary,
// including transformations during typing, rather than trying to clean after insertion.
const newLute = Lute.New;
Lute.New = (...arguments_: any[]) => {
    const lute = newLute(...arguments_);
    for (const method of ["Md2VditorIRDOM","Md2VditorDOM","SpinVditorIRDOM","SpinVditorDOM","SpinVditorSVDOM","HTML2VditorDOM","HTML2VditorIRDOM","Md2HTML","VditorDOM2HTML","VditorIRDOM2HTML"]) {
        if (typeof lute[method] !== "function") continue;
        const original = lute[method];
        lute[method] = (...parameters: any[]) => sanitize(original(...parameters), !method.endsWith("HTML"));
    }
    return lute;
};

const bridge = new DocumentBridge(parameters.get("session") ?? "", parameters.get("document") ?? "",
    message => console.log("LATHE_MARKDOWN:" + JSON.stringify(message)), show, text => { status.textContent = text; });
(window as any).latheMarkdown = bridge;
const receive = bridge.receive.bind(bridge);
bridge.receive = message => {
    if (["flush","document","accepted","preferences"].includes(message.kind)) commit();
    if (message.kind === "preferences") {
        if (bridge.blocked) return;
        preferences = {...defaults(),...message.preferences};
        if (ready && !bridge.composing) build();
        return;
    }
    receive(message);
    permissions();
};

function active(): HTMLElement { return preferences.mode === "preview" ? viewer : editor?.vditor[editor.getCurrentMode()].element ?? container; }
function scrollElement(): HTMLElement { return preferences.mode === "preview" ? viewer : active(); }
function permissions() {
    if (!ready) return;
    const writable = bridge.writable && !bridge.blocked && preferences.mode !== "preview";
    if (writable) editor.enable(); else editor.disabled();
    if (sourceEditor) sourceEditor.readOnly = !writable;
    for (const name of ["native-undo","native-redo"]) {
        const button = container.querySelector(`[data-type="${name}"]`) as HTMLButtonElement | null;
        if (button) button.disabled = !writable;
    }
}
function textNodes(root: HTMLElement): Text[] {
    const result: Text[] = [], walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
    while (walker.nextNode()) result.push(walker.currentNode as Text);
    return result;
}
function selection() {
    const selected = window.getSelection();
    if (!selected?.rangeCount || !active().contains(selected.anchorNode)) return null;
    const range = selected.getRangeAt(0), prefix = range.cloneRange();
    prefix.selectNodeContents(active()); prefix.setEnd(range.startContainer, range.startOffset);
    const start = prefix.toString().length;
    return {start,end:start+range.toString().length,text:range.toString(),backward:selected.anchorNode === range.endContainer && selected.anchorOffset === range.endOffset};
}
function restoreSelection(selected: ReturnType<typeof selection>, oldText: string) {
    if (!selected) return;
    const root = active(), text = root.textContent ?? "";
    let {start,end} = selected;
    if (preferences.mode !== "preview") {
        for (const change of separateChanges(oldText,text).reverse()) {
            start = transformOffset(start, change); end = transformOffset(end, change);
        }
    }
    if (selected.text && text.slice(start,end) !== selected.text) {
        const found = text.indexOf(selected.text);
        if (found >= 0) { start = found; end = found + selected.text.length; }
    }
    const point = (offset: number): [Node,number] => {
        for (const node of textNodes(root)) {
            if (offset <= node.length) return [node,offset];
            offset -= node.length;
        }
        return [root,root.childNodes.length];
    };
    const [startNode,startOffset] = point(Math.max(0,start)), [endNode,endOffset] = point(Math.max(0,end));
    const selectedRange = window.getSelection();
    if (selected.backward) selectedRange?.setBaseAndExtent(endNode,endOffset,startNode,startOffset);
    else selectedRange?.setBaseAndExtent(startNode,startOffset,endNode,endOffset);
}
function show(text: string) {
    if (bridge.blocked && ready) { permissions(); return; }
    if (sourceEditor && sourceEditorSpan && !bridge.composing && text !== latest) {
        for (const change of separateChanges(latest,text).reverse()) {
            sourceEditorSpan.start = transformOffset(sourceEditorSpan.start,change);
            sourceEditorSpan.end = transformOffset(sourceEditorSpan.end,change);
        }
        const next = display(text.slice(sourceEditorSpan.start,sourceEditorSpan.end));
        let start = sourceEditor.selectionStart, end = sourceEditor.selectionEnd;
        const direction = sourceEditor.selectionDirection;
        for (const change of separateChanges(sourceEditor.value,next).reverse()) { start = transformOffset(start,change); end = transformOffset(end,change); }
        sourceEditor.value = next; sourceEditor.setSelectionRange(start,end,direction);
    }
    latest = text;
    if (!ready) { if (!editor) build(); return; }
    if (bridge.composing) return;
    if (text === projection.source) { permissions(); return; }
    const selected = selection(), oldText = active().textContent ?? "", scroll = scrollElement().scrollTop;
    projecting = true;
    try {
        editor.setValue(display(text), true);
        lastSerialized = editor.getValue(); projection.reset(text,lastSerialized);
        renderViewer(); renderOutline(); renderSourceBlocks(); updateFind();
        restoreSelection(selected,oldText); scrollElement().scrollTop = scroll;
    } catch (error) { status.textContent = String(error); bridge.blocked = true; }
    finally { projecting = false; permissions(); }
}
function commit() {
    if (!ready || projecting || !bridge.writable || preferences.mode === "preview" || bridge.blocked) return;
    try {
        const actual = editor.getValue();
        if (actual === lastSerialized) return;
        if (!intent) throw new Error("The input has no safe source scope.");
        let canonical: string;
        if (intent.sourceView) {
            const changes = separateChanges(projection.canonical,preserveTableLayout(projection.canonical,actual)).filter(change => change.end >= intent!.start && change.start <= intent!.end);
            canonical = projection.canonical;
            for (const change of changes.reverse()) canonical = canonical.slice(0,change.start)+change.text+canonical.slice(change.end);
        } else {
            const children = editableBlocks();
            const blocks = children.map(block => preserveTableLayout(projection.canonical,serializeBlock(block)));
            const matching=(text:string|null) => blocks.flatMap((block,index)=>block===text ? [index] : []);
            const previous = intent.previous === null ? -1 : matching(intent.previous).at(intent.previousOccurrence ?? 0);
            const next = intent.next === null ? blocks.length : matching(intent.next).reverse().at(intent.nextOccurrence ?? 0);
            if (previous === undefined || next === undefined || next <= previous) throw new Error("The edited block cannot be mapped safely. Its source remains available.");
            const start = previous+1, end = next;
            let replacement = serializeBlockHtml(children.slice(start,end).map(block=>block.outerHTML).join(""));
            replacement = preserveTableLayout(projection.canonical.slice(intent.start,intent.end),replacement);
            if (intent.start === intent.end) replacement = replacement.trimEnd()+"\n\n";
            canonical = projection.canonical.slice(0,intent.start)+replacement+projection.canonical.slice(intent.end);
        }
        const source = projection.edit(canonical);
        // Reparsing moves reference definitions and adjusts table layout. Keep the
        // source mapping in its original block order rather than adopting that output.
        lastSerialized=actual; intent = null;
        latest = source; bridge.input(source); renderOutline(); updateFind();
    } catch (error) { retainUnmappedInput(error); }
}
function retainUnmappedInput(error:unknown) {
    // A recovery draft may contain serializer normalization; it must never be
    // submitted to the authoritative buffer as an ordinary edit.
    bridge.blocked = true;
    bridge.input(editor.getValue());
    status.textContent = `${String(error)} Visual input retained; use Recover Draft.`;
    permissions(); updateFind();
}
function preserveTableLayout(before:string,after:string):string {
    const row = (line:string) => line.trim().split(/(?<!\\)\|/);
    const normalized = (line:string) => row(line).map(cell=>/^\s*:?-+:?\s*$/.test(cell) ? cell.trim().replace(/-+/g,"---") : cell.trim()).join("|");
    const oldRows = before.split("\n").filter(line=>/^\s*\|.*\|\s*$/.test(line));
    if (!oldRows.some(line=>row(line).every(cell=>!cell.trim() || /^\s*:?-+:?\s*$/.test(cell)))) return after;
    let index=0;
    return after.split("\n").map(line=>{
        if (!/^\s*\|.*\|\s*$/.test(line)) return line;
        const identical=oldRows.find(old=>normalized(old)===normalized(line));
        const old=oldRows[index++];
        if (identical) return identical;
        if (!old || row(old).length!==row(line).length) return line;
        return row(line).map((cell,position)=>{
            const previous=row(old)[position];
            if (previous.trim()===cell.trim()) return previous;
            return (previous.match(/^\s*/)?.[0]??"")+cell.trim()+(previous.match(/\s*$/)?.[0]??"");
        }).join("|");
    }).join("\n");
}
function serializeBlockHtml(html:string):string {
    const lute = editor.vditor.lute;
    return editor.getCurrentMode() === "ir" ? lute.VditorIRDOM2Md(html) : lute.VditorDOM2Md(html);
}
function serializeBlock(block:Element):string { return serializeBlockHtml(block.outerHTML); }
function editableBlocks():Element[] {
    return [...active().children].filter(block=>!["link-ref-defs-block","footnotes-block"].includes(block.getAttribute("data-type") ?? ""));
}
function captureIntent(target?:Node, deletion?:"backward"|"forward") {
    if (!ready || projecting || preferences.mode === "preview" || bridge.blocked) return;
    commit();
    if (bridge.blocked) return;
    try { captureScope(target,deletion); } catch (error) { retainUnmappedInput(error); }
}
function captureScope(target?:Node, deletion?:"backward"|"forward") {
    const root = active(), selected = window.getSelection();
    const range = selected?.rangeCount ? selected.getRangeAt(0) : null;
    if (editor.getCurrentMode() === "sv") {
        const selection_ = selection(); if (!selection_) return;
        const text = root.textContent ?? "";
        intent = {start:text.lastIndexOf("\n",Math.max(0,selection_.start-1))+1,end:(text.indexOf("\n",selection_.end)+1 || text.length),previous:null,next:null,sourceView:true}; return;
    }
    const children = editableBlocks();
    const blockIndex = (node:Node|null):number => children.findIndex(block=>block===node || block.contains(node));
    let first = blockIndex(target ?? range?.startContainer ?? null);
    let last = target ? first : blockIndex(range?.endContainer ?? null);
    if (first < 0 || last < first) return;
    if (deletion && range?.collapsed && first === last) {
        const prefix = range.cloneRange(), suffix = range.cloneRange();
        prefix.selectNodeContents(children[first]); prefix.setEnd(range.startContainer,range.startOffset);
        suffix.selectNodeContents(children[last]); suffix.setStart(range.endContainer,range.endOffset);
        if (deletion === "backward" && !prefix.toString() && first > 0) first--;
        if (deletion === "forward" && !suffix.toString() && last+1 < children.length) last++;
    }
    const blocks = children.map(block=>preserveTableLayout(projection.canonical,serializeBlock(block)));
    const occurrences={previousOccurrence:blocks.slice(0,first).filter(block=>block===blocks[first-1]).length-1,nextOccurrence:blocks.slice(last+1).filter(block=>block===blocks[last+1]).length-1};
    let offset = 0;
    for (let index=0; index<first; index++) {
        const found = projection.canonical.indexOf(blocks[index].trimEnd(),offset);
        if (found >= 0) offset = found + blocks[index].trimEnd().length;
    }
    const before = preserveTableLayout(projection.canonical,serializeBlockHtml(children.slice(first,last+1).map(block=>block.outerHTML).join("")));
    if (!before.trim()) {
        const next = blocks[last+1] ?? null;
        const insertion = next ? projection.canonical.indexOf(next.trimEnd(),offset) : projection.canonical.length;
        if (insertion < 0) throw new Error("The inserted block cannot be mapped safely.");
        intent={start:insertion,end:insertion,previous:blocks[first-1]??null,next,...occurrences,sourceView:false}; return;
    }
    const start = projection.canonical.indexOf(before,offset);
    if (start < 0) {
        const trimmed = before.trimEnd(), found = projection.canonical.indexOf(trimmed,offset);
        if (found < 0) throw new Error("The selected source block cannot be mapped safely.");
        intent={start:found,end:found+trimmed.length,previous:blocks[first-1]??null,next:blocks[last+1]??null,...occurrences,sourceView:false};
    } else intent={start,end:start+before.length,previous:blocks[first-1]??null,next:blocks[last+1]??null,...occurrences,sourceView:false};
}
function build(selected = selection(), oldText = active().textContent ?? "") {
    if (bridge.composing) return;
    ready = false; projecting = true;
    const currentGeneration = ++generation;
    if (editor) editor.destroy();
    container.replaceChildren();
    editor = new Vditor(container, {
        value:display(latest), mode:preferences.mode === "preview" ? "ir" : preferences.mode,
        lang:"en_US", icon:false, i18n:(window as any).VditorI18n, cdn:location.origin + "/vendor",
        _lutePath:location.origin + "/vendor/lute.js", width:"100%",height:"100%",minHeight:200,
        cache:{enable:false}, toolbarConfig:{pin:true},
        customWysiwygToolbar:() => {},
        toolbar:["emoji","headings","bold","italic","strike","link","|","list","ordered-list","check","outdent","indent","|","quote","line","code","inline-code","table","insert-before","insert-after","|",
            {name:"native-undo",tip:"Undo",icon:"Undo",click:() => bridge.history("undo")},
            {name:"native-redo",tip:"Redo",icon:"Redo",click:() => bridge.history("redo")}],
        preview:{delay:0,mode:"both",actions:[],hljs:{enable:false,lineNumber:false},
            markdown:{sanitize:true,codeBlockPreview:false,mathBlockPreview:false,footnotes:true,toc:false},
            theme:{current:"",path:location.origin + "/vendor"},render:{media:{enable:false}},transform:(html:string) => sanitize(html),parse:() => renderOutline()},
        hint:{emojiPath:"",parse:false}, link:{isOpen:false,click:(element:HTMLElement) => openLink(linkDestination(element))}, image:{isPreview:false},
        input:() => commit(),
        after:() => {
            if (currentGeneration !== generation) return;
            ready = true; projecting = false;
            editor.vditor.undo.undo = () => bridge.history("undo");
            editor.vditor.undo.redo = () => bridge.history("redo");
            editor.vditor.undo.addToUndoStack = () => {};
            editor.vditor.undo.resetIcon = () => {};
            lastSerialized=editor.getValue(); projection.reset(latest,lastSerialized);
            (get("mode") as HTMLSelectElement).value = preferences.mode;
            container.hidden = preferences.mode === "preview"; viewer.hidden = preferences.mode !== "preview";
            document.body.classList.toggle("nowrap",!preferences.wrap); outline.hidden = !preferences.outline;
            renderViewer(); renderOutline(); renderSourceBlocks(); permissions();
            restoreSelection(selected,oldText); scrollElement().scrollTop = preferences.scroll;
            status.textContent = bridge.blocked ? "Visual draft retained; use Recover Draft." : "Markdown Editor";
            updateFind();
        },
    });
}
function renderViewer() {
    if (preferences.mode === "preview") viewer.innerHTML = sanitize(editor.vditor.lute.Md2HTML(display(latest)));
}
function headingElements() { return [...(preferences.mode === "sv" ? editor.vditor.preview.previewElement : active()).querySelectorAll<HTMLElement>("h1,h2,h3,h4,h5,h6")]; }
function renderOutline() {
    outline.replaceChildren();
    for (const heading of headingElements()) {
        let text = heading.textContent ?? "";
        if (preferences.mode === "ir") {
            const rendered = document.createElement("div");
            rendered.innerHTML = sanitize(editor.vditor.lute.Md2HTML(serializeBlock(heading)));
            text = rendered.textContent ?? "";
        }
        const label = text.replace(/^#+\s*/,"").replace(/\s*\{#[^}]+\}\s*$/, "").trim();
        const identifier = label.toLowerCase().replace(/[^\p{L}\p{N}\s-]/gu, "").trim().replace(/\s+/g,"-");
        heading.dataset.anchor = identifier;
        const raw = latest.split(/\r?\n/).find(line => line.replace(/^#{1,6}\s+/,"").replace(/\s*\{#[^}]+\}\s*$/,"") === label);
        const id = raw?.match(/\{#([^}]+)\}/)?.[1];
        if (id) heading.dataset.explicitAnchor = id;
        const button = document.createElement("button"); button.textContent = label;
        button.style.paddingLeft = `${Number(heading.tagName.slice(1))*10}px`;
        button.addEventListener("click",() => heading.scrollIntoView({block:"start"})); outline.append(button);
    }
}
function sourceSpans(): {start:number;end:number;label:string}[] {
    const spans: {start:number;end:number;label:string}[] = [];
    const frontmatter = latest.match(/^---\r?\n[\s\S]*?\r?\n---(?:\r?\n|$)/);
    if (frontmatter) spans.push({start:0,end:frontmatter[0].length,label:"Front matter"});
    for (const match of latest.matchAll(/(?:^<[^\n]+(?:\r?\n(?!(?:\r?\n))[^\n]+)*)|(?:^#{1,6}[^\n]*\{#[^}]+\}[^\n]*)|(?:^```[^\n]*\r?\n[\s\S]*?^```[^\n]*)|(?:^ {0,3}\[[^\]]+\]:[^\n]*(?:\r?\n[ \t]+[^\n]+)*)/gm)) {
        spans.push({start:match.index,end:match.index+match[0].length,label:match[0].split(/\r?\n/)[0].slice(0,80)});
    }
    return spans;
}
function renderSourceBlocks() {
    if (sourceEditor && document.activeElement === sourceEditor) return;
    const blocks = get("blocks"); blocks.replaceChildren(); sourceEditor = null;
    for (const span of sourceSpans()) {
        const details = document.createElement("details"), label = document.createElement("summary"), field = document.createElement("textarea");
        label.textContent = span.label; field.value = latest.slice(span.start,span.end); field.readOnly = !bridge.writable || preferences.mode === "preview";
        field.addEventListener("focus",() => { sourceEditor = field; sourceEditorSpan = span; });
        field.addEventListener("input",() => {
            if (!bridge.writable || bridge.blocked || preferences.mode === "preview") return;
            const blockSource=fromDisplay(latest.slice(span.start,span.end),field.value);
            const source = latest.slice(0,span.start) + blockSource + latest.slice(span.end);
            if (!withinBudget(source)) { status.textContent = "Source block exceeds the visual document limit."; return; }
            span.end = span.start + blockSource.length; latest = source;
            bridge.input(source); projecting = true; editor.setValue(display(source),true); lastSerialized=editor.getValue(); projection.reset(source,lastSerialized); projecting = false; updateFind();
        });
        field.addEventListener("blur",() => { sourceEditor = null; sourceEditorSpan = null; renderSourceBlocks(); });
        details.append(label,field); blocks.append(details);
    }
    get("source-blocks").hidden = !blocks.childElementCount;
}
function persist() { bridge.emit({kind:"preferences",preferences}); }
function mode(mode: Mode) {
    if (!ready) return;
    if (bridge.composing || sourceEditor) { status.textContent = "Finish the current composition or source block edit before switching modes."; return; }
    commit(); if (bridge.blocked) return;
    bridge.afterFlush.push(() => {
        const selected = selection(), oldText = active().textContent ?? "";
        preferences.scroll = scrollElement().scrollTop; preferences.mode = mode; persist(); build(selected,oldText);
    }); bridge.submit(); bridge.finishFlush();
}
function format(control:string) {
    if (!ready || !bridge.writable || bridge.blocked || preferences.mode === "preview") return;
    captureIntent(); (container.querySelector(`[data-type="${control}"]`) as HTMLButtonElement | null)?.click();
}
function openLink(url: string) {
    if (!url) { status.textContent = "This link has no destination."; return; }
    if (url.startsWith("#")) {
        let fragment: string;
        try { fragment = decodeURIComponent(url.slice(1)); } catch { return; }
        const root = preferences.mode === "sv" ? editor.vditor.preview.previewElement : active();
        const target = [...root.querySelectorAll<HTMLElement>("[id]")].find(element=>element.id===fragment)
            ?? headingElements().find(heading => heading.dataset.anchor === fragment || heading.dataset.explicitAnchor === fragment);
        target?.scrollIntoView({block:"start"}); return;
    }
    bridge.emit({kind:"open_link",url});
}
function linkDestination(element:HTMLElement):string {
    const href=element.getAttribute("href"); if (href) return href;
    const text=element.textContent?.trim() ?? "";
    if (text.startsWith("[") && text.endsWith("]")) {
        const identifier=text.slice(1,-1).toLowerCase();
        const definition=latest.split(/\r?\n/).find(line=>line.match(/^\s*\[([^\]]+)\]:/)?.[1].toLowerCase()===identifier);
        return definition?.match(/\]:\s*(?:<([^>]+)>|([^\s]+))/)?.slice(1).find(Boolean) ?? "";
    }
    return text.match(/^\s*\(?(?:<([^>]+)>|([^\s)]+))/)?.slice(1).find(Boolean) ?? "";
}
document.addEventListener("click",event => {
    const anchor = (event.target as Element).closest("a[href]");
    if (anchor && (container.contains(anchor) || viewer.contains(anchor))) {
        event.preventDefault(); event.stopImmediatePropagation(); openLink(anchor.getAttribute("href") ?? "");
    }
},true);
document.addEventListener("compositionstart",event => { if (container.contains(event.target as Node) || get("source-blocks").contains(event.target as Node)) bridge.composing = true; },true);
document.addEventListener("mousedown",event => {
    if (!container.contains(event.target as Node)) return;
    const target = event.target as Element;
    captureIntent(target instanceof HTMLInputElement && active().contains(target) ? target : undefined);
},true);
container.addEventListener("click",() => queueMicrotask(commit));
container.addEventListener("click",event => {
    if (editor?.getCurrentMode() !== "sv") return;
    const control = (event.target as Element).closest("[data-type]")?.getAttribute("data-type");
    if (!["code","table","line"].includes(control ?? "")) return;
    event.preventDefault(); event.stopImmediatePropagation();
    if (!bridge.writable || bridge.blocked || bridge.composing) return;
    const selected = selection(); if (!selected) return;
    const replacement = control === "code" ? "```\n"+selected.text+"\n```" : control === "line" ? "---" : `| ${selected.text || "col1"} | col2 | col3 |\n| --- | --- | --- |\n|  |  |  |\n|  |  |  |`;
    const canonical = projection.canonical.slice(0,selected.start)+replacement+projection.canonical.slice(selected.end);
    const source = projection.edit(canonical);
    latest = source; intent = null; projecting = true;
    editor.setValue(display(source),true); lastSerialized=editor.getValue(); projection.reset(source,lastSerialized); projecting = false;
    bridge.input(source); editor.focus(); renderSourceBlocks();
},true);
document.addEventListener("compositionend",event => {
    if (!container.contains(event.target as Node) && !get("source-blocks").contains(event.target as Node)) return;
    queueMicrotask(() => { commit(); bridge.compositionEnd(); show(bridge.draft); });
},true);
document.addEventListener("beforeinput",event => {
    if (!container.contains(event.target as Node)) return;
    const input = event as InputEvent;
    if (input.inputType === "historyUndo" || input.inputType === "historyRedo") {
        event.preventDefault(); event.stopImmediatePropagation(); bridge.history(input.inputType === "historyUndo" ? "undo" : "redo");
    } else if (!bridge.writable || bridge.blocked || preferences.mode === "preview" || input.data && !withinBudget(latest + input.data)) event.preventDefault();
    else captureIntent(undefined,input.inputType === "deleteContentBackward" ? "backward" : input.inputType === "deleteContentForward" ? "forward" : undefined);
},true);
document.addEventListener("paste",event => {
    if (event.target instanceof HTMLTextAreaElement && get("source-blocks").contains(event.target)) {
        event.preventDefault(); event.stopImmediatePropagation();
        if (!bridge.writable || bridge.blocked || preferences.mode === "preview") return;
        const field=event.target, text=event.clipboardData?.getData("text/plain") ?? "";
        if (!withinBudget(latest+text)) { status.textContent="Paste exceeds the visual document limit."; return; }
        field.setRangeText(display(text),field.selectionStart,field.selectionEnd,"end");
        field.dispatchEvent(new InputEvent("input",{bubbles:true,inputType:"insertFromPaste",data:text})); return;
    }
    if (!container.contains(event.target as Node)) return;
    const html = event.clipboardData?.getData("text/html"), text = event.clipboardData?.getData("text/plain") ?? "";
    event.preventDefault(); event.stopImmediatePropagation();
    if (!bridge.writable || bridge.blocked || preferences.mode === "preview") return;
    captureIntent();
    const markdown = html ? editor.html2md(sanitize(html)) : text;
    if (!withinBudget(latest + markdown)) { status.textContent = "Paste exceeds the visual document limit."; return; }
    editor.insertValue(markdown); commit();
},true);
document.addEventListener("drop",event => {
    if (container.contains(event.target as Node)) { event.preventDefault(); event.stopImmediatePropagation(); status.textContent = "File import is unavailable."; }
},true);
document.addEventListener("copy",event => {
    if (event.target instanceof HTMLTextAreaElement && get("source-blocks").contains(event.target)) {
        const field=event.target, text=field.value.slice(field.selectionStart,field.selectionEnd);
        event.preventDefault(); event.clipboardData?.setData("text/plain",text); bridge.emit({kind:"copy",text}); return;
    }
    const selected = window.getSelection();
    if (!selected || !active().contains(selected.anchorNode)) return;
    event.preventDefault(); event.stopImmediatePropagation();
    event.clipboardData?.setData("text/plain",selected.toString());
    bridge.emit({kind:"copy",text:selected.toString()});
},true);
document.addEventListener("selectionchange",() => {
    if (!ready || projecting) return;
    const field = document.activeElement;
    if (field instanceof HTMLTextAreaElement || field instanceof HTMLInputElement && field.type === "text") {
        bridge.emit({kind:"input_state",text:field.value,start:field.selectionStart ?? 0,end:field.selectionEnd ?? 0}); return;
    }
    const selected = selection(); if (!selected) return;
    bridge.emit({kind:"input_state",text:active().textContent ?? "",start:selected.start,end:selected.end});
});
document.addEventListener("scroll",() => {
    if (!ready || projecting) return;
    const scroll = Math.min(100_000_000,Math.round(Math.max(0,scrollElement().scrollTop)));
    if (scroll !== preferences.scroll) { preferences.scroll = scroll; persist(); }
},true);

const findBar = get("find"), query = get("query") as HTMLInputElement, matchCase = get("case") as HTMLInputElement;
let matches: Range[] = [], currentMatch = -1;
function updateFind() {
    matches = []; currentMatch = -1;
    (CSS as any).highlights?.delete("current-find");
    const root = active(), nodes = textNodes(root), text = nodes.map(node => node.data).join("");
    const needle = matchCase.checked ? query.value : query.value.toLowerCase(), haystack = matchCase.checked ? text : text.toLowerCase();
    const point = (offset:number): [Node,number] => {
        for (const node of nodes) { if (offset <= node.length) return [node,offset]; offset -= node.length; }
        return [root,root.childNodes.length];
    };
    if (!findBar.hidden && needle) for (let offset = haystack.indexOf(needle); offset >= 0 && matches.length < 10000; offset = haystack.indexOf(needle,offset+needle.length)) {
        const range = new Range(), [start,startOffset] = point(offset), [end,endOffset] = point(offset+needle.length);
        range.setStart(start,startOffset); range.setEnd(end,endOffset); matches.push(range);
    }
    (CSS as any).highlights?.set("find",new (window as any).Highlight(...matches));
    get("count").textContent = `${matches.length} matches`;
}
function moveFind(direction:number) {
    if (!matches.length) return;
    currentMatch = currentMatch < 0 ? (direction > 0 ? 0 : matches.length-1) : (currentMatch + direction + matches.length) % matches.length;
    const range = matches[currentMatch]; (CSS as any).highlights?.set("current-find",new (window as any).Highlight(range));
    const node = range.startContainer.parentElement; node?.scrollIntoView({block:"center"});
    get("count").textContent = `${currentMatch+1} of ${matches.length}`;
}
function openFind() { findBar.hidden = false; query.focus(); query.select(); updateFind(); }
function closeFind() { findBar.hidden = true; (CSS as any).highlights?.delete("find"); (CSS as any).highlights?.delete("current-find"); if (preferences.mode !== "preview") editor.focus(); else viewer.focus(); }
query.addEventListener("input",updateFind); matchCase.addEventListener("change",updateFind);
get("next").addEventListener("click",() => moveFind(1)); get("previous").addEventListener("click",() => moveFind(-1));
get("find-open").addEventListener("click",openFind); get("find-close").addEventListener("click",closeFind);
document.addEventListener("keydown",event => {
    const key = event.key.toLowerCase(), command = event.ctrlKey || event.metaKey;
    if (container.contains(event.target as Node)) captureIntent(undefined,key === "backspace" ? "backward" : key === "delete" ? "forward" : undefined);
    if (command && key === "f") { event.preventDefault(); event.stopImmediatePropagation(); openFind(); }
    else if (command && (key === "z" || key === "y")) { event.preventDefault(); event.stopImmediatePropagation(); bridge.history(event.shiftKey || key === "y" ? "redo" : "undo"); }
    else if (!findBar.hidden && key === "escape") { event.preventDefault(); event.stopImmediatePropagation(); closeFind(); }
    else if (!findBar.hidden && event.target === query && key === "enter") { event.preventDefault(); moveFind(event.shiftKey ? -1 : 1); }
    else if (command && event.altKey && ["7","8","9"].includes(key)) { event.preventDefault(); event.stopImmediatePropagation(); mode(({7:"wysiwyg",8:"ir",9:"sv"} as any)[key]); }
    else if (command && !event.altKey && container.contains(event.target as Node)) {
        const control = ({b:event.shiftKey ? null:"bold",i:event.shiftKey ? "outdent":"italic",d:"strike",k:"link",l:"list",o:event.shiftKey ? "indent":"ordered-list",j:"check",";":"quote",h:event.shiftKey ? "line":null,u:"code",g:"inline-code",m:"table"} as any)[key];
        if (control) { event.preventDefault(); event.stopImmediatePropagation(); format(control); }
    }
},true);
(get("mode") as HTMLSelectElement).addEventListener("change",event => mode((event.target as HTMLSelectElement).value as Mode));
get("outline-toggle").addEventListener("click",() => { preferences.outline = !preferences.outline; outline.hidden = !preferences.outline; persist(); renderOutline(); });
get("wrap").addEventListener("click",() => { preferences.wrap = !preferences.wrap; document.body.classList.toggle("nowrap",!preferences.wrap); persist(); });
get("reset").addEventListener("click",() => {
    if (bridge.composing) return;
    commit(); if (bridge.blocked) return;
    bridge.afterFlush.push(() => { preferences = defaults(); persist(); build(); }); bridge.submit(); bridge.finishFlush();
});
get("copy-markdown").addEventListener("click",() => { commit(); bridge.emit({kind:"copy",text:bridge.draft}); });
get("copy-html").addEventListener("click",() => bridge.emit({kind:"copy",text:sanitize(editor.getHTML())}));
(window as any).latheRich = {openFind,mode,format,commit,projection,sanitize, get editor() {return editor;}, get ready() {return ready;},
    replaceText(before:string,after:string) {
        const node = textNodes(active()).find(node => node.data.includes(before));
        if (!node) throw new Error(`Rendered text not found: ${before}`);
        const offset = node.data.indexOf(before);
        window.getSelection()?.setBaseAndExtent(node,offset,node,offset+before.length);
        captureIntent(node);
        node.data = node.data.replace(before,after); commit();
    },
    appendText(text:string) {
        const block = editableBlocks().at(-1) as HTMLElement | undefined;
        const node = textNodes(block ?? active()).at(-1);
        if (!node) throw new Error("Editable end block is unavailable");
        window.getSelection()?.setBaseAndExtent(node,node.length,node,node.length);
        captureIntent(node); node.data += text; commit();
    }};
bridge.emit({kind:"ready"});
