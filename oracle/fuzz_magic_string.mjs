// Differential fuzzing for src/magic_string.rs: random operation sequences on random
// strings, with magic-string's results (or errors) recorded for the Rust side to replay.
import MagicString from 'magic-string';
import fs from 'node:fs';

const [out, count = 20000, seed = 1] = process.argv.slice(2);
let s = Number(seed);
const rand = () => ((s = (s * 1103515245 + 12345) % 2147483648) / 2147483648);
const int = (n) => Math.floor(rand() * n);
const pick = (a) => a[int(a.length)];
const words = ['', 'x', 'ab', '\n', 'foo\nbar', '{', '}', '  ', 'é', '😀', 'a\n\nb'];

const cases = [];
for (let c = 0; c < Number(count); c++) {
	const alphabet = rand() < 0.2 ? 'ab\nc é' : 'abcdef\n ';
	const len = int(24);
	let original = '';
	for (let i = 0; i < len; i++) original += alphabet[int(alphabet.length)];
	// positions must fall on char boundaries in both UTF-16 and UTF-8: avoid surrogates in the original
	const ms = new MagicString(original);
	const ops = [];
	const n = 1 + int(10);
	for (let i = 0; i < n; i++) {
		const a = int(len + 1), b = int(len + 1);
		const [lo, hi] = a <= b ? [a, b] : [b, a];
		const op = pick(['overwrite', 'overwriteContentOnly', 'update', 'remove', 'move', 'appendLeft', 'appendRight', 'prependLeft', 'prependRight', 'append', 'prepend', 'slice']);
		const text = pick(words);
		let args, result;
		try {
			switch (op) {
				case 'overwrite': args = [lo, hi, text]; ms.overwrite(lo, hi, text); break;
				case 'overwriteContentOnly': args = [lo, hi, text]; ms.overwrite(lo, hi, text, { contentOnly: true }); break;
				case 'update': args = [lo, hi, text]; ms.update(lo, hi, text); break;
				case 'remove': args = [lo, hi]; ms.remove(lo, hi); break;
				case 'move': { const idx = int(len + 1); args = [lo, hi, idx]; ms.move(lo, hi, idx); break; }
				case 'appendLeft': case 'appendRight': case 'prependLeft': case 'prependRight': args = [a, text]; ms[op](a, text); break;
				case 'append': case 'prepend': args = [text]; ms[op](text); break;
				case 'slice': args = [lo, hi]; result = ms.slice(lo, hi); break;
			}
			ops.push({ op, args, ok: true, result });
		} catch (e) {
			ops.push({ op, args, ok: false, error: e.message });
		}
	}
	let final, mappings;
	try { final = ms.toString(); mappings = ms.generateMap({ hires: true }).mappings; } catch (e) { final = null; }
	cases.push({ original, ops, final, mappings });
}
fs.writeFileSync(out, JSON.stringify(cases));
console.log(`${cases.length} cases`);
