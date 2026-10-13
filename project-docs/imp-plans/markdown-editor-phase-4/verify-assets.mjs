import assert from "node:assert/strict";
import {readFile,readdir,mkdir,copyFile} from "node:fs/promises";
import {createHash} from "node:crypto";
import {fileURLToPath} from "node:url";
import {spawnSync} from "node:child_process";

const root=new URL("../../../",import.meta.url);
const frontend=new URL("crates/markdown_editor/frontend/",root);
const hash=bytes=>createHash("sha256").update(bytes).digest("hex");
async function files(directory,prefix="") {
    const result=[];
    for(const entry of await readdir(directory,{withFileTypes:true})) {
        const path=prefix+entry.name;
        if(entry.isDirectory()) result.push(...await files(new URL(entry.name+"/",directory),path+"/"));
        else result.push(path);
    }
    return result;
}
const generated=["application.js","rich.js","renderers.js","themes.js",...await files(new URL("vendor/",frontend),"vendor/")];
const manifest=JSON.parse(await readFile(new URL("vendor/manifest.json",frontend),"utf8"));
for(const entry of manifest) if(entry.sha256) {
    const bytes=await readFile(new URL(entry.path,frontend));
    assert.equal(hash(bytes),entry.sha256,entry.path);assert.equal(bytes.length,entry.bytes,entry.path);
}
if(process.argv[2]) {
    const isolated=new URL("file:///"+process.argv[2].replaceAll("\\","/").replace(/\/$/,"")+"/");
    const destination=new URL("crates/markdown_editor/frontend/",isolated);
    await mkdir(destination,{recursive:true});await mkdir(new URL("crates/markdown_editor/src/",isolated),{recursive:true});
    await mkdir(new URL("project-docs/imp-plans/markdown-editor-baseline/",isolated),{recursive:true});
    for(const path of ["package.json","package-lock.json","build.mjs","application.ts","rich.ts","renderers.ts"]) await copyFile(new URL(path,frontend),new URL(path,destination));
    await copyFile(new URL("project-docs/imp-plans/markdown-editor-baseline/runtime.json",root),new URL("project-docs/imp-plans/markdown-editor-baseline/runtime.json",isolated));
    for(const [command,args] of [[process.platform==="win32"?"npm.cmd":"npm",["ci","--ignore-scripts"]],[process.execPath,["build.mjs"]]]) {
        const result=spawnSync(command,args,{cwd:fileURLToPath(destination),encoding:"utf8",shell:process.platform==="win32"&&command==="npm.cmd"});
        process.stdout.write(result.stdout??"");process.stderr.write(result.stderr??"");assert.equal(result.status,0,command);
    }
    for(const path of generated) assert.equal(hash(await readFile(new URL(path,destination))),hash(await readFile(new URL(path,frontend))),path);
    assert.equal(hash(await readFile(new URL("../src/packaged_resources.rs",destination))),hash(await readFile(new URL("../src/packaged_resources.rs",frontend))));
    console.log(`Isolated npm ci/build reproduced ${generated.length+1} exact generated assets`);
}
console.log(`Verified ${manifest.filter(entry=>entry.sha256).length} pinned dependency hashes`);
