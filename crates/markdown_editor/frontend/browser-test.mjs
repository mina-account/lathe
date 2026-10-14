import {test, before, after} from "node:test";
import assert from "node:assert/strict";
import {readFile,writeFile,mkdtemp,stat} from "node:fs/promises";
import {tmpdir} from "node:os";
import {join} from "node:path";
import {createServer} from "node:http";
import {chromium} from "playwright";

let server, browser, origin;
const corpus = JSON.parse(await readFile(new URL("../../../project-docs/imp-plans/markdown-editor-baseline/fixtures.json",import.meta.url),"utf8"));
const rust = await readFile(new URL("../src/browser.rs",import.meta.url),"utf8");
const csp = rust.match(/CONTENT_SECURITY_POLICY: &str = "([^"]+)"/)[1];
const resources = new Set(["index.html","application.js","application.css","rich.js","vendor/vditor.js","vendor/lute.js","vendor/en_US.js","vendor/purify.js","vendor/vditor.css","vendor/icons.js"]);
before(async () => {
    for (const {path} of JSON.parse(await readFile(new URL("vendor/manifest.json",import.meta.url),"utf8"))) resources.add(path);
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
    page.on("pageerror",error => errors.push(error.stack));
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

test("normalization boundaries preserve autolink markers and hard breaks through edits and flush",async()=>{
    for (const mode of ["ir","wysiwyg","sv"]) for (const newline of ["\n","\r\n"]) for (const [block,target] of [
        ["<https://example.com/target>","target"],["<target@example.com>","com"],
        ["first  \nnext","first"],["__target__ and _keep_","target"],
        ["before &amp; target and &#65;","target"],["target\n=======","target"],["target\n-------","target"],
    ]) {
        const source=("Before\n\n"+block+"\n\nTail").replace(/\n/g,newline), {page,context}=await open(source,mode);
        await selectText(page,target); await page.keyboard.insertText("CHANGED"); await page.evaluate(()=>window.latheRich.commit());
        assert.equal(await page.evaluate(()=>window.latheMarkdown.blocked),false,`${mode}: ${block}`);
        assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source.replace(target,"CHANGED"),`${mode}: ${block}`);
        await acknowledge(page);
        await selectText(page,"CHANGED"); await page.keyboard.insertText("SECOND"); await page.evaluate(()=>window.latheRich.commit());
        assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source.replace(target,"SECOND"),`${mode}: repeated ${block}`);
        await acknowledge(page); await page.evaluate(()=>window.latheMarkdown.receive({kind:"flush",request:994}));
        assert.deepEqual(await page.evaluate(()=>({blocked:window.latheMarkdown.blocked,flushed:window.testMessages.some(message=>message.kind==="flushed"&&message.request===994),same:window.latheMarkdown.base===window.latheRich.projection.source})),{blocked:false,flushed:true,same:true});
        await context.close();
    }
});

test("every frozen local renderer has output beside its preserved editable source",async()=>{
    const inputs={mermaid:"graph TD; A-->B",graphviz:"digraph G { a -> b; }",echarts:'{"xAxis":{"data":["A","B"]},"yAxis":{},"series":[{"type":"bar","data":[1,2]}]}',abc:"X:1\nT:Example\nM:4/4\nK:C\nCDEF GABc|",flowchart:"st=>start: Start\ne=>end: End\nst->e",markmap:"# Root\n\n- Child",smiles:"CCO",math:"\\ce{H2O} + x^2",mindmap:"- Root\n    - Child"};
    for (const [language,input] of Object.entries(inputs)) {
        const source=`Before\n\n\`\`\`${language}\n${input}\n\`\`\`\n\nTail`, {page,context,external}=await open(source,"preview");
        await page.waitForSelector(`[data-renderer="${language}"][data-state="ready"]`,{timeout:20000});
        assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source);
        assert.equal(await page.locator(`[data-renderer="${language}"] svg, [data-renderer="${language}"] .katex`).count()>0,true,language);
        const text=await page.locator(`[data-renderer="${language}"]`).textContent();
        if(language==="mermaid" || language==="graphviz") { assert.match(text,/a|A/,language); assert.match(text,/b|B/,language); }
        if(language==="markmap" || language==="mindmap") { assert.match(text,/Root/); assert.match(text,/Child/); }
        if(language==="flowchart") { assert.match(text,/Start/); assert.match(text,/End/); }
        if(language==="abc") assert.match(text,/Example/);
        if(language==="smiles") assert.match(text,/O/);
        if(language==="math") assert.match(text,/H.*2.*O/);
        if(language==="math" || language==="markmap") assert.equal(await page.evaluate(()=>window.katex.version),"0.16.9");
        if(language==="echarts") { assert.match(text,/A/); assert.match(text,/B/); assert(await page.locator('[data-renderer="echarts"] svg path').count()>2); }
        assert.deepEqual(external,[]); await context.close();
    }
    for(const combined of ["```math\n\\ce{H2O}\n```\n\n```markmap\n# Root\n- Child\n```\n\n```math\nx^2\n```","```markmap\n# Root\n- Child\n```\n\n```math\n\\ce{H2O}\n```\n\n```math\nx^2\n```"]) {
        const {page,context}=await open(combined,"ir");
        await page.waitForFunction(()=>document.querySelectorAll('#blocks [data-renderer][data-state="ready"]').length===3);
        assert.equal(await page.evaluate(()=>window.katex.version),"0.16.9");
        assert.match(await page.locator('[data-renderer="math"]').first().textContent(),/H.*2.*O/);
        assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),combined);
        const field=page.locator('#blocks textarea').nth(combined.startsWith('```math')?0:1);
        await field.fill("```math\n\\ce{CO2}\n```");await field.blur();await acknowledge(page);
        await page.waitForFunction(()=>document.querySelector('[data-renderer="math"]')?.dataset.state==="ready" && /C.*O.*2/.test(document.querySelector('[data-renderer="math"]').textContent));
        assert.equal(await page.evaluate(()=>window.katex.version),"0.16.9");
        assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),combined.replace('H2O','CO2'));await context.close();
    }
});

test("malformed renderers report errors beside exact source and never execute chart code",async()=>{
    const invalid={mermaid:"graph TD { ???",graphviz:"digraph { a ->",echarts:'({series:[],run:()=>{window.hostileExecuted=true}})',abc:"invalid",flowchart:"INVALID -> ???",markmap:"invalid",smiles:"invalid ???",math:"{ INVALID",mindmap:"# Root\n- Child",plantuml:"INVALID -> ???"};
    for(const [language,input] of Object.entries(invalid)) {
        const source=`\`\`\`${language}\n${input}\n\`\`\``,{page,context}=await open(source,"preview");
        await page.waitForSelector(`[data-renderer="${language}"][data-state="error"]`,{timeout:20000});
        assert.equal(await page.locator("#blocks textarea").inputValue(),source);
        assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source);
        assert.equal(await page.evaluate(()=>window.hostileExecuted),undefined);await context.close();
    }
    const {page,context}=await open("```wavedrom\n{signal:[]}\n```\n\n```sequence\nA->B: hello\n```","preview");
    assert.equal(await page.locator("[data-renderer]").count(),0);await context.close();
});

test("Mermaid sequence, sequential ABC and SMILES, and math delimiters render offline in every presentation",async()=>{
    const source="Escaped \\$5, `\\$code$`, numeric $2x+1$ and $\\ce{H2O}$.\n\n$$\na^2+b^2=c^2\n$$\n\n```mermaid\nsequenceDiagram\nAlice->>Bob: Hello\n```\n\n```abc\nX:1\nK:C\nCDEF|\n```\n\n```smiles\nCCO\n```\n\nTail";
    for(const mode of ["ir","wysiwyg","sv","preview"]) {
        const {page,context,external}=await open(source,mode);
        try { await page.waitForFunction(()=>["mermaid","abc","smiles"].every(language=>document.querySelector(`[data-renderer="${language}"][data-state="ready"]`)) && document.querySelectorAll('[data-renderer="math"][data-state="ready"]').length>=3); }
        catch(error){throw new Error(`${mode}: ${JSON.stringify(await page.locator('[data-renderer]').evaluateAll(elements=>elements.map(element=>({language:element.dataset.renderer,state:element.dataset.state,text:element.textContent.slice(0,200)}))))}`,{cause:error});}
        assert.equal(await page.locator('[data-renderer="math"][data-state="error"]').count(),0);
        const root=mode==="preview"?page.locator('#viewer'):mode==="sv"?page.locator('.vditor-preview'):page.locator(`.vditor-${mode}`);
        assert(await root.locator('.katex').count()>=3,`${mode}: in-flow inline and block math`);
        assert(await page.locator('[data-renderer="mermaid"] svg').count());
        assert.deepEqual(external,[]);assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source);
        if(mode!=="preview") {
            await selectText(page,"Tail");await page.keyboard.insertText("Changed");await page.evaluate(()=>window.latheRich.commit());
            assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source.replace('Tail','Changed'));await acknowledge(page);
            await selectText(page,"Changed");await page.keyboard.insertText("Again");await page.evaluate(()=>window.latheRich.commit());
            assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source.replace('Tail','Again'));await acknowledge(page);
            await page.evaluate(()=>window.latheMarkdown.receive({kind:'flush',request:772}));
            assert(await page.evaluate(()=>window.testMessages.some(message=>message.kind==='flushed'&&message.request===772)));
            await selectText(page,"Again");
            const composition=await context.newCDPSession(page);
            await page.evaluate(()=>window.mathComposingRoot=window.latheRich.editor.vditor[window.latheRich.editor.getCurrentMode()].element);
            await composition.send('Input.imeSetComposition',{text:'中文😀',selectionStart:4,selectionEnd:4});
            await page.evaluate(()=>{window.latheRich.commit();window.latheMarkdown.receive({kind:'flush',request:773});});
            assert.equal(await page.evaluate(()=>window.testMessages.some(message=>message.kind==='flushed'&&message.request===773)),false);
            assert(await page.evaluate(()=>window.mathComposingRoot===window.latheRich.editor.vditor[window.latheRich.editor.getCurrentMode()].element));
            await composition.send('Input.insertText',{text:'中文😀'});await page.waitForFunction(()=>window.latheMarkdown.pending);
            assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source.replace('Tail','中文😀'));await acknowledge(page);
            assert(await page.evaluate(()=>window.testMessages.some(message=>message.kind==='flushed'&&message.request===773)));
        }
        await context.close();
    }
});

