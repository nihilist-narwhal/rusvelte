// Oracle for the template converter: the `htmlx2jsx` test entry point of svelte2tsx
// (not exported from the npm bundle, so rebuilt from the bundle's internals).
import { createRequire } from 'node:module';
import { parse, VERSION } from 'svelte/compiler';
import fs from 'node:fs';
import path from 'node:path';
const require = createRequire(import.meta.url);
// The npm bundle doesn't export these: make a copy that does
const internals = new URL('./svelte2tsx-internals.cjs', import.meta.url);
if (!fs.existsSync(internals)) {
	const bundle = fs.readFileSync(require.resolve('svelte2tsx'), 'utf-8');
	fs.writeFileSync(internals, bundle + '\nexports.__internals = { MagicString, parseHtmlx, convertHtmlxToJsx };\n');
}
const { MagicString, parseHtmlx, convertHtmlxToJsx } = require('./svelte2tsx-internals.cjs').__internals;

function htmlx2jsx(htmlx, options) {
	const { htmlxAst, tags } = parseHtmlx(htmlx, parse, { ...options });
	const str = new MagicString(htmlx);
	convertHtmlxToJsx(str, htmlxAst, tags, { ...options, namespace: options?.preserveAttributeCase ? 'foreign' : undefined });
	return str.toString();
}

const [corpus, out] = process.argv.slice(2);
const files = fs.globSync('**/*.svelte', { cwd: corpus }).sort();
fs.mkdirSync(out, { recursive: true });
const manifest = [];
console.error = () => {};
let errors = 0;
for (const rel of files) {
	const source = fs.readFileSync(path.join(corpus, rel), 'utf-8');
	let result;
	try {
		result = { ok: htmlx2jsx(source, { typingsNamespace: 'svelteHTML', svelte5Plus: Number(VERSION[0]) >= 5 }) };
	} catch (e) {
		errors++;
		result = { error: String(e.message).split('\n')[0] };
	}
	const id = rel.replaceAll('/', '__');
	fs.writeFileSync(path.join(out, id + '.json'), JSON.stringify(result));
	manifest.push({ id, rel });
}
fs.writeFileSync(path.join(out, 'manifest.json'), JSON.stringify(manifest));
console.log(`${files.length} files, ${errors} errors`);
