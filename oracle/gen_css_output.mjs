// Oracle for the CSS output port (`render_stylesheet`): `compile(source, { filename,
// generate: 'client', ...compileOptions })`, recording `css?.code` and `css?.hasGlobal`.
//   node gen_css_output.mjs <corpus dir> <out dir> [base options as JSON]
//
// For Svelte's test samples, `compileOptions` come from each sample's `_config.js` (imports are
// stubbed, so only the options object matters), on top of the base options (what the suite's
// harness adds: `{"cssHash":"svelte-xyz"}` for css, a constant hash like the css suite's
// `cssHash: () => 'svelte-xyz'`; `{"experimental":{"async":true},"runes":true}` for runtime-runes). A custom `cssHash` is called for real and its inputs and
// result recorded, so the Rust side can check the inputs and reuse the result.
import { compile } from 'svelte/compiler';
import fs from 'node:fs';
import path from 'node:path';

const [corpus, out, base = '{}'] = process.argv.slice(2);
const base_options = JSON.parse(base);
if (typeof base_options.cssHash === 'string') {
	const hash = base_options.cssHash;
	base_options.cssHash = () => hash;
}

const config_cache = new Map();
let config_failures = 0;

/** Evaluate a sample's `_config.js` with its imports replaced by stubs */
async function load_config(dir) {
	if (config_cache.has(dir)) return config_cache.get(dir);
	let config = {};
	const file = path.join(dir, '_config.js');
	if (fs.existsSync(file)) {
		let code = fs.readFileSync(file, 'utf-8');
		const names = [];
		code = code.replace(/^import\s+([\s\S]*?)\s+from\s+['"][^'"]+['"];?/gm, (_, clause) => {
			clause = clause.trim();
			const ns = clause.match(/^\*\s+as\s+(\w+)/);
			if (ns) {
				names.push(ns[1]);
				return '';
			}
			const braces = clause.match(/\{([\s\S]*)\}/);
			if (braces) {
				for (const spec of braces[1].split(',')) {
					const s = spec.trim();
					if (!s) continue;
					const m = s.match(/(?:\w+\s+as\s+)?(\w+)$/);
					names.push(m[1]);
				}
			}
			const def = clause.replace(/\{[\s\S]*\}/, '').replace(/,/g, ' ').trim();
			if (def && !def.startsWith('*')) names.push(def);
			return '';
		});
		code = code.replace(/^import\s+['"][^'"]+['"];?/gm, '');
		const stubs = names
			.map((n) =>
				n === 'test' ? 'const test = (x) => x;' : `const ${n} = __stub;`
			)
			.join('\n');
		const prelude = `const __stub = new Proxy(function () {}, { get: (t, k) => k === Symbol.toPrimitive ? () => '' : __stub, apply: () => __stub, construct: () => __stub });\n`;
		try {
			const mod = await import(
				'data:text/javascript,' + encodeURIComponent(prelude + stubs + '\n' + code)
			);
			config = mod.default ?? {};
		} catch (e) {
			config_failures++;
			console.error(`config ${file}: ${e.message}`);
		}
	}
	config_cache.set(dir, config);
	return config;
}

/** The JSON-able part of the compile options */
function plain(options) {
	const result = {};
	for (const [k, v] of Object.entries(options)) {
		if (typeof v === 'function') continue;
		if (v && typeof v === 'object' && !Array.isArray(v)) {
			result[k] = plain(v);
		} else {
			result[k] = v;
		}
	}
	return result;
}

const files = fs
	.globSync('**/*.svelte', { cwd: corpus })
	.filter((f) => !f.split('/').some((s) => s === '_output' || s === 'node_modules'))
	.sort();
fs.mkdirSync(out, { recursive: true });
const manifest = [];
let errors = 0;
let with_css = 0;
const t0 = performance.now();
let compile_ms = 0;
for (const rel of files) {
	const source = fs.readFileSync(path.join(corpus, rel), 'utf-8').replace(/\r\n/g, '\n');
	const config = await load_config(path.join(corpus, path.dirname(rel)));
	const compile_options = { ...base_options, ...(config.compileOptions ?? {}) };
	/** @type {any} */
	const result = {};
	const css_hash = compile_options.cssHash;
	if (css_hash) {
		compile_options.cssHash = (input) => {
			const hash = css_hash(input);
			result.cssHash = { filename: input.filename, name: input.name, result: hash };
			return hash;
		};
	}
	const options = { filename: rel, ...compile_options, generate: 'client' };
	result.options = plain(options);
	try {
		const t = performance.now();
		const r = compile(source, options);
		compile_ms += performance.now() - t;
		result.css = r.css ? { code: r.css.code, hasGlobal: r.css.hasGlobal } : null;
		// injected styles: the `$$css` object in the JS (without the dev-mode source map comment,
		// which isn't ported)
		const m = r.js.code.match(/const \$\$css = \{\s*hash: ('(?:[^'\\]|\\.)*'|"(?:[^"\\]|\\.)*"),\s*code: ('(?:[^'\\]|\\.)*'|"(?:[^"\\]|\\.)*")\s*\};/);
		if (m) {
			const code = new Function('return ' + m[2])().replace(/\n\/\*# sourceMappingURL=[^]*\*\/$/, '');
			result.injected = { hash: new Function('return ' + m[1])(), code };
		}
		if (r.css) with_css++;
	} catch (e) {
		errors++;
		result.error = e.code ?? String(e.message).split('\n')[0];
	}
	const id = rel.replaceAll('/', '__');
	fs.writeFileSync(path.join(out, id + '.json'), JSON.stringify(result));
	manifest.push({ id, rel });
}
fs.writeFileSync(path.join(out, 'manifest.json'), JSON.stringify(manifest));
console.log(
	`${files.length} files, ${with_css} with css, ${errors} errors, ${config_failures} configs failed; ` +
		`${(performance.now() - t0).toFixed(0)} ms (compile ${compile_ms.toFixed(0)} ms)`
);