test("theme choices and app colors update the existing document without moving selection or history",async()=>{
    const {page,context}=await open("# Heading\n\nordinary selected paragraph\n\n```rust\nfn main() {}\n```","ir");
    await selectText(page,"selected");
    await page.evaluate(()=>{window.originalEditor=window.latheRich.editor;window.originalProjection=window.latheRich.projection;});
    const baseline=JSON.parse(await readFile(new URL("../../../project-docs/imp-plans/markdown-editor-baseline/runtime.json",import.meta.url),"utf8"));
    for(const theme of baseline.contentThemes)await page.selectOption("#content-theme",theme);
    for(const theme of baseline.codeThemes)await page.selectOption("#code-theme",theme);
    await page.evaluate(()=>window.latheMarkdown.receive({kind:"appearance",dark:true,use_app_theme_colors:true,show_line_numbers:false,background:"rgb(10, 20, 30)",foreground:"rgb(200, 210, 220)",accent:"rgb(40, 50, 60)",custom_css:".vditor-reset p { letter-spacing: 3px; }",plantuml_service:null}));
    assert.equal(await page.evaluate(()=>window.latheRich.editor===window.originalEditor),true);
    assert.equal(await page.evaluate(()=>window.getSelection().toString()),"selected");
    assert.equal(await page.evaluate(()=>getComputedStyle(document.body).backgroundColor),"rgb(10, 20, 30)");
    assert.equal(await page.evaluate(()=>getComputedStyle(document.querySelector('.vditor-ir p')).letterSpacing),"3px");
    assert.equal(await page.locator('.line-gutter').count(),0);
    assert.equal(await page.evaluate(()=>window.testMessages.filter(message=>["edit","draft","undo","redo"].includes(message.kind)).length),0);
    await context.close();
});

test("image resources use project byte messages and preserve source through real neighboring edits",async()=>{
    const source="Before\n\n![image](assets/%E4%B8%AD%20%23.png)\n\nTail",png="iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Wl6rQAAAABJRU5ErkJggg==";
    for(const mode of ["ir","wysiwyg","sv","preview"]) {
        const {page,context,external}=await open(source,mode);
        await page.evaluate(png=>{for(const message of window.testMessages.filter(message=>message.kind==="resource"))window.latheMarkdown.receive({kind:"resource",request:message.request,mime:"image/png",bytes:png,error:null});},png);
        try { await page.waitForFunction(()=>[...document.querySelectorAll('img')].some(image=>image.src.startsWith("blob:") && image.naturalWidth===1)); }
        catch(error){throw new Error(`${mode}: ${JSON.stringify(await page.evaluate(()=>({images:[...document.querySelectorAll('img')].map(image=>image.outerHTML),messages:window.testMessages,status:document.querySelector('#status').textContent})))}`,{cause:error});}
        if(mode!=="preview") {
            await selectText(page,"Tail");await page.keyboard.insertText("Changed");await page.evaluate(()=>window.latheRich.commit());
            assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source.replace("Tail","Changed"));
        }
        assert.deepEqual(external,[]);await context.close();
    }
});

test("loaded inline images keep durable destinations and titles through paragraph edits and history",async()=>{
    const png="iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Wl6rQAAAABJRU5ErkJggg==";
    for(const mode of ["wysiwyg","ir","sv","preview"]) for(const title of ["",' "Retained title"']) {
        const source=`Before ![image](assets/a.png${title}) target after`,{page,context,external}=await open(source,mode);
        await page.evaluate(png=>{
            const bridge=window.latheMarkdown;
            const respond=message=>bridge.receive({kind:"resource",request:message.request,mime:"image/png",bytes:png,error:null});
            for(const message of window.testMessages.filter(message=>message.kind==="resource"))respond(message);
            bridge.send=message=>{window.testMessages.push(message);if(message.kind==="resource")respond(message);};
        },png);
        await page.waitForFunction(()=>[...document.querySelectorAll('img')].some(image=>image.src.startsWith("blob:")&&image.naturalWidth===1));
        if(mode!=="preview") {
            for(const [before,after] of [["target","CHANGED"],["CHANGED","SECOND"]]) {
                await selectText(page,before);await page.keyboard.insertText(after);await page.evaluate(()=>window.latheRich.commit());
                assert.equal(await page.evaluate(()=>window.latheMarkdown.blocked),false);
                assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source.replace("target",after),mode+title);
                await acknowledge(page);
            }
            await page.keyboard.press("Control+z");
            assert.equal(await page.evaluate(()=>window.testMessages.some(message=>message.kind==="undo")),true);
            await page.evaluate(source=>window.latheMarkdown.receive({kind:"history_accepted",request:window.latheMarkdown.historyPending,revision:3,text:source,writable:true}),source.replace("target","CHANGED"));
            assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source.replace("target","CHANGED"));
        } else assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source);
        await page.evaluate(()=>window.latheMarkdown.receive({kind:"flush",request:995}));
        assert.equal(await page.evaluate(()=>window.testMessages.some(message=>message.kind==="flushed"&&message.request===995)),true);
        assert.deepEqual(await page.evaluate(()=>[...new Set(window.testMessages.filter(message=>message.kind==="resource").map(message=>message.url))]),["assets/a.png"]);
        assert.deepEqual(external,[]);await context.close();
    }
});

test("PlantUML is explicitly service backed and sanitizes configured service output",async()=>{
    const source="```plantuml\n@startuml\nAlice -> Bob: Hello\n@enduml\n```",{page,context,external}=await open(source,"preview");
    await page.waitForSelector('[data-renderer="plantuml"][data-state="error"]');
    assert.match(await page.locator('[data-renderer="plantuml"]').textContent(),/cannot render offline/);
    await page.evaluate(()=>{
        window.latheMarkdown.receive({kind:"appearance",plantuml_service:"https://configured.invalid/plantuml",dark:false,show_line_numbers:true});
        window.latheMarkdown.send=message=>{window.testMessages.push(message);if(message.kind==="plantuml")window.latheMarkdown.receive({kind:"resource",request:message.request,mime:"image/svg+xml",bytes:btoa('<svg onload="window.hostileExecuted=true" xmlns="http://www.w3.org/2000/svg"><script>window.hostileExecuted=true</script><text onclick="window.hostileExecuted=true">Alice and Bob</text><a href="javascript:evil()">unsafe</a></svg>'),error:null});};
        window.latheMarkdown.receive({kind:"document",revision:1,text:window.latheMarkdown.draft+"\n",writable:true,draft:null});
    });
    await page.waitForSelector('[data-renderer="plantuml"][data-state="ready"]');
    assert.equal(await page.locator('[data-renderer="plantuml"] script,[data-renderer="plantuml"] [onclick],[data-renderer="plantuml"] a').count(),0);
    assert.equal(await page.evaluate(()=>window.hostileExecuted),undefined);assert.deepEqual(external,[]);await context.close();
});

test("clipboard and drop file messages capture the original source caret after pending edits flush",async()=>{
    for(const mode of ["ir","wysiwyg","sv"]) for(const transfer of ["paste","drop"]) {
        const source="Before\n\n**alpha** middle Tail",{page,context}=await open(source,mode);
        await selectText(page,"middle");
        await page.evaluate(transfer=>{
            const data=new DataTransfer();data.items.add(new File([new Uint8Array([1,2,3])],"clipboard.png",{type:"image/png"}));
            const root=window.latheRich.editor.vditor[window.latheRich.editor.getCurrentMode()].element;
            root.dispatchEvent(transfer==="paste"?new ClipboardEvent("paste",{clipboardData:data,bubbles:true,cancelable:true}):new DragEvent("drop",{dataTransfer:data,bubbles:true,cancelable:true}));
        },transfer);
        await page.waitForFunction(()=>window.testMessages.some(message=>message.kind==="import_images"));
        const message=await page.evaluate(()=>window.testMessages.find(message=>message.kind==="import_images"));
        assert.equal(message.position,source.indexOf("middle"),`${mode}/${transfer}`);assert.equal(message.base_revision,0);assert.equal(message.images[0].bytes,"AQID");
        assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source);await context.close();
    }
});

test("resource failures, resets, hostile SVG and custom CSS leave source and document permissions intact",async()=>{
    const source="Before\n\n![original](assets/a.png)\n\nTail",{page,context,external}=await open(source,"wysiwyg");
    await page.evaluate(()=>{for(const request of window.testMessages.filter(message=>message.kind==="resource"))window.latheMarkdown.receive({kind:"resource",request:request.request,mime:"",bytes:"",error:"Disconnected"});});
    await page.waitForSelector("img.resource-error");
    await selectText(page,"Tail");await page.keyboard.insertText("Changed");await page.evaluate(()=>window.latheRich.commit());
    assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source.replace("Tail","Changed"));await acknowledge(page);
    await page.evaluate(()=>window.latheMarkdown.receive({kind:"resources_reset"}));
    try{await page.waitForFunction(()=>window.testMessages.filter(message=>message.kind==="resource").length>=2,{},{timeout:2000});}catch(error){throw new Error(`${mode}: ${JSON.stringify(await page.evaluate(()=>({images:[...document.querySelectorAll("img")].map(image=>image.outerHTML),messages:window.testMessages})))}`,{cause:error});}
    await page.evaluate(()=>window.latheMarkdown.receive({kind:"appearance",dark:false,use_app_theme_colors:true,show_line_numbers:false,background:"#fff",foreground:"#222",accent:"blue",custom_css:'@import url("https://evil.invalid/steal.css"); .vditor-reset { background-image:url("file:///private"); }',plantuml_service:null}));
    assert.deepEqual(external,[]);assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source.replace("Tail","Changed"));await context.close();
});

test("empty and blank documents accept first keyboard input in every editing mode",async()=>{
    for (const mode of ["ir","wysiwyg","sv"]) for (const source of ["","\n","\n\n","\r\n","\r\n\r\n","   "," \n\t\n"]) {
        const {page,context}=await open(source,mode);
        await page.evaluate(()=>window.latheRich.editor.vditor[window.latheRich.editor.getCurrentMode()].element.setAttribute("data-empty-test",""));
        await page.locator("[data-empty-test]").click(); await page.keyboard.type("abc");
        await page.evaluate(()=>window.latheRich.commit());
        assert.equal(await page.evaluate(()=>window.latheMarkdown.blocked),false,`${mode}: ${JSON.stringify(source)}`);
        const draft=await page.evaluate(()=>window.latheMarkdown.draft);
        assert.equal(draft.replace("abc","" ).trim(),"",`${mode}: ${JSON.stringify(source)}`);
        assert(draft.includes("abc"),`${mode}: all typed characters retained`);
        assert(draft.replace("abc","").includes(source),`${mode}: original blank source retained: ${JSON.stringify({source,draft})}`);
        await acknowledge(page); await acknowledge(page); await page.evaluate(()=>window.latheMarkdown.receive({kind:"flush",request:995}));
        assert(await page.evaluate(()=>window.testMessages.some(message=>message.kind==="flushed"&&message.request===995)));
        await context.close();
    }
});

