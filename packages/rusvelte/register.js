// Makes `import ... from 'svelte/compiler'` (in vite-plugin-svelte, SvelteKit, anything loaded
// after this) get a module that re-exports the installed svelte/compiler with `compile` and
// `compileModule` replaced by rusvelte's (see compiler.js). Load it before those packages:
//
//   NODE_OPTIONS="--import rusvelte/register" vite build
//
// or call `register()` before importing them dynamically (vite.js does).
import { registerHooks } from 'node:module';

const PACKAGE = new URL('./', import.meta.url).href;
const SHIM = new URL('./svelte-compiler.js', import.meta.url).href;
const COMPILER = new URL('./compiler.js', import.meta.url).href;

let registered = false;

export function register() {
	if (registered) return;
	registered = true;
	registerHooks({
		resolve(specifier, context, next) {
			const parent = context.parentURL ?? '';
			// the generated module's own imports resolve from the real compiler's location
			if (parent.startsWith(SHIM + '?')) {
				const real = new URL(parent).searchParams.get('real');
				return next(specifier, { ...context, parentURL: real });
			}
			if (specifier === 'svelte/compiler' && !parent.startsWith(PACKAGE) && context.conditions?.includes('import')) {
				const real = next(specifier, context);
				return { url: `${SHIM}?real=${encodeURIComponent(real.url)}`, format: 'module', shortCircuit: true };
			}
			return next(specifier, context);
		},
		load(url, context, next) {
			if (!url.startsWith(SHIM + '?')) return next(url, context);
			const real = new URL(url).searchParams.get('real');
			const at = (path) => JSON.stringify(new URL(path, real).href);
			const source = `
export * from ${JSON.stringify(real)};
import * as real from ${JSON.stringify(real)};
import * as state from ${at('./state.js')};
import { CompileDiagnostic } from ${at('./utils/compile_diagnostic.js')};
import { merge_with_preprocessor_map, get_source_name } from ${at('./utils/mapped_code.js')};
import { SourceMap as MagicSourceMap } from 'magic-string';
import { create } from ${JSON.stringify(COMPILER)};
export const { compile, compileModule } = create({ real, state, CompileDiagnostic, merge_with_preprocessor_map, get_source_name, MagicSourceMap });
`;
			return { format: 'module', source, shortCircuit: true };
		}
	});
}

register();
