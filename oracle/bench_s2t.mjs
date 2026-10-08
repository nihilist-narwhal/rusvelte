// Time svelte2tsx over a corpus with svelte-check's options (sources preloaded)
import { svelte2tsx } from 'svelte2tsx';
import { parse, VERSION } from 'svelte/compiler';
import fs from 'node:fs';
import path from 'node:path';
const dir = process.argv[2];
const files = fs.globSync('**/*.svelte', { cwd: dir }).map((f) => [path.basename(f), fs.readFileSync(path.join(dir, f), 'utf-8')]);
console.error = () => {};
const run = () => {
	for (const [filename, src] of files) {
		try {
			svelte2tsx(src, { parse, version: VERSION, filename, isTsFile: /lang=["']ts["']/.test(src), mode: 'ts', emitOnTemplateError: false, emitJsDoc: true });
		} catch {}
	}
};
for (let i = 0; i < 3; i++) run();
const t = [];
for (let i = 0; i < 15; i++) { const s = performance.now(); run(); t.push(performance.now() - s); }
t.sort((a, b) => a - b);
console.log(`js   svelte2tsx: ${files.length} files, median ${t[t.length >> 1].toFixed(1)} ms`);
