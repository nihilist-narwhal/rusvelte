// Use rusvelte from vite.config without NODE_OPTIONS: wrap the import of the Svelte plugin so it
// happens after the `svelte/compiler` hook is registered.
//
//   import { rusvelte } from 'rusvelte/vite';
//   export default defineConfig({
//     plugins: [rusvelte(() => import('@sveltejs/kit/vite').then((m) => m.sveltekit()))]
//   });
//
// (or `import('@sveltejs/vite-plugin-svelte').then((m) => m.svelte())`). The import must be
// dynamic: a static import of vite-plugin-svelte anywhere in the config (vitePreprocess, say)
// binds the real compiler before the hook exists. Vite accepts a promise of plugins.
import { register } from './register.js';
export { stats } from './compiler.js';

/**
 * @template T
 * @param {() => Promise<T>} load
 * @returns {Promise<T>}
 */
export function rusvelte(load) {
	register();
	return load();
}
