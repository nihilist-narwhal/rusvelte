// The compilations of the code generation oracle: Svelte's test suites with the options each
// suite's harness uses, or a directory of components with default options. Shared by
// gen_codegen.mjs and compiler comparisons.
import { parse } from 'acorn';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { stripTypeScriptTypes } from 'node:module';

const here = path.dirname(fileURLToPath(import.meta.url));
const tests = path.join(here, '../svelte-upstream/packages/svelte/tests');
export const suites = ['runtime-runes', 'runtime-legacy', 'hydration', 'server-side-rendering', 'css', 'snapshot'];

/** The object literal of a `_config.js`'s `test({ ... })`, evaluated where it's a literal */
function read_config(dir) {
	const file = path.join(dir, '_config.js');
	if (!fs.existsSync(file)) return {};
	const text = fs.readFileSync(file, 'utf-8');
	const ast = parse(text, { ecmaVersion: 'latest', sourceType: 'module' });
	const decl = ast.body.find((n) => n.type === 'ExportDefaultDeclaration');
	let obj = decl?.declaration;
	if (obj?.type === 'CallExpression') obj = obj.arguments[0];
	if (obj?.type !== 'ObjectExpression') return {};
	const config = {};
	for (const p of obj.properties) {
		if (p.type !== 'Property' || p.computed) continue;
		const key = p.key.name ?? p.key.value;
		if (!['compileOptions', 'immutable', 'accessors', 'error', 'load_compiled', 'skip', 'mode', 'skip_mode'].includes(key)) continue;
		const src = text.slice(p.value.start, p.value.end);
		try {
			config[key] = new Function(`return (${src});`)();
		} catch {
			config.unsupported = (config.unsupported ?? []).concat(key);
		}
	}
	return config;
}

/** Options per suite and generate mode, like each suite's harness */
function suite_options(suite, config, generate) {
	const co = config.compileOptions;
	switch (suite) {
		case 'runtime-runes':
		case 'runtime-legacy': {
			const runes = suite === 'runtime-runes';
			return {
				generate: 'client',
				dev: undefined,
				hmr: undefined,
				experimental: { async: runes },
				fragments: 'html',
				...co,
				immutable: config.immutable,
				accessors: 'accessors' in config ? config.accessors : true,
				runes: co && 'runes' in co ? co.runes : runes
			};
		}
		case 'hydration':
			return generate === 'client' ? { accessors: true, fragments: 'html', ...co } : { ...co };
		case 'server-side-rendering':
			return { experimental: { async: true, ...co?.experimental }, ...co };
		case 'css':
			return { cssHash: () => 'svelte-xyz', ...co };
		default:
			return { ...co };
	}
}

function serialisable(options) {
	return JSON.parse(
		JSON.stringify(options, (k, v) => (typeof v === 'function' ? { fn: v.toString() } : v))
	);
}

/**
 * @param {string} target a suite name or a directory
 * @returns {{ id: string, filename: string, text: string, module: boolean, options: any, record: any }[]}
 */
export function codegen_jobs(target, base = {}) {
const jobs = [];
if (suites.includes(target)) {
	const root = path.join(tests, target, 'samples');
	for (const name of fs.readdirSync(root).sort()) {
		const cwd = path.join(root, name);
		if (!fs.statSync(cwd).isDirectory()) continue;
		const config = read_config(cwd);
		const generates =
			target === 'server-side-rendering' ? ['server'] : ['client', 'server'];
		for (const file of fs.globSync('**', { cwd }).sort()) {
			if (file.startsWith('_') || file.includes('/_') || !fs.statSync(path.join(cwd, file)).isFile()) continue;
			const module = file.endsWith('.svelte.js');
			if (!module && !file.endsWith('.svelte')) continue;
			for (const generate of generates) {
				if (file.endsWith('.server.svelte') && generate !== 'server') continue;
				if (file.endsWith('.client.svelte') && generate !== 'client') continue;
				jobs.push({ sample: name, file, cwd, config, generate, module });
			}
		}
	}
} else {
	for (const file of fs.globSync('**/*.{svelte,svelte.js,svelte.ts}', { cwd: target }).sort()) {
		if (file.includes('node_modules')) continue;
		for (const generate of ['client', 'server']) {
			jobs.push({ sample: '', file, cwd: target, config: {}, generate, module: !file.endsWith('.svelte') });
		}
	}
}

const result = [];
for (const job of jobs) {
	const filename = path.join(job.cwd, job.file);
	let text = fs.readFileSync(filename, 'utf-8').replace(/\r\n/g, '\n');
	// `compileModule` takes JavaScript: a `.svelte.ts` module is compiled with its types
	// stripped, as the bundler does before vite-plugin-svelte sees it
	let stripped = false;
	if (filename.endsWith('.ts')) {
		stripped = true;
		try {
			text = stripTypeScriptTypes(text);
		} catch (e) {
			// enums, parameter properties etc. need a real TypeScript transform
			stripped = { error: String(e.message).split('\n')[0] };
		}
	}
	let options;
	if (job.module) {
		const o = suites.includes(target) ? suite_options(target, job.config, job.generate) : base;
		options = { filename, generate: job.generate, dev: o.dev, experimental: o.experimental };
	} else {
		options = {
			filename,
			...(suites.includes(target) ? suite_options(target, job.config, job.generate) : base),
			generate: job.generate
		};
		if (target === 'runtime-runes' || target === 'runtime-legacy') options.rootDir = job.cwd;
	}
	const record = { sample: job.sample, file: job.file, generate: job.generate, module: job.module, options: serialisable(options) };
	if (job.config.unsupported) record.unsupported_config = job.config.unsupported;
	const id = [job.sample, job.file.replaceAll('/', '__'), job.generate].filter(Boolean).join('__');
	result.push({ id, filename, text, stripped, module: job.module, options, record });
}
return result;
}