test("explicit setext marker edits and heading formatting remain effective",async()=>{
    const source="Before\n\ntarget\n=======\n\nTail", {page,context}=await open(source,"sv");
    await selectText(page,"======"); await page.keyboard.insertText("---"); await page.evaluate(()=>window.latheRich.commit());
    assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source.replace("=======","------"));
    assert.equal(await page.evaluate(()=>window.latheMarkdown.blocked),false);
    await context.close();
    for (const mode of ["ir","wysiwyg"]) {
        const {page,context}=await open(source,mode);
        await selectText(page,"target");
        if (mode === "ir") await page.locator('[data-type=bold]').first().click();
        else { await page.locator('[data-type=headings]').first().click(); await page.locator('[data-tag=h3]').click(); }
        await page.waitForFunction(()=>window.latheMarkdown.pending); await page.evaluate(()=>window.latheRich.commit());
        assert.equal(await page.evaluate(()=>window.latheMarkdown.blocked),false);
        assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),mode === "ir" ? "Before\n\n**target**\n=======\n\nTail" : "Before\n\n### target\n\nTail");
        await context.close();
    }
});

test("empty rich composition retains input and waits for acknowledgment before flush",async()=>{
    for (const mode of ["ir","wysiwyg","sv"]) for (const source of ["","\n","\r\n","  \n\n"]) {
        const {page,context}=await open(source,mode);
        await page.evaluate(()=>window.latheRich.editor.vditor[window.latheRich.editor.getCurrentMode()].element.setAttribute("data-empty-test",""));
        await page.locator("[data-empty-test]").click();
        const composition=await context.newCDPSession(page);
        await composition.send("Input.imeSetComposition",{text:"中文😀",selectionStart:4,selectionEnd:4});
        await page.evaluate(()=>{
            const root=window.latheRich.editor.vditor[window.latheRich.editor.getCurrentMode()].element;
            window.composingRoot=root;
            window.latheRich.commit(); window.latheMarkdown.receive({kind:"flush",request:996});
        });
        assert.equal(await page.evaluate(()=>window.latheMarkdown.blocked),false,`${mode}: ${JSON.stringify(source)}`);
        assert(await page.evaluate(()=>window.composingRoot.textContent.includes("中文😀")));
        assert.equal(await page.evaluate(()=>window.testMessages.some(message=>message.kind==="edit"||message.kind==="flushed")),false);
        await composition.send("Input.insertText",{text:"中文😀"});
        await page.waitForFunction(()=>window.latheMarkdown.pending);
        const draft=await page.evaluate(()=>window.latheMarkdown.draft);
        assert(draft.includes("中文😀"),JSON.stringify(await page.evaluate(()=>({draft:window.latheMarkdown.draft,canonical:window.latheRich.projection.canonical,dom:window.composingRoot.innerHTML,messages:window.testMessages,status:document.querySelector("#status").textContent}))));
        assert(draft.replace("中文😀","").startsWith(source));
        await acknowledge(page);
        assert(await page.evaluate(()=>window.testMessages.some(message=>message.kind==="flushed"&&message.request===996)));
        await context.close();
    }
});

test("Viewer selection retains the same repeated Unicode occurrence across presentations",async()=>{
    for (const mode of ["ir","wysiwyg","sv"]) for (const word of ["word","中文😀word"]) for (const backward of [false,true]) {
        const source=`# **${word}**\n\nEarlier [${word}](https://example.com).\n\nLast ${word} end`, {page,context}=await open(source,mode);
        await page.evaluate(({word,backward})=>{
            const root=window.latheRich.editor.vditor[window.latheRich.editor.getCurrentMode()].element; root.focus();
            const walker=document.createTreeWalker(root,NodeFilter.SHOW_TEXT);
            while (walker.nextNode()) {
                const node=walker.currentNode,index=node.data.indexOf(`Last ${word} end`); if(index<0)continue;
                const start=index+5,end=start+word.length;
                window.getSelection().setBaseAndExtent(node,backward ? end:start,node,backward ? start:end); return;
            }
            throw new Error("Last occurrence missing");
        },{word,backward});
        for (const destination of ["preview",mode]) {
            await page.evaluate(destination=>window.latheRich.mode(destination),destination); await page.waitForFunction(()=>window.latheRich.ready);
            assert.deepEqual(await page.evaluate(()=>({text:window.getSelection().toString(),parent:window.getSelection().anchorNode.parentElement.textContent,backward:window.getSelection().anchorOffset>window.getSelection().focusOffset})),{text:word,parent:`Last ${word} end`,backward},`${mode} to ${destination}`);
        }
        assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source);
        assert.equal(await page.evaluate(()=>window.testMessages.some(message=>message.kind==="edit")),false);
        await context.close();
    }
});

test("source-field overflow is prevented or retained behind save and mode barriers",async()=>{
    for (const [character,count,composition] of [["x",8*1024*1024,false],["中",3*1024*1024,true],['"',4*1024*1024,true]]) {
        const source="---\na: b\n---\n\nTail", {page,context}=await open(source);
        await page.locator("#source-blocks").evaluate(element=>element.open=true);
        await page.locator("#blocks details").first().evaluate(element=>element.open=true);
        const field=page.locator("#blocks textarea").first(); await field.focus();
        const result=await field.evaluate((field,{character,count,composition})=>{
            const previous=field.value, insertion=character.repeat(count);
            field.select();
            const event=new InputEvent("beforeinput",{bubbles:true,cancelable:true,inputType:"insertText",data:insertion});
            field.dispatchEvent(event);
            const prevented=event.defaultPrevented, unchanged=field.value===previous;
            if (composition) field.dispatchEvent(new CompositionEvent("compositionstart",{bubbles:true}));
            field.value="---\na: "+insertion+"\n---\n";
            field.dispatchEvent(new InputEvent("input",{bubbles:true,inputType:composition ? "insertCompositionText":"insertText",data:insertion}));
            if (composition) field.dispatchEvent(new CompositionEvent("compositionend",{bubbles:true}));
            return {prevented,unchanged};
        },{character,count,composition});
        assert.deepEqual(result,{prevented:true,unchanged:true});
        await field.blur();
        await page.evaluate(()=>{
            window.latheMarkdown.receive({kind:"flush",request:991});
            window.latheRich.mode("wysiwyg");
            window.latheMarkdown.receive({kind:"preferences",preferences:{mode:"sv"}});
        });
        assert.deepEqual(await page.evaluate(({character,count,source})=>{
            const bridge=window.latheMarkdown, expected="---\na: "+character.repeat(count)+"\n---\n\nTail";
            return {exact:bridge.draft===expected,retained:window.testMessages.some(message=>message.kind==="draft"&&message.text===expected),blocked:bridge.blocked,base:bridge.base===source,pending:bridge.pending===null,flushed:window.testMessages.some(message=>message.kind==="flushed"),edited:window.testMessages.some(message=>message.kind==="edit"),mode:window.latheRich.editor.getCurrentMode(),field:document.querySelector("#blocks textarea").value===expected.slice(0,-5)};
        },{character,count,source}),{exact:true,retained:true,blocked:true,base:true,pending:true,flushed:false,edited:false,mode:"ir",field:true});
        await context.close();
    }
});

test("source-field insertion budget includes replacement selection and surrounding source",async()=>{
    const {page,context}=await open("---\na: b\n---\n\nTail");
    const result=await page.locator("#blocks textarea").first().evaluate(field=>{
        const limit=8*1024*1024;
        field.select();
        const replacement=new InputEvent("beforeinput",{bubbles:true,cancelable:true,inputType:"insertText",data:"x".repeat(limit-10)});
        field.dispatchEvent(replacement);
        field.setSelectionRange(0,0);
        const insertion=new InputEvent("beforeinput",{bubbles:true,cancelable:true,inputType:"insertText",data:"x".repeat(limit-10)});
        field.dispatchEvent(insertion);
        field.value="x".repeat(limit-8); field.setSelectionRange(field.value.length,field.value.length);
        const newline=new InputEvent("beforeinput",{bubbles:true,cancelable:true,inputType:"insertLineBreak"});
        field.dispatchEvent(newline);
        return {replacement:replacement.defaultPrevented,insertion:insertion.defaultPrevented,newline:newline.defaultPrevented};
    });
    assert.deepEqual(result,{replacement:false,insertion:true,newline:true}); await context.close();
});

test("similar tables retain their own row padding through repeated rich edits",async()=>{
    for (const mode of ["ir","wysiwyg","sv"]) for (const newline of ["\n","\r\n"]) {
        const source=["| Name | Value |","| --- | --- |","| Same | First |","","Between","","|  Name  |  Value  |","| :------ | -----: |","|  Same  |  Second  |","| Same | Second |","","Tail"].join(newline);
        const {page,context}=await open(source,mode);
        await selectText(page,"Second"); await page.keyboard.insertText("Edited"); await page.evaluate(()=>window.latheRich.commit());
        assert.equal(await page.evaluate(()=>window.latheMarkdown.blocked),false);
        assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source.replace("Second","Edited")); await acknowledge(page);
        await selectText(page,"Second"); await page.keyboard.insertText("Repeated"); await page.evaluate(()=>window.latheRich.commit());
        assert.equal(await page.evaluate(()=>window.latheMarkdown.blocked),false);
        assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source.replace("Second","Edited").replace("Second","Repeated")); await acknowledge(page);
        await selectText(page,"First"); await page.keyboard.insertText("Original table edited"); await page.evaluate(()=>window.latheRich.commit());
        assert.equal(await page.evaluate(()=>window.latheMarkdown.blocked),false);
        assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source.replace("Second","Edited").replace("Second","Repeated").replace("First","Original table edited"));
        await context.close();
    }
});

test("delayed rich input is captured before flush and immediate mode changes",async () => {
    for (const mode of ["ir","wysiwyg"]) for (const operation of ["flush","mode"]) {
        const {page,context}=await open("Before\n\nTarget\n\nTail",mode);
        await selectText(page,"Target"); await page.keyboard.insertText("Changed");
        await page.evaluate(operation => operation === "flush" ? window.latheMarkdown.receive({kind:"flush",request:99}) : window.latheRich.mode("sv"),operation);
        assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),"Before\n\nChanged\n\nTail");
        assert.equal(await page.evaluate(()=>window.testMessages.some(message=>message.kind==="flushed")),false);
        await acknowledge(page); await page.waitForFunction(()=>window.latheRich.ready);
        assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),"Before\n\nChanged\n\nTail");
        if(operation==="flush") assert(await page.evaluate(()=>window.testMessages.some(message=>message.kind==="flushed" && message.request===99)));
        else assert.equal(await page.evaluate(()=>window.latheRich.editor.getCurrentMode()),"sv");
        await context.close();
    }
});

test("rapid rich scopes retain earlier edits across keyboard and mouse interaction",async () => {
    for (const mode of ["ir","wysiwyg"]) for(const mouse of [false,true]) {
        const {page,context}=await open("First sentence\n\nSecond sentence\n\nTail",mode);
        await selectText(page,"First"); await page.keyboard.insertText("Edited first");
        if(mouse) await page.locator(mode==="ir" ? '.vditor-ir p' : '.vditor-wysiwyg p').filter({hasText:"Second"}).click();
        await selectText(page,"Second"); await page.keyboard.insertText("Edited second");
        await page.waitForFunction(()=>window.latheMarkdown.draft.includes("Edited second"));
        await page.evaluate(()=>window.latheMarkdown.receive({kind:"flush",request:100}));
        assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),"Edited first sentence\n\nEdited second sentence\n\nTail");
        await acknowledge(page); await acknowledge(page);
        assert(await page.evaluate(()=>window.testMessages.some(message=>message.kind==="flushed" && message.request===100)));
        await context.close();
    }
});

