// Loads a built native module and compiles a component (client and server) and a module:
//   node smoke-test.cjs <path to rusvelte.node>
const path = require('node:path');
const native = require(path.resolve(process.argv[2]));

const component = `<script>
	let count = $state(0);
</script>

<button onclick={() => count++}>clicks: {count}</button>

<style>
	button { color: red; }
</style>`;

function check(label, json, expect) {
	const result = JSON.parse(json);
	if (!result.js) throw new Error(`${label}: ${json}`);
	for (const s of expect) {
		if (!result.js.code.includes(s)) throw new Error(`${label}: output lacks ${s}\n${result.js.code}`);
	}
	console.log(`${label}: ok (${result.js.code.length} chars, mappings ${result.js.mappings.length})`);
}

console.log(`rusvelte reproduces svelte ${native.svelteVersion()}`);
check('client', native.compile(component, JSON.stringify({ filename: 'App.svelte', generate: 'client' })), ['$.state(0)', 'svelte-']);
check('server', native.compile(component, JSON.stringify({ filename: 'App.svelte', generate: 'server' })), ['$$renderer']);
check('module', native.compileModule('export class Counter { n = $state(1); }', JSON.stringify({ filename: 'n.svelte.js', generate: 'client' })), ['$.state(1)', '$.get(this.#n)']);
