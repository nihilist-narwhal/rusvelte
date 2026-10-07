# svelte-rs

An experimental Rust port of the Svelte 5 compiler. So far: the parser, equivalent to
`parse(source, { modern: true })` from `svelte/compiler` 5.57.2. JavaScript inside
components is parsed with [oxc](https://oxc.rs).

## Status

- Output matches the JS parser exactly on all 4,567 `.svelte` files in Svelte's test suite
  (ASTs, or error code + message). For 3 files with invalid JS, the error position differs
  from acorn's.
- Currently ~2× slower than the JS parser: JS subtrees are stored as `serde_json::Value`.
  Next step is keeping them as oxc's typed AST.

## Setup

```sh
git clone https://github.com/sveltejs/svelte.git svelte-upstream
git -C svelte-upstream checkout 707c28146b0f0a6d5404a1bd4769874c3c24851a
cd oracle && npm install && node gen.mjs ../svelte-upstream/packages/svelte/tests expected && cd ..
```

`oracle/gen.mjs` runs the real Svelte parser over every `.svelte` file in the test suite and
stores the results in `oracle/expected`.

## Checking and benchmarking

```sh
cargo run --release --bin compare -- svelte-upstream/packages/svelte/tests oracle/expected
cargo run --release --bin bench -- svelte-upstream/packages/svelte/tests oracle/expected
(cd oracle && node bench.mjs ../svelte-upstream/packages/svelte/tests)
```

`VERBOSE=1` makes `compare` print the first difference for each failing file.

## Layout

- `src/parser/` — port of `phases/1-parse` (template, tags, elements, CSS, options)
- `src/js.rs` — oxc integration: acorn-style `parseExpressionAt`, comment attachment,
  and normalization of oxc's ESTree output to acorn's shape
- `src/ast.rs` — the Svelte template AST and its JSON serialization
- `src/errors.rs` — generated from Svelte's `errors.js` by `tools/gen_errors.mjs`