test("rich Backspace and Delete joins preserve affected paragraphs and untouched source",async () => {
    for (const mode of ["ir","wysiwyg"]) for(const backward of [true,false]) {
        const source='[id]: <https://example.com> "Keep"\r\n\r\nFirst paragraph\r\n\r\nSecond paragraph\r\n\r\nTail';
        const {page,context}=await open(source,mode);
        await selectText(page,backward ? "Second paragraph" : "First paragraph");
        await page.keyboard.press(backward ? "ArrowLeft" : "ArrowRight");
        await page.keyboard.press(backward ? "Backspace" : "Delete");
        await page.waitForFunction(()=>window.latheMarkdown.pending || document.querySelector('#status').textContent.includes("cannot be mapped safely"));
        await page.evaluate(()=>window.latheMarkdown.receive({kind:"flush",request:101}));
        assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source.replace("First paragraph\r\n\r\nSecond paragraph","First paragraphSecond paragraph"));
        await acknowledge(page);
        assert(await page.evaluate(()=>window.testMessages.some(message=>message.kind==="flushed" && message.request===101)));
        await page.evaluate(()=>window.latheRich.mode("sv")); await page.waitForFunction(()=>window.latheRich.ready && window.latheRich.editor.getCurrentMode()==="sv");
        assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source.replace("First paragraph\r\n\r\nSecond paragraph","First paragraphSecond paragraph"));
        await context.close();
    }
});

test("unmappable rich edits retain recovery text and block successful barriers and rebuilds",async () => {
    for (const mode of ["ir","wysiwyg"]) {
        const {page,context}=await open("Before\n\nTarget\n\nTail",mode);
        await selectText(page,"Target"); await page.keyboard.insertText("Changed");
        // An unrelated DOM mutation makes the captured unchanged neighbor unavailable.
        await page.evaluate(()=>{
            const editor=window.latheRich.editor,root=editor.vditor[editor.getCurrentMode()].element;
            root.firstElementChild.remove(); window.latheRich.commit(); window.latheMarkdown.receive({kind:"flush",request:102}); window.latheRich.mode("sv");
        });
        assert(await page.evaluate(()=>window.latheMarkdown.blocked));
        assert((await page.evaluate(()=>window.latheMarkdown.draft)).includes("Changed"));
        assert(await page.evaluate(()=>window.testMessages.some(message=>message.kind==="draft" && message.text.includes("Changed"))));
        assert.equal(await page.evaluate(()=>window.testMessages.some(message=>["edit","flushed"].includes(message.kind))),false);
        assert.equal(await page.evaluate(()=>window.latheRich.editor.getCurrentMode()),mode);
        assert((await page.locator(mode==="ir" ? ".vditor-ir" : ".vditor-wysiwyg").innerText()).includes("Changed"));
        await context.close();
    }
});

test("Find recomputes counts and DOM highlights after host updates and ordinary input",async () => {
    for(const mode of ["ir","wysiwyg","sv","preview"]) {
        const {page,context}=await open("Word word",mode);
        await page.click("#find-open"); await page.fill("#query","word"); assert.equal(await page.locator("#count").innerText(),"2 matches");
        await page.evaluate(()=>window.latheMarkdown.receive({kind:"document",revision:1,text:"No match",writable:true,draft:null}));
        assert.equal(await page.locator("#count").innerText(),"0 matches"); await page.click("#next");
        assert.equal(await page.locator("#count").innerText(),"0 matches");
        assert.equal(await page.evaluate(()=>CSS.highlights.get("find").size),0);
        if(mode!=="preview") {
            await selectText(page,"No match"); await page.keyboard.insertText("word");
            await page.evaluate(()=>window.latheMarkdown.receive({kind:"flush",request:103}));
            assert.equal(await page.locator("#count").innerText(),"1 matches");
            assert.equal(await page.evaluate(()=>CSS.highlights.get("find").size),1);
        }
        await context.close();
    }
});

test("viewer footnotes, backlinks and ordinary HTML IDs resolve inside the document",async () => {
    const source='First[^one]\n\n[ordinary](#query)\n\n'+"paragraph\n\n".repeat(100)+'<p id="query">Ordinary anchor</p>\n\n[^one]: Last footnote';
    const {page,context}=await open(source,"preview");
    await page.locator('#viewer a[href="#footnotes-def-1"]').click();
    assert(await page.locator('#viewer').evaluate(root=>root.scrollTop>0));
    await page.locator('#viewer a[href="#footnotes-ref-1"]').click();
    assert(await page.locator('#viewer').evaluate(root=>root.scrollTop<100));
    await page.locator('#viewer a[href="#query"]').click();
    assert(await page.locator('#viewer').evaluate(root=>root.scrollTop>0));
    assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source);
    assert.deepEqual(await page.evaluate(()=>window.testMessages.filter(message=>message.kind==="open_link")),[]);
    await context.close();
});

test("heading outline fragments use rendered inline text in every presentation",async () => {
    const source='[jump](#target)\n\n'+"paragraph\n\n".repeat(50)+'# [Target](https://example.com)';
    for(const mode of ["ir","wysiwyg","sv","preview"]) {
        const {page,context}=await open(source,mode); await page.click("#outline-toggle");
        assert.equal(await page.locator('#outline button').innerText(),"Target");
        const root=mode==="preview" ? '#viewer' : mode==="sv" ? '.vditor-preview' : mode==="ir" ? '.vditor-ir' : '.vditor-wysiwyg';
        if(mode==="ir") await page.locator(`${root} [data-type=a]`).filter({hasText:"jump"}).click();
        else await page.locator(`${root} a[href="#target"]`).click();
        assert(await page.locator(`${root} h1`).evaluate(heading=>{
            const bounds=heading.getBoundingClientRect(); return bounds.top < innerHeight && bounds.bottom > 0;
        }),mode);
        assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source);
        await context.close();
    }
});

test("canceled composition after delayed input retains exact source beside relocated definitions",async () => {
    const source='# Heading\n<script>inert</script>\n[ref]: /a\n```unknown\nx\n```\nTail';
    for(const mode of ["ir","wysiwyg"]) {
        const {page,context}=await open(source,mode);
        await selectText(page,"Heading"); await page.keyboard.insertText("Edited");
        await page.evaluate(()=>window.latheMarkdown.receive({kind:"flush",request:104})); await acknowledge(page);
        await page.evaluate(()=>{
            window.latheMarkdown.composing=true;
            window.latheRich.appendText(" canceled"); window.latheRich.replaceText(" canceled","");
            window.latheMarkdown.compositionEnd(); window.latheMarkdown.receive({kind:"flush",request:105});
        });
        assert.equal(await page.evaluate(()=>window.latheMarkdown.blocked),false);
        assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source.replace("Heading","Edited"));
        await acknowledge(page);
        assert(await page.evaluate(()=>window.testMessages.some(message=>message.kind==="flushed" && message.request===105)));
        await context.close();
    }
});
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

test("Find refreshes active ranges and navigation after asynchronous host math output",async()=>{
    for(const mode of ["preview","sv","ir","wysiwyg"]) {
        const source="Before $x$ after",{page,context}=await open(source,mode);
        await page.waitForFunction(()=>[...document.querySelectorAll('[data-renderer="math"]')].some(element=>element.dataset.state==="ready"));
        await page.click("#find-open");await page.fill("#query","z");
        assert.equal(await page.locator("#count").innerText(),"0 matches");
        const next="Before $z$ after\n\nAgain $z$";
        await page.evaluate(next=>window.latheMarkdown.receive({kind:"document",revision:1,text:next,writable:true,draft:null}),next);
        await page.waitForFunction(()=>{
            const outputs=[...document.querySelectorAll('[data-renderer="math"]')].filter(element=>element.isConnected&&element.offsetParent!==null);
            return outputs.length>=2&&outputs.every(element=>element.dataset.state==="ready"&&element.textContent.includes("z"));
        });
        const ranges=await page.evaluate(()=>[...CSS.highlights.get("find")??[]].map(range=>range.toString()));
        assert.equal(ranges.length,["ir","wysiwyg"].includes(mode)?4:2,mode+": active math source and output ranges");
        assert(ranges.every(value=>value==="z"));
        assert.equal(await page.locator("#count").innerText(),`${ranges.length} matches`,mode);
        await page.click("#next");assert.equal(await page.locator("#count").innerText(),`1 of ${ranges.length}`);
        await page.click("#previous");assert.equal(await page.locator("#count").innerText(),`${ranges.length} of ${ranges.length}`);
        assert.equal(await page.evaluate(()=>CSS.highlights.get("current-find")?.size),1);
        assert.equal(await page.evaluate(()=>[...CSS.highlights.get("find")].every(range=>range.startContainer.isConnected&&range.endContainer.isConnected)),true);
        assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),next);
        assert.deepEqual(await page.evaluate(()=>window.testMessages.filter(message=>["draft","edit","undo","redo"].includes(message.kind))),[]);
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


test("retained source controls track ordinary edits and consecutive source changes",async()=>{
    for (const mode of ["ir","wysiwyg","sv"]) for(const acknowledged of [false,true]) for(const newline of ["\n","\r\n"]) {
        const source='Prefix\n\n```unknown\noriginal fence\n```\n\nMiddle\n\n```other\nsecond fence\n```\n\nTail'.replace(/\n/g,newline);
        const {page,context}=await open(source,mode);
        await selectText(page,'Prefix'); await page.keyboard.insertText('Longer prefix');
        await page.evaluate(()=>window.latheMarkdown.receive({kind:'flush',request:110}));
        if(acknowledged) await acknowledge(page);
        await page.locator('#source-blocks > summary').click();
        await page.locator('#blocks details > summary').first().click();
        const first=page.locator('#blocks textarea').first();
        await first.fill('```unknown\nchanged fence\n```');
        assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source.replace('Prefix','Longer prefix').replace('original fence','changed fence'),`${mode}/${acknowledged}`);
        await acknowledge(page); await acknowledge(page);
        await first.evaluate(field=>{window.retainedSource=field;field.setSelectionRange(12,19);});
        await page.evaluate(newline=>window.latheMarkdown.receive({kind:'document',revision:window.latheMarkdown.revision+1,text:'Native prefix'+newline+newline+window.latheMarkdown.base,writable:true,draft:null}),newline);
        assert(await first.evaluate(field=>field===window.retainedSource && field.selectionStart===12 && field.selectionEnd===19));
        await first.fill('```unknown\nchanged fence with more text\n```');
        await acknowledge(page);
        await page.locator('#blocks details > summary').nth(1).click();
        await first.focus();
        await first.evaluate(field=>{
            window.retainedSource=field;
            field.dispatchEvent(new CompositionEvent('compositionstart',{bubbles:true}));
            field.value=field.value.replace('more text','\u4e2d\u6587 composition');
            field.dispatchEvent(new InputEvent('input',{bubbles:true,inputType:'insertCompositionText',data:'\u4e2d\u6587'}));
            if(!field.isConnected || document.activeElement!==field || field!==window.retainedSource)throw new Error('Source control replaced');
            field.dispatchEvent(new CompositionEvent('compositionend',{bubbles:true,data:'\u4e2d\u6587'}));
        });
        await page.waitForFunction(()=>!window.latheMarkdown.composing);
        await page.locator('#blocks textarea').nth(1).fill('```other\nchanged second fence\n```');
        const expected='Native prefix'+newline+newline+source.replace('Prefix','Longer prefix').replace('original fence','changed fence with \u4e2d\u6587 composition').replace('second fence','changed second fence');
        assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),expected);
        await page.evaluate(()=>window.latheMarkdown.receive({kind:'flush',request:111}));
        await acknowledge(page); await acknowledge(page);
        assert.equal(await page.evaluate(()=>window.latheMarkdown.blocked),false);
        assert(await page.evaluate(()=>window.testMessages.some(message=>message.kind==='flushed' && message.request===111)));
        await context.close();
    }
});

