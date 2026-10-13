import {test, before, after} from "node:test";
import assert from "node:assert/strict";
import {readFile} from "node:fs/promises";
import {createServer} from "node:http";
import {chromium} from "playwright";

let server, browser, origin;
const corpus = JSON.parse(await readFile(new URL("../../../project-docs/imp-plans/markdown-editor-baseline/fixtures.json",import.meta.url),"utf8"));
const rust = await readFile(new URL("../src/browser.rs",import.meta.url),"utf8");
const csp = rust.match(/CONTENT_SECURITY_POLICY: &str = "([^"]+)"/)[1];
const resources = new Set(["index.html","application.js","application.css","rich.js","vendor/vditor.js","vendor/lute.js","vendor/en_US.js","vendor/purify.js","vendor/vditor.css","vendor/icons.js"]);
before(async () => {
    server = createServer(async (request,response) => {
        const path = new URL(request.url,"http://localhost").pathname.slice(1);
        if (!resources.has(path)) {response.statusCode=404; response.end(); return;}
        response.setHeader("Content-Security-Policy",csp);
        response.setHeader("Content-Type",path.endsWith(".js") ? "text/javascript" : path.endsWith(".css") ? "text/css" : "text/html");
        response.end(await readFile(new URL(path,import.meta.url)));
    });
    await new Promise(resolve => server.listen(0,"127.0.0.1",resolve));
    origin = `http://127.0.0.1:${server.address().port}`;
    browser = await chromium.launch({headless:true,...(process.env.PLAYWRIGHT_CHROMIUM ? {executablePath:process.env.PLAYWRIGHT_CHROMIUM} : {})});
});
after(async () => {await browser?.close(); server?.closeAllConnections(); await new Promise(resolve => server?.close(resolve));});
async function open(source,mode="ir",writable=true) {
    const context = await browser.newContext(), page = await context.newPage();
    const errors = [], external = [];
    page.on("pageerror",error => errors.push(error.message));
    await context.route("**/*",route => {
        if (route.request().url().startsWith(origin+"/")) return route.continue();
        external.push(route.request().url()); return route.abort();
    });
    await page.goto(`${origin}/index.html?session=browser-test&document=1`);
    await page.waitForFunction(() => window.latheMarkdown);
    await page.evaluate(({source,mode,writable}) => {
        window.testMessages=[];
        window.latheMarkdown.send = message => window.testMessages.push(message);
        window.latheMarkdown.receive({kind:"preferences",preferences:{mode,outline:false,wrap:true,scroll:0}});
        window.latheMarkdown.receive({kind:"document",revision:0,text:source,writable,draft:null});
    },{source,mode,writable});
    await page.waitForFunction(() => window.latheRich?.ready);
    const close=context.close.bind(context);
    context.close=async()=>{await close();assert.deepEqual(errors,[],"Browser exceptions");};
    return {page,context,errors,external};
}
async function acknowledge(page) {
    await page.evaluate(() => {
        const bridge=window.latheMarkdown;
        if (bridge.pending) bridge.receive({kind:"accepted",request:bridge.pending.request,sequence:bridge.pending.sequence,revision:bridge.revision+1,text:bridge.pending.after});
    });
}
async function selectText(page,text) {
    await page.evaluate(text => {
        const editor=window.latheRich.editor, root=editor.vditor[editor.getCurrentMode()].element;
        root.focus();
        const walker=document.createTreeWalker(root,NodeFilter.SHOW_TEXT);
        while (walker.nextNode()) {
            const node=walker.currentNode, index=node.data.indexOf(text);
            if (index < 0) continue;
            window.getSelection().setBaseAndExtent(node,index,node,index+text.length); return;
        }
        throw new Error(`Text not found: ${text}`);
    },text);
}
test("real pinned runtime mounts rich content with production CSP", async () => {
    const {page,context,errors,external} = await open("# Heading\n\n**bold** text\n");
    assert.equal(await page.evaluate(() => window.latheRich.editor.version),"3.11.2");
    assert.equal(await page.locator(".vditor-ir strong").innerText(),"bold");
    assert.deepEqual(errors,[]); assert.deepEqual(external,[]);
    await context.close();
});
test("all preservation fixtures open without emitting document mutations in every mode", async () => {
    for (const fixture of corpus.roundTrips) for (const mode of ["ir","wysiwyg","sv","preview"]) {
        const {page,context,errors} = await open(fixture.markdown,mode);
        assert.equal(await page.evaluate(() => window.latheMarkdown.draft),fixture.markdown,`${fixture.id}/${mode}`);
        assert.deepEqual(await page.evaluate(() => window.testMessages.filter(message => ["draft","edit","undo","redo"].includes(message.kind))),[],`${fixture.id}/${mode}`);
        assert.deepEqual(errors,[],`${fixture.id}/${mode}`); await context.close();
    }
});
test("ordinary rich DOM edits preserve adjacent original spans in every editing mode", async () => {
    for (const fixture of corpus.roundTrips) for (const mode of ["ir","wysiwyg","sv"]) {
        const source = fixture.markdown + "\n\nordinary editable sentence";
        const {page,context} = await open(source,mode);
        await selectText(page,"editable");
        await page.keyboard.insertText("changed");
        await page.waitForFunction(() => window.latheMarkdown.draft.includes("changed"));
        assert.equal(await page.evaluate(() => window.latheMarkdown.draft),source.replace("editable","changed"),`${fixture.id}/${mode}`);
        await acknowledge(page); await selectText(page,"changed"); await page.keyboard.insertText("repeated");
        try { await page.waitForFunction(()=>window.latheMarkdown.draft.includes("repeated"),{},{timeout:3000}); }
        catch(error) { throw new Error(`${fixture.id}/${mode}: ${JSON.stringify(await page.evaluate(()=>({source:window.latheMarkdown.draft,canonical:window.latheRich.projection.canonical,value:window.latheRich.editor.getValue(),status:document.querySelector('#status').textContent,dom:window.latheRich.editor.vditor[window.latheRich.editor.getCurrentMode()].element.innerHTML})))}`,{cause:error}); }
        assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source.replace("editable","repeated"),`${fixture.id}/${mode}/second edit`);
        await context.close();
    }
});

