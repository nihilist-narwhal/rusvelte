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
import { parse } from 'acorn';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const [target, out] = process.argv.slice(2);
const here = path.dirname(fileURLToPath(import.meta.url));
const tests = path.join(here, '../svelte-upstream/packages/svelte/tests');
const suites = ['runtime-runes', 'runtime-legacy', 'hydration', 'server-side-rendering', 'css', 'snapshot'];

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
	for (const file of fs.globSync('**/*.{svelte,svelte.js}', { cwd: target }).sort()) {
		if (file.includes('node_modules')) continue;
		for (const generate of ['client', 'server']) {
			jobs.push({ sample: '', file, cwd: target, config: {}, generate, module: file.endsWith('.svelte.js') });
		}
	}
}

fs.mkdirSync(out, { recursive: true });
const manifest = [];
let errors = 0;
for (const job of jobs) {
	const filename = path.join(job.cwd, job.file);
	const text = fs.readFileSync(filename, 'utf-8').replace(/\r\n/g, '\n');
	let options;
	if (job.module) {
		const o = suites.includes(target) ? suite_options(target, job.config, job.generate) : {};
		options = { filename, generate: job.generate, dev: o.dev, experimental: o.experimental };
	} else {
		options = {
			filename,
			...(suites.includes(target) ? suite_options(target, job.config, job.generate) : {}),
			generate: job.generate
		};
		if (target === 'runtime-runes' || target === 'runtime-legacy') options.rootDir = job.cwd;
	}
	const record = { sample: job.sample, file: job.file, generate: job.generate, module: job.module, options: serialisable(options) };
	if (job.config.unsupported) record.unsupported_config = job.config.unsupported;
	try {
		const r = job.module ? compileModule(text, options) : compile(text, options);
		record.js = r.js.code;
		if (r.css) {
			record.css = r.css.code;
			record.has_global = r.css.hasGlobal;
		}
	} catch (e) {
		errors++;
		record.error = e.code ? { code: e.code, message: e.message } : { crash: String(e.message).split('\n')[0] };
	}
	const id = [job.sample, job.file.replaceAll('/', '__'), job.generate].filter(Boolean).join('__');
	fs.writeFileSync(path.join(out, id + '.json'), JSON.stringify(record));
	manifest.push({ id, source: filename });
}
fs.writeFileSync(path.join(out, 'manifest.json'), JSON.stringify(manifest));
console.log(`${jobs.length} compilations, ${errors} errors`);
