# rusvelte (npm)

The rusvelte compiler as a drop-in for `svelte/compiler` in Vite builds. vite-plugin-svelte and
SvelteKit keep working as they are; their calls to `compile` and `compileModule` go to the Rust
port instead, and get the same results the official compiler would return.

Experimental. It reproduces Svelte 5.57.2; with any other version installed it steps aside and
the official compiler does the work.

## Use

```sh
npm install -D rusvelte
```

Prebuilt native modules cover macOS (arm64, x64) and Linux (x64 and arm64 with glibc 2.17 or
newer, x64 with musl). Windows is coming in the next release; until then, and on any other
platform, the package warns and uses the official compiler. From a checkout of the repository instead:
`cargo build --release -p rusvelte-node && node packages/rusvelte/build.js`.

Then either load it for the whole process:

```sh
NODE_OPTIONS="--import rusvelte/register" vite build
```

or wrap the Svelte plugin in `vite.config`:

```js
import { rusvelte } from 'rusvelte/vite';

export default defineConfig({
	plugins: [rusvelte(() => import('@sveltejs/kit/vite').then((m) => m.sveltekit()))]
});
```

The import has to be dynamic, and nothing else in the config may import vite-plugin-svelte
statically, or it binds the official compiler before rusvelte can step in.

## What it does

`register` adds a Node module hook: `svelte/compiler` resolves to a module that re-exports the
installed compiler with `compile` and `compileModule` replaced. Those call the native module,
then build the result with the installed compiler's own helpers (warnings with their frames,
source map objects, merging of preprocessor maps), so results have the same shape and content.

It falls back to the official compiler for:
- any Svelte version other than the one this build reproduces (5.57.2);
- options it doesn't implement, such as a `cssHash` function;
- components that fail to compile, so errors are exactly Svelte's;
- anything the native side can't handle.

## Environment variables

- `RUSVELTE_VERIFY=1`: also compile everything with the official compiler and report any
  difference (slower; for checking a project).
- `RUSVELTE_DEBUG=1`: report why a compilation fell back.
- `RUSVELTE_NATIVE=/path/to/rusvelte.node`: use another build of the native module.
- `RUSVELTE_ALLOW_VERSION_MISMATCH=1`: use rusvelte with other Svelte versions anyway. Its
  output targets 5.57.2's runtime, so this can break your app.

## Status

- Identical results to `svelte/compiler`, source maps included, on 20,410 compilations of
  real projects through this package (`oracle/check_rusvelte_package.mjs`), in the option
  sets Vite uses.
- Production `vite build` output identical to the official compiler's on the projects tried.
- Preprocessors (`vitePreprocess`, PostCSS, ...) still run in JavaScript, as before.
