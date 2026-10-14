import assert from "node:assert/strict";
import {copyFile,mkdir,mkdtemp,readFile} from "node:fs/promises";
import {tmpdir} from "node:os";
import {join} from "node:path";
import {pathToFileURL,fileURLToPath} from "node:url";
import {spawnSync} from "node:child_process";

const root=new URL("../../../",import.meta.url);
const runtime=JSON.parse(await readFile(new URL("runtime.json",import.meta.url),"utf8"));
const directory=await mkdtemp(join(tmpdir(),"lathe-phase4-runtime-"));
const isolated=pathToFileURL(directory+"/");
const evidence="project-docs/imp-plans/markdown-editor-phase-4/";
const paths=new Set([
    ...Object.keys(runtime.assets),
    ...Object.keys(runtime.sourceHashesNormalizedLF).map(path=>"crates/markdown_editor/frontend/"+path),
    "crates/markdown_editor/src/assets.rs","crates/markdown_editor/src/item.rs","crates/fs/src/fs.rs",
    "project-docs/imp-plans/markdown-editor-baseline/runtime.json",
    ...["record-runtime.mjs","verify.mjs","checks.json","image-name-requests.json","source-file-requests.json","windows-observed.jsonl","wayland-observed.jsonl","x11-observed.jsonl"].map(path=>evidence+path),
]);
for(const path of paths) {
    const destination=new URL(path,isolated);
    await mkdir(new URL("./",destination),{recursive:true});
    await copyFile(new URL(path,root),destination);
}
for(const script of ["record-runtime.mjs","verify.mjs"]) {
    const result=spawnSync(process.execPath,[fileURLToPath(new URL(evidence+script,isolated))],{encoding:"utf8"});
    process.stdout.write(result.stdout??"");process.stderr.write(result.stderr??"");
    assert.equal(result.status,0,script);
}
const generated=JSON.parse(await readFile(new URL(evidence+"runtime.json",isolated),"utf8"));
assert.equal(Object.keys(generated.nativeSourceHashesNormalizedLF).length,3);
console.log(`Runtime regeneration and verification passed without field patching: ${directory}`);
