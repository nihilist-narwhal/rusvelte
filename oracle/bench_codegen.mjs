// Times svelte/compiler over a codegen oracle corpus (each record's options), client and server,
// warm: `node bench_codegen.mjs <oracle out dir>`. Compare with `compare_codegen`'s "ms compiling".
import { compile, compileModule } from 'svelte/compiler';
import fs from 'node:fs';
import path from 'node:path';
const dir = process.argv[2];
const manifest = JSON.parse(fs.readFileSync(path.join(dir, 'manifest.json'), 'utf-8'));
const jobs = [];
for (const e of manifest) {
	const r = JSON.parse(fs.readFileSync(path.join(dir, e.id + '.json'), 'utf-8'));
	if (r.error) continue;
	jobs.push({ text: fs.readFileSync(e.source, 'utf-8').replace(/\r\n/g, '\n'), module: r.module, options: r.options });
}
function run(gen) {
	const t = performance.now();
	for (const j of jobs) if (j.options.generate === gen) (j.module ? compileModule : compile)(j.text, j.options);
	return performance.now() - t;
}
for (const gen of ['client', 'server']) {
	run(gen); run(gen);
	const times = [run(gen), run(gen), run(gen)].sort((a, b) => a - b);
	console.log(`${gen}: ${jobs.filter((j) => j.options.generate === gen).length} compilations, best ${times[0].toFixed(0)} ms, median ${times[1].toFixed(0)} ms`);
}
