import {readFile,readdir,writeFile} from "node:fs/promises";
import {createHash} from "node:crypto";
const root=new URL("../../../",import.meta.url),frontend=new URL("crates/markdown_editor/frontend/",root);
const hash=bytes=>createHash("sha256").update(bytes).digest("hex");
async function files(directory,prefix="") {
    const result=[];
    for(const entry of await readdir(directory,{withFileTypes:true})) {
        if(entry.isDirectory())result.push(...await files(new URL(entry.name+"/",directory),prefix+entry.name+"/"));
        else result.push(prefix+entry.name);
    }
    return result;
}
const generated=["application.js","rich.js","renderers.js","themes.js",...await files(new URL("vendor/",frontend),"vendor/")];
const assets={};
for(const path of generated)assets["crates/markdown_editor/frontend/"+path]=hash(await readFile(new URL(path,frontend)));
assets["crates/markdown_editor/src/packaged_resources.rs"]=hash(await readFile(new URL("crates/markdown_editor/src/packaged_resources.rs",root)));
const sourceHashesNormalizedLF={};
for(const path of ["package.json","package-lock.json","build.mjs","application.ts","rich.ts","renderers.ts","index.html","application.css"]) sourceHashesNormalizedLF[path]=hash((await readFile(new URL(path,frontend),"utf8")).replaceAll("\r\n","\n"));
const baseline=JSON.parse(await readFile(new URL("project-docs/imp-plans/markdown-editor-baseline/runtime.json",root),"utf8"));
await writeFile(new URL("runtime.json",import.meta.url),JSON.stringify({baselineSourceCommit:baseline.sourceCommit,baselineExtension:baseline.extension,baselineVersion:baseline.version,vditor:"3.11.2",katex:"0.16.9",markmapEmbeddedKatex:"0.16.0; isolated from the pinned math global",node:"24.16.0",playwright:"1.56.1",browser:"Microsoft Edge155.0.4283.45",cefBindings:"154.5.0",cefRuntime:"154.0.34",generatedAssetCount:generated.length+1,pinnedDependencyHashes:332,sourceHashesNormalizedLF,assets},null,2)+"\n");
console.log(`Recorded ${generated.length+1} generated asset hashes`);
