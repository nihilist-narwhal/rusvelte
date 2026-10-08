// Time `compile(source, { dev: true, generate: false, filename })` over a corpus (warm).
//   node bench_compile.mjs <corpus dir> [iters]
import { compile } from 'svelte/compiler';
import fs from 'node:fs';
import path from 'node:path';

const [corpus, iters = '10'] = process.argv.slice(2);
const files = fs
	.globSync('**/*.svelte', { cwd: corpus })
	.sort()
	.map((rel) => [fs.readFileSync(path.join(corpus, rel), 'utf-8'), path.basename(rel)]);

function run() {
	for (const [source, filename] of files) {
		try {
			compile(source, { dev: true, generate: false, filename });
		} catch {}
	}
}

for (let i = 0; i < 5; i++) run();
const times = [];
for (let i = 0; i < +iters; i++) {
	const t = performance.now();
	run();
	times.push(performance.now() - t);
}
times.sort((a, b) => a - b);
console.log(`${files.length} files: median ${times[times.length >> 1].toFixed(1)} ms/pass`);
