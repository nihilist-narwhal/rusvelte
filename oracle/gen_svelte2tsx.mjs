// Oracle for the svelte2tsx port. Modes:
//   check   — the options svelte-check --tsgo passes (TS detection like svelte-check, real basename)
//   samples — svelte2tsx's own test harness config, derived from the sample directory name
import { svelte2tsx } from 'svelte2tsx';
import { parse, VERSION } from 'svelte/compiler';
import fs from 'node:fs';
import path from 'node:path';

const [corpus, out, mode = 'check'] = process.argv.slice(2);

function isTsSvelte(text) {
	const scriptTagRegex = /<script\b((?:\s+[^=>'"\/\s]+(?:=(?:"[^"]*"|'[^']*'|[^>\s]+))?)*)\s*>/gi;
	const langAttrRegex = /\blang\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'=<>`]+))/i;
	let m;
	while ((m = scriptTagRegex.exec(text)) !== null) {
		const lang = langAttrRegex.exec(m[1] ?? '');
		if (!lang) continue;
		const value = (lang[1] ?? lang[2] ?? lang[3] ?? '').toLowerCase();
		if (value === 'ts' || value === 'typescript') return true;
	}
	return false;
}

function options(rel, source) {
	if (mode === 'samples') {
		const sample = rel.split('/')[0];
		return {
			filename: path.basename(rel),
			isTsFile: sample.startsWith('ts-'),
			namespace: sample.endsWith('-foreign-ns') ? 'foreign' : null,
			typingsNamespace: 'svelteHTML',
			mode: sample.endsWith('-dts') ? 'dts' : 'ts',
			accessors: sample.startsWith('accessors-config'),
			emitJsDoc: sample.startsWith('jsdoc-'),
			emitOnTemplateError: false
		};
	}
	return {
		filename: path.basename(rel),
		isTsFile: isTsSvelte(source),
		mode: 'ts',
		emitOnTemplateError: false,
		emitJsDoc: true
	};
}

const files = fs.globSync('**/*.svelte', { cwd: corpus }).sort();
fs.mkdirSync(out, { recursive: true });
const manifest = [];
console.error = () => {};
let errors = 0;
for (const rel of files) {
	const source = fs.readFileSync(path.join(corpus, rel), 'utf-8');
	const opts = options(rel, source);
	let result;
	try {
		result = { ok: svelte2tsx(source, { parse, version: VERSION, ...opts }).code };
	} catch (e) {
		errors++;
		result = { error: String(e.message).split('\n')[0] };
	}
	const id = rel.replaceAll('/', '__');
	fs.writeFileSync(path.join(out, id + '.json'), JSON.stringify(result));
	manifest.push({ id, rel, options: opts });
}
fs.writeFileSync(path.join(out, 'manifest.json'), JSON.stringify(manifest));
console.log(`${files.length} files, ${errors} errors`);
