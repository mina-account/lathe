import { stripTypeScriptTypes } from "node:module";
import { readFile, writeFile, mkdir, copyFile, readdir } from "node:fs/promises";
import { createHash } from "node:crypto";
for (const name of ["application", "rich", "renderers"]) {
    const source = (await readFile(new URL(`${name}.ts`, import.meta.url), "utf8")).replace(/\r\n/g, "\n");
    const javascript = stripTypeScriptTypes(source, { mode: "strip" }).replace(/[ \t]+$/gm, "").trimEnd() + "\n";
    await writeFile(new URL(`${name}.js`, import.meta.url), javascript);
}
await mkdir(new URL("vendor", import.meta.url), {recursive:true});
for (const [source, target] of [
    ["vditor/dist/index.min.js", "vditor.js"], ["vditor/dist/index.css", "vditor.css"],
    ["vditor/dist/js/lute/lute.min.js", "lute.js"], ["vditor/dist/js/i18n/en_US.js", "en_US.js"],
    ["vditor/dist/js/icons/ant.js", "icons.js"],
    ["dompurify/dist/purify.min.js", "purify.js"],
    ["vditor/LICENSE", "VDITOR-LICENSE"], ["dompurify/LICENSE", "DOMPURIFY-LICENSE"],
    ["diff-match-patch/LICENSE", "DIFF-MATCH-PATCH-LICENSE"],
]) await copyFile(new URL(`node_modules/${source}`, import.meta.url), new URL(`vendor/${target}`, import.meta.url));
const packaged = [];
async function copyTree(source, target) {
    await mkdir(new URL(target,import.meta.url),{recursive:true});
    for (const entry of await readdir(new URL(source,import.meta.url),{withFileTypes:true})) {
        if (entry.isDirectory()) await copyTree(`${source}/${entry.name}`,`${target}/${entry.name}`);
        else {
            const path=`${target}/${entry.name}`;
            await copyFile(new URL(`${source}/${entry.name}`,import.meta.url),new URL(path,import.meta.url));
            const bytes=await readFile(new URL(path,import.meta.url));
            packaged.push({path,bytes:bytes.length,sha256:createHash("sha256").update(bytes).digest("hex")});
        }
    }
}
for (const directory of ["abcjs","echarts","flowchart.js","graphviz","highlight.js","katex","markmap","mermaid","plantuml","smiles-drawer"]) {
    await copyTree(`node_modules/vditor/dist/js/${directory}`,`vendor/dist/js/${directory}`);
}
await copyTree("node_modules/vditor/dist/css/content-theme","vendor/dist/css/content-theme");
const baseline=JSON.parse(await readFile(new URL("../../../project-docs/imp-plans/markdown-editor-baseline/runtime.json",import.meta.url),"utf8"));
await writeFile(new URL("themes.js",import.meta.url),"export const themes="+JSON.stringify({content:baseline.contentThemes,code:baseline.codeThemes})+";\n");
packaged.push({path:"themes.js"},{path:"renderers.js"});
const mime=path=>path.endsWith(".js")?"text/javascript":path.endsWith(".css")?"text/css":path.endsWith(".woff2")?"font/woff2":path.endsWith(".woff")?"font/woff":path.endsWith(".ttf")?"font/ttf":path.endsWith(".svg")?"image/svg+xml":path.endsWith(".png")?"image/png":path.endsWith(".json")?"application/json":"application/octet-stream";
packaged.sort((left,right)=>left.path.localeCompare(right.path));
await writeFile(new URL("vendor/manifest.json",import.meta.url),JSON.stringify(packaged,null,2)+"\n");
await writeFile(new URL("../src/packaged_resources.rs",import.meta.url),`#[rustfmt::skip]\npub fn resource(path: &str) -> Option<(&'static str, &'static [u8])> {\n    match path {\n${packaged.map(({path})=>`        "/${path}" => Some(("${mime(path)}", include_bytes!("../frontend/${path}"))),`).join("\n")}\n        _ => None,\n    }\n}\n`);