test("mode, viewer, outline, Find, wrapping and reset never serialize an authoritative edit",async () => {
    const source=corpus.roundTrips.map(fixture=>fixture.markdown).join("\n\n");
    const {page,context}=await open(source);
    for (const mode of ["wysiwyg","sv","preview","ir"]) {
        await page.selectOption("#mode",mode); await page.waitForFunction(mode=>window.latheRich.ready && document.querySelector("#mode").value===mode,mode);
        await page.click("#outline-toggle"); await page.click("#wrap"); await page.click("#find-open");
        await page.fill("#query","Heading"); await page.click("#next"); await page.click("#previous"); await page.check("#case"); await page.keyboard.press("Escape");
    }
    await page.click("#reset"); await page.waitForFunction(()=>window.latheRich.ready);
    assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source);
    assert.deepEqual(await page.evaluate(()=>window.testMessages.filter(message=>["draft","edit","undo","redo"].includes(message.kind))),[]);
    await context.close();
});

test("reference formatting controls edit selected blocks through real Vditor toolbar events",async () => {
    for (const mode of ["ir","wysiwyg","sv"]) for (const [control,expected] of [["bold","**Target**"],["italic","*Target*"],["strike","~~Target~~"],["inline-code","`Target`"],["list","* Target"],["ordered-list","1. Target"],["check","[ ]"],["quote","> Target"],["code","```"],["line","---"],["link","]("],["table","|"]]) {
        const source="Before untouched\n\nTarget\n\nAfter untouched";
        const {page,context}=await open(source,mode); await selectText(page,"Target");
        await page.locator(`[data-type="${control}"]`).first().click();
        try { await page.waitForFunction(()=>window.latheMarkdown.pending,{},{timeout:3000}); }
        catch (error) { throw new Error(`${mode}/${control}: ${await page.evaluate(()=>({value:window.latheRich.editor.getValue(),status:document.querySelector('#status').textContent})).then(JSON.stringify)}`,{cause:error}); }
        const draft=await page.evaluate(()=>window.latheMarkdown.draft);
        assert(draft.includes(expected),`${mode}/${control}: ${JSON.stringify(draft)}`);
        assert(draft.startsWith("Before untouched\n\n"),`${mode}/${control} prefix`);
        assert(draft.endsWith("\n\nAfter untouched"),`${mode}/${control} suffix: ${JSON.stringify(draft)}`);
        await context.close();
    }
});

