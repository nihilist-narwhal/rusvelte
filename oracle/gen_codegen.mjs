// Oracle for code generation: the `js.code` and `css.code` of `compile()` (and
// `compileModule()` for `.svelte.js` files), client and server.
//   node gen_codegen.mjs <suite|dir> <out dir>
// <suite> is one of Svelte's test suites (runtime-runes, runtime-legacy, hydration,
// server-side-rendering, css, snapshot), compiled with the options that suite's harness uses
// (tests/helpers.js `compile_directory` and each suite's setup) and each sample's
// `_config.js` `compileOptions`. Anything else is a directory of components compiled with
// default options.
// Each record holds the options used, so the Rust side can compile with the same ones.
// A `cssHash` function in the options is recorded as `{ "fn": <source> }`.
import { compile, compileModule } from 'svelte/compiler';
import fs from 'node:fs';
import path from 'node:path';
import { codegen_jobs } from './codegen_jobs.mjs';

const [target, out] = process.argv.slice(2);
const jobs = codegen_jobs(target);

fs.mkdirSync(out, { recursive: true });
const manifest = [];
let errors = 0;
for (const job of jobs) {
	const record = job.record;
	try {
		const r = job.module ? compileModule(job.text, job.options) : compile(job.text, job.options);
		record.js = r.js.code;
		if (r.css) {
			record.css = r.css.code;
			record.has_global = r.css.hasGlobal;
		}
	} catch (e) {
		errors++;
		record.error = e.code ? { code: e.code, message: e.message } : { crash: String(e.message).split('\n')[0] };
	}
	fs.writeFileSync(path.join(out, job.id + '.json'), JSON.stringify(record));
	manifest.push({ id: job.id, source: job.filename });
}
fs.writeFileSync(path.join(out, 'manifest.json'), JSON.stringify(manifest));
console.log(`${jobs.length} compilations, ${errors} errors`);
