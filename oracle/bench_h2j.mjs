import { createRequire } from 'node:module';
import { parse } from 'svelte/compiler';
import fs from 'node:fs';
import path from 'node:path';
const require = createRequire(import.meta.url);
const { MagicString, parseHtmlx, convertHtmlxToJsx } = require('./svelte2tsx-internals.cjs').__internals;
const dir = process.argv[2];
const files = fs.globSync('**/*.svelte', { cwd: dir }).map((f) => fs.readFileSync(path.join(dir, f), 'utf-8'));
console.error = () => {};
const run = () => { for (const src of files) { try { const { htmlxAst, tags } = parseHtmlx(src, parse, { svelte5Plus: true }); const s = new MagicString(src); convertHtmlxToJsx(s, htmlxAst, tags, { svelte5Plus: true, typingsNamespace: 'svelteHTML' }); s.toString(); } catch {} } };
for (let i = 0; i < 3; i++) run();
const t = []; for (let i = 0; i < 15; i++) { const s = performance.now(); run(); t.push(performance.now() - s); }
t.sort((a, b) => a - b); console.log(`js   htmlx2jsx: ${files.length} files, median ${t[t.length >> 1].toFixed(1)} ms`);