test("rich HTML/SVG and clipboard HTML stay inert and preserve untouched source",async () => {
    const source='<div onclick="window.__executed=1"><svg id="document-svg" onload="window.__executed=1"><script>window.__executed=1</script></svg><img src="https://foreign.invalid/x" onerror="window.__executed=1"><input type="file"><a href="javascript:window.__executed=1">hostile</a></div>\n\nPaste here';
    for (const mode of ["ir","wysiwyg","sv","preview"]) {
        const {page,context,external}=await open(source,mode);
        assert.equal(await page.evaluate(()=>window.__executed),undefined);
        assert.equal(await page.locator(".vditor-ir script,.vditor-wysiwyg script,#viewer script,#document-svg,[onclick],[onerror],[onload],input[type=file]").count(),0);
        assert.equal(await page.evaluate(()=>window.latheRich.sanitize('<svg><script>evil()</script></svg>').includes('<svg')),false);
        assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source);
        if (mode!=="preview") {
            await selectText(page,"Paste here");
            await page.evaluate(() => {
                const transfer=new DataTransfer(); transfer.setData("text/html",'<p>Pasted <b>bold</b></p><script>window.__executed=1</script><svg onload="window.__executed=1"></svg>'); transfer.setData("text/plain","Pasted bold");
                document.activeElement.dispatchEvent(new ClipboardEvent("paste",{bubbles:true,cancelable:true,clipboardData:transfer}));
            });
            assert.equal(await page.evaluate(()=>window.__executed),undefined);
            assert((await page.evaluate(()=>window.latheMarkdown.draft)).startsWith(source.split("\n\n")[0]));
        }
        assert.deepEqual(external,[]); await context.close();
    }
});

test("document IDs cannot replace trusted source controls or alter rendered document content",async () => {
    const source='<div id="blocks">Preserved HTML sentinel</div>\n\n# Heading {#explicit}\n\nOrdinary text';
    for (const mode of ["ir","wysiwyg","sv","preview"]) {
        const {page,context}=await open(source,mode);
        assert((await page.locator('#source-blocks > #blocks textarea').count())>=2);
        if (mode==='preview') assert.equal(await page.locator('#viewer #blocks').innerText(),'Preserved HTML sentinel');
        await page.click('#copy-markdown');
        assert.equal(await page.evaluate(()=>window.testMessages.filter(message=>message.kind==='copy').at(-1).text),source);
        assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source);
        assert.equal(await page.evaluate(()=>window.testMessages.some(message=>message.kind==='edit')),false);
        await context.close();
    }
});

test("viewer and read-only projections retain copy, Find and source but reject edits",async () => {
    for (const [mode,writable] of [["preview",true],["ir",false],["wysiwyg",false],["sv",false]]) {
        const source="# Viewer\n\n**Selected** text\n\n- [x] Checked task\n- [ ] Unchecked task";
        const {page,context}=await open(source,mode,writable);
        await page.click("#copy-markdown"); await page.click("#copy-html");
        const copies=await page.evaluate(()=>window.testMessages.filter(message=>message.kind==="copy"));
        assert.equal(copies[0].text,source); assert(copies[1].text.includes("<strong>Selected</strong>"));
        assert.deepEqual(await page.evaluate(html=>[...new DOMParser().parseFromString(html,'text/html').querySelectorAll('input[type=checkbox]')].map(input=>({checked:input.checked,disabled:input.disabled})),copies[1].text),[{checked:true,disabled:true},{checked:false,disabled:true}]);
        if (mode==="preview") assert.deepEqual(await page.locator('#viewer input[type=checkbox]').evaluateAll(inputs=>inputs.map(input=>({checked:input.checked,disabled:input.disabled}))),[{checked:true,disabled:true},{checked:false,disabled:true}]);
        await page.click("#find-open"); await page.fill("#query","Selected"); assert.equal(await page.locator("#count").innerText(),"1 matches");
        assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source);
        assert.equal(await page.evaluate(()=>window.testMessages.some(message=>message.kind==="edit")),false);
        await context.close();
    }
});

test("Find counts, previous/next wrapping, case and Escape preserve document and history",async () => {
    for (const mode of ["ir","wysiwyg","sv","preview"]) {
        const source="Word word WORD", {page,context}=await open(source,mode);
        await page.click("#find-open"); await page.fill("#query","word");
        assert.equal(await page.locator("#count").innerText(),"3 matches");
        await page.click("#previous"); assert.equal(await page.locator("#count").innerText(),"3 of 3");
        await page.click("#next"); assert.equal(await page.locator("#count").innerText(),"1 of 3");
        await page.check("#case"); assert.equal(await page.locator("#count").innerText(),"1 matches");
        await page.keyboard.press("Escape"); assert(await page.locator("#find").isHidden());
        assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source);
        assert.equal(await page.evaluate(()=>window.testMessages.some(message=>["draft","edit","undo","redo"].includes(message.kind))),false);
        await context.close();
    }
});

