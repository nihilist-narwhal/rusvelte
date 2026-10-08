// Oracle for the analysis port: `compile(source, { dev: true, generate: false, filename })`,
// recording the warnings, or the error thrown.
//   node gen_compile.mjs <corpus dir> <out dir> [extra compile options as JSON]
import { compile } from 'svelte/compiler';
import fs from 'node:fs';
import path from 'node:path';

const [corpus, out, extra = '{}'] = process.argv.slice(2);
const extra_options = JSON.parse(extra);

const loc = (l) => l && { line: l.line, column: l.column, character: l.character };
const diag = (d) => ({ code: d.code, message: d.message, start: loc(d.start), end: loc(d.end) });

const files = fs.globSync('**/*.svelte', { cwd: corpus }).sort();
fs.mkdirSync(out, { recursive: true });
const manifest = [];
let errors = 0;
let with_warnings = 0;
for (const rel of files) {
	const source = fs.readFileSync(path.join(corpus, rel), 'utf-8');
	let result;
	try {
		const r = compile(source, { ...extra_options, dev: true, generate: false, filename: path.basename(rel) });
		result = { warnings: r.warnings.map(diag) };
		if (r.warnings.length) with_warnings++;
	} catch (e) {
		errors++;
		result = e.code ? { error: diag(e) } : { crash: String(e.message).split('\n')[0] };
	}
	const id = rel.replaceAll('/', '__');
	fs.writeFileSync(path.join(out, id + '.json'), JSON.stringify(result));
	manifest.push({ id, rel });
}
fs.writeFileSync(path.join(out, 'manifest.json'), JSON.stringify(manifest));
fs.writeFileSync(path.join(out, 'options.json'), JSON.stringify(extra_options));
console.log(`${files.length} files, ${with_warnings} with warnings, ${errors} errors`);
