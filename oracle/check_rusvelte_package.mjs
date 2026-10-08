// Checks packages/rusvelte against svelte/compiler: compiles every .svelte / .svelte.js /
// .svelte.ts file of a directory through the package's `svelte/compiler` hook and through the
// real compiler, with the option sets vite-plugin-svelte uses, and compares the results
// (code, css, warnings with their positions and frames, metadata, and with MAPS=1 the maps).
//
//   node check_rusvelte_package.mjs <dir>
import fs from 'node:fs';
import path from 'node:path';
import { stripTypeScriptTypes } from 'node:module';
import { register } from '../packages/rusvelte/register.js';
import { stats } from '../packages/rusvelte/compiler.js';

register();
const ours = await import('svelte/compiler');
const real = await import(new URL('./node_modules/svelte/src/compiler/index.js', import.meta.url).href);
if (ours.compile === real.compile) throw new Error('the hook is not active');

const dir = process.argv[2];
const check_maps = !!process.env.MAPS;
const option_sets = [
	{ generate: 'client', dev: false, css: 'external' },
	{ generate: 'client', dev: true, hmr: true, css: 'external' },
	{ generate: 'server', dev: false, css: 'external' },
	{ generate: 'server', dev: true, css: 'external' },
	{ generate: 'client', dev: false, css: 'injected' },
	// relative filenames (the default `rootDir` is the working directory, here outside `dir`)
	{ generate: 'server', dev: true, css: 'external', rootDir: path.resolve(dir) },
	{ generate: 'client', dev: true, hmr: true, css: 'external', rootDir: path.resolve(dir) }
];

function plain(result) {
	return {
		js: result.js.code,
		css: result.css && { code: result.css.code, hasGlobal: result.css.hasGlobal },
		warnings: result.warnings.map((w) => ({ ...w, name: w.name, string: w.toString() })),
		metadata: result.metadata,
		...(check_maps && { js_map: result.js.map.toString(), css_map: result.css?.map.toString() })
	};
}

let total = 0;
let same = 0;
const differences = {};
for (const file of fs.globSync('**/*.{svelte,svelte.js,svelte.ts}', { cwd: dir }).sort()) {
	if (file.includes('node_modules')) continue;
	const filename = path.resolve(dir, file);
	let source = fs.readFileSync(filename, 'utf-8');
	const module = !file.endsWith('.svelte');
	if (file.endsWith('.ts')) {
		try {
			source = stripTypeScriptTypes(source);
		} catch {
			continue;
		}
	}
	for (const set of option_sets) {
		if (module && (set.css === 'injected' || set.hmr)) continue;
		const options = module ? { filename, generate: set.generate, dev: set.dev, rootDir: set.rootDir } : { filename, ...set };
		let a, b;
		try {
			b = plain((module ? real.compileModule : real.compile)(source, { ...options }));
		} catch (e) {
			b = { error: e.code ?? String(e) };
		}
		try {
			a = plain((module ? ours.compileModule : ours.compile)(source, { ...options }));
		} catch (e) {
			a = { error: e.code ?? String(e) };
		}
		total++;
		const sa = JSON.stringify(a);
		const sb = JSON.stringify(b);
		if (sa === sb) {
			same++;
			continue;
		}
		const key = Object.keys(b).find((k) => JSON.stringify(a[k]) !== JSON.stringify(b[k])) ?? 'shape';
		const d = (differences[`${key} (${set.generate}${set.dev ? ', dev' : ''}${set.css === 'injected' ? ', injected' : ''})`] ??= []);
		d.push(file);
	}
}
for (const [key, files] of Object.entries(differences).sort((x, y) => y[1].length - x[1].length)) {
	console.log(`${String(files.length).padStart(5)}  ${key}  (e.g. ${files[0]})`);
}
console.log(`${same}/${total} identical; native ${stats.native}, fallbacks ${JSON.stringify(stats.fallback)}`);