test("editable source controls copy selections through the host and paste without changing CRLF",async () => {
    const source="---\r\ntitle: one\r\n---\r\n\r\nBody", {page,context}=await open(source);
    await page.locator('#source-blocks > summary').click(); await page.locator('#blocks details').first().locator('summary').click();
    await page.locator('#blocks textarea').first().evaluate(field=>{field.focus(); const start=field.value.indexOf('one'); field.setSelectionRange(start,start+3); field.dispatchEvent(new ClipboardEvent('copy',{bubbles:true,cancelable:true,clipboardData:new DataTransfer()}));});
    assert.equal(await page.evaluate(()=>window.testMessages.filter(message=>message.kind==='copy').at(-1)?.text),'one');
    await page.locator('#blocks textarea').first().evaluate(field=>{const data=new DataTransfer(); data.setData('text/plain','two'); field.dispatchEvent(new ClipboardEvent('paste',{bubbles:true,cancelable:true,clipboardData:data}));});
    await page.waitForFunction(()=>window.latheMarkdown.pending);
    assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source.replace('one','two'));
    await context.close();
});

test("headings, emoji, indentation, insertion, nested lists and task toggles preserve surrounding spans",async () => {
    for (const mode of ["ir","wysiwyg","sv"]) for (const control of ["headings","emoji"]) {
        const source="Before intact\n\nTarget\n\nAfter intact", {page,context}=await open(source,mode);
        await selectText(page,"Target"); await page.locator(`[data-type=${control}]`).first().click();
        await page.locator(control==="headings" ? '[data-tag=h2]' : '.vditor-emojis button[data-key=smile]').click();
        await page.waitForFunction(()=>window.latheMarkdown.pending);
        const draft=await page.evaluate(()=>window.latheMarkdown.draft);
        assert(draft.includes(control==="headings" ? "## Target":"😄"),`${mode}/${control}: ${draft}`);
        assert(draft.startsWith("Before intact\n\n") && draft.endsWith("\n\nAfter intact")); await context.close();
    }
    for (const mode of ["ir","wysiwyg"]) for (const control of ["insert-before","insert-after"]) {
        const source="Before intact\n\nTarget\n\nAfter intact", {page,context}=await open(source,mode);
        await selectText(page,"Target"); await page.locator(`[data-type=${control}]`).click(); await page.keyboard.insertText("Inserted");
        await page.waitForFunction(()=>window.latheMarkdown.draft.includes("Inserted"));
        const draft=await page.evaluate(()=>window.latheMarkdown.draft);
        assert(draft.startsWith("Before intact\n\n") && draft.endsWith("\n\nAfter intact"),`${mode}/${control}: ${draft}`);
        assert(control==="insert-before" ? draft.indexOf("Inserted") < draft.indexOf("Target") : draft.indexOf("Inserted") > draft.indexOf("Target"));
        await context.close();
    }
    for (const mode of ["ir","wysiwyg"]) {
        const source="Before intact\n\n- parent\n- child\n- [ ] todo\n\nAfter intact", {page,context}=await open(source,mode);
        await selectText(page,"child"); await page.evaluate(()=>window.latheRich.editor.vditor[window.latheRich.editor.getCurrentMode()].element.dispatchEvent(new MouseEvent("mouseup",{bubbles:true})));
        await page.locator('[data-type=indent]').click(); await page.waitForFunction(()=>window.latheMarkdown.pending);
        assert.match(await page.evaluate(()=>window.latheMarkdown.draft),/\n[ \t]+[*-] child/);
        await acknowledge(page); await selectText(page,"child"); await page.evaluate(()=>window.latheRich.editor.vditor[window.latheRich.editor.getCurrentMode()].element.dispatchEvent(new MouseEvent("mouseup",{bubbles:true})));
        await page.locator('[data-type=outdent]').click(); await page.waitForFunction(()=>window.latheMarkdown.pending); await acknowledge(page);
        const root=mode==="ir" ? ".vditor-ir" : ".vditor-wysiwyg";
        await page.locator(`${root} input[type=checkbox]`).click(); await page.waitForFunction(()=>window.latheMarkdown.pending);
        const draft=await page.evaluate(()=>window.latheMarkdown.draft);
        assert.match(draft,/\[[xX]\] todo/); assert(draft.startsWith("Before intact\n\n") && draft.endsWith("\n\nAfter intact")); await acknowledge(page);
        await selectText(page,"child"); await page.keyboard.insertText("nested edited"); await page.waitForFunction(()=>window.latheMarkdown.draft.includes("nested edited"));
        assert((await page.evaluate(()=>window.latheMarkdown.draft)).endsWith("\n\nAfter intact")); await context.close();
    }
});

