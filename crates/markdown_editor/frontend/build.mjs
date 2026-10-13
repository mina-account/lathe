import { stripTypeScriptTypes } from "node:module";
import { readFile, writeFile, mkdir, copyFile } from "node:fs/promises";
for (const name of ["application", "rich"]) {
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
