// `compile` and `compileModule` backed by rusvelte, with the same results as `svelte/compiler`'s.
// The pieces of the real compiler they need (its `state`, `CompileDiagnostic` and source map
// helpers) are passed in by the generated module in register.js, so warnings and maps are
// built by Svelte's own code. Anything rusvelte doesn't handle falls back to the real compiler:
// other Svelte versions, unsupported options, compile errors (so their messages and frames
// are Svelte's own) and internal failures.
import * as native from './native.js';

const debug = !!process.env.RUSVELTE_DEBUG;
// `RUSVELTE_VERIFY=1` also runs the real compiler on every compilation and reports differences
const verify = !!process.env.RUSVELTE_VERIFY;

/** Counts of native compilations and fallbacks (by reason), for `RUSVELTE_DEBUG` and tests */
export const stats = { native: 0, fallback: {} };

/** @param {string} reason */
function fell_back(reason) {
	stats.fallback[reason] = (stats.fallback[reason] ?? 0) + 1;
	if (debug && stats.fallback[reason] === 1) console.warn(`[rusvelte] falling back to svelte/compiler: ${reason}`);
}

// Options rusvelte implements, with the values it accepts (`undefined` is always fine).
// Handled here rather than natively: `sourcemap`, `outputFilename`, `cssOutputFilename`,
// `warningFilter` and `modernAst`.
const COMPONENT_OPTIONS = {
	filename: 'string',
	generate: ['client', 'server'],
	dev: 'boolean',
	hmr: 'boolean',
	css: ['external', 'injected'],
	runes: 'boolean',
	experimental: { async: 'boolean' },
	accessors: 'boolean',
	immutable: 'boolean',
	customElement: 'boolean',
	discloseVersion: 'boolean',
	compatibility: { componentApi: [4, 5] },
	namespace: ['html', 'svg', 'mathml'],
	preserveComments: 'boolean',
	preserveWhitespace: 'boolean',
	fragments: ['html', 'tree'],
	rootDir: 'string',
	name: 'string',
	sourcemap: 'any',
	outputFilename: 'string',
	cssOutputFilename: 'string',
	warningFilter: 'function',
	modernAst: 'boolean'
};

const MODULE_OPTIONS = {
	filename: 'string',
	generate: ['client', 'server'],
	dev: 'boolean',
	rootDir: 'string',
	experimental: { async: 'boolean' },
	warningFilter: 'function'
};

/**
 * The first option rusvelte can't handle, or null
 * @param {Record<string, any>} options
 * @param {Record<string, any>} spec
 * @param {string} [prefix]
 * @returns {string | null}
 */
function unsupported_option(options, spec, prefix = '') {
	for (const [key, value] of Object.entries(options)) {
		if (value === undefined) continue;
		const expected = spec[key];
		const name = prefix + key;
		if (expected === undefined) return name;
		if (expected === 'any') continue;
		if (Array.isArray(expected)) {
			if (!expected.includes(value)) return `${name}: ${JSON.stringify(value)}`;
		} else if (typeof expected === 'object') {
			if (value === null || typeof value !== 'object') return name;
			const inner = unsupported_option(value, expected, name + '.');
			if (inner) return inner;
		} else if (typeof value !== expected) {
			return name;
		}
	}
	return null;
}

/** Options as the native side reads them (functions and maps left out) */
function native_options(options) {
	const { sourcemap, warningFilter, outputFilename, cssOutputFilename, modernAst, ...rest } = options;
	return JSON.stringify(rest);
}

/** `remove_bom` */
function remove_bom(source) {
	return source.charCodeAt(0) === 0xfeff ? source.slice(1) : source;
}

/** magic-string's `getRelativePath` */
function get_relative_path(from, to) {
	const from_parts = from.split(/[/\\]/);
	const to_parts = to.split(/[/\\]/);
	from_parts.pop();
	while (from_parts[0] === to_parts[0]) {
		from_parts.shift();
		to_parts.shift();
	}
	if (from_parts.length) {
		let i = from_parts.length;
		while (i--) from_parts[i] = '..';
	}
	return from_parts.concat(to_parts).join('/');
}

/** esrap's `SourceMap` (same fields, in the same order) */
class JsSourceMap {
	version = 3;
	names = [];

	constructor(mappings, source_name, source) {
		this.sources = [source_name || null];
		this.sourcesContent = [source || null];
		this.mappings = mappings;
	}

	toString() {
		return JSON.stringify(this);
	}

	toUrl() {
		return 'data:application/json;charset=utf-8;base64,' + Buffer.from(this.toString(), 'utf-8').toString('base64');
	}
}

/**
 * @param {{
 *   real: typeof import('svelte/compiler'),
 *   state: any,
 *   CompileDiagnostic: any,
 *   merge_with_preprocessor_map: Function,
 *   get_source_name: Function,
 *   MagicSourceMap: any
 * }} svelte
 */
