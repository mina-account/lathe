
const dependencies                          = {
    mermaid:["mermaid/mermaid.min.js"], graphviz:["graphviz/viz.js"], echarts:["echarts/echarts.min.js"],
    abc:["abcjs/abcjs_basic.min.js"], flowchart:["flowchart.js/flowchart.min.js"],
    markmap:["markmap/markmap.min.js"], smiles:["smiles-drawer/smiles-drawer.min.js"],
    math:["katex/katex.min.js","katex/mhchem.min.js"], mindmap:["echarts/echarts.min.js"],
    plantuml:["plantuml/plantuml-encoder.min.js"],
};
const loaded = new Map                      ();
let pinnedMath    ;
let identifier = 0;
export const rendererLanguages = Object.keys(dependencies);
export async function highlightCode(root            )               {
    await script("highlight.js/highlight.min.js");
    await script("highlight.js/third-languages.js");
    for(const code of root.querySelectorAll             ("pre code")) {
        if(code.dataset.highlighted || !code.isConnected)continue;
        const language=[...code.classList].find(name=>name.startsWith("language-"))?.slice(9) ?? "plaintext";
        if(rendererLanguages.includes(language))continue;
        const highlighter=(window       ).hljs;
        const result=highlighter.highlight(code.textContent ?? "",{language:highlighter.getLanguage(language)?language:"plaintext",ignoreIllegals:true});
        code.innerHTML=sanitizedOutput(result.value);code.classList.add("hljs");code.dataset.highlighted="true";
    }
}
function script(path       )               {
    if (!loaded.has(path)) loaded.set(path,new Promise((resolve,reject)=>{
        const element=document.createElement("script"); element.src=`/vendor/dist/js/${path}`;
        element.id=`lathe-renderer-${path.replace(/[^a-z0-9]/gi,"-")}`;
        element.onload=()=>{
            if(path==="katex/katex.min.js") pinnedMath=(window       ).katex;
            // Markmap embeds an older KaTeX and assigns it to the shared global.
            if(path==="markmap/markmap.min.js") (window       ).katex=pinnedMath;
            resolve();
        }; element.onerror=()=>reject(new Error(`Packaged renderer missing: ${path}`));
        document.head.append(element);
    }));
    return loaded.get(path) ;
}
export function sanitizedOutput(html       )        {
    const template=document.createElement("template");template.innerHTML=html;
    for(const label of template.content.querySelectorAll("foreignObject")) {
        const content=document.createElement("template");content.innerHTML=DOMPurify.sanitize(label.innerHTML,{USE_PROFILES:{html:true},FORBID_TAGS:["script","style","img","iframe","object","embed"],ALLOW_DATA_ATTR:false});
        const text=document.createElementNS("http://www.w3.org/2000/svg","text");
        text.textContent=content.content.textContent;text.setAttribute("x",String(Number(label.getAttribute("x"))||0));
        text.setAttribute("y",String((Number(label.getAttribute("y"))||0)+16));text.setAttribute("font-size","14");
        label.replaceWith(text);
    }
    return DOMPurify.sanitize(template.innerHTML,{
        USE_PROFILES:{html:true,svg:true,svgFilters:true,mathMl:true},
        FORBID_TAGS:["script","style","foreignobject","iframe","object","embed","image","a","form","input","button","textarea","select","audio","video","link","meta","base"],
        FORBID_ATTR:["href","xlink:href","src","srcset","name","tabindex"], ALLOW_DATA_ATTR:false,
    });
}
function validateData(value    ,depth=0)      {
    if (depth>32) throw new Error("Chart data exceeds its nesting limit.");
    if (typeof value === "string" && /(?:javascript:|data:|https?:|file:|image:\/\/|<|url\s*\()/i.test(value)) throw new Error("Chart data contains an unsupported URL or markup.");
    if (value && typeof value === "object") for (const [key,child] of Object.entries(value)) {
        if (["__proto__","constructor","prototype"].includes(key)) throw new Error("Unsafe chart property.");
        validateData(child,depth+1);
    }
}
function tree(source       )     {
    const root={name:"",children:[]         }, stack=[{indent:-1,node:root}];
    for (const line of source.split(/\r?\n/).filter(line=>line.trim())) {
        const match=line.match(/^(\s*)[-*+]\s+(.+)$/);
        if (!match) throw new Error("Mindmap expects a nested Markdown list.");
        const indent=match[1].replace(/\t/g,"    ").length;
        while (stack.length>1 && stack.at(-1) .indent>=indent) stack.pop();
        const node={name:match[2],children:[]         }; stack.at(-1) .node.children.push(node); stack.push({indent,node});
    }
    if (root.children.length !== 1) throw new Error("Mindmap expects one root.");
    return root.children[0];
}
export async function renderFence(element            ,language       ,source       ,plantuml                                 )               {
    const token=String(++identifier); element.dataset.renderToken=token;
    element.dataset.renderer=language; element.dataset.state="pending"; element.textContent="Rendering…";
    if (!dependencies[language]) return;
    try {
        if (!source.trim() || source.length>128*1024) throw new Error("Renderer input is empty or exceeds 128 KiB.");
        if(language==="markmap") for(const path of dependencies.math) await script(path);
        for (const path of dependencies[language]) await script(path);
        const runtime=window       , output=document.createElement("div"); output.style.cssText="width:800px;height:320px";
        // Libraries that measure SVG need a mounted staging area. It cannot receive
        // document HTML; only renderer-owned nodes are created here.
        output.hidden=false; output.style.position="absolute"; output.style.left="-10000px"; document.body.append(output);
        try {
            switch (language) {
                case "math": output.innerHTML=pinnedMath.renderToString(source,{displayMode:element.tagName!=="SPAN",throwOnError:true,trust:false,output:"html",maxExpand:1000,maxSize:100}); break;
                case "mermaid": {
                    runtime.mermaid.initialize({startOnLoad:false,securityLevel:"strict",suppressErrorRendering:true,maxTextSize:128*1024,flowchart:{htmlLabels:false},theme:"default"});
                    const result=await runtime.mermaid.render(`lathe-diagram-${++identifier}`,source); output.innerHTML=sanitizedOutput(result.svg); break;
                }
                case "graphviz": {
                    const worker=new Worker("/vendor/dist/js/graphviz/full.render.js");
                    try { output.innerHTML=sanitizedOutput((await new runtime.Viz({worker}).renderSVGElement(source)).outerHTML); } finally { worker.terminate(); }
                    break;
                }
                case "echarts": case "mindmap": {
                    const data=language==="echarts" ? JSON.parse(source) : {series:[{type:"tree",data:[tree(source)],initialTreeDepth:-1,label:{position:"right"}}]};
                    validateData(data);
                    if (language==="echarts" && (!data || typeof data!=="object" || !Array.isArray(data.series))) throw new Error("ECharts expects a JSON object with a series array.");
                    const chart=runtime.echarts.init(output,null,{renderer:"svg",width:800,height:320});
                    let svg="";
                    try { chart.setOption({...data,animation:false,tooltip:{show:false}}); svg=chart.renderToSVGString(); } finally { chart.dispose(); }
                    output.innerHTML=sanitizedOutput(svg);
                    break;
                }
                case "abc": {
                    if (/[<>]|(?:javascript|https?|file):/i.test(source)) throw new Error("ABC labels cannot contain markup or URLs.");
                    if (!/^X:\s*\d+/m.test(source) || !/^K:/m.test(source)) throw new Error("ABC notation requires X: and K: headers.");
                    const tunes=runtime.ABCJS.renderAbc(output,source); if (!tunes.length || tunes.some((tune    )=>tune.warnings?.length)) throw new Error(tunes.flatMap((tune    )=>tune.warnings??[]).join("; ") || "Invalid ABC notation."); break;
                }
                case "flowchart":
                    if (/<|(?:javascript|https?|file):/i.test(source)) throw new Error("Flowchart labels cannot contain markup or URLs.");
                    runtime.flowchart.parse(source).drawSVG(output); break;
                case "markmap": {
                    if (!/^\s*(?:#|[-*+])\s+/m.test(source)) throw new Error("Markmap expects Markdown headings or lists.");
                    const transformed=new runtime.markmap.Transformer().transform(source);
                    const clean=(node    )=>{node.content=sanitizedOutput(node.content); for(const child of node.children??[]) clean(child);}; clean(transformed.root);
                    const svg=document.createElementNS("http://www.w3.org/2000/svg","svg"); svg.style.cssText="width:800px;height:320px"; output.append(svg);
                    const map=runtime.markmap.Markmap.create(svg,{duration:0,zoom:false,pan:false},transformed.root); await map.fit(); break;
                }
                case "smiles": {
                    await new Promise      ((resolve,reject)=>runtime.SmilesDrawer.parse(source,(data    )=>{
                        const svg=document.createElementNS("http://www.w3.org/2000/svg","svg"); svg.id=`lathe-smiles-${++identifier}`; output.append(svg);
                        try { new runtime.SmilesDrawer.SvgDrawer({}).draw(data,svg,"light"); resolve(); } catch(error) { reject(error); }
                    },reject)); break;
                }
                case "plantuml": output.innerHTML=sanitizedOutput(await plantuml(runtime.plantumlEncoder.encode(source))); break;
            }
            const html=sanitizedOutput(output.innerHTML);
            if (!html || language!=="math" && !html.includes("<svg")) throw new Error("Renderer produced no diagram.");
            if(element.dataset.renderToken!==token)return;
            element.innerHTML=html; element.dataset.state="ready";
        } finally { output.remove(); }
    } catch(error) { if(element.dataset.renderToken===token) { element.textContent=`${language} render error: ${String(error)}`; element.dataset.state="error"; } }
}
