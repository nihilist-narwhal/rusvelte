// The native module: `compile(source, optionsJson)` and `compileModule(source, optionsJson)`
// return JSON (see bindings/node/src/lib.rs). `RUSVELTE_NATIVE` overrides its path.
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const native = require(process.env.RUSVELTE_NATIVE || './rusvelte.node');

export const compile = native.compile;
export const compileModule = native.compileModule;
/** The Svelte version whose output this build reproduces */
export const svelteVersion = native.svelteVersion();
