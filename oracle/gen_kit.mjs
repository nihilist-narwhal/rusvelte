// Oracle for the kit-file port (src/svelte2tsx/kit.rs): runs svelte2tsx's
// internalHelpers.upsertKitFile over every .ts/.js file of a workspace the way
// svelte-check --incremental/--tsgo does (emitSvelteFiles in svelte-check/src/incremental.ts).
//
// Usage: node gen_kit.mjs <workspace> <out.json> [settings.json]
//
// Files are found like svelte-check's findFiles (skipping node_modules and dot directories);
// every one gets `isKit` (internalHelpers.isKitFile), kit files also the upsert result.
import { createRequire } from 'node:module';
import fs from 'node:fs';
import path from 'node:path';

const require = createRequire(import.meta.url);
const ts = require('typescript');
const { internalHelpers } = require('svelte2tsx');

const [workspaceArg, out, settingsArg] = process.argv.slice(2);
const workspacePath = path.resolve(workspaceArg);
const settings = settingsArg
	? JSON.parse(fs.readFileSync(settingsArg, 'utf-8'))
	: {
			paramsPath: 'src/params',
			serverHooksPath: 'src/hooks.server',
			clientHooksPath: 'src/hooks.client',
			universalHooksPath: 'src/hooks'
		};

function walk(dir, files) {
	for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
		const full = dir + '/' + entry.name;
		if (entry.isDirectory()) {
			if (entry.name === 'node_modules' || entry.name.startsWith('.')) continue;
			walk(full, files);
		} else if (entry.name.endsWith('.ts') || entry.name.endsWith('.js')) {
			files.push(full);
		}
	}
	return files;
}

const emitDir = path.join(workspacePath, '.svelte-kit', '.svelte-check', 'svelte');
const files = walk(workspacePath, []).sort();
const entries = [];
let kit = 0;
let errors = 0;
for (const sourcePath of files) {
	const rel = path.relative(workspacePath, sourcePath);
	const isKit = internalHelpers.isKitFile(sourcePath, settings);
	if (!isKit) {
		entries.push({ rel, isKit });
		continue;
	}
	kit++;
	const text = fs.readFileSync(sourcePath, 'utf-8');
	const isTsFile = sourcePath.endsWith('.ts');
	const outPath = path.join(emitDir, rel).replace(/\\/g, '/');
	let result;
	try {
		result =
			internalHelpers.upsertKitFile(
				ts,
				sourcePath,
				settings,
				() =>
					ts.createSourceFile(
						sourcePath,
						text,
						ts.ScriptTarget.Latest,
						true,
						isTsFile ? ts.ScriptKind.TS : ts.ScriptKind.JS
					),
				undefined,
				{ workspacePath, generatedPath: outPath }
			) ?? null;
	} catch (e) {
		errors++;
		result = { error: String(e.message).split('\n')[0] };
	}
	entries.push({ rel, isKit, result });
}
fs.mkdirSync(path.dirname(path.resolve(out)), { recursive: true });
fs.writeFileSync(out, JSON.stringify({ workspacePath, settings, entries }, null, 1));
console.log(`${files.length} files, ${kit} kit files, ${errors} errors`);