test("native formatting actions respect source and Find focus",async()=>{
    for(const mode of ['ir','wysiwyg','sv']) for(const focus of ['source','find']) {
        const source='---\ntitle: one\n---\n\nTarget';
        const {page,context}=await open(source,mode); await selectText(page,'Target');
        if(focus==='source') {
            await page.locator('#source-blocks > summary').click(); await page.locator('#blocks details > summary').first().click();
            await page.locator('#blocks textarea').first().focus();
        } else {await page.click('#find-open'); await page.fill('#query','Target');}
        await page.evaluate(()=>window.latheRich.format('bold'));
        assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source,`${mode}/${focus}`);
        assert.equal(await page.evaluate(()=>window.latheMarkdown.blocked),false);
        assert.equal(await page.evaluate(()=>document.activeElement.tagName),focus==='source' ? 'TEXTAREA' : 'INPUT');
        assert.equal(await page.evaluate(()=>window.testMessages.some(message=>['draft','edit'].includes(message.kind))),false);
        await selectText(page,'Target'); await page.evaluate(()=>window.latheRich.format('bold'));
        await page.waitForFunction(()=>window.latheMarkdown.pending);
        assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source.replace('Target','**Target**'));
        await context.close();
    }
});

test("explicit IDs identify actual inline-formatted heading blocks",async()=>{
    const source='[bold](#bold-id) [linked](#linked-id) [code](#code-id) [setext](#setext-id)\n\n<h1>Raw HTML heading</h1>\n\n```text\n# Target {#bold-id}\n```\n\n# Target {#other-id}\n\n'+ 'paragraph\n\n'.repeat(50)+'# **Target** {#bold-id}\n\n## [Target](https://example.com) {#linked-id}\n\n### `Target` {#code-id}\n\n*Target* {#setext-id}\n===\n';
    for(const mode of ['ir','wysiwyg','sv','preview']) {
        const {page,context}=await open(source,mode);
        const root=mode==='preview' ? '#viewer' : mode==='sv' ? '.vditor-preview' : mode==='ir' ? '.vditor-ir' : '.vditor-wysiwyg';
        assert.deepEqual(await page.locator(`${root} h1,${root} h2,${root} h3`).evaluateAll(headings=>headings.map(heading=>heading.dataset.explicitAnchor)),[undefined,'other-id','bold-id','linked-id','code-id','setext-id'],mode);
        for(const [label,id] of [['bold','bold-id'],['linked','linked-id'],['code','code-id'],['setext','setext-id']]) {
            if(mode==='ir') await page.locator(`${root} [data-type=a]`).filter({hasText:label}).click();
            else await page.locator(`${root} a[href="#${id}"]`).click();
            assert(await page.locator(root).evaluate((root,id)=>{
                const heading=root.querySelector(`[data-explicit-anchor="${id}"]`); if(!heading)return false;
                const bounds=heading.getBoundingClientRect(); return bounds.top<innerHeight && bounds.bottom>0;
            },id),`${mode}/${id}`);
        }
        assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source);
        assert.equal(await page.evaluate(()=>window.testMessages.some(message=>message.kind==='edit')),false);
        await context.close();
    }
});

test("Unicode case-insensitive Find highlights exact original text ranges",async()=>{
    const source='\u0130 Word \u0130 **WORD** \u{1f600} word \u{10400} End \u039f\u03a3';
    for(const mode of ['ir','wysiwyg','sv','preview']) {
        const {page,context}=await open(source,mode);
        await page.click('#find-open');
        for(const [query,expected] of [['word',['Word','WORD','word']],['i\u0307',['\u0130','\u0130']],['\u{10428}',['\u{10400}']],['\u03bf\u03c2',['\u039f\u03a3']]]) {
            await page.fill('#query',query);
            assert.equal(await page.locator('#count').innerText(),`${expected.length} matches`,`${mode}/${query}`);
            assert.deepEqual(await page.evaluate(()=>[...CSS.highlights.get('find')].map(range=>range.toString())),expected,`${mode}/${query}`);
            await page.click('#previous'); assert.equal(await page.locator('#count').innerText(),`${expected.length} of ${expected.length}`);
            await page.click('#next'); assert.equal(await page.locator('#count').innerText(),`1 of ${expected.length}`);
        }
        await page.fill('#query','Word'); await page.check('#case');
        assert.deepEqual(await page.evaluate(()=>[...CSS.highlights.get('find')].map(range=>range.toString())),['Word']);
        assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source);
        assert.equal(await page.evaluate(()=>window.testMessages.some(message=>['draft','edit','undo','redo'].includes(message.kind))),false);
        await context.close();
    }
});

test("native image paste follows document focus and keeps Find text paste",async()=>{
    const source="Before\n\n```rust\nfn main() {}\n```\n\nTail",{page,context}=await open(source);
    await page.click("#find-open"); await page.fill("#query","Before");
    await page.evaluate(()=>window.latheRich.nativePaste("needle"));
    assert.equal(await page.inputValue("#query"),"Beforeneedle");
    assert.equal(await page.evaluate(()=>window.testMessages.filter(message=>message.kind==="pick_images").length),0);
    for(const selector of ['#case','#code-theme','#mode','#outline-toggle']){
        await page.focus(selector);await page.evaluate(()=>window.latheRich.nativePaste("text"));
        assert.equal(await page.evaluate(()=>window.testMessages.filter(message=>message.kind==="pick_images").length),0);
    }
    await page.locator('#source-blocks').evaluate(details=>details.open=true); await page.locator('#blocks details').first().evaluate(details=>details.open=true); await page.locator('#blocks textarea').focus(); await page.evaluate(()=>window.latheRich.nativePaste("text"));
    assert.equal(await page.evaluate(()=>window.testMessages.filter(message=>message.kind==="pick_images").length),1);
    assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source);await context.close();
});

test("aggregate image imports stay below the protocol byte limit and leave editing live",async()=>{
    const {page,context}=await open("Before\n\nTail","sv"); await selectText(page,"Before");
    await page.evaluate(()=>{
        const data=new DataTransfer();
        const header=Uint8Array.from(atob('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVQIHWP4z8DwHwAFgAI/ScLttAAAAABJRU5ErkJggg=='),character=>character.charCodeAt(0));
        for(let index=0;index<4;index++){const bytes=new Uint8Array(4179323);bytes.set(header);data.items.add(new File([bytes],`image-${index}.png`,{type:"image/png"}));}
        document.activeElement.dispatchEvent(new DragEvent("drop",{dataTransfer:data,bubbles:true,cancelable:true}));
    });
    await page.waitForFunction(()=>window.testMessages.some(message=>message.kind==="import_images")||/batch|limit/i.test(document.querySelector('#status').textContent));
    const imports=await page.evaluate(()=>window.testMessages.filter(message=>message.kind==="import_images"));
    for(const message of imports)assert(Buffer.byteLength(JSON.stringify(message))<=16781312);
    assert.equal(imports.length,0);assert.match(await page.locator('#status').textContent(),/batch.*limit|limit.*batch/i);
    await selectText(page,"Before");await page.keyboard.insertText("Edited");await page.evaluate(()=>window.latheRich.commit());
    assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),"Edited\n\nTail");await context.close();
});

test("raw drive and UNC image destinations reach host validation without DOM fetch",async()=>{
    for(const mode of ["ir","wysiwyg","sv","preview"]){
        const destinations=['C:/project/assets/a.png','//server/share/project/assets/a.png','javascript:alert(1)','file:///private'];
        const source=destinations.map(destination=>`![image](${destination})`).join('\n\n'),{page,context,external}=await open(source,mode);
        await page.evaluate(()=>{
            const bridge=window.latheMarkdown;
            const reject=message=>bridge.receive({kind:"resource",request:message.request,mime:"",bytes:"",error:"Outside the owning worktree"});
            bridge.send=message=>{window.testMessages.push(message);if(message.kind==="resource")queueMicrotask(()=>reject(message));};
            for(const message of window.testMessages.filter(message=>message.kind==="resource"))reject(message);
        });
        try{await page.waitForFunction(()=>window.testMessages.filter(message=>message.kind==="resource").length>=2,undefined,{timeout:2000});}
        catch(error){throw new Error(`${mode}: ${JSON.stringify(await page.evaluate(()=>({images:[...document.querySelectorAll("img")].map(image=>image.outerHTML),messages:window.testMessages,html:window.latheRich.editor.vditor.lute.Md2HTML(window.latheMarkdown.draft),sanitized:window.latheRich.sanitize('<img data-lathe-image="C:/project/assets/a.png">',false,true)})))}`,{cause:error});}
        const requested=await page.evaluate(()=>window.testMessages.filter(message=>message.kind==="resource").map(message=>message.url));
        for(const destination of destinations.slice(0,2))assert(requested.includes(destination),`${mode} ${destination}`);
        assert.equal(await page.locator('img[src]').count(),0);assert.deepEqual(external,[]);
        await page.evaluate(()=>window.latheRich.commit());assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source);await context.close();
    }
});

test("CommonMark renderer fences render every presentation while source stays exact",async()=>{
    for(const mode of ["ir","wysiwyg","sv","preview"])for(const fence of ['~~~mermaid\ngraph TD; A-->B\n~~~','````mermaid\ngraph TD; A-->B\n````','  ```mermaid\n  graph TD; A-->B\n  ```','> ```mermaid\n> graph TD; A-->B\n> ```','- Parent\n  - Child\n\n    ~~~mermaid\n    graph TD; A-->B\n    ~~~']){
        const source=`Before\r\n\r\n${fence.replaceAll('\n','\r\n')}\r\n\r\nTail`,{page,context}=await open(source,mode);
        await page.waitForSelector('[data-renderer="mermaid"][data-state="ready"]',{timeout:10000});
        const expected=fence.startsWith('- Parent')?fence.slice(fence.indexOf('    ~~~')):fence;
        assert.equal(await page.locator('#blocks textarea').first().inputValue(),expected);
        await page.evaluate(()=>window.latheRich.commit());assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source);
        if(mode!=="preview"){await selectText(page,"Before");await page.keyboard.insertText("Edited");await page.evaluate(()=>window.latheRich.commit());assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source.replace("Before","Edited"));}
        await context.close();
    }
});

