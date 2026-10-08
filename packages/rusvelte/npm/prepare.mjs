// Assembles the npm packages from the release workflow's native builds:
//
//   node prepare.mjs <artifacts dir> <out dir>
//
// <artifacts dir> holds one `rusvelte.<target>.node` per target. Writes <out dir>/compiler (the
// main package, @rusveltejs/compiler) and one <out dir>/platform-<platform> package
// (@rusveltejs/<platform>) per binary, all at the main package's version, with the license files.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const pkg_dir = path.join(here, '..');
const repo = path.join(pkg_dir, '../..');
const [artifacts, out] = process.argv.slice(2);

// Rust target → npm package suffix and its `os` / `cpu` / `libc` fields
const TARGETS = {
	'aarch64-apple-darwin': { name: 'darwin-arm64', os: 'darwin', cpu: 'arm64' },
	'x86_64-apple-darwin': { name: 'darwin-x64', os: 'darwin', cpu: 'x64' },
	'x86_64-unknown-linux-gnu': { name: 'linux-x64-gnu', os: 'linux', cpu: 'x64', libc: 'glibc' },
	'aarch64-unknown-linux-gnu': { name: 'linux-arm64-gnu', os: 'linux', cpu: 'arm64', libc: 'glibc' },
	'x86_64-unknown-linux-musl': { name: 'linux-x64-musl', os: 'linux', cpu: 'x64', libc: 'musl' },
	'x86_64-pc-windows-msvc': { name: 'win32-x64-msvc', os: 'win32', cpu: 'x64' }
};

const main = JSON.parse(fs.readFileSync(path.join(pkg_dir, 'package.json'), 'utf-8'));
const licenses = ['LICENSE', 'THIRD_PARTY_NOTICES.md'];
fs.rmSync(out, { recursive: true, force: true });

for (const [target, t] of Object.entries(TARGETS)) {
	const name = `@rusveltejs/${t.name}`;
	// only the platforms the main package lists are published
	if (!(name in main.optionalDependencies)) continue;
	if (main.optionalDependencies[name] !== main.version) {
		throw new Error(`optionalDependencies.${name} must be ${main.version}`);
	}
	const binary = path.join(artifacts, `rusvelte.${target}.node`);
	if (!fs.existsSync(binary)) throw new Error(`missing ${binary}`);
	const dir = path.join(out, `platform-${t.name}`);
	fs.mkdirSync(dir, { recursive: true });
	fs.copyFileSync(binary, path.join(dir, 'rusvelte.node'));
	for (const f of licenses) fs.copyFileSync(path.join(repo, f), path.join(dir, f));
	const manifest = {
		name,
		version: main.version,
		description: `The ${t.name} native module of rusvelte (@rusveltejs/compiler)`,
		homepage: main.homepage,
		repository: main.repository,
		license: main.license,
		main: 'rusvelte.node',
		files: ['rusvelte.node', ...licenses],
		os: [t.os],
		cpu: [t.cpu],
		...(t.libc && { libc: [t.libc] })
	};
	fs.writeFileSync(path.join(dir, 'package.json'), JSON.stringify(manifest, null, 2) + '\n');
	fs.writeFileSync(path.join(dir, 'README.md'), `# ${name}\n\nThe ${t.name} native module of [rusvelte](https://github.com/nihilist-narwhal/rusvelte). Install \`@rusveltejs/compiler\` instead; it depends on this.\n`);
}

const dir = path.join(out, 'compiler');
fs.mkdirSync(dir, { recursive: true });
for (const f of main.files) {
	const from = licenses.includes(f) ? path.join(repo, f) : path.join(pkg_dir, f);
	fs.copyFileSync(from, path.join(dir, f));
}
const { scripts, ...published } = main;
fs.writeFileSync(path.join(dir, 'package.json'), JSON.stringify(published, null, 2) + '\n');
console.log(`prepared ${main.name} ${main.version} and ${Object.keys(main.optionalDependencies).length} platform packages in ${out}`);
