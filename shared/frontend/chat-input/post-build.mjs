import { createHash } from "crypto";
import { readFileSync, writeFileSync } from "fs";
import { dirname, resolve } from "path";
import { fileURLToPath } from "url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const staticDir = resolve(__dirname, "../../../predefined/plugins/team/static");
const jsFile = resolve(staticDir, "chat-input.js");
const hashFile = resolve(staticDir, "chat-input.hash");

const content = readFileSync(jsFile);
const hash = createHash("sha256").update(content).digest("hex").slice(0, 12);
writeFileSync(hashFile, hash);
console.log(`  → chat-input.hash: ${hash}`);
