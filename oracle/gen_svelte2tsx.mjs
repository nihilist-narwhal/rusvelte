// Oracle for the svelte2tsx port: run svelte2tsx with the options svelte-check --tsgo uses
// and record the generated code (or the error) per file.
import { svelte2tsx } from 'svelte2tsx';
import { parse, VERSION } from 'svelte/compiler';
import fs from 'node:fs';
import path from 'node:path';

const [corpus, out] = process.argv.slice(2);
const files = fs.globSync('**/*.svelte', { cwd: corpus }).sort();
fs.mkdirSync(out, { recursive: true });
const manifest = [];
console.error = () => {}; // svelte2tsx logs "Error leaving node" before throwing
let errors = 0;
for (const rel of files) {
	const source = fs.readFileSync(path.join(corpus, rel), 'utf-8');
	const isTsFile = /<script\s[^>]*lang=["']?ts/.test(source);
	let result;
	try {
		const tsx = svelte2tsx(source, {
			parse,
			version: VERSION,
			filename: 'Component.svelte',
			isTsFile,
			mode: 'ts',
			emitOnTemplateError: false,
			emitJsDoc: true
		});
		result = { ok: tsx.code };
	} catch (e) {
		errors++;
		result = { error: String(e.message).split('\n')[0] };
	}
	const id = rel.replaceAll('/', '__');
	fs.writeFileSync(path.join(out, id + '.json'), JSON.stringify(result));
	manifest.push({ id, rel, isTsFile });
}
fs.writeFileSync(path.join(out, 'manifest.json'), JSON.stringify(manifest));
console.log(`${files.length} files, ${errors} errors`);