test("table cells and row/column operations retain adjacent source and use Vditor's table UI",async () => {
    for (const mode of ["ir","wysiwyg","sv"]) {
        const source="Before intact\n\n| left | right |\n| :--- | ---: |\n| Alpha | Beta |\n\nAfter intact", {page,context}=await open(source,mode);
        await selectText(page,"Alpha"); await page.keyboard.insertText("Edited cell"); await page.waitForFunction(()=>window.latheMarkdown.pending);
        assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source.replace("Alpha","Edited cell")); await acknowledge(page);
        const cell=page.locator('.vditor-reset td').first(); await cell.waitFor();
        await page.click("#wrap"); assert.equal(await cell.evaluate(cell=>getComputedStyle(cell).whiteSpace),"nowrap");
        assert.equal(await cell.evaluate(cell=>getComputedStyle(cell.closest('table')).overflowX),"auto");
        await page.click("#wrap"); assert.equal(await cell.evaluate(cell=>getComputedStyle(cell).whiteSpace),"normal");
        assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source.replace("Alpha","Edited cell"));
        if (mode==="wysiwyg") {
            await page.locator('.vditor-wysiwyg td').filter({hasText:"Edited cell"}).click();
            try { await page.locator('.vditor-wysiwyg .vditor-panel [data-type=insertRow]').last().click({timeout:3000}); }
            catch(error) { throw new Error(JSON.stringify(await page.evaluate(()=>({style:window.latheRich.editor.vditor.wysiwyg.popover.getAttribute('style'),rect:window.latheRich.editor.vditor.wysiwyg.popover.getBoundingClientRect().toJSON(),selection:window.getSelection()?.anchorNode?.parentElement?.outerHTML,dom:window.latheRich.editor.vditor.wysiwyg.element.parentElement.outerHTML}))),{cause:error}); }
            await page.waitForFunction(()=>window.latheMarkdown.pending);
            const draft=await page.evaluate(()=>window.latheMarkdown.draft);
            assert(draft.startsWith("Before intact\n\n") && draft.endsWith("\n\nAfter intact")); assert(draft.split("\n").filter(line=>line.startsWith("|")).length>=4);
            await acknowledge(page);
            for (const control of ["insertColumn","deleteColumn","deleteRow"]) {
                await page.locator('.vditor-wysiwyg td').first().click();
                await page.locator(`.vditor-wysiwyg .vditor-panel [data-type=${control}]`).last().click();
                await page.waitForFunction(()=>window.latheMarkdown.pending);
                const draft=await page.evaluate(()=>window.latheMarkdown.draft);
                assert(draft.startsWith("Before intact\n\n") && draft.endsWith("\n\nAfter intact"),control);
                await acknowledge(page);
            }
        }
        await context.close();
    }
});

test("duplicate neighboring blocks keep the intentional edit at its original source position",async () => {
    for (const mode of ["ir","wysiwyg","sv"]) {
        const source="same\n\nsame\n\nTarget\n\nsame\n\nsame", {page,context}=await open(source,mode);
        await selectText(page,"Target"); await page.keyboard.insertText("same"); await page.waitForFunction(()=>window.latheMarkdown.pending);
        assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source.replace("Target","same"));
        await acknowledge(page); await context.close();
    }
});

