#!/usr/bin/env python3
"""svelte-check's sanity fixtures (language-tools packages/svelte-check/test-*) against
svelte-check-rs: same workspaces, same expected errors.

Usage: tools/check_sanity.py <language-tools checkout> <node_modules with TypeScript 7 and svelte>
"""
import json, os, shutil, subprocess, sys, tempfile

lt, node_modules = sys.argv[1], os.path.abspath(sys.argv[2])
binary = os.path.join(os.path.dirname(__file__), '..', 'target', 'release', 'svelte-check-rs')
expected_errors = [
    ('Index.svelte', 3, 21, 2307), ('Index.svelte', 5, 8, 2322), ('Index.svelte', 8, 4, 2367),
    ('Index.svelte', 11, 4, 2367), ('Index.svelte', 15, 1, 2741), ('Jsdoc.svelte', 9, 23, 2322),
    ('src/routes/+page.ts', 0, 13, 2322),
]
tmp = tempfile.mkdtemp()
failed = 0
for name, expected in [('test-success', []), ('test-error', expected_errors)]:
    ws = os.path.join(tmp, name)
    shutil.copytree(os.path.join(lt, 'packages', 'svelte-check', name), ws)
    os.symlink(node_modules, os.path.join(ws, 'node_modules'))
    for extra in [[], ['--incremental'], ['--incremental']]:
        r = subprocess.run([binary, '--workspace', ws, '--tsconfig', os.path.join(ws, 'tsconfig.json'), '--output', 'machine-verbose'] + extra, capture_output=True, text=True)
        errors = []
        for line in r.stdout.splitlines():
            i = line.find('{')
            if i >= 0:
                e = json.loads(line[i:])
                if e['type'] == 'ERROR':
                    errors.append((e['filename'], e['start']['line'], e['start']['character'], e['code']))
        ok = sorted(errors) == sorted(expected)
        failed += not ok
        print('PASS' if ok else 'FAIL', name, ' '.join(extra), '' if ok else f'got {sorted(errors)} {r.stderr[:500]}')
shutil.rmtree(tmp)
sys.exit(1 if failed else 0)
