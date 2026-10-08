// The native module: `compile(source, optionsJson)` and `compileModule(source, optionsJson)`
// return JSON (see bindings/node/src/lib.rs). It comes from `RUSVELTE_NATIVE` if set, else a
// local build (`rusvelte.node`, made by `npm run build`), else the prebuilt package for this
// platform (`rusvelte-<platform>-<arch>[-<libc>]`, an optional dependency).
import { createRequire } from 'node:module';
import fs from 'node:fs';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);

/** The suffix of this platform's prebuilt package */
function platform_package() {
	const { platform, arch } = process;
	if (platform === 'linux') {
		const glibc = process.report?.getReport?.().header?.glibcVersionRuntime;
		return `rusvelte-linux-${arch}-${glibc ? 'gnu' : 'musl'}`;
	}
	if (platform === 'win32') return `rusvelte-win32-${arch}-msvc`;
	return `rusvelte-${platform}-${arch}`;
}

function load() {
	if (process.env.RUSVELTE_NATIVE) return require(process.env.RUSVELTE_NATIVE);
	const local = fileURLToPath(new URL('./rusvelte.node', import.meta.url));
	if (fs.existsSync(local)) return require(local);
	const name = platform_package();
	try {
		return require(name);
	} catch (e) {
		throw new Error(
			`rusvelte: no native module for ${process.platform}-${process.arch} (looked for the package ${name}). ` +
				`Install it, or build one from the repository (cargo build --release -p rusvelte-node).`,
			{ cause: e }
		);
	}
}

const native = load();

export const compile = native.compile;
export const compileModule = native.compileModule;
/** The Svelte version whose output this build reproduces */
export const svelteVersion = native.svelteVersion();
