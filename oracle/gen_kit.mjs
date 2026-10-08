// Oracle for the kit-file port (src/svelte2tsx/kit.rs): runs svelte2tsx's
// internalHelpers.upsertKitFile over every .ts/.js file of a workspace the way
// svelte-check --incremental/--tsgo does (emitSvelteFiles in svelte-check/src/incremental.ts).
//
// Usage: node gen_kit.mjs <workspace> <out.json> [settings.json|-] [stress mode]
//
// Files are found like svelte-check's findFiles (skipping node_modules and dot directories);
// every one gets `isKit` (internalHelpers.isKitFile), kit files also the upsert result.
//
// Stress modes upsert every file's text under a made-up kit file name next to it
// (`route`: +page.server.<ext>, `hooks`: src/hooks.server.<ext>, `params`: src/params/x.<ext>),
// with the file's own directory as the workspace, so that every `../` import leaves it and
// gets rewritten. Each entry records the arguments used (fileName, workspacePath,
// generatedPath).
import { createRequire } from 'node:module';
import fs from 'node:fs';
import path from 'node:path';

const require = createRequire(import.meta.url);
const ts = require('typescript');
const { internalHelpers } = require('svelte2tsx');

const [workspaceArg, out, settingsArg] = process.argv.slice(2);
const workspacePath = path.resolve(workspaceArg);
const stress = process.argv[5];
const settings = settingsArg && settingsArg !== '-'
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

const stressNames = { route: '+page.server', hooks: 'src/hooks.server', params: 'src/params/x' };
if (stress && !stressNames[stress]) throw new Error(`unknown stress mode ${stress}`);
const files = walk(workspacePath, []).sort();
const entries = [];
let kit = 0;
let errors = 0;
for (const filePath of files) {
	const rel = path.relative(workspacePath, filePath);
	let sourcePath = filePath;
	let workspace = workspacePath;
	if (stress) {
		workspace = path.dirname(filePath);
		sourcePath = workspace + '/' + stressNames[stress] + path.extname(filePath);
	}
	const isKit = internalHelpers.isKitFile(sourcePath, settings);
	if (!isKit) {
		entries.push({ rel, isKit });
		continue;
	}
	kit++;
	const text = fs.readFileSync(filePath, 'utf-8');
	const isTsFile = sourcePath.endsWith('.ts');
	const emitDir = path.join(workspace, '.svelte-kit', '.svelte-check', 'svelte');
	const outPath = path.join(emitDir, path.relative(workspace, sourcePath)).replace(/\\/g, '/');
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
				{ workspacePath: workspace, generatedPath: outPath }
			) ?? null;
	} catch (e) {
		errors++;
		result = { error: String(e.message).split('\n')[0] };
	}
	// internalHelpers.toOriginalPos around every insertion
	if (result?.addedCode) {
		const positions = new Set([0, result.text.length]);
		for (const a of result.addedCode) {
			for (const d of [-1, 0, 1]) {
				positions.add(a.generatedPos + d);
				positions.add(a.generatedPos + a.length + d);
			}
		}
		result.mapped = [...positions]
			.filter((p) => p >= 0)
			.sort((a, b) => a - b)
			.map((p) => {
				const { pos, inGenerated } = internalHelpers.toOriginalPos(p, result.addedCode);
				return [p, pos, inGenerated];
			});
	}
	entries.push({ rel, isKit, fileName: sourcePath, workspacePath: workspace, generatedPath: outPath, result });
}
fs.mkdirSync(path.dirname(path.resolve(out)), { recursive: true });
fs.writeFileSync(out, JSON.stringify({ workspacePath, settings, entries }, null, 1));
console.log(`${files.length} files, ${kit} kit files, ${errors} errors`);
