// Oracle for code generation: the `js.code` and `css.code` of `compile()` (and
// `compileModule()` for `.svelte.js` files), client and server.
//   node gen_codegen.mjs <suite|dir> <out dir> [base options JSON]
// <suite> is one of Svelte's test suites (runtime-runes, runtime-legacy, hydration,
// server-side-rendering, css, snapshot), compiled with the options that suite's harness uses
// (tests/helpers.js `compile_directory` and each suite's setup) and each sample's
// `_config.js` `compileOptions`. Anything else is a directory of components compiled with
// default options. Base options (e.g. `'{"dev":true}'`) are added to (and override) these.
// Each record holds the options used, so the Rust side can compile with the same ones.
// A `cssHash` function in the options is recorded as `{ "fn": <source> }`.
import { compile, compileModule } from 'svelte/compiler';
import fs from 'node:fs';
import path from 'node:path';
import { codegen_jobs } from './codegen_jobs.mjs';

const [target, out, base] = process.argv.slice(2);
const jobs = codegen_jobs(target, base ? JSON.parse(base) : {});

fs.mkdirSync(out, { recursive: true });
const manifest = [];
let errors = 0;
for (const job of jobs) {
	const record = job.record;
	try {
		if (job.stripped?.error) throw new Error(`TypeScript not strippable: ${job.stripped.error}`);
		const r = job.module ? compileModule(job.text, job.options) : compile(job.text, job.options);
		record.js = r.js.code;
		record.js_map = { sources: r.js.map.sources, mappings: r.js.map.mappings };
		if (r.css) {
			record.css = r.css.code;
			record.has_global = r.css.hasGlobal;
			record.css_map = { file: r.css.map.file, sources: r.css.map.sources, mappings: r.css.map.mappings };
		}
		record.warnings = r.warnings.map((w) => ({ code: w.code, position: w.position ?? null }));
		record.runes = r.metadata.runes;
	} catch (e) {
		errors++;
		record.error = e.code ? { code: e.code, message: e.message } : { crash: String(e.message).split('\n')[0] };
	}
	fs.writeFileSync(path.join(out, job.id + '.json'), JSON.stringify(record));
	let source = job.filename;
	if (job.stripped) {
		// the stripped text, as that's what was compiled
		source = path.resolve(out, '_src', job.id + '.js');
		fs.mkdirSync(path.dirname(source), { recursive: true });
		fs.writeFileSync(source, job.text);
	}
	manifest.push({ id: job.id, source });
}
fs.writeFileSync(path.join(out, 'manifest.json'), JSON.stringify(manifest));
console.log(`${jobs.length} compilations, ${errors} errors`);