test("picker click retains the source textarea caret through flush",async()=>{
    for(const mode of ["ir","wysiwyg","sv"]){
    const source="Before\r\n\r\n```rust\r\nfn main() {}\r\n```\r\n\r\nTail",{page,context}=await open(source,mode);
    await page.locator('#source-blocks').evaluate(details=>details.open=true); await page.locator('#blocks details').first().evaluate(details=>details.open=true);
    await page.locator('#blocks textarea').first().evaluate(field=>{field.focus();field.setSelectionRange(field.value.indexOf('main')+2,field.value.indexOf('main')+2);});
    await page.click('#insert-image');
    const message=await page.evaluate(()=>window.testMessages.find(message=>message.kind==="pick_images"));
    assert.equal(message.position,source.indexOf('main')+2);assert.equal(message.base_revision,0);assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source);await context.close();
    }
});

test("keyboard image picker retains document and source carets across focus and host updates",async()=>{
    for(const mode of ["ir","wysiwyg","sv"]){
        for(const key of ["Enter","Space"]){
            const source="Before\r\n\r\n```rust\r\nfn main() {}\r\n```\r\n\r\nTail",{page,context}=await open(source,mode);
            await page.locator('#source-blocks').evaluate(details=>details.open=true);await page.locator('#blocks details').first().evaluate(details=>details.open=true);
            await page.locator('#blocks textarea').first().evaluate(field=>{field.focus();field.setSelectionRange(field.value.indexOf('main')+2,field.value.indexOf('main')+2);});
            for(let tabs=0;tabs<64 && !await page.locator('#insert-image').evaluate(button=>button===document.activeElement);tabs++)await page.keyboard.press('Tab');
            assert(await page.locator('#insert-image').evaluate(button=>button===document.activeElement),"keyboard reaches image button");
            if(key==="Space")await page.evaluate(source=>window.latheMarkdown.receive({kind:"document",revision:1,text:"Prefix\r\n"+source,writable:true,draft:null}),source);
            await page.keyboard.press(key);
            const message=await page.evaluate(()=>window.testMessages.find(message=>message.kind==="pick_images")),expected=(key==="Space"?"Prefix\r\n":"")+source;
            assert.equal(message.position,expected.indexOf('main')+2,`${mode}/${key}: source caret`);assert.equal(message.base_revision,key==="Space"?1:0);
            assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),expected);assert.equal(await page.evaluate(()=>window.testMessages.filter(message=>["draft","edit"].includes(message.kind)).length),0);
            if(key==="Space"){
                await page.evaluate(()=>window.latheMarkdown.receive({kind:"document",revision:2,text:"Tiny",writable:true,draft:null}));
                await page.keyboard.press('Space');
                const replacement=await page.evaluate(()=>window.testMessages.filter(message=>message.kind==="pick_images").at(-1));
                assert(replacement.position>=0 && replacement.position<=4,`${mode}: replacement source offset stays valid`);assert.equal(replacement.base_revision,2);
            }
            await context.close();
        }
        const source="Before\n\nTail",{page,context}=await open(source,mode);
        await page.evaluate(()=>{const root=window.latheRich.editor.vditor[window.latheRich.editor.getCurrentMode()].element;root.focus();const walker=document.createTreeWalker(root,NodeFilter.SHOW_TEXT);let node;while(node=walker.nextNode())if(node.data.includes('Before')){window.getSelection().setBaseAndExtent(node,3,node,3);break;}});
        await page.locator('#insert-image').focus();
        await page.evaluate(source=>window.latheMarkdown.receive({kind:"document",revision:1,text:"New\n"+source,writable:true,draft:null}),source);
        await page.locator('#insert-image').focus();
        await page.keyboard.press('Enter');
        const message=await page.evaluate(()=>window.testMessages.find(message=>message.kind==="pick_images"));assert.equal(message.position,7,`${mode}: document caret`);assert.equal(message.base_revision,1);assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),"New\n"+source);
        await context.close();
    }
});

test("packaged code themes load and style code in every presentation",async()=>{
    for(const mode of ["ir","wysiwyg","sv","preview"]){
        const source=(await readFile(new URL('../src/smoke.rs',import.meta.url),'utf8')).match(/const RENDERER_FIXTURE: &str = r#"([\s\S]*?)"#;/)[1],{page,context}=await open(source,mode),responses=[];
        page.on('response',response=>{if(response.url().includes('/highlight.js/styles/'))responses.push({url:response.url(),status:response.status()});});
        const code=mode==='sv'?'.vditor-preview pre code.hljs':mode==='preview'?'#viewer pre code.hljs':'#document pre code.hljs';
        await page.waitForFunction(()=>[...document.querySelectorAll('[data-renderer]')].every(element=>element.dataset.state!=='pending'));
        try{await page.waitForSelector(`${code} .hljs-keyword`,{timeout:10000});}
        catch(error){throw new Error(`${mode}: ${JSON.stringify(await page.locator('#document code').evaluateAll(elements=>elements.filter(element=>element.textContent.includes('fn main')).map(element=>element.outerHTML)))}`,{cause:error});}
        const colors=[];
        for(const theme of ['github','github-dark']){
            await page.selectOption('#code-theme',theme);
            await page.waitForFunction(theme=>[...document.styleSheets].some(sheet=>sheet.href?.endsWith(`/${theme}.min.css`)),theme);
            colors.push(await page.locator(`${code} .hljs-keyword`).first().evaluate(element=>getComputedStyle(element).color));
        }
        assert.notEqual(colors[0],colors[1]);assert(responses.every(response=>response.status===200));
        await page.evaluate(()=>window.latheRich.commit());assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source);
        if(mode!=="sv"){
            await page.selectOption('#mode','sv');await page.waitForSelector('.vditor-preview pre code.hljs .hljs-keyword');
            await page.waitForFunction(()=>document.getElementById('vditorHljsStyle')?.getAttribute('href')?.endsWith('/github-dark.min.css'));
            assert.equal(await page.locator('.vditor-preview pre code.hljs .hljs-keyword').first().evaluate(element=>getComputedStyle(element).color),colors[1]);
            assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source);
        }
        await context.close();
    }
});

test("every offered content and code stylesheet loads in every presentation",async()=>{
    for(const mode of ["ir","wysiwyg","sv","preview"]){
        const source='Before\n\n```rust\nfn main() {}\n```\n\nTail',{page,context}=await open(source,mode),requests=[];
        page.on('response',response=>{if(response.url().includes('/content-theme/')||response.url().includes('/highlight.js/styles/'))requests.push({url:response.url(),status:response.status()});});
        for(const selector of ['#content-theme','#code-theme'])for(const value of await page.locator(`${selector} option`).evaluateAll(options=>options.map(option=>option.value).filter(Boolean))){
            await page.selectOption(selector,value);
            const suffix=selector==='#code-theme'?`/highlight.js/styles/${value}.min.css`:`/content-theme/${value}.css`;
            await page.waitForFunction(suffix=>[...document.styleSheets].some(sheet=>sheet.href?.endsWith(suffix)),suffix,{timeout:3000});
        }
        assert(requests.length>=250);assert(requests.every(request=>request.status===200));
        await page.evaluate(()=>window.latheRich.commit());assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source);await context.close();
    }
});

test("delayed theme completion retains subsequent reader scrolling and heading navigation",async()=>{
    const source=Array.from({length:200},(_,index)=>`${index%40===0?'## Heading '+index+'\n\n':''}Paragraph ${index}`).join('\n\n');
    for(const [mode,action] of [['preview','Space'],...['preview','ir','wysiwyg','sv'].flatMap(mode=>[[mode,'scroll'],[mode,'outline']])]){
        const {page,context}=await open(source,mode);
        await page.waitForFunction(()=>[...document.styleSheets].some(sheet=>sheet.href?.endsWith('/light.css')));
        // Theme geometry can trigger browser layout anchoring independently of the adapter's restoration.
        await page.evaluate(({mode,action})=>{const editor=window.latheRich.editor;window.readingTestElement=mode==='sv' && action==='outline'?editor.vditor.preview.element:document.querySelector('#viewer').hidden?editor.vditor[editor.getCurrentMode()].element:document.querySelector('#viewer');window.readingTestElement.style.overflowAnchor='none';},{mode,action});
        await page.evaluate(()=>{window.readingEvents=[];window.readingTestElement.addEventListener('scroll',()=>window.readingEvents.push(['scroll',window.readingTestElement.scrollTop]));document.addEventListener('load',event=>{if(event.target instanceof HTMLLinkElement)window.readingEvents.push(['stylesheet',event.target.href,window.readingTestElement.scrollTop]);},true);});
        if(action==='outline')await page.click('#outline-toggle');
        let release,requested;const delay=new Promise(resolve=>release=resolve),request=new Promise(resolve=>requested=resolve);
        await page.route('**/content-theme/dark.css',async route=>{requested();await delay;await route.continue();});
        await page.selectOption('#content-theme','dark');await request;
        if(action==='Space'){await page.bringToFront();await page.locator('#viewer').focus();await page.keyboard.press('Space');}
        else if(action==='scroll')assert.equal(await page.evaluate(()=>{window.readingTestElement.scrollTop=600;window.readingEvents.push(['reader-assignment',window.readingTestElement.scrollTop]);return window.readingTestElement.scrollTop;}),600,`${mode}: direct scroll assignment`);
        else await page.locator('#outline button').filter({hasText:'Heading 120'}).click();
        try{await page.waitForFunction(()=>window.readingTestElement.scrollTop>100);}
        catch(error){throw new Error(`${mode}/${action}: ${JSON.stringify(await page.evaluate(()=>({scroll:window.readingTestElement.scrollTop,height:window.readingTestElement.clientHeight,total:window.readingTestElement.scrollHeight,focus:document.activeElement?.id,documentFocused:document.hasFocus(),events:window.readingEvents})))}`,{cause:error});}
        await page.waitForTimeout(450);
        const before=await page.evaluate(()=>window.readingTestElement.scrollTop);
        release();await page.waitForFunction(()=>[...document.styleSheets].some(sheet=>sheet.href?.endsWith('/dark.css')));await page.waitForTimeout(100);
        const after=await page.evaluate(()=>window.readingTestElement.scrollTop);
        assert.equal(after,before,`${mode}/${action}: late theme must retain reader position`);
        assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source);assert.equal(await page.evaluate(()=>window.testMessages.filter(message=>['draft','edit','history'].includes(message.kind)).length),0);
        await context.close();
    }
});

