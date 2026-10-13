import { DocumentBridge, SourceProjection, display, fromDisplay, separateChanges, transformOffset, withinBudget } from "./application.js";
import { renderFence, rendererLanguages, highlightCode } from "./renderers.js";
import { themes } from "./themes.js";

declare const Vditor: any;
declare const Lute: any;
declare const DOMPurify: any;
type Mode = "ir" | "wysiwyg" | "sv" | "preview";
type Preferences = {mode:Mode;outline:boolean;wrap:boolean;scroll:number;content_theme:string|null;code_theme:string|null};
const defaults = (): Preferences => ({mode:"ir",outline:false,wrap:true,scroll:0,content_theme:null,code_theme:null});
const applicationControls = new Map([...document.querySelectorAll<HTMLElement>("[id]")].map(element=>[element.id,element]));
const get = (id: string) => applicationControls.get(id)!;
const status = get("status");
const container = get("document"), viewer = get("viewer"), outline = get("outline");
const parameters = new URL(location.href).searchParams;
let editor: any = null, generation = 0, projecting = false, ready = false, preferences = defaults();
let latest = "", sourceEditor: HTMLTextAreaElement | null = null;
let lastSerialized = "";
const sourceControls = new Map<HTMLTextAreaElement,{start:number;end:number}>();
type Intent = {start:number;end:number;previous:string|null;next:string|null;previousOccurrence?:number;nextOccurrence?:number;sourceView:boolean;preserveSetext?:boolean};
let intent: Intent | null = null;
const projection = new SourceProjection();
let appearance:any={dark:false,use_app_theme_colors:true,show_line_numbers:true,custom_css:"",background:"#fff",foreground:"#222",accent:"#2670b7",plantuml_service:null};
const contentStyle=document.createElement("link"),codeStyle=document.createElement("link"),customStyle=document.createElement("style");
contentStyle.rel=codeStyle.rel="stylesheet"; document.head.append(contentStyle,codeStyle,customStyle);
const resources=new Map<number,{resolve:(message:any)=>void;reject:(error:Error)=>void}>();
const imageUrls=new Set<string>();
let resourceRequest=0;
let resourceGeneration=0;
let readingGeneration=0;
const forbiddenTags = ["script","style","iframe","object","embed","svg","math","template","form","input","button","select","textarea","audio","video","source","base","meta","link"];
DOMPurify.addHook("uponSanitizeAttribute", (_node:Element, attribute:any,configuration:any) => {
    if (attribute.attrName.startsWith("data-lathe-") && !configuration.LATHE_IMAGES) attribute.keepAttr=false;
    if (attribute.attrName === "class") attribute.attrValue = attribute.attrValue.replace(/\blanguage-math\b/g,"lathe-math").replace(/\blanguage-(?:mermaid|graphviz|echarts|abc|flowchart|plantuml|markmap|mindmap|smiles)\b/g,"language-text");
});
DOMPurify.addHook("afterSanitizeAttributes", (node:Element, _data:any, configuration:any) => {
    if (node.tagName==="IMG") {
        const source=node.getAttribute("src");
        if (source) { node.removeAttribute("src"); node.removeAttribute("data-lathe-original"); node.setAttribute("data-lathe-image",source); }
    }
    if (node.tagName !== "INPUT") return;
    if (node.getAttribute("type")?.toLowerCase() !== "checkbox") node.remove();
    else if (!configuration.ALLOW_DATA_ATTR) node.setAttribute("disabled","");
});
export function sanitize(html: string, editing = false, images = false): string {
    return DOMPurify.sanitize(html, {
        FORBID_TAGS: forbiddenTags.filter(tag => tag !== "input"),
        FORBID_ATTR:["style","srcset","formaction","autofocus","name"],
        ADD_TAGS:editing ? ["wbr"] : [], ALLOW_DATA_ATTR:editing,
        LATHE_IMAGES:images,
        ADD_ATTR:images?["data-lathe-image","data-lathe-original"]:[],
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
        lute[method] = (...parameters: any[]) => sanitize(original(...parameters), !method.endsWith("HTML"), !method.startsWith("Md2") && !method.startsWith("HTML2"));
    }
    for(const method of ["VditorDOM2Md","VditorIRDOM2Md","VditorDOM2HTML","VditorIRDOM2HTML"]) {
        const original=lute[method];
        if(typeof original!=="function")continue;
        lute[method]=(html:string,...parameters:any[])=>{
            const template=document.createElement("template"); template.innerHTML=html;
            for(const preview of template.content.querySelectorAll('.vditor-ir__preview,.vditor-wysiwyg__preview')) preview.remove();
            for(const image of template.content.querySelectorAll("img[data-lathe-original],img[data-lathe-image]")) {
                image.setAttribute("src",image.getAttribute("data-lathe-original") ?? image.getAttribute("data-lathe-image") ?? "");
                image.removeAttribute("data-lathe-original"); image.removeAttribute("data-lathe-image");
            }
            return original(template.innerHTML,...parameters);
        };
    }
    return lute;
};

const bridge = new DocumentBridge(parameters.get("session") ?? "", parameters.get("document") ?? "",
    message => console.log("LATHE_MARKDOWN:" + JSON.stringify(message)), show, text => { status.textContent = text; });
(window as any).latheMarkdown = bridge;
const receive = bridge.receive.bind(bridge);
bridge.receive = message => {
    if(message.kind==="resources_reset") {
        resourceGeneration++;
        for(const url of imageUrls)URL.revokeObjectURL(url);imageUrls.clear();
        for(const image of document.querySelectorAll<HTMLImageElement>("img[data-lathe-original]")) { image.removeAttribute("src");image.dataset.latheImage=image.dataset.latheOriginal; }
        if(ready)void loadImages(renderedRoot());return;
    }
    if (message.kind === "resource") {
        const pending=resources.get(message.request); resources.delete(message.request);
        if (message.error) pending?.reject(new Error(message.error)); else pending?.resolve(message);
        return;
    }
    if (message.kind === "appearance") { appearance=message; applyAppearance(); return; }
    if (["flush","document","accepted","preferences"].includes(message.kind)) commit();
    if (message.kind === "preferences") {
        if (bridge.blocked) return;
        const previousMode=preferences.mode;
        preferences = {...defaults(),...message.preferences};
        if (ready && !bridge.composing && previousMode!==preferences.mode) build();
        else if (ready) { outline.hidden=!preferences.outline; document.body.classList.toggle("nowrap",!preferences.wrap); applyAppearance(); }
        return;
    }
    receive(message);
    permissions();
};

function applyAppearance() {
    const scroll=ready?scrollElement().scrollTop:0,reading=readingGeneration;
    const restore=()=>{if(ready && readingGeneration===reading)scrollElement().scrollTop=scroll;};
    contentStyle.onload=codeStyle.onload=restore;
    const content=preferences.content_theme ?? appearance.content_theme ?? (appearance.dark?"dark":"light");
    const code=preferences.code_theme ?? appearance.code_theme ?? (appearance.dark?"github-dark":"github");
    if (themes.content.includes(content)) contentStyle.href=`/vendor/dist/css/content-theme/${content}.css`;
    if (themes.code.includes(code)) codeStyle.href=`/vendor/dist/js/highlight.js/styles/${code}.css`;
    (get("content-theme") as HTMLSelectElement).value=preferences.content_theme ?? "";
    (get("code-theme") as HTMLSelectElement).value=preferences.code_theme ?? "";
    document.body.classList.toggle("app-theme",appearance.use_app_theme_colors);
    document.body.classList.toggle("line-numbers",appearance.show_line_numbers && preferences.mode==="ir");
    container.classList.toggle("vditor--dark",appearance.dark);
    for(const [name,value] of Object.entries({background:appearance.background,foreground:appearance.foreground,accent:appearance.accent})) document.documentElement.style.setProperty(`--app-${name}`,String(value));
    customStyle.textContent=String(appearance.custom_css ?? "").slice(0,128*1024);
    lineNumbers();
    if(ready && !sourceEditor && !bridge.composing)renderSourceBlocks();
    restore();
}
function requestResource(kind:"resource"|"plantuml",value:string):Promise<any> {
    if (resources.size>=16) return Promise.reject(new Error("Too many pending image resources."));
    const request=++resourceRequest;
    return new Promise((resolve,reject)=>{
        resources.set(request,{resolve,reject}); bridge.emit({kind,request,...(kind==="resource"?{url:value}:{encoded:value})});
        setTimeout(()=>{if(resources.delete(request))reject(new Error("Project resource timed out."));},35000);
    });
}
async function loadImages(root:HTMLElement) {
    const generation=resourceGeneration;
    const activeUrls=new Set([...document.querySelectorAll<HTMLImageElement>("img")].map(image=>image.src));
    for(const url of imageUrls)if(!activeUrls.has(url)){URL.revokeObjectURL(url);imageUrls.delete(url);}
    for (const element of root.querySelectorAll<HTMLImageElement>("img[data-lathe-image]")) {
        const url=element.dataset.latheImage!; delete element.dataset.latheImage; element.dataset.latheOriginal=url;
        try {
            const result=await requestResource("resource",url);
            if (!element.isConnected || generation!==resourceGeneration) continue;
            const bytes=Uint8Array.from(atob(result.bytes),character=>character.charCodeAt(0));
            const scroll=scrollElement().scrollTop,reading=readingGeneration;
            element.onload=()=>{if(reading===readingGeneration)scrollElement().scrollTop=scroll;};
            const object=URL.createObjectURL(new Blob([bytes],{type:result.mime})); imageUrls.add(object); element.src=object;
        } catch(error) { if (element.isConnected) { element.classList.add("resource-error");status.textContent=`Image unavailable: ${String(error)}`; } }
    }
}
function lineNumbers() {
    let gutter=container.querySelector<HTMLElement>(".line-gutter");
    if (!ready || !appearance.show_line_numbers || preferences.mode!=="ir") { gutter?.remove(); return; }
    if (!gutter) { gutter=document.createElement("div"); gutter.className="line-gutter"; container.append(gutter); }
    gutter.replaceChildren(); let offset=0;
    const bounds=container.getBoundingClientRect();
    for(const block of editableBlocks()) {
        const serialized=serializeBlock(block), found=projection.canonical.indexOf(serialized,offset);
        if(found<0)continue; offset=found+serialized.length;
        const number=document.createElement("span"); number.textContent=String(latest.slice(0,projection.sourceOffset(found)).split(/\r?\n/).length);
        number.style.top=`${block.getBoundingClientRect().top-bounds.top}px`; gutter.append(number);
    }
}

function insertionPosition():number {
    const selected=window.getSelection();
    if (sourceEditor) {
        const span=sourceControls.get(sourceEditor)!;
        return span.start+fromDisplay(latest.slice(span.start,span.end),sourceEditor.value.slice(0,sourceEditor.selectionStart)).length;
    }
    if(!selected?.rangeCount || !active().contains(selected.anchorNode))return latest.length;
    const range=selected.getRangeAt(0),root=active();
    if(editor.getCurrentMode()==="sv") { const prefix=range.cloneRange();prefix.selectNodeContents(root);prefix.setEnd(range.startContainer,range.startOffset);return projection.sourceOffset(prefix.toString().length); }
    let block:Node|null=range.startContainer;
    while(block && block.parentNode!==root)block=block.parentNode;
    if(!(block instanceof Element))return latest.length;
    const blocks=editableBlocks(),index=blocks.indexOf(block);
    let before=0;
    for(const preceding of blocks.slice(0,index)) {
        const serialized=serializeBlock(preceding),found=projection.canonical.indexOf(serialized,before);
        if(found<0)throw new Error("Image insertion position cannot be mapped safely.");
        before=found+serialized.length;
    }
    const start=projection.canonical.indexOf(serializeBlock(block),before);
    if(start<0)throw new Error("Image insertion block cannot be mapped safely.");
    const prefix=range.cloneRange();prefix.selectNodeContents(block);prefix.setEnd(range.startContainer,range.startOffset);
    const mapping=new SourceProjection();mapping.reset(serializeBlock(block),block.textContent ?? "");
    const offset=prefix.toString().length;
    const marked=(range.startContainer instanceof Element?range.startContainer:range.startContainer.parentElement)?.closest('strong,em,s,del,a,code,[data-type="strong"],[data-type="em"]');
    return projection.sourceOffset(start+mapping.sourceOffset(offset,marked && offset>0?"end":"start"));
}
function importAfterFlush(input:{kind:string;[key:string]:unknown},position?:number,source?:string) {
    if (!ready || !bridge.writable || bridge.blocked || preferences.mode==="preview" || bridge.composing)return;
    commit();if(bridge.blocked)return;
    position ??= insertionPosition();source ??= latest;
    bridge.afterFlush.push(()=>{
        for(const change of separateChanges(source!,latest).reverse())position=transformOffset(position!,change);
        bridge.emit({...input,position,base_revision:bridge.revision});
    });bridge.submit();bridge.finishFlush();
}
async function importFiles(files:File[]) {
    if(files.length>16){status.textContent="Import at most 16 images.";return;}
    commit();const position=insertionPosition(),source=latest,images=[];
    try {
        for(const file of files) {
            if(file.size>4*1024*1024)throw new Error("Image exceeds the 4 MiB limit.");
            const bytes=new Uint8Array(await file.arrayBuffer());let binary="";
            for(let index=0;index<bytes.length;index+=32768)binary+=String.fromCharCode(...bytes.subarray(index,index+32768));
            images.push({name:file.name,bytes:btoa(binary)});
        }
        importAfterFlush({kind:"import_images",images},position,source);
    }catch(error){status.textContent=String(error);}
}

function active(): HTMLElement { return preferences.mode === "preview" ? viewer : editor?.vditor[editor.getCurrentMode()].element ?? container; }
function renderedRoot():HTMLElement { return preferences.mode==="sv" ? editor.vditor.preview.previewElement : active(); }
function scrollElement(): HTMLElement { return preferences.mode === "preview" ? viewer : active(); }
function permissions() {
    if (!ready) return;
    const writable = bridge.writable && !bridge.blocked && preferences.mode !== "preview";
    if (writable) editor.enable(); else editor.disabled();
    for (const field of sourceControls.keys()) field.readOnly = !writable;
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
    for (const change of separateChanges(oldText,text).reverse()) {
        start = transformOffset(start, change); end = transformOffset(end, change);
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
    updateSourceControls(text);
    if (!ready) { if (!editor) build(); return; }
    if (bridge.composing) return;
    if (text === projection.source) { permissions(); return; }
    const selected = selection(), oldText = active().textContent ?? "", scroll = scrollElement().scrollTop;
    projecting = true;
    try {
        editor.setValue(display(text), true);
        initializeEmptyBlock();
        lastSerialized = editor.getValue(); projection.reset(text,lastSerialized);
        renderViewer(); renderOutline(); renderSourceBlocks(); updateFind();
        void loadImages(renderedRoot()); lineNumbers();
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
        let canonical: string, serializedCanonical: string | null = null;
        if (intent.sourceView) {
            const preserved = preserveTableLayout(projection.canonical,actual);
            const changes = separateChanges(projection.canonical,intent.preserveSetext ? preserveSetextLayout(projection.canonical,preserved) : preserved).filter(change => change.end >= intent!.start && change.start <= intent!.end);
            canonical = projection.canonical;
            for (const change of changes.reverse()) canonical = canonical.slice(0,change.start)+change.text+canonical.slice(change.end);
            if (intent.preserveSetext) serializedCanonical = preserveSetextLayout(actual,canonical);
        } else {
            const children = editableBlocks();
            const blocks = children.map(serializeBlock);
            const matching=(text:string|null) => blocks.flatMap((block,index)=>block===text ? [index] : []);
            const previous = intent.previous === null ? -1 : matching(intent.previous).at(intent.previousOccurrence ?? 0);
            const next = intent.next === null ? blocks.length : matching(intent.next).reverse().at(intent.nextOccurrence ?? 0);
            if (previous === undefined || next === undefined || next <= previous) throw new Error("The edited block cannot be mapped safely. Its source remains available.");
            const start = previous+1, end = next;
            let replacement = serializeBlockHtml(children.slice(start,end).map(block=>block.outerHTML).join(""));
            replacement = preserveTableLayout(projection.canonical.slice(intent.start,intent.end),replacement);
            if (intent.preserveSetext) {
                serializedCanonical = projection.canonical.slice(0,intent.start)+replacement+projection.canonical.slice(intent.end);
                replacement = preserveSetextLayout(projection.canonical.slice(intent.start,intent.end),replacement);
            }
            if (intent.start === intent.end) replacement = replacement.trimEnd()+"\n\n";
            canonical = projection.canonical.slice(0,intent.start)+replacement+projection.canonical.slice(intent.end);
        }
        const source = projection.edit(canonical);
        if (serializedCanonical !== null) projection.reset(source,serializedCanonical);
        // Reparsing moves reference definitions and adjusts table layout. Keep the
        // source mapping in its original block order rather than adopting that output.
        lastSerialized=actual; intent = null;
        updateSourceControls(source); bridge.input(source); renderOutline(); renderSourceBlocks(); updateFind(); void loadImages(renderedRoot()); lineNumbers();
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
function tableRows(text:string):string[][] {
    const tables:string[][] = [];
    let rows:string[] = [];
    for (const line of [...text.split("\n"),""]) {
        if (/^\s*\|.*\|\s*$/.test(line)) rows.push(line);
        else if (rows.length) {
            if (rows.some(row=>row.trim().split(/(?<!\\)\|/).every(cell=>!cell.trim() || /^\s*:?-+:?\s*$/.test(cell)))) tables.push(rows);
            rows = [];
        }
    }
    return tables;
}
function preserveTableLayout(before:string,after:string,tableOffset=0):string {
    const row = (line:string) => line.trim().split(/(?<!\\)\|/);
    const tables = tableRows(before);
    let tableIndex=tableOffset-1, index=0, inTable=false;
    return after.split("\n").map(line=>{
        if (!/^\s*\|.*\|\s*$/.test(line)) { inTable=false; return line; }
        if (!inTable) { tableIndex++; index=0; inTable=true; }
        const old=tables[tableIndex]?.[index++];
        if (!old || row(old).length!==row(line).length) return line;
        return row(line).map((cell,position)=>{
            const previous=row(old)[position];
            if (previous.trim()===cell.trim() || /^\s*:?-+:?\s*$/.test(previous) && previous.trim().replace(/-+/g,"---")===cell.trim().replace(/-+/g,"---")) return previous;
            return (previous.match(/^\s*/)?.[0]??"")+cell.trim()+(previous.match(/\s*$/)?.[0]??"");
        }).join("|");
    }).join("\n");
}
function serializeBlockHtml(html:string):string {
    const lute = editor.vditor.lute;
    return editor.getCurrentMode() === "ir" ? lute.VditorIRDOM2Md(html) : lute.VditorDOM2Md(html);
}
function preserveSetextLayout(before:string,after:string):string {
    const original=before.split("\n"), changed=after.split("\n");
    if (original.length !== changed.length) return after;
    for (let index=1; index<original.length; index++) {
        if (/^(=+|-+)$/.test(original[index]) && /^(=+|-+)$/.test(changed[index]) && original[index][0] === changed[index][0]) changed[index]=original[index];
    }
    return changed.join("\n");
}
function serializeBlock(block:Element):string { return serializeBlockHtml(block.outerHTML); }
function editableBlocks():Element[] {
    return [...active().children].filter(block=>!["link-ref-defs-block","footnotes-block"].includes(block.getAttribute("data-type") ?? ""));
}
function initializeEmptyBlock() {
    if (editor.getCurrentMode() === "sv" || editableBlocks().length || preferences.mode === "preview") return;
    const paragraph = document.createElement("p"); paragraph.dataset.block = "0";
    if (editor.getCurrentMode() === "ir") {
        const text = document.createElement("span"); text.dataset.type = "text"; paragraph.append(text);
    }
    paragraph.append(document.createElement("br")); active().append(paragraph);
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
        const start=text.lastIndexOf("\n",Math.max(0,selection_.start-1))+1,end=(text.indexOf("\n",selection_.end)+1 || text.length);
        const preserveSetext=!text.slice(start,selection_.end).includes("\n") && /^(=+|-+)$/.test(text.slice(end,text.indexOf("\n",end)+1 || text.length).trim());
        intent = {start,end,previous:null,next:null,sourceView:true,preserveSetext}; return;
    }
    const children = editableBlocks();
    if (!children.length && (root.contains(target ?? range?.startContainer ?? null) || document.activeElement === root)) {
        intent = {start:projection.canonical.length,end:projection.canonical.length,previous:null,next:null,sourceView:false}; return;
    }
    const blockIndex = (node:Node|null):number => children.findIndex(block=>block===node || block.contains(node));
    let first = blockIndex(target ?? range?.startContainer ?? null);
    let last = target ? first : blockIndex(range?.endContainer ?? null);
    if (first < 0 || last < first) return;
    const markerSelection=[target ?? range?.startContainer,range?.endContainer].some(node=>node?.parentElement?.closest('[data-type="heading-marker"]'));
    const preserveSetext=first === last && /^[=-]$/.test(children[first].getAttribute("data-marker") ?? "") && !markerSelection;
    if (deletion && range?.collapsed && first === last) {
        const prefix = range.cloneRange(), suffix = range.cloneRange();
        prefix.selectNodeContents(children[first]); prefix.setEnd(range.startContainer,range.startOffset);
        suffix.selectNodeContents(children[last]); suffix.setStart(range.endContainer,range.endOffset);
        if (deletion === "backward" && !prefix.toString() && first > 0) first--;
        if (deletion === "forward" && !suffix.toString() && last+1 < children.length) last++;
    }
    const serialized = children.map(serializeBlock);
    let tableOffset = 0;
    const blocks = serialized.map(block=>{
        const preserved=preserveTableLayout(projection.canonical,block,tableOffset);
        tableOffset += tableRows(block).length;
        return preserved;
    });
    const occurrences={previousOccurrence:serialized.slice(0,first).filter(block=>block===serialized[first-1]).length-1,nextOccurrence:serialized.slice(last+1).filter(block=>block===serialized[last+1]).length-1};
    let offset = 0;
    for (let index=0; index<first; index++) {
        const found = projection.canonical.indexOf(blocks[index].trimEnd(),offset);
        if (found >= 0) offset = found + blocks[index].trimEnd().length;
    }
    const before = preserveTableLayout(projection.canonical,serializeBlockHtml(children.slice(first,last+1).map(block=>block.outerHTML).join("")),tableRows(serialized.slice(0,first).join("\n")).length);
    if (!before.trim()) {
        const next = blocks[last+1] ?? null;
        const insertion = next ? projection.canonical.indexOf(next.trimEnd(),offset) : projection.canonical.length;
        if (insertion < 0) throw new Error("The inserted block cannot be mapped safely.");
        intent={start:insertion,end:insertion,previous:serialized[first-1]??null,next:serialized[last+1]??null,...occurrences,sourceView:false}; return;
    }
    const start = projection.canonical.indexOf(before,offset);
    if (start < 0) {
        const trimmed = before.trimEnd(), found = projection.canonical.indexOf(trimmed,offset);
        if (found < 0) throw new Error("The selected source block cannot be mapped safely.");
        intent={start:found,end:found+trimmed.length,previous:serialized[first-1]??null,next:serialized[last+1]??null,...occurrences,sourceView:false,preserveSetext};
    } else intent={start,end:start+before.length,previous:serialized[first-1]??null,next:serialized[last+1]??null,...occurrences,sourceView:false,preserveSetext};
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
            math:{engine:"KaTeX",inlineDigit:true},
            markdown:{sanitize:true,codeBlockPreview:false,mathBlockPreview:false,footnotes:true,toc:false},
            theme:{current:"",path:location.origin + "/vendor"},render:{media:{enable:false}},transform:(html:string) => sanitize(html,false,true),parse:() => { renderOutline();if(ready) { renderMath(editor.vditor.preview.previewElement);void loadImages(editor.vditor.preview.previewElement);void highlightCode(editor.vditor.preview.previewElement); } }},
        hint:{emojiPath:"",parse:false}, link:{isOpen:false,click:(element:HTMLElement) => openLink(linkDestination(element))}, image:{isPreview:false},
        input:() => commit(),
        after:() => {
            if (currentGeneration !== generation) return;
            ready = true; projecting = false;
            editor.vditor.undo.undo = () => bridge.history("undo");
            editor.vditor.undo.redo = () => bridge.history("redo");
            editor.vditor.undo.addToUndoStack = () => {};
            editor.vditor.undo.resetIcon = () => {};
            initializeEmptyBlock();
            lastSerialized=editor.getValue(); projection.reset(latest,lastSerialized);
            (get("mode") as HTMLSelectElement).value = preferences.mode;
            container.hidden = preferences.mode === "preview"; viewer.hidden = preferences.mode !== "preview";
            document.body.classList.toggle("nowrap",!preferences.wrap); outline.hidden = !preferences.outline;
            renderViewer(); renderOutline(); renderSourceBlocks(); permissions();
            applyAppearance(); void loadImages(renderedRoot());
            restoreSelection(selected,oldText); scrollElement().scrollTop = preferences.scroll;
            status.textContent = bridge.blocked ? "Visual draft retained; use Recover Draft." : "Markdown Editor";
            updateFind();
        },
    });
}
function renderViewer() {
    if (preferences.mode === "preview") {
        viewer.innerHTML = sanitize(editor.vditor.lute.Md2HTML(display(latest)),false,true);
        renderMath(viewer);
        void loadImages(viewer);
        void highlightCode(viewer);
    }
}
function renderMath(root:HTMLElement) {
    if(preferences.mode==="sv" || preferences.mode==="preview") {
        for(const math of root.querySelectorAll<HTMLElement>('span.lathe-math,div.lathe-math')) {
            const source=math.dataset.source ?? math.textContent ?? "";
            if(math.dataset.source===source && math.dataset.renderer==="math")continue;
            math.dataset.source=source;void renderFence(math,"math",source,plantuml);
        }
        return;
    }
    for(const code of root.querySelectorAll<HTMLElement>('code[data-type="math-inline"],code[data-type="math-block"]')) {
        const parent=code.closest<HTMLElement>('span[data-type="inline-node"],span.vditor-wysiwyg__block,div[data-type="math-block"]');
        if(!parent)continue;
        let preview=parent.querySelector<HTMLElement>(':scope > .vditor-ir__preview,:scope > .vditor-wysiwyg__preview');
        if(!preview){preview=document.createElement(code.dataset.type==="math-inline"?"span":"div");preview.className=preferences.mode==="ir"?"vditor-ir__preview":"vditor-wysiwyg__preview";preview.dataset.render="1";parent.append(preview);}
        const source=(code.textContent ?? "").replace(/\u200b/g,"");
        if(preview.dataset.source===source && preview.dataset.renderer==="math")continue;
        preview.dataset.source=source;void renderFence(preview,"math",source,plantuml);
    }
}
async function plantuml(source:string):Promise<string> {
    if (!appearance.plantuml_service) throw new Error("PlantUML requires an explicitly configured network service; it cannot render offline.");
    const result=await requestResource("plantuml",source);
    return new TextDecoder().decode(Uint8Array.from(atob(result.bytes),character=>character.charCodeAt(0)));
}
function renderSourceFence(details:HTMLElement,span:{start:number;end:number}) {
    const source=latest.slice(span.start,span.end);
    const fence=source.match(/^```(\S+)\s*\r?\n([\s\S]*?)\r?\n```/);
    const match=fence ?? (source.startsWith("$") ? [source,"math",source.replace(/^\${1,2}|\${1,2}$/g,"").trim()] : null);
    if (!match || !rendererLanguages.includes(match[1])) return;
    let output=details.querySelector<HTMLElement>(".renderer-output");
    if (output?.dataset.source===source && (match[1]!=="plantuml" || output.dataset.service===(appearance.plantuml_service ?? ""))) return;
    if (!output) { output=document.createElement("div"); output.className="renderer-output vditor-reset"; details.append(output); }
    output.dataset.source=source; details.open=true; get("source-blocks").setAttribute("open","");
    output.dataset.service=appearance.plantuml_service ?? "";
    document.body.classList.add("has-renderers");
    void renderFence(output,match[1],match[2],plantuml);
}
function headingElements() { return [...(preferences.mode === "sv" ? editor.vditor.preview.previewElement : active()).querySelectorAll<HTMLElement>("h1,h2,h3,h4,h5,h6")]; }
function renderOutline() {
    outline.replaceChildren();
    const renderedSource = document.createElement("div");
    renderedSource.innerHTML = editor.vditor.lute.Md2VditorIRDOM(display(latest));
    const sourceHeadings = [...renderedSource.querySelectorAll("h1,h2,h3,h4,h5,h6")];
    for (const [index,heading] of headingElements().entries()) {
        let text = heading.textContent ?? "";
        if (preferences.mode === "ir") {
            const rendered = document.createElement("div");
            rendered.innerHTML = sanitize(editor.vditor.lute.Md2HTML(serializeBlock(heading)));
            text = rendered.textContent ?? "";
        }
        const label = text.replace(/^#+\s*/,"").replace(/\s*\{#[^}]+\}\s*$/, "").trim();
        const identifier = label.toLowerCase().replace(/[^\p{L}\p{N}\s-]/gu, "").trim().replace(/\s+/g,"-");
        heading.dataset.anchor = identifier;
        const id = sourceHeadings[index]?.querySelector('[data-type="heading-id"]')?.textContent?.match(/\{#([^}]+)\}/)?.[1];
        if (id) heading.dataset.explicitAnchor = id; else delete heading.dataset.explicitAnchor;
        const button = document.createElement("button"); button.textContent = label;
        button.style.paddingLeft = `${Number(heading.tagName.slice(1))*10}px`;
        button.addEventListener("click",() => heading.scrollIntoView({block:"start"})); outline.append(button);
    }
}
function sourceSpans(): {start:number;end:number;label:string}[] {
    const spans: {start:number;end:number;label:string}[] = [];
    const frontmatter = latest.match(/^---\r?\n[\s\S]*?\r?\n---(?:\r?\n|$)/);
    if (frontmatter) spans.push({start:0,end:frontmatter[0].length,label:"Front matter"});
    for (const match of latest.matchAll(/(?:^<[^\r\n]+(?:\r?\n(?!(?:\r?\n))[^\r\n]+)*)|(?:^#{1,6}[^\r\n]*\{#[^}]+\}[^\r\n]*)|(?:^```[^\r\n]*\r?\n[\s\S]*?^```[^\r\n]*)|(?:^ {0,3}\[[^\]]+\]:[^\r\n]*(?:\r?\n[ \t]+[^\r\n]+)*)/gm)) {
        spans.push({start:match.index,end:match.index+match[0].length,label:match[0].split(/\r?\n/)[0].slice(0,80)});
    }
    const parsed=document.createElement("template"); parsed.innerHTML=editor.vditor.lute.Md2HTML(display(latest));
    const math=[...parsed.content.querySelectorAll(".lathe-math")].map(element=>element.textContent?.trim());
    const inlineCode=[...latest.matchAll(/(`+)[^\r\n]*?\1/g)].map(match=>({start:match.index,end:match.index+match[0].length}));
    for(const expression of new Set(math)) {
        if(!expression)continue;
        const escaped=expression.replace(/[.*+?^${}()|[\]\\]/g,"\\$&").replace(/\n/g,"\\r?\\n");
        for(const match of latest.matchAll(new RegExp(`(?<!\\\\)\\$\\$\\s*${escaped}\\s*\\$\\$|(?<!\\\\)\\$(?!\\$)${escaped}\\$`,"g"))) {
            const start=match.index,end=start+match[0].length;
            if([...spans,...inlineCode].some(span=>start<span.end && end>span.start))continue;
            spans.push({start,end,label:match[0].startsWith("$$")?"Block math":"Inline math"});
        }
    }
    return spans.sort((left,right)=>left.start-right.start);
}
function updateSourceControls(text:string, edited?:{field:HTMLTextAreaElement;start:number;end:number;text:string}) {
    // Diffing identical adjacent source blocks can assign a textarea edit to the
    // wrong occurrence; retain its known replacement scope.
    const changes = edited ? [edited] : separateChanges(latest,text).reverse();
    for (const [field,span] of sourceControls) {
        if (field === edited?.field) span.end = edited.start + edited.text.length;
        else for (const change of changes) {
            span.start = span.start >= change.start && span.start < change.end ? change.start : transformOffset(span.start,change);
            span.end = transformOffset(span.end,change);
        }
        if (field === edited?.field || bridge.composing && field === sourceEditor) continue;
        const next = display(text.slice(span.start,span.end));
        if (field.value === next) continue;
        let start = field.selectionStart, end = field.selectionEnd;
        const direction = field.selectionDirection;
        for (const change of separateChanges(field.value,next).reverse()) { start = transformOffset(start,change); end = transformOffset(end,change); }
        field.value = next; field.setSelectionRange(start,end,direction);
    }
    latest = text;
}
function renderSourceBlocks() {
    if(ready && !bridge.composing && !sourceEditor)renderMath(renderedRoot());
    if (sourceEditor && document.activeElement === sourceEditor) return;
    const blocks = get("blocks"), retained = new Map(sourceControls); sourceEditor = null;
    let index = 0;
    for (const span of sourceSpans()) {
        const existing = [...retained].find(([_field,previous])=>previous.start===span.start && previous.end===span.end);
        if (existing) {
            const [field] = existing, details = field.parentElement!;
            details.querySelector("summary")!.textContent = span.label;
            renderSourceFence(details,span);
            if (blocks.children[index] !== details) blocks.insertBefore(details,blocks.children[index] ?? null);
            retained.delete(field); index++; continue;
        }
        const details = document.createElement("details"), label = document.createElement("summary"), field = document.createElement("textarea");
        label.textContent = span.label; field.value = display(latest.slice(span.start,span.end)); field.readOnly = !bridge.writable || bridge.blocked || preferences.mode === "preview";
        sourceControls.set(field,span);
        field.addEventListener("focus",() => { sourceEditor = field; });
        field.addEventListener("beforeinput",event => {
            const input=event as InputEvent;
            if (!bridge.writable || bridge.blocked || preferences.mode === "preview") { event.preventDefault(); return; }
            const inserted=input.data ?? (["insertLineBreak","insertParagraph"].includes(input.inputType) ? "\n" : input.dataTransfer?.getData("text/plain"));
            if (!input.cancelable || inserted === undefined) return;
            const next=field.value.slice(0,field.selectionStart)+inserted+field.value.slice(field.selectionEnd);
            const source=sourceFieldValue(field,next);
            if (!withinBudget(source)) { event.preventDefault(); status.textContent="Source block exceeds the visual document limit."; }
        });
        field.addEventListener("input",() => {
            if (!bridge.writable || bridge.blocked || preferences.mode === "preview") return;
            const blockSource=fromDisplay(latest.slice(span.start,span.end),field.value);
            const source = latest.slice(0,span.start) + blockSource + latest.slice(span.end);
            updateSourceControls(source,{field,start:span.start,end:span.end,text:blockSource});
            bridge.input(source);
            if (bridge.blocked) { permissions(); return; }
            projecting = true; editor.setValue(display(source),true); lastSerialized=editor.getValue(); projection.reset(source,lastSerialized); projecting = false; renderOutline(); updateFind();
        });
        field.addEventListener("blur",() => { sourceEditor = null; renderSourceBlocks(); });
        details.append(label,field); renderSourceFence(details,span); blocks.insertBefore(details,blocks.children[index] ?? null); index++;
    }
    for (const field of retained.keys()) { field.parentElement?.remove(); sourceControls.delete(field); }
    get("source-blocks").hidden = !blocks.childElementCount;
}
function sourceFieldValue(field:HTMLTextAreaElement,value:string):string {
    const span=sourceControls.get(field);
    if (!span) return latest;
    return latest.slice(0,span.start)+fromDisplay(latest.slice(span.start,span.end),value)+latest.slice(span.end);
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
    if (!active().contains(document.activeElement) || bridge.composing) return;
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
    updateSourceControls(source); intent = null; projecting = true;
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
    if(container.contains(event.target as Node) && event.clipboardData?.files.length) { event.preventDefault();event.stopImmediatePropagation();void importFiles([...event.clipboardData.files]);return; }
    if (event.target instanceof HTMLTextAreaElement && get("source-blocks").contains(event.target)) {
        event.preventDefault(); event.stopImmediatePropagation();
        if (!bridge.writable || bridge.blocked || preferences.mode === "preview") return;
        const field=event.target, text=event.clipboardData?.getData("text/plain") ?? "";
        const next=field.value.slice(0,field.selectionStart)+display(text)+field.value.slice(field.selectionEnd);
        if (!withinBudget(sourceFieldValue(field,next))) { status.textContent="Paste exceeds the visual document limit."; return; }
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
    if (container.contains(event.target as Node)) { event.preventDefault(); event.stopImmediatePropagation();if(event.dataTransfer?.files.length)void importFiles([...event.dataTransfer.files]); }
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
    lineNumbers();
},true);
document.addEventListener("wheel",()=>readingGeneration++,true);
document.addEventListener("touchmove",()=>readingGeneration++,true);
document.addEventListener("keydown",event=>{if(["ArrowUp","ArrowDown","PageUp","PageDown","Home","End"].includes(event.key))readingGeneration++;},true);

const findBar = get("find"), query = get("query") as HTMLInputElement, matchCase = get("case") as HTMLInputElement;
let matches: Range[] = [], currentMatch = -1;
function updateFind() {
    matches = []; currentMatch = -1;
    (CSS as any).highlights?.delete("current-find");
    const root = active(), nodes = textNodes(root), text = nodes.map(node => node.data).join("");
    const needle = matchCase.checked ? query.value : query.value.toLowerCase(), haystack = matchCase.checked ? text : text.toLowerCase();
    const starts:number[] = [], ends:number[] = [];
    if (!matchCase.checked && !findBar.hidden && needle) {
        let originalOffset = 0;
        // Whole-string lowercasing retains contextual forms such as final sigma;
        // code-point lengths map expansion positions back to their original text.
        for (const character of text) {
            for (let index=0; index<character.toLowerCase().length; index++) {
                starts.push(originalOffset); ends.push(originalOffset+character.length);
            }
            originalOffset += character.length;
        }
    }
    const point = (offset:number): [Node,number] => {
        for (const node of nodes) { if (offset <= node.length) return [node,offset]; offset -= node.length; }
        return [root,root.childNodes.length];
    };
    if (!findBar.hidden && needle) for (let offset = haystack.indexOf(needle); offset >= 0 && matches.length < 10000; offset = haystack.indexOf(needle,offset+needle.length)) {
        const range = new Range(), [start,startOffset] = point(matchCase.checked ? offset : starts[offset]), [end,endOffset] = point(matchCase.checked ? offset+needle.length : ends[offset+needle.length-1]);
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
get("copy-html").addEventListener("click",() => {
    const template=document.createElement("template");template.innerHTML=sanitize(editor.getHTML(),false,true);
    for(const image of template.content.querySelectorAll("img[data-lathe-image],img[data-lathe-original]")) {
        image.setAttribute("src",image.getAttribute("data-lathe-original")??image.getAttribute("data-lathe-image")??"");image.removeAttribute("data-lathe-original");image.removeAttribute("data-lathe-image");
    }
    bridge.emit({kind:"copy",text:template.innerHTML});
});
for(const [name,values] of [["content-theme",themes.content],["code-theme",themes.code]] as const) {
    const select=get(name) as HTMLSelectElement;
    for(const value of values){const option=document.createElement("option");option.value=value;option.textContent=value;select.append(option);}
    select.addEventListener("change",()=>{if(name==="content-theme")preferences.content_theme=select.value||null;else preferences.code_theme=select.value||null;persist();applyAppearance();});
}
get("insert-image").addEventListener("click",()=>importAfterFlush({kind:"pick_images",clipboard:false}));
window.addEventListener("beforeunload",()=>{for(const url of imageUrls)URL.revokeObjectURL(url);});
(window as any).latheRich = {openFind,mode,format,commit,projection,sanitize,
    nativePaste:()=>importAfterFlush({kind:"pick_images",clipboard:true}), nativeDrop:()=>importAfterFlush({kind:"pick_images",clipboard:false,drop:true}),
    get editor() {return editor;}, get ready() {return ready;},
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
