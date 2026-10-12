import { stripTypeScriptTypes } from "node:module";
import { readFile, writeFile } from "node:fs/promises";
const source = await readFile(new URL("application.ts", import.meta.url), "utf8");
const javascript = stripTypeScriptTypes(source, { mode: "strip" }).replace(/[ \t]+$/gm, "");
await writeFile(new URL("application.js", import.meta.url), javascript);
