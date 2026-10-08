// Copies the native module cargo built into this package as rusvelte.node
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const target = path.join(here, '../../target/release');
const name = { darwin: 'librusvelte_node.dylib', linux: 'librusvelte_node.so', win32: 'rusvelte_node.dll' }[process.platform];
fs.copyFileSync(path.join(target, name), path.join(here, 'rusvelte.node'));
console.log(`copied ${name} to rusvelte.node`);