test("delayed theme completion preserves saved scroll without later reader navigation",async()=>{
    const source=Array.from({length:200},(_,index)=>`Paragraph ${index}`).join('\n\n');
    for(const mode of ['ir','wysiwyg','sv','preview']){
        const {page,context}=await open(source,mode);
        await page.waitForFunction(()=>[...document.styleSheets].some(sheet=>sheet.href?.endsWith('/light.css')));
        await page.evaluate(()=>{const editor=window.latheRich.editor;const reading=document.querySelector('#viewer').hidden?editor.vditor[editor.getCurrentMode()].element:document.querySelector('#viewer');reading.scrollTop=600;});
        await page.waitForTimeout(100);
        let release,requested;const delay=new Promise(resolve=>release=resolve),request=new Promise(resolve=>requested=resolve);
        await page.route('**/content-theme/dark.css',async route=>{requested();await delay;await route.continue();});
        await page.selectOption('#content-theme','dark');await request;
        release();await page.waitForFunction(()=>[...document.styleSheets].some(sheet=>sheet.href?.endsWith('/dark.css')));await page.waitForTimeout(100);
        assert.equal(await page.evaluate(()=>{const editor=window.latheRich.editor;return (document.querySelector('#viewer').hidden?editor.vditor[editor.getCurrentMode()].element:document.querySelector('#viewer')).scrollTop;}),600,`${mode}: saved scroll`);
        assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source);assert.equal(await page.evaluate(()=>window.testMessages.filter(message=>['draft','edit','history'].includes(message.kind)).length),0);
        await context.close();
    }
});

test("theme completion preserves real navigation before its queued scroll event",async()=>{
    const source=Array.from({length:200},(_,index)=>`Paragraph ${index}`).join('\n\n');
    for(const mode of ['ir','wysiwyg','sv','preview']){
        const {page,context}=await open(source,mode);
        await page.waitForFunction(()=>[...document.styleSheets].some(sheet=>sheet.href?.endsWith('/light.css')));
        let release,requested;const delay=new Promise(resolve=>release=resolve),request=new Promise(resolve=>requested=resolve);
        await page.route('**/content-theme/dark.css',async route=>{requested();await delay;await route.continue();});
        await page.evaluate(()=>{
            window.readerAssignment=null;
            document.addEventListener('load',event=>{
                if(!(event.target instanceof HTMLLinkElement) || !event.target.href.endsWith('/dark.css'))return;
                const editor=window.latheRich.editor,reading=document.querySelector('#viewer').hidden?editor.vditor[editor.getCurrentMode()].element:document.querySelector('#viewer');
                reading.scrollTop=600;window.readerAssignment=reading.scrollTop;
            },true);
        });
        await page.selectOption('#content-theme','dark');await request;
        release();await page.waitForFunction(()=>window.readerAssignment!==null);
        assert.equal(await page.evaluate(()=>window.readerAssignment),600,`${mode}: actual direct navigation`);
        assert.equal(await page.evaluate(()=>{const editor=window.latheRich.editor;return (document.querySelector('#viewer').hidden?editor.vditor[editor.getCurrentMode()].element:document.querySelector('#viewer')).scrollTop;}),600,`${mode}: completion must not beat queued scroll event`);
        assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source);assert.equal(await page.evaluate(()=>window.testMessages.filter(message=>['draft','edit','history'].includes(message.kind)).length),0);
        await context.close();
    }
});

test("browser File imports bound legal Windows Unicode basenames before native submission",async()=>{
    const source="Before\n\nTail",png=Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAYAAABytg0kAAAAEUlEQVR4nGP4z8DwH4QZYAwAR8oH+WdZbrcAAAAASUVORK5CYII=","base64");
    const names=["\u4e2d".repeat(100)+".png","\u4e2d".repeat(251)+".png","\u{10400}".repeat(100)+".png","\u4e2d".repeat(83)+".png","a".repeat(251)+".png"];
    const directory=await mkdtemp(join(tmpdir(),"lathe-browser-image-names-"));
    if(process.platform==="win32")for(const name of names){
        assert(name.length<=255);await writeFile(join(directory,name),png);assert.equal((await stat(join(directory,name))).size,png.length);
    }
    const traces=[];
    for(const mode of ["ir","wysiwyg","sv"])for(const transfer of ["paste","drop"])for(const name of names){
        const {page,context}=await open(source,mode);await selectText(page,"Tail");
        await page.evaluate(({name,bytes,transfer})=>{
            const data=new DataTransfer();data.items.add(new File([Uint8Array.from(bytes)],name,{type:"image/png"}));
            const root=window.latheRich.editor.vditor[window.latheRich.editor.getCurrentMode()].element;
            root.dispatchEvent(transfer==="paste"?new ClipboardEvent("paste",{clipboardData:data,bubbles:true,cancelable:true}):new DragEvent("drop",{dataTransfer:data,bubbles:true,cancelable:true}));
        },{name,bytes:[...png],transfer});
        await page.waitForFunction(()=>window.testMessages.some(message=>message.kind==="import_images"));
        const request=await page.evaluate(()=>window.testMessages.find(message=>message.kind==="import_images"));
        traces.push({mode,transfer,originalName:name,request});
        assert.equal(request.position,8);assert.equal(request.base_revision,0);assert.equal(request.images[0].bytes,png.toString("base64"));
        assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source);
        await selectText(page,"Tail");await page.keyboard.insertText("Edited");await page.evaluate(()=>window.latheRich.commit());
        assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),"Before\n\nEdited");await acknowledge(page);
        await page.evaluate(()=>window.latheMarkdown.receive({kind:"flush",request:993}));
        assert.equal(await page.evaluate(()=>window.testMessages.some(message=>message.kind==="flushed"&&message.request===993)),true);
        await context.close();
    }
    if(process.env.LATHE_MARKDOWN_BROWSER_IMAGE_NAMES)await writeFile(process.env.LATHE_MARKDOWN_BROWSER_IMAGE_NAMES,JSON.stringify(traces,null,2)+"\n");
    for(const {originalName,request} of traces){
        const name=request.images[0].name;
        assert(Buffer.byteLength(name)<=255,`${Buffer.byteLength(originalName)}-byte original must fit native name budget: ${Buffer.byteLength(name)}`);
        assert(!name.includes("\uFFFD"));assert(name.endsWith(".png"));
        if(Buffer.byteLength(originalName)<=255)assert.equal(name,originalName);
    }
    console.log(`Verified ${traces.length} real File import requests; native Windows original files: ${process.platform==="win32"?names.length:0}`);
});

async function imageResourceReply(page,url,error,bytes="") {
    await page.waitForFunction(url=>window.testMessages.some(message=>message.kind==="resource"&&message.url===url&&!window.repliedImages?.includes(message.request)),url);
    try {await page.waitForFunction(({url,error,bytes})=>{
        window.repliedImages??=[];
        for(const request of window.testMessages.filter(message=>message.kind==="resource"&&message.url===url&&!window.repliedImages.includes(message.request))){
            window.repliedImages.push(request.request);
            window.latheMarkdown.receive({kind:"resource",request:request.request,mime:bytes?"image/png":"",bytes,error});
        }
        const editor=window.latheRich.editor,mode=editor.getCurrentMode();
        const root=document.querySelector('#mode').value==="preview"?document.querySelector('#viewer'):mode==="sv"?editor.vditor.preview.previewElement:editor.vditor[mode].element;
        const image=[...root.querySelectorAll('img')].find(image=>image.dataset.latheOriginal===url);
        return error?image?.classList.contains('resource-error'):image?.naturalWidth>0;
    },{url,error,bytes},{timeout:5000});}catch(failure){throw new Error(JSON.stringify(await page.evaluate(()=>({mode:document.querySelector('#mode').value,images:[...document.querySelectorAll('img')].map(image=>image.outerHTML),requests:window.testMessages.filter(message=>message.kind==="resource"),replied:window.repliedImages,status:document.querySelector('#status').textContent}))),{cause:failure});}
}
async function imageResourceState(page,url) {
    return page.evaluate(url=>{
        const rich=window.latheRich,editor=rich.editor,mode=editor.getCurrentMode();
        const root=document.querySelector('#mode').value==="preview"?document.querySelector('#viewer'):mode==="sv"?editor.vditor.preview.previewElement:editor.vditor[mode].element;
        const image=[...root.querySelectorAll('img')].find(image=>image.dataset.latheOriginal===url);
        return {width:image?.naturalWidth??0,error:image?.classList.contains('resource-error')??false,status:document.querySelector('#status').textContent};
    },url);
}
test("successful image resource reload clears only recovered image failures",async()=>{
    const png="iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAYAAABytg0kAAAAEUlEQVR4nGP4z8DwH4QZYAwAR8oH+WdZbrcAAAAASUVORK5CYII=",source="Before\n\n![a](assets/a.png)\n\n![b](assets/b.png)\n\nTail";
    for(const mode of ["preview","ir","wysiwyg","sv"]){
        const {page,context,external}=await open(source,mode);
        await imageResourceReply(page,"assets/a.png","A missing");await imageResourceReply(page,"assets/b.png","B missing");
        await page.waitForFunction(()=>document.querySelectorAll('img.resource-error').length>=2);
        assert.match((await imageResourceState(page,"assets/b.png")).status,/B missing/);
        await page.evaluate(()=>window.latheMarkdown.receive({kind:"resources_reset"}));
        await imageResourceReply(page,"assets/a.png",null,png);
        await page.waitForFunction(()=>[...document.querySelectorAll('img')].some(image=>image.dataset.latheOriginal==="assets/a.png"&&image.naturalWidth===2));
        assert.equal((await imageResourceState(page,"assets/a.png")).error,false,mode+": recovered image is not outlined");
        assert.equal((await imageResourceState(page,"assets/b.png")).error,true,mode+": remaining failure stays outlined");
        assert.match((await imageResourceState(page,"assets/b.png")).status,/B missing/);
        await imageResourceReply(page,"assets/b.png",null,png);
        await page.waitForFunction(()=>[...document.querySelectorAll('img')].filter(image=>image.src.startsWith('blob:')&&image.naturalWidth===2).length>=2);
        assert.equal((await imageResourceState(page,"assets/b.png")).error,false);
        assert.equal((await imageResourceState(page,"assets/b.png")).status,"Markdown Editor");
        assert.deepEqual(await page.evaluate(()=>({source:window.latheMarkdown.draft,mutations:window.testMessages.filter(message=>['edit','draft','undo','redo'].includes(message.kind))})),{source,mutations:[]});
        assert.equal(await page.evaluate(()=>window.testMessages.filter(message=>message.kind==="resource").every(message=>typeof message.url==="string"&&message.url.length>0)),true,"all image requests keep their actual destination");
        assert.deepEqual(external,[]);await context.close();
    }
});
test("image resource retry ignores stale failures and preserves native import notices",async()=>{
    const png="iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAYAAABytg0kAAAAEUlEQVR4nGP4z8DwH4QZYAwAR8oH+WdZbrcAAAAASUVORK5CYII=",source="Before\n\n![a](assets/a.png)\n\nTail";
    for(const mode of ["preview","ir","wysiwyg","sv"]){
        const {page,context,external}=await open(source,mode);
        await page.waitForFunction(()=>window.testMessages.some(message=>message.kind==="resource"));
        const stale=await page.evaluate(()=>window.testMessages.filter(message=>message.kind==="resource").map(message=>message.request));
        await page.evaluate(stale=>{window.repliedImages=[...stale];window.latheMarkdown.receive({kind:"resources_reset"});},stale);
        await imageResourceReply(page,"assets/a.png",null,png);
        await page.waitForFunction(()=>[...document.querySelectorAll('img')].some(image=>image.src.startsWith('blob:')&&image.naturalWidth===2));
        await page.evaluate(stale=>{for(const request of stale)window.latheMarkdown.receive({kind:"resource",request,mime:"",bytes:"",error:"Obsolete missing file"});},stale);
        assert.equal((await imageResourceState(page,"assets/a.png")).error,false,mode+": stale failure must not mark new image");
        assert.equal((await imageResourceState(page,"assets/a.png")).status,"Markdown Editor");
        await page.evaluate(()=>window.latheMarkdown.receive({kind:"resources_reset"}));
        await imageResourceReply(page,"assets/a.png","Current missing file");
        await page.waitForSelector('img.resource-error');
        await page.evaluate(()=>{window.latheMarkdown.receive({kind:"notice",text:"Image import failed: permission denied"});window.latheMarkdown.receive({kind:"resources_reset"});});
        await imageResourceReply(page,"assets/a.png",null,png);
        await page.waitForFunction(()=>[...document.querySelectorAll('img')].some(image=>image.src.startsWith('blob:')&&image.naturalWidth===2));
        assert.equal((await imageResourceState(page,"assets/a.png")).error,false);
        assert.equal((await imageResourceState(page,"assets/a.png")).status,"Image import failed: permission denied");
        await page.evaluate(()=>window.latheMarkdown.receive({kind:"resources_reset"}));
        await imageResourceReply(page,"assets/a.png","Another current image failure");
        await page.waitForSelector('img.resource-error');
        assert.equal((await imageResourceState(page,"assets/a.png")).status,"Image import failed: permission denied");
        await page.evaluate(()=>window.latheMarkdown.receive({kind:"notice",text:"Markdown Editor"}));
        assert.match((await imageResourceState(page,"assets/a.png")).status,/Another current image failure/);
        assert.deepEqual(await page.evaluate(()=>({source:window.latheMarkdown.draft,mutations:window.testMessages.filter(message=>['edit','draft','undo','redo'].includes(message.kind))})),{source,mutations:[]});
        assert.equal(await page.evaluate(()=>window.testMessages.filter(message=>message.kind==="resource").every(message=>typeof message.url==="string"&&message.url.length>0)),true,"all image requests keep their actual destination");
        assert.deepEqual(external,[]);await context.close();
    }
});

