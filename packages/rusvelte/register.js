// Makes `import ... from 'svelte/compiler'` (in vite-plugin-svelte, SvelteKit, anything loaded
// after this) get a module that re-exports the installed svelte/compiler with `compile` and
// `compileModule` replaced by rusvelte's (see compiler.js). Load it before those packages:
//
//   NODE_OPTIONS="--import @rusveltejs/compiler/register" vite build
//
// or call `register()` before importing them dynamically (vite.js does).
import { registerHooks } from 'node:module';

const SHIM = new URL('./svelte-compiler.js', import.meta.url).href;
const COMPILER = new URL('./compiler.js', import.meta.url).href;

// one hook per process, even with several copies of this package installed: a second hook
// would wrap the first one's module instead of the real compiler
const REGISTERED = Symbol.for('@rusveltejs/compiler/register');

export function register() {
	if (globalThis[REGISTERED]) return;
	globalThis[REGISTERED] = true;
	registerHooks({
		resolve(specifier, context, next) {
			const parent = context.parentURL ?? '';
			if (specifier === 'svelte/compiler' && !parent.startsWith(SHIM) && context.conditions?.includes('import')) {
				const real = next(specifier, context);
				return { url: `${SHIM}?real=${encodeURIComponent(real.url)}`, format: 'module', shortCircuit: true };
			}
			return next(specifier, context);
		},
		load(url, context, next) {
			if (!url.startsWith(SHIM + '?')) return next(url, context);
			const real = JSON.stringify(new URL(url).searchParams.get('real'));
			const source = `
export * from ${real};
import * as real from ${real};
import { create } from ${JSON.stringify(COMPILER)};
export const { compile, compileModule } = await create(real, ${real});
`;
			return { format: 'module', source, shortCircuit: true };
		}
	});
}

register();
