// The native module: `compile(source, optionsJson)` and `compileModule(source, optionsJson)`
// return JSON (see bindings/node/src/lib.rs). It comes from `RUSVELTE_NATIVE` if set, else a
// local build (`rusvelte.node`, made by `npm run build`), else the prebuilt package for this
// platform (`@rusveltejs/<platform>-<arch>[-<libc>]`, an optional dependency).
import { createRequire } from 'node:module';
import fs from 'node:fs';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);

/** The suffix of this platform's prebuilt package */
function platform_package() {
	const { platform, arch } = process;
	if (platform === 'linux') {
		const glibc = process.report?.getReport?.().header?.glibcVersionRuntime;
		return `@rusveltejs/linux-${arch}-${glibc ? 'gnu' : 'musl'}`;
	}
	if (platform === 'win32') return `@rusveltejs/win32-${arch}-msvc`;
	return `@rusveltejs/${platform}-${arch}`;
}

function load() {
	if (process.env.RUSVELTE_NATIVE) return require(process.env.RUSVELTE_NATIVE);
	const local = fileURLToPath(new URL('./rusvelte.node', import.meta.url));
	if (fs.existsSync(local)) return require(local);
	return require(platform_package());
}

/** The native module, or null where there's none (compiler.js then uses svelte/compiler) */
export let native = null;
/** Why the native module couldn't be loaded */
export let load_error = null;
try {
	native = load();
	// a build from another rusvelte version may not have this API
	if (typeof native.compile !== 'function' || typeof native.compileModule !== 'function' || typeof native.svelteVersion !== 'function') {
		throw new Error('it lacks compile, compileModule or svelteVersion');
	}
	native.version = native.svelteVersion();
} catch (e) {
	native = null;
	const where = process.env.RUSVELTE_NATIVE ?? platform_package();
	load_error = `no native module for ${process.platform}-${process.arch} (${where}): ${e.message.split('\n')[0]}`;
}

export const compile = native?.compile;
export const compileModule = native?.compileModule;
/** The Svelte version whose output this build reproduces */
export const svelteVersion = native?.version ?? null;