test("selection and scroll survive native source updates, mode switches and composition",async () => {
    const source="# Heading\n\n"+"paragraph\n\n".repeat(50)+"hello selected text\n\nTail", {page,context}=await open(source);
    await selectText(page,"selected");
    await page.evaluate(()=>window.latheRich.editor.vditor.ir.element.scrollTop=137);
    await page.evaluate(()=>window.latheMarkdown.receive({kind:"document",revision:1,text:"prefix\n\n"+window.latheMarkdown.base,writable:true,draft:null}));
    assert.equal(await page.evaluate(()=>window.getSelection().toString()),"selected");
    assert.equal(await page.evaluate(()=>window.latheRich.editor.vditor.ir.element.scrollTop),137);
    for (const mode of ["wysiwyg","sv","ir"]) {
        await page.selectOption("#mode",mode); await page.waitForFunction(()=>window.latheRich.ready);
        assert.equal(await page.evaluate(()=>window.getSelection().toString()),"selected");
        assert.equal(await page.evaluate(()=>window.latheRich.editor.vditor[window.latheRich.editor.getCurrentMode()].element.scrollTop),137);
    }
    await page.evaluate(()=>{
        const root=window.latheRich.editor.vditor.ir.element;
        root.dispatchEvent(new CompositionEvent("compositionstart",{bubbles:true}));
        window.composingNode=root.firstChild;
        window.latheRich.replaceText("hello","hello 中文");
        window.latheMarkdown.receive({kind:"document",revision:2,text:window.latheMarkdown.base.replace("Tail","External tail"),writable:true,draft:null});
        if(root.firstChild!==window.composingNode)throw new Error("Composition DOM replaced");
        root.dispatchEvent(new CompositionEvent("compositionend",{bubbles:true,data:"中文"}));
    });
    await page.waitForFunction(()=>!window.latheMarkdown.composing && window.latheMarkdown.pending);
    assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),"prefix\n\n"+source.replace("hello","hello 中文").replace("Tail","External tail"));
    await context.close();
});

test("unsupported source block composition ends, rebases host updates and releases save barriers",async () => {
    for(const newline of ["\n","\r\n"]) {
    const source=['---','title: "Original"','---','','Body'].join(newline);
    const {page,context}=await open(source); await page.locator('#source-blocks > summary').click(); await page.locator('#blocks details').first().locator('summary').click();
    const field=page.locator('#blocks textarea').first(); await field.focus();
    await field.evaluate(field=>{
        field.dispatchEvent(new CompositionEvent("compositionstart",{bubbles:true}));
        window.composingSourceField=field; field.value=field.value.replace("Original","中文"); field.dispatchEvent(new InputEvent("input",{bubbles:true,inputType:"insertCompositionText",data:"中文"}));
        window.latheMarkdown.receive({kind:"document",revision:1,text:window.latheMarkdown.base.replace("Body","External body"),writable:true,draft:null});
        window.latheMarkdown.receive({kind:"flush",request:90});
        if(!field.isConnected || field!==window.composingSourceField)throw new Error("Source composition DOM replaced");
        field.dispatchEvent(new CompositionEvent("compositionend",{bubbles:true,data:"中文"}));
    });
    await page.waitForFunction(()=>!window.latheMarkdown.composing && window.latheMarkdown.pending);
    assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source.replace("Original","中文").replace("Body","External body"));
    await acknowledge(page);
    assert(await page.evaluate(()=>window.testMessages.some(message=>message.kind==="flushed" && message.request===90)));
    await context.close();
    }
});

test("outline and internal anchors navigate while permitted external and relative links reach the host",async () => {
    const source='# Heading {#explicit}\n\n[anchor](#explicit) [external](https://example.com) [relative](other%20file.markdown#heading)\n\n'+"paragraph\n\n".repeat(50)+'## Last heading';
    for(const mode of ["ir","wysiwyg","sv","preview"]) {
        const {page,context}=await open(source,mode); await page.click("#outline-toggle"); await page.waitForFunction(()=>document.querySelectorAll('#outline button').length===2);
        await page.locator('#outline button').last().click();
        const root=mode==="preview" ? '#viewer' : mode==="sv" ? '.vditor-preview' : mode==="ir" ? '.vditor-ir' : '.vditor-wysiwyg';
        if(mode==="ir") {
            await page.locator(`${root} [data-type=a]`).filter({hasText:"anchor"}).click();
            await page.locator(`${root} [data-type=a]`).filter({hasText:"external"}).click();
            await page.locator(`${root} [data-type=a]`).filter({hasText:"relative"}).click();
        } else {
            await page.locator(`${root} a[href='#explicit']`).first().click();
            await page.locator(`${root} a[href='https://example.com']`).first().click(); await page.locator(`${root} a[href='other%20file.markdown#heading']`).first().click();
        }
        assert.deepEqual(await page.evaluate(()=>window.testMessages.filter(message=>message.kind==="open_link").map(message=>message.url)),["https://example.com","other%20file.markdown#heading"]);
        assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source); await context.close();
    }
});
