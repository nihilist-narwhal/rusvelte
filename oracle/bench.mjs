// Time svelte/compiler's parse() over the corpus (sources preloaded, warm JIT)
import { parse } from 'svelte/compiler';
import fs from 'node:fs';
import path from 'node:path';
const [corpus, iters = 10] = process.argv.slice(2);
const files = JSON.parse(fs.readFileSync('expected/manifest.json'))
	.map((m) => ({ src: fs.readFileSync(path.join(corpus, m.rel), 'utf-8'), loose: m.loose }));
const bytes = files.reduce((n, f) => n + f.src.length, 0);
const run = () => { for (const f of files) { try { parse(f.src, { modern: true, loose: f.loose }); } catch {} } };
for (let i = 0; i < 3; i++) run(); // warmup
const times = [];
for (let i = 0; i < +iters; i++) { const t = performance.now(); run(); times.push(performance.now() - t); }
times.sort((a, b) => a - b);
console.log(`js:   ${files.length} files, ${(bytes / 1e6).toFixed(2)} MB, median ${times[times.length >> 1].toFixed(1)} ms/pass`);