export function create(svelte) {
	const { real, state, CompileDiagnostic, merge_with_preprocessor_map, get_source_name, MagicSourceMap } = svelte;

	/** results rusvelte produced (not the real compiler) */
	const native_results = new WeakSet();

	class CompileWarning extends CompileDiagnostic {
		name = 'CompileWarning';
	}

	if (!native.native) {
		console.warn(`[rusvelte] ${native.load_error}; using svelte/compiler`);
		return { compile: real.compile, compileModule: real.compileModule };
	}

	const version_ok = real.VERSION === native.svelteVersion || !!process.env.RUSVELTE_ALLOW_VERSION_MISMATCH;
	if (!version_ok) {
		console.warn(
			`[rusvelte] svelte ${real.VERSION} is installed, but this rusvelte build reproduces svelte ${native.svelteVersion}; using svelte/compiler`
		);
	}

	/**
	 * Runs the native compiler; null means "use the real one"
	 * @param {boolean} module
	 */
	function run(source, options, module) {
		// `validate-options.js` defaults `rootDir` to the working directory
		if (options.rootDir === undefined && typeof process !== 'undefined') options = { ...options, rootDir: process.cwd() };
		if (!version_ok) return fell_back(`svelte ${real.VERSION}`), null;
		const bad = unsupported_option(options, module ? MODULE_OPTIONS : COMPONENT_OPTIONS);
		if (bad) return fell_back(`option ${bad}`), null;
		const result = JSON.parse((module ? native.compileModule : native.compile)(source, native_options(options)));
		if (result.unsupported) return fell_back(result.unsupported), null;
		// let the real compiler throw, so the error is exactly Svelte's
		if (result.error) return fell_back(`compile error ${result.error.code}`), null;
		stats.native++;
		return result;
	}

	/** `state` as compile leaves it, so `CompileDiagnostic` fills in filename, start/end, frame */
	function set_state(source, options, runes) {
		state.reset({ warning: options.warningFilter, filename: options.filename });
		state.set_source(source);
		state.adjust({ dev: !!options.dev, rootDir: options.rootDir ?? process.cwd(), runes });
	}

	function warnings_of(result, options) {
		const filter = options.warningFilter ?? (() => true);
		const out = [];
		for (const w of result.warnings) {
			const warning = new CompileWarning(w.code, w.message, w.position ?? undefined);
			if (filter(warning)) out.push(warning);
		}
		return out;
	}

	/** Reports the first difference between our result and the real compiler's */
	function check(kind, source, options, ours) {
		const theirs = (kind === 'module' ? real.compileModule : real.compile)(source, options);
		const pairs = [
			['js', ours.js.code, theirs.js.code],
			['css', ours.css?.code, theirs.css?.code],
			['warnings', JSON.stringify(ours.warnings), JSON.stringify(theirs.warnings)]
		];
		for (const [what, a, b] of pairs) {
			if (a === b) continue;
			const al = (a ?? '').split('\n');
			const bl = (b ?? '').split('\n');
			let i = 0;
			while (i < al.length && al[i] === bl[i]) i++;
			const shown = Object.fromEntries(Object.entries(options).filter(([, v]) => typeof v !== 'function'));
			console.warn(
				`[rusvelte] ${what} differs for ${options.filename} with ${JSON.stringify({ ...shown, sourcemap: undefined })}\n  line ${i + 1}\n  svelte:   ${bl[i]}\n  rusvelte: ${al[i]}`
			);
			stats.verify_failures = (stats.verify_failures ?? 0) + 1;
			return;
		}
	}

	/** @type {typeof real.compile} */
	function compile(source, options) {
		const result = compile_native(source, options);
		if (verify && native_results.has(result)) check('component', remove_bom(source), options, result);
		return result;
	}

	/** @type {typeof real.compile} */
	function compile_native(source, options) {
		source = remove_bom(source);
		const result = run(source, options, false);
		if (!result) return real.compile(source, options);
		set_state(source, options, result.runes);
		// `validate_component_options` defaults the filename
		options = { ...options, filename: options.filename ?? '(unknown)' };

		const js_source_name = get_source_name(options.filename, options.outputFilename, 'input.svelte');
		const js = { code: result.js.code, map: new JsSourceMap(result.js.mappings, js_source_name, source) };
		merge_with_preprocessor_map(js, options, js_source_name);

		let css = null;
		if (result.css) {
			const file = options.cssOutputFilename || options.filename;
			const map = new MagicSourceMap({
				file: file ? file.split(/[/\\]/).pop() : undefined,
				sources: [options.filename ? get_relative_path(file || '', options.filename) : file || ''],
				sourcesContent: [source],
				names: [],
				mappings: []
			});
			map.mappings = result.css.mappings;
			css = { code: result.css.code, map, hasGlobal: result.css.hasGlobal };
			merge_with_preprocessor_map(css, options, css.map.sources[0]);
		}

		const warnings = warnings_of(result, options);
		let ast;
		const compiled = {
			js,
			css,
			warnings,
			metadata: { runes: result.runes },
			// parsed on demand: vite-plugin-svelte doesn't read it
			get ast() {
				return (ast ??= real.parse(source, { modern: !!options.modernAst, filename: options.filename }));
			},
			set ast(value) {
				ast = value;
			}
		};
		native_results.add(compiled);
		return compiled;
	}

	/** @type {typeof real.compileModule} */
	function compileModule(source, options) {
		source = remove_bom(source);
		const result = run(source, options, true);
		if (!result) return real.compileModule(source, options);
		set_state(source, options, true);
		options = { ...options, filename: options.filename ?? '(unknown)' };
		const js_source_name = get_source_name(options.filename, undefined, 'input.svelte.js');
		return {
			js: { code: result.js.code, map: new JsSourceMap(result.js.mappings, js_source_name, source) },
			css: null,
			warnings: warnings_of(result, options),
			metadata: { runes: true },
			ast: null
		};
	}

	return { compile, compileModule };
}
