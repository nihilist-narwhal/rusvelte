// Oracle: run the real Svelte parser over every .svelte file in the corpus and
// record the modern AST (or the error) as JSON, one file per input.
import { parse } from 'svelte/compiler';
import fs from 'node:fs';
import path from 'node:path';

const [corpus, out] = process.argv.slice(2);
const files = fs.globSync('**/*.svelte', { cwd: corpus }).sort();
fs.mkdirSync(out, { recursive: true });
const manifest = [];
for (const rel of files) {
	const source = fs.readFileSync(path.join(corpus, rel), 'utf-8');
	const loose = path.basename(path.dirname(rel)).startsWith('loose-');
	let result;
	try {
		result = { ok: JSON.parse(JSON.stringify(parse(source, { modern: true, loose }), (k, v) => (typeof v === "bigint" || v instanceof RegExp ? null : v))) };
	} catch (e) {
		if (!e.code) throw e;
		result = { error: { code: e.code, message: e.message.split('\n')[0], position: e.position } };
	}
	const id = rel.replaceAll('/', '__');
	fs.writeFileSync(path.join(out, id + '.json'), JSON.stringify(result));
	manifest.push({ id, rel, loose });
}
fs.writeFileSync(path.join(out, 'manifest.json'), JSON.stringify(manifest));
const errors = manifest.filter((m) => 'error' in JSON.parse(fs.readFileSync(path.join(out, m.id + '.json'))));
console.log(`${files.length} files, ${errors.length} parse errors`);
