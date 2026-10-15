import assert from "node:assert/strict";
import {test} from "node:test";
import {mkdtemp, mkdir, readFile, writeFile} from "node:fs/promises";
import {tmpdir} from "node:os";
import {join} from "node:path";
import {spawnSync} from "node:child_process";
import {createHash} from "node:crypto";
import {fileURLToPath} from "node:url";

const staging = fileURLToPath(new URL("package-markdown-runtime.mjs",import.meta.url));
for (const platform of ["windows","linux"]) test(`Pinned ${platform} runtime stages only its closure and rejects missing resources`,async()=>{
    const directory = await mkdtemp(join(tmpdir(),"lathe-markdown-package-test-"));
    const source = join(directory,"source"), installed = join(directory,"installed");
    await mkdir(join(source,"locales"),{recursive:true});
    const files = ["icudtl.dat","resources.pak","chrome_100_percent.pak","chrome_200_percent.pak","v8_context_snapshot.bin","CREDITS.html","vk_swiftshader_icd.json","locales/en-US.pak",...(platform === "windows" ? ["libcef.dll","chrome_elf.dll","d3dcompiler_47.dll","dxcompiler.dll","dxil.dll","vk_swiftshader.dll","vulkan-1.dll"] : ["libcef.so","libvk_swiftshader.so","libvulkan.so.1"])];
    const archive = {type:"minimal",name:`cef_binary_154.0.34+g14c5a08+chromium-154.0.8037.98_${platform}64_minimal.tar.bz2`,sha1:"a".repeat(40)};
    await writeFile(join(source,"archive.json"),JSON.stringify(archive));
    for (const path of files) if (path !== "resources.pak") await writeFile(join(source,path),path);
    let result = spawnSync(process.execPath,[staging,source,installed],{encoding:"utf8"});
    assert.notEqual(result.status,0);
    assert.match(result.stderr,/Incomplete Markdown runtime: resources.pak/);
    await writeFile(join(source,"resources.pak"),"resources.pak");
    await writeFile(join(source,"unused-build-file.dll"),"not part of CEF");
    result = spawnSync(process.execPath,[staging,source,installed],{encoding:"utf8"});
    assert.equal(result.status,0,result.stderr);
    const manifest = JSON.parse(await readFile(join(installed,"markdown-runtime.json"),"utf8"));
    assert.equal(manifest.cef,"154.0.34");
    assert.equal(manifest.bindings,"154.5.0");
    assert.deepEqual(manifest.archive,archive);
    assert.deepEqual(manifest.files.map(file=>file.path).sort(),files.sort());
    for (const file of manifest.files) {
        const bytes = await readFile(join(installed,file.path));
        assert.equal(bytes.length,file.bytes);
        assert.equal(createHash("sha256").update(bytes).digest("hex"),file.sha256);
    }
    assert.match(await readFile(join(installed,"markdown-editor-notices/NOTICES.md"),"utf8"),/redistributed dependencies/);
    assert.match(await readFile(join(installed,"CEF-LICENSE.txt"),"utf8"),/Chromium Embedded[\s/]*Framework/);
});
