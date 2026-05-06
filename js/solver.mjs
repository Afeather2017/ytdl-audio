import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import * as meriyah from "meriyah";
import * as astring from "astring";

const require = createRequire(import.meta.url);
const coreCode = readFileSync(new URL("./yt.solver.core.js", import.meta.url), "utf-8");

globalThis.meriyah = meriyah;
globalThis.astring = astring;
const jsc = eval(`${coreCode}\n; jsc;`);

let input = "";
process.stdin.setEncoding("utf-8");
process.stdin.on("data", (chunk) => {
  input += chunk;
});
process.stdin.on("end", () => {
  try {
    const parsed = JSON.parse(input);
    const result = jsc(parsed);
    process.stdout.write(JSON.stringify(result));
  } catch (e) {
    process.stdout.write(
      JSON.stringify({ type: "error", error: e.message + "\n" + e.stack })
    );
  }
});
