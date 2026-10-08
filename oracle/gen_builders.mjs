// Expected output for the builder tests in `src/estree/tests.rs`: each case builds a program
// with Svelte's `utils/builders.js` and prints it with esrap's `ts` language, like the
// compiler does. The Rust test builds the same program with `estree::builders`.
//   node gen_builders.mjs   (prints Rust `const` items to paste into the test)
import * as b from './node_modules/svelte/src/compiler/utils/builders.js';
import { print } from 'esrap';
import ts from 'esrap/languages/ts';

const program = (body) => ({ type: 'Program', sourceType: 'module', body });
const long = (name) => b.id(name.repeat(4));

/** @type {Record<string, () => any[]>} */
const cases = {
	imports: () => [
		b.import_all('$', 'svelte/internal/client'),
		b.imports([['a', 'a'], ['b', 'c']], 'mod'),
		b.imports([], 'side-effect')
	],
	calls: () => [
		b.stmt(b.call('$.set', b.id('x'), b.literal(1))),
		b.stmt(b.call('f', b.id('a'), undefined, null, b.id('b'), false, undefined)),
		b.stmt(b.maybe_call(b.member(b.id('obj'), 'fn'), b.id('arg'))),
		b.stmt(b.call(long('first'), long('second'), long('third'), long('fourth'), long('fifth'))),
		b.stmt(
			b.call(
				'$.template_effect',
				b.thunk(b.block([b.stmt(b.call('$.set_text', b.id('text'), b.id('value')))]))
			)
		),
		b.stmt(b.new('Foo', b.literal('x'), b.spread(b.id('rest'))))
	],
	declarations: () => [
		b.const('x', b.literal(1)),
		b.let('y'),
		b.var(b.object_pattern([b.prop('init', b.id('a'), b.id('a')), b.rest(b.id('others'))]), b.id('obj')),
		b.declaration('let', [
			b.declarator('alpha', b.literal('aaaaaaaaaaaaaa')),
			b.declarator('beta', b.literal('bbbbbbbbbbbbbbbbbbbbbbbbbb')),
			b.declarator('gamma')
		]),
		b.const(b.array_pattern([b.id('p'), null, b.assignment_pattern(b.id('q'), b.literal(2))]), b.id('arr'))
	],
	literals: () => [
		b.stmt(
			b.array([
				b.literal("it's"),
				b.literal('line\nbreak\\'),
				b.literal(0.1),
				b.literal(1e21),
				b.literal(123456789012),
				b.literal(1.5e-7),
				b.literal(-0),
				b.literal(-2),
				b.true,
				b.false,
				b.null,
				b.void0
			])
		),
		b.stmt(b.template([b.quasi('a`b${c}\\'), b.quasi('end', true)], [b.id('x')]))
	],
	functions: () => [
		b.function_declaration(b.id('f'), [b.id('a'), b.rest(b.id('b'))], b.block([b.return(b.id('a'))])),
		b.stmt(b.arrow([], b.object([b.init('a', b.literal(1))]))),
		b.stmt(b.arrow([b.id('x')], b.await(b.call('g', b.id('x'))), true)),
		b.stmt(b.arrow([b.id('x')], b.await(b.call('g', b.await(b.id('x')))), true)),
		b.const('t', b.thunk(b.call('h'))),
		b.const('u', b.thunk(b.call('h', b.id('x')))),
		b.const('v', b.unthunk(b.arrow([b.id('a'), b.id('b')], b.call('k', b.id('a'), b.id('b'))))),
		b.stmt(b.function(b.id('named'), [], b.block([]), true)),
		b.stmt(b.thunk(b.await(b.id('p')), true))
	],
	objects: () => [
		b.const(
			'o',
			b.object([
				b.init('simple', b.literal(1)),
				b.init('needs-quotes', b.literal(2)),
				b.prop('init', b.literal('computed'), b.id('c'), true),
				b.get('value', [b.return(b.id('v'))]),
				b.set('value', [b.stmt(b.assignment('=', b.id('v'), b.id('$$value')))]),
				b.spread(b.id('rest'))
			])
		),
		b.stmt(b.object([])),
		b.stmt(b.assignment('=', b.object_pattern([b.prop('init', b.id('a'), b.id('a'))]), b.id('x')))
	],
	classes: () => [
		b.stmt(
			b.class_expression(
				b.id('Ignored'),
				{
					type: 'ClassBody',
					body: [
						b.prop_def(b.private_id('count'), b.literal(0)),
						b.prop_def(b.id('label'), null, false, true),
						b.method('constructor', b.id('constructor'), [b.id('x')], [
							b.stmt(b.assignment('=', b.member(b.this, b.private_id('count')), b.id('x')))
						]),
						b.method('get', b.id('count'), [], [b.return(b.member(b.this, b.private_id('count')))]),
						b.method('method', b.literal('key'), [], [], true, true)
					]
				},
				b.id('Base')
			)
		)
	],
	control: () => [
		b.if(b.binary('===', b.id('a'), b.literal(1)), b.block([b.stmt(b.update('++', b.id('a')))]), b.block([b.debugger])),
		b.if(b.id('x'), b.if(b.id('y'), b.stmt(b.id('z')), null), b.stmt(b.id('w'))),
		b.for(b.let('i', b.literal(0)), b.binary('<', b.id('i'), b.literal(10)), b.update('++', b.id('i'), true), b.block([])),
		b.for_of(b.const('item'), b.id('items'), b.block([b.stmt(b.call('use', b.id('item')))]), true),
		b.labeled('outer', b.do_while(b.id('cond'), b.block([b.stmt(b.unary('!', b.id('x')))]))),
		b.throw_error('oops'),
		b.empty,
		b.stmt(b.id('after_empty'))
	],
	expressions: () => [
		b.stmt(b.logical('??', b.logical('||', b.id('a'), b.id('b')), b.id('c'))),
		b.stmt(b.binary('*', b.binary('+', b.id('a'), b.id('b')), b.binary('-', b.id('c'), b.id('d')))),
		b.stmt(b.binary('-', b.id('a'), b.binary('-', b.id('b'), b.id('c')))),
		b.stmt(b.binary('**', b.unary('-', b.id('a')), b.literal(2))),
		b.stmt(b.unary('-', b.unary('-', b.id('a')))),
		b.stmt(b.unary('typeof', b.id('a'))),
		b.stmt(b.conditional(b.id('test'), b.literal('yes'), b.literal('no'))),
		b.stmt(
			b.conditional(
				b.id('test'),
				b.call('consequent_function_name', b.id('argument')),
				b.call('alternate_function_name', b.id('argument'))
			)
		),
		b.stmt(b.sequence([b.id('a'), b.id('b')])),
		b.stmt(b.member(b.call('f'), b.id('x'), true, true)),
		b.stmt(b.member(b.binary('+', b.id('a'), b.id('b')), 'c')),
		b.stmt(b.member_id('a.b.c.d')),
		b.stmt(b.assignment('+=', b.member(b.id('a'), b.literal(0), true), b.literal(1))),
		b.stmt(b.new(b.call('make'))),
		b.stmt(b.call(b.arrow([], b.block([])))),
		b.stmt(b.call(b.function(null, [], b.block([]))))
	],
	exports: () => [
		b.export_default(b.function_declaration(b.id('Component'), [b.id('$$anchor')], b.block([b.var('x', b.literal(1))]))),
		b.export_default(b.arrow([], b.id('x')))
	]
};

const out = {};
for (const [name, build] of Object.entries(cases)) {
	out[name] = print(program(build()), ts({ comments: [] })).code;
}
for (const [name, code] of Object.entries(out)) {
	console.log(`const ${name.toUpperCase()}: &str = ${JSON.stringify(code).replace(/\\u([0-9a-f]{4})/g, '\\u{$1}')};\n`);
}
