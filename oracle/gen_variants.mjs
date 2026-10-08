// Make variants of a corpus that push the analysis down other paths:
//   runes-on  — `<svelte:options runes={true} />` prepended
//   runes-off — `<svelte:options runes={false} />` prepended
//   ts        — `lang="ts"` added to every <script>
// Files that already have <svelte:options> are copied unchanged for the runes variants.
//   node gen_variants.mjs <corpus dir> <out dir>
import fs from 'node:fs';
import path from 'node:path';

const [corpus, out] = process.argv.slice(2);
const files = fs.globSync('**/*.svelte', { cwd: corpus }).sort();

for (const variant of ['runes-on', 'runes-off', 'ts']) {
	for (const rel of files) {
		let source = fs.readFileSync(path.join(corpus, rel), 'utf-8');
		if (variant === 'ts') {
			source = source.replace(/<script(?![^>]*\blang=)(\s[^>]*)?>/g, (m, attrs = '') => `<script lang="ts"${attrs}>`);
		} else if (!source.includes('<svelte:options')) {
			source = `<svelte:options runes={${variant === 'runes-on'}} />\n` + source;
		}
		const dest = path.join(out, variant, rel);
		fs.mkdirSync(path.dirname(dest), { recursive: true });
		fs.writeFileSync(dest, source);
	}
}
console.log(`${files.length} files x 3 variants`);
