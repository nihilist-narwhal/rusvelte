// Oracle for the esrap port: parse JS files with acorn the way Svelte does
// (`phases/1-parse/acorn.js`: module, locations, comments collected with `onComment` and
// block comments de-indented), print them with esrap's `ts({ comments })` language as
// `phases/3-transform/index.js` does, and record the code and the source map's mappings.
//   node gen_esrap.mjs <dir | @file-list> <out.json> [base dir for relative paths in the list]
// A directory is searched for `**/*.js` and `**/*.mjs`. Minified files and files acorn can't
// parse as a module are skipped (and listed with the reason).
import { parse } from 'acorn';
import { print } from 'esrap';
import ts from 'esrap/languages/ts';
import fs from 'node:fs';
import path from 'node:path';

const [target, out, base_arg] = process.argv.slice(2);

let base;
let files;
if (target.startsWith('@')) {
	const list = target.slice(1);
	base = base_arg ?? path.dirname(list);
	files = fs.readFileSync(list, 'utf-8').split('\n').filter(Boolean);
} else {
	base = target;
	files = fs
		.globSync(['**/*.js', '**/*.mjs'], { cwd: target })
		.filter((f) => fs.statSync(path.join(target, f)).isFile())
		.sort();
}

/** Port of `get_comment_handlers(...).onComment` from Svelte's `phases/1-parse/acorn.js` */
function comment_collector(source, comments) {
	return (block, value, start, end, start_loc, end_loc) => {
		if (block && /\n/.test(value)) {
			let a = start;
			while (a > 0 && source[a - 1] !== '\n') a -= 1;

			let b = a;
			while (/[ \t]/.test(source[b])) b += 1;

			const indentation = source.slice(a, b);
			value = value.replace(new RegExp(`^${indentation}`, 'gm'), '');
		}

		comments.push({
			type: block ? 'Block' : 'Line',
			value,
			start,
			end,
			loc: { start: start_loc, end: end_loc }
		});
	};
}

function is_minified(source) {
	const lines = source.split('\n');
	const long = lines.filter((l) => l.length > 500).length;
	return long > 0 && (long > 2 || source.length / lines.length > 200);
}

const results = [];
const skipped = [];
let print_ms = 0;

for (const rel of files) {
	const file = path.resolve(base, rel);
	let source;
	try {
		source = fs.readFileSync(file, 'utf-8');
	} catch (e) {
		skipped.push({ path: rel, reason: 'unreadable' });
		continue;
	}
	if (source.charCodeAt(0) === 0xfeff) source = source.slice(1);
	if (is_minified(source)) {
		skipped.push({ path: rel, reason: 'minified' });
		continue;
	}

	const comments = [];
	let ast;
	try {
		ast = parse(source, {
			onComment: comment_collector(source, comments),
			sourceType: 'module',
			ecmaVersion: 'latest',
			locations: true
		});
	} catch (e) {
		skipped.push({ path: rel, reason: 'acorn: ' + e.message });
		continue;
	}

	try {
		const t = performance.now();
		const result = print(ast, ts({ comments }));
		print_ms += performance.now() - t;
		const mappings = result.map.mappings;
		results.push({ path: rel, code: result.code, mappings });
	} catch (e) {
		skipped.push({ path: rel, reason: 'esrap: ' + e.message });
	}
}

fs.writeFileSync(out, JSON.stringify({ base: path.resolve(base), files: results, skipped, print_ms }));
const reasons = {};
for (const s of skipped) {
	const r = s.reason.startsWith('acorn') ? 'acorn error' : s.reason.startsWith('esrap') ? s.reason : s.reason;
	reasons[r] = (reasons[r] ?? 0) + 1;
}
console.log(`${results.length} printed, ${skipped.length} skipped`, reasons, `esrap ${print_ms.toFixed(0)} ms`);