async function selectedSourceField(page) {
    await page.locator('#source-blocks').evaluate(element=>element.open=true);
    await page.locator('#blocks details').first().evaluate(element=>element.open=true);
    const field=page.locator('#blocks textarea').first();await field.focus();
    await field.evaluate(field=>field.setSelectionRange(field.value.indexOf("selected source"),field.value.indexOf("selected source")+"selected source".length));
    return field;
}
test("source control File transfers retain source and caret through asynchronous reads and flush",async()=>{
    const template="Before \u{1f600}\n\n```unknown\nselected source\n```\n\nTail",png="iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAYAAABytg0kAAAAEUlEQVR4nGP4z8DwH4QZYAwAR8oH+WdZbrcAAAAASUVORK5CYII=",traces=[];
    for(const newline of ["\n","\r\n"])for(const mode of ["ir","wysiwyg","sv"])for(const transfer of ["paste","drop"])for(const mixed of [false,true]){
        const source=template.replaceAll("\n",newline),prefix="Host \u{1f600}"+newline;
        const {page,context}=await open(source,mode),field=await selectedSourceField(page);
        const before=await field.evaluate(field=>({value:field.value,start:field.selectionStart,end:field.selectionEnd}));
        await field.evaluate((field,{transfer,mixed,png})=>{
            window.fileReadStarted=false;
            const data=new DataTransfer(),file=new File([Uint8Array.from(atob(png),character=>character.charCodeAt(0))],"source.png",{type:"image/png"});
            file.arrayBuffer=()=>{window.fileReadStarted=true;return new Promise(resolve=>window.releaseSourceFile=()=>resolve(Uint8Array.from(atob(png),character=>character.charCodeAt(0)).buffer));};
            data.items.add(file);if(mixed)data.setData("text/plain","do not replace selected source");
            const event=transfer==="paste"?new ClipboardEvent("paste",{clipboardData:data,bubbles:true,cancelable:true}):new DragEvent("drop",{dataTransfer:data,bubbles:true,cancelable:true});
            field.dispatchEvent(event);window.fileTransferPrevented=event.defaultPrevented;
        },{transfer,mixed,png});
        assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source,`${mode}/${transfer}/${mixed}: no premature deletion or text replacement`);
        assert.deepEqual(await field.evaluate(field=>({value:field.value,start:field.selectionStart,end:field.selectionEnd})),before);
        assert.equal(await page.evaluate(()=>window.fileReadStarted),true,"source File must reach importer");
        assert.equal(await page.evaluate(()=>window.testMessages.some(message=>message.kind==="import_images")),false,"read is pending");
        await page.evaluate(({source,prefix})=>window.latheMarkdown.receive({kind:"document",revision:1,text:prefix+source,writable:true,draft:null}),{source,prefix});
        assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),prefix+source);
        await page.evaluate(()=>window.releaseSourceFile());
        await page.waitForFunction(()=>window.testMessages.some(message=>message.kind==="import_images"));
        const request=await page.evaluate(()=>window.testMessages.find(message=>message.kind==="import_images"));
        assert.equal(request.position,(prefix+source).indexOf("selected source"));assert.equal(request.base_revision,1);assert.equal(request.images[0].bytes,png);
        assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),prefix+source);
        assert.equal(await page.evaluate(()=>window.testMessages.some(message=>['edit','draft','undo','redo'].includes(message.kind))),false);
        traces.push({newline:newline==="\n"?"lf":"crlf",mode,transfer,mixed,source:prefix+source,request});await context.close();
    }
    if(process.env.LATHE_MARKDOWN_SOURCE_FILE_TRACES)await writeFile(process.env.LATHE_MARKDOWN_SOURCE_FILE_TRACES,JSON.stringify(traces,null,2)+"\n");
    assert.equal(traces.length,24);console.log("24 actual source-control File requests retained LF/CRLF/Unicode source and rebased caret");
});
test("source control failed File reads preserve selection and text-only paste keeps control ownership",async()=>{
    const source="Before\r\n\r\n```unknown\r\nselected source\r\n```\r\n\r\nTail";
    for(const mode of ["ir","wysiwyg","sv"])for(const transfer of ["paste","drop"]){
        const {page,context}=await open(source,mode),field=await selectedSourceField(page);
        const before=await field.evaluate(field=>({value:field.value,start:field.selectionStart,end:field.selectionEnd}));
        await field.evaluate((field,transfer)=>{
            const data=new DataTransfer(),file=new File([new Uint8Array([1])],"failed.png",{type:"image/png"});
            file.arrayBuffer=async()=>{throw new Error("Fixture image read failed");};data.items.add(file);
            field.dispatchEvent(transfer==="paste"?new ClipboardEvent("paste",{clipboardData:data,bubbles:true,cancelable:true}):new DragEvent("drop",{dataTransfer:data,bubbles:true,cancelable:true}));
        },transfer);
        assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source,"failure must leave selected source intact");
        await page.waitForFunction(()=>/Fixture image read failed/.test(document.querySelector('#status').textContent));
        assert.deepEqual(await field.evaluate(field=>({value:field.value,start:field.selectionStart,end:field.selectionEnd})),before);
        assert.equal(await page.evaluate(()=>window.testMessages.some(message=>['edit','draft','import_images'].includes(message.kind))),false);
        await field.evaluate((field,transfer)=>{
            const data=new DataTransfer(),file=new File([new Uint8Array(4*1024*1024+1)],"large.png",{type:"image/png"});
            window.largeFileRead=false;file.arrayBuffer=async()=>{window.largeFileRead=true;throw new Error("oversize file must not be read");};data.items.add(file);
            field.dispatchEvent(transfer==="paste"?new ClipboardEvent("paste",{clipboardData:data,bubbles:true,cancelable:true}):new DragEvent("drop",{dataTransfer:data,bubbles:true,cancelable:true}));
        },transfer);
        await page.waitForFunction(()=>/4 MiB/.test(document.querySelector('#status').textContent));
        assert.equal(await page.evaluate(()=>window.largeFileRead),false);
        assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source);
        assert.deepEqual(await field.evaluate(field=>({value:field.value,start:field.selectionStart,end:field.selectionEnd})),before);
        await page.evaluate(source=>window.latheMarkdown.receive({kind:"document",revision:2,text:source,writable:false,draft:null}),source);
        await field.evaluate((field,transfer)=>{
            const data=new DataTransfer(),file=new File([new Uint8Array([1])],"readonly.png",{type:"image/png"});
            window.readOnlyFileRead=false;file.arrayBuffer=async()=>{window.readOnlyFileRead=true;throw new Error("readonly file must not be read");};data.items.add(file);
            field.dispatchEvent(transfer==="paste"?new ClipboardEvent("paste",{clipboardData:data,bubbles:true,cancelable:true}):new DragEvent("drop",{dataTransfer:data,bubbles:true,cancelable:true}));
        },transfer);
        assert.equal(await page.evaluate(()=>window.readOnlyFileRead),false);
        assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source);
        assert.equal(await page.evaluate(()=>window.testMessages.some(message=>['edit','draft','import_images'].includes(message.kind))),false);
        await page.evaluate(source=>window.latheMarkdown.receive({kind:"document",revision:3,text:source,writable:true,draft:null}),source);
        await page.locator('#find').evaluate(element=>element.hidden=false);await page.locator('#query').focus();
        await page.locator('#query').evaluate(field=>{const data=new DataTransfer();data.items.add(new File([new Uint8Array([1])],"find.png",{type:"image/png"}));const event=new ClipboardEvent('paste',{clipboardData:data,bubbles:true,cancelable:true});field.dispatchEvent(event);window.findFilePrevented=event.defaultPrevented;});
        assert.equal(await page.evaluate(()=>window.findFilePrevented),false);
        assert.equal(await page.evaluate(()=>window.testMessages.some(message=>['import_images','pick_images'].includes(message.kind))),false);
        const currentField=await selectedSourceField(page);
        await currentField.evaluate(field=>{const data=new DataTransfer();data.setData('text/plain','replacement text');field.dispatchEvent(new ClipboardEvent('paste',{clipboardData:data,bubbles:true,cancelable:true}));});
        assert.equal(await page.evaluate(()=>window.latheMarkdown.draft),source.replace('selected source','replacement text'));
        await context.close();
    }
});
