import {copyFile, mkdir, readFile, readdir, writeFile, stat} from "node:fs/promises";
import {createHash} from "node:crypto";
import {resolve, join} from "node:path";
import {fileURLToPath} from "node:url";

const [sourceArgument, destinationArgument] = process.argv.slice(2);
if (!sourceArgument || !destinationArgument) throw new Error("Usage: node script/package-markdown-runtime.mjs <Cargo binary folder> <installed libexec folder>");
const source = resolve(sourceArgument), destination = resolve(destinationArgument);
if (source === destination) throw new Error("Runtime staging must use a separate destination");
const archive = JSON.parse(await readFile(join(source,"archive.json"),"utf8"));
if (archive.type !== "minimal" || !/^cef_binary_154\.0\.34\+g14c5a08\+chromium-154\.0\.8037\.98_(windows|linux)/.test(archive.name) || !/^[a-f0-9]{40}$/.test(archive.sha1)) {
    throw new Error("Markdown requires the pinned CEF 154.0.34 minimal runtime copied by cef-dll-sys 154.5.0");
}
const windows = archive.name.includes("_windows"), library = windows ? "libcef.dll" : "libcef.so";
const required = [library,"icudtl.dat","resources.pak","chrome_100_percent.pak","chrome_200_percent.pak","v8_context_snapshot.bin","locales/en-US.pak","CREDITS.html"];
for (const path of required) {
    if (!(await stat(join(source,path)).catch(()=>null))?.isFile()) throw new Error(`Incomplete Markdown runtime: ${path}. Build with --features zed/markdown-browser before packaging.`);
}
await mkdir(destination,{recursive:true});
const files = required.filter(path=>!path.startsWith("locales/"));
const platformFiles = windows ? ["chrome_elf.dll","d3dcompiler_47.dll","dxcompiler.dll","dxil.dll","vk_swiftshader.dll","vulkan-1.dll"] : ["libvk_swiftshader.so","libvulkan.so.1"];
for (const path of [...platformFiles,"vk_swiftshader_icd.json"]) {
    if (!(await stat(join(source,path)).catch(()=>null))?.isFile()) throw new Error(`Incomplete pinned CEF runtime: ${path}`);
    files.push(path);
}
for (const entry of await readdir(join(source,"locales"),{withFileTypes:true})) if (entry.isFile() && entry.name.endsWith(".pak")) files.push(`locales/${entry.name}`);
const manifest = {cef:"154.0.34",bindings:"154.5.0",chromium:"154.0.8037.98",archive,helper:"The installed Lathe editor executable handles CEF --type subprocesses",files:[]};
for (const path of files.sort()) {
    const target = join(destination,path);
    await mkdir(resolve(target,".."),{recursive:true});
    await copyFile(join(source,path),target);
    const bytes = await readFile(target);
    manifest.files.push({path,bytes:bytes.length,sha256:createHash("sha256").update(bytes).digest("hex")});
}
const repository = fileURLToPath(new URL("../",import.meta.url));
await copyFile(join(repository,"crates/markdown_editor/CEF-LICENSE.txt"),join(destination,"CEF-LICENSE.txt"));
const notices = join(destination,"markdown-editor-notices");
await mkdir(notices,{recursive:true});
const vendor = join(repository,"crates/markdown_editor/frontend/vendor");
for (const name of ["VDITOR-LICENSE","DOMPURIFY-LICENSE","DIFF-MATCH-PATCH-LICENSE"]) await copyFile(join(vendor,name),join(notices,name));
await copyFile(join(vendor,"dist/js/highlight.js/LICENSE"),join(notices,"HIGHLIGHT-JS-LICENSE"));
await copyFile(join(repository,"crates/markdown_editor/NOTICES.md"),join(notices,"NOTICES.md"));
await copyFile(join(vendor,"manifest.json"),join(notices,"frontend-manifest.json"));
await writeFile(join(destination,"markdown-runtime.json"),JSON.stringify(manifest,null,2)+"\n");
await writeFile(join(destination,"markdown-runtime.sha256"),manifest.files.map(file=>`${file.sha256}  ${file.path}`).join("\n")+"\n");
console.log(`Staged pinned CEF ${manifest.cef}: ${manifest.files.length} files and Markdown notices into ${destination}`);
