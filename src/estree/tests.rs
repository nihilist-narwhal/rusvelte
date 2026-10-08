//! Builders + printer against Svelte's `builders.js` + esrap. The expected strings come from
//! `oracle/gen_builders.mjs`, which builds the same programs in JS.

use super::builders as b;
use super::print::{print, PrintOptions};
use super::*;

fn program(body: Vec<Node>) -> String {
    let program = Node::new(NodeKind::Program(Program { body, source_type: SourceType::Module }));
    print(&program, &PrintOptions::default()).code
}

fn long(name: &str) -> Node {
    b::id(name.repeat(4))
}

#[test]
fn imports() {
    let code = program(vec![
        b::import_all("$", "svelte/internal/client"),
        b::imports(&[("a", "a"), ("b", "c")], "mod"),
        b::imports(&[], "side-effect"),
    ]);
    assert_eq!(code, IMPORTS);
}

#[test]
fn calls() {
    let code = program(vec![
        b::stmt(b::call("$.set", [b::id("x"), b::literal(1)])),
        b::stmt(b::call("f", [Some(b::id("a")), None, None, Some(b::id("b")), None, None])),
        b::stmt(b::maybe_call(b::member(b::id("obj"), "fn"), b::id("arg"))),
        b::stmt(b::call(long("first"), [long("second"), long("third"), long("fourth"), long("fifth")])),
        b::stmt(b::call(
            "$.template_effect",
            b::thunk(b::block(vec![b::stmt(b::call("$.set_text", [b::id("text"), b::id("value")]))])),
        )),
        b::stmt(b::new("Foo", vec![b::literal("x"), b::spread(b::id("rest"))])),
    ]);
    assert_eq!(code, CALLS);
}

#[test]
fn declarations() {
    let code = program(vec![
        b::r#const("x", b::literal(1)),
        b::r#let("y", None),
        b::var(b::object_pattern(vec![b::prop("init", b::id("a"), b::id("a")), b::rest(b::id("others"))]), b::id("obj")),
        b::declaration(
            "let",
            vec![
                b::declarator("alpha", b::literal("aaaaaaaaaaaaaa")),
                b::declarator("beta", b::literal("bbbbbbbbbbbbbbbbbbbbbbbbbb")),
                b::declarator("gamma", None),
            ],
        ),
        b::r#const(
            b::array_pattern([Some(b::id("p")), None, Some(b::assignment_pattern(b::id("q"), b::literal(2)))]),
            b::id("arr"),
        ),
    ]);
    assert_eq!(code, DECLARATIONS);
}

#[test]
fn literals() {
    let code = program(vec![
        b::stmt(b::array([
            b::literal("it's"),
            b::literal("line\nbreak\\"),
            b::literal(0.1),
            b::literal(1e21),
            b::literal(123456789012.0),
            b::literal(1.5e-7),
            b::literal(-0.0),
            b::literal(-2),
            b::r#true(),
            b::r#false(),
            b::null(),
            b::void0(),
        ])),
        b::stmt(b::template(vec![b::quasi("a`b${c}\\"), b::quasi_with("end", true)], vec![b::id("x")])),
    ]);
    assert_eq!(code, LITERALS);
}

#[test]
fn functions() {
    let code = program(vec![
        b::function_declaration(b::id("f"), vec![b::id("a"), b::rest(b::id("b"))], b::block(vec![b::r#return(b::id("a"))])),
        b::stmt(b::arrow(vec![], b::object(vec![b::init("a", b::literal(1))]))),
        b::stmt(b::arrow_with(vec![b::id("x")], b::r#await(b::call("g", b::id("x"))), true)),
        b::stmt(b::arrow_with(vec![b::id("x")], b::r#await(b::call("g", b::r#await(b::id("x")))), true)),
        b::r#const("t", b::thunk(b::call("h", ()))),
        b::r#const("u", b::thunk(b::call("h", b::id("x")))),
        b::r#const("v", b::unthunk(b::arrow(vec![b::id("a"), b::id("b")], b::call("k", [b::id("a"), b::id("b")])))),
        b::stmt(b::function_with(b::id("named"), vec![], b::block(vec![]), true)),
        b::stmt(b::thunk_with(b::r#await(b::id("p")), true)),
    ]);
    assert_eq!(code, FUNCTIONS);
}

#[test]
fn objects() {
    let code = program(vec![
        b::r#const(
            "o",
            b::object(vec![
                b::init("simple", b::literal(1)),
                b::init("needs-quotes", b::literal(2)),
                b::prop_with("init", b::literal("computed"), b::id("c"), true),
                b::get("value", vec![b::r#return(b::id("v"))]),
                b::set("value", vec![b::stmt(b::assignment("=", b::id("v"), b::id("$$value")))]),
                b::spread(b::id("rest")),
            ]),
        ),
        b::stmt(b::object(vec![])),
        b::stmt(b::assignment("=", b::object_pattern(vec![b::prop("init", b::id("a"), b::id("a"))]), b::id("x"))),
    ]);
    assert_eq!(code, OBJECTS);
}

#[test]
fn classes() {
    let body = Node::new(NodeKind::ClassBody(Body {
        body: vec![
            b::prop_def(b::private_id("count"), b::literal(0)),
            b::prop_def_with(b::id("label"), None, false, true),
            b::method(
                "constructor",
                b::id("constructor"),
                vec![b::id("x")],
                vec![b::stmt(b::assignment("=", b::member(b::this(), b::private_id("count")), b::id("x")))],
            ),
            b::method("get", b::id("count"), vec![], vec![b::r#return(b::member(b::this(), b::private_id("count")))]),
            b::method_with("method", b::literal("key"), vec![], vec![], true, true),
        ],
    }));
    let code = program(vec![b::stmt(b::class_expression(b::id("Ignored"), body, b::id("Base")))]);
    assert_eq!(code, CLASSES);
}

#[test]
fn control() {
    let code = program(vec![
        b::r#if(
            b::binary("===", b::id("a"), b::literal(1)),
            b::block(vec![b::stmt(b::update("++", b::id("a")))]),
            b::block(vec![b::debugger()]),
        ),
        b::r#if(b::id("x"), b::r#if(b::id("y"), b::stmt(b::id("z")), None), b::stmt(b::id("w"))),
        b::r#for(
            b::r#let("i", b::literal(0)),
            b::binary("<", b::id("i"), b::literal(10)),
            b::update_with("++", b::id("i"), true),
            b::block(vec![]),
        ),
        b::for_of_with(b::r#const("item", None), b::id("items"), b::block(vec![b::stmt(b::call("use", b::id("item")))]), true),
        b::labeled("outer", b::do_while(b::id("cond"), b::block(vec![b::stmt(b::unary("!", b::id("x")))]))),
        b::throw_error("oops"),
        b::empty(),
        b::stmt(b::id("after_empty")),
    ]);
    assert_eq!(code, CONTROL);
}

#[test]
fn expressions() {
    let code = program(vec![
        b::stmt(b::logical("??", b::logical("||", b::id("a"), b::id("b")), b::id("c"))),
        b::stmt(b::binary("*", b::binary("+", b::id("a"), b::id("b")), b::binary("-", b::id("c"), b::id("d")))),
        b::stmt(b::binary("-", b::id("a"), b::binary("-", b::id("b"), b::id("c")))),
        b::stmt(b::binary("**", b::unary("-", b::id("a")), b::literal(2))),
        b::stmt(b::unary("-", b::unary("-", b::id("a")))),
        b::stmt(b::unary("typeof", b::id("a"))),
        b::stmt(b::conditional(b::id("test"), b::literal("yes"), b::literal("no"))),
        b::stmt(b::conditional(
            b::id("test"),
            b::call("consequent_function_name", b::id("argument")),
            b::call("alternate_function_name", b::id("argument")),
        )),
        b::stmt(b::sequence(vec![b::id("a"), b::id("b")])),
        b::stmt(b::member_with(b::call("f", ()), b::id("x"), true, true)),
        b::stmt(b::member(b::binary("+", b::id("a"), b::id("b")), "c")),
        b::stmt(b::member_id("a.b.c.d")),
        b::stmt(b::assignment("+=", b::member_with(b::id("a"), b::literal(0), true, false), b::literal(1))),
        b::stmt(b::new(b::call("make", ()), vec![])),
        b::stmt(b::call(b::arrow(vec![], b::block(vec![])), ())),
        b::stmt(b::call(b::r#function(None, vec![], b::block(vec![])), ())),
    ]);
    assert_eq!(code, EXPRESSIONS);
}

#[test]
fn exports() {
    let code = program(vec![
        b::export_default(b::function_declaration(
            b::id("Component"),
            vec![b::id("$$anchor")],
            b::block(vec![b::var("x", b::literal(1))]),
        )),
        b::export_default(b::arrow(vec![], b::id("x"))),
    ]);
    assert_eq!(code, EXPORTS);
}

#[test]
fn numbers() {
    use super::print::js_number_to_string as s;
    assert_eq!(s(1.0), "1");
    assert_eq!(s(1e21), "1e+21");
    assert_eq!(s(1e20), "100000000000000000000");
    assert_eq!(s(1.5e-7), "1.5e-7");
    assert_eq!(s(0.000001), "0.000001");
    assert_eq!(s(123.456), "123.456");
    assert_eq!(s(-0.0), "0");
    assert_eq!(s(f64::NAN), "NaN");
    assert_eq!(s(2.5e300), "2.5e+300");
}

#[test]
fn convert_and_print() {
    use oxc_allocator::Allocator;
    let source = "// a\nconst x = /** @type {number} */ (y); // trailing\n\nlet { a = 1, ...b } = c;\n({ d = 2 } = e);\nimport.meta.url;\n";
    let alloc = Allocator::default();
    let parsed = oxc_parser::Parser::new(&alloc, source, oxc_span::SourceType::mjs()).parse();
    let locator = crate::locator::Locator::new(source);
    let converter = convert::Converter::new(&locator, false);
    let program = converter.program(&parsed.program);
    let comments = convert::collect_comments(&parsed.program, &locator);
    let code = print(&program, &PrintOptions { comments: &comments, ..Default::default() }).code;
    assert_eq!(
        code,
        "// a\nconst x = /** @type {number} */ (y); // trailing\n\nlet { a = 1, ...b } = c;\n\n({ d = 2 } = e);\nimport.meta.url;"
    );
    // origins point at the oxc nodes
    let oxc_ast::ast::Statement::VariableDeclaration(decl) = &parsed.program.body[0] else { panic!() };
    let NodeKind::Program(p) = &program.kind else { panic!() };
    assert_eq!(p.body[0].origin, Some(&**decl as *const _ as usize));
}

#[test]
fn typescript() {
    use oxc_allocator::Allocator;
    let source = "import type { T } from 'a';\nimport { type U, v } from 'b';\nexport type { W };\nexport {};\ninterface I {}\nlet x: number = y as any;\nfunction f(this: Window, a?: string): void {}\nabstract class C { abstract m(): void; declare d: number; x = 1 }\nimport z = require('z');\n";
    let alloc = Allocator::default();
    let parsed = oxc_parser::Parser::new(&alloc, source, oxc_span::SourceType::ts().with_module(true)).parse();
    let locator = crate::locator::Locator::new(source);
    let program = convert::Converter::new(&locator, true).program(&parsed.program);
    let code = print(&program, &PrintOptions::default()).code;
    assert_eq!(
        code,
        "import { v } from 'b';\n\nlet x = y;\n\nfunction f(a) {}\n\nclass C {\n\tx = 1;\n}\n\nimport z = require('z');"
    );
}
const IMPORTS: &str = "import * as $ from 'svelte/internal/client';\nimport { a, b as c } from 'mod';\nimport 'side-effect';";

const CALLS: &str = "$.set(x, 1);\nf(a, void 0, void 0, b);\nobj.fn?.(arg);\nfirstfirstfirstfirst(secondsecondsecondsecond, thirdthirdthirdthird, fourthfourthfourthfourth, fifthfifthfifthfifth);\n\n$.template_effect(() => {\n\t$.set_text(text, value);\n});\n\nnew Foo('x', ...rest);";

const DECLARATIONS: &str = "const x = 1;\nlet y;\nvar { a, ...others } = obj;\n\nlet alpha = 'aaaaaaaaaaaaaa',\n\tbeta = 'bbbbbbbbbbbbbbbbbbbbbbbbbb',\n\tgamma;\n\nconst [p,, q = 2] = arr;";

const LITERALS: &str = "[\n\t'it\\'s',\n\t'line\\nbreak\\\\',\n\t0.1,\n\t1e+21,\n\t123456789012,\n\t1.5e-7,\n\t0,\n\t-2,\n\ttrue,\n\tfalse,\n\tnull,\n\tvoid 0\n];\n\n`a\\`b\\${c}\\\\${x}end`;";

const FUNCTIONS: &str = "function f(a, ...b) {\n\treturn a;\n}\n\n() => ({ a: 1 });\n(x) => g(x);\nasync (x) => await g(await x);\n\nconst t = h;\nconst u = () => h(x);\nconst v = k;\n\n(async function named() {});\n() => p;";

const OBJECTS: &str = "const o = {\n\tsimple: 1,\n\t'needs-quotes': 2,\n\t['computed']: c,\n\tget value() {\n\t\treturn v;\n\t},\n\n\tset value($$value) {\n\t\tv = $$value;\n\t},\n\t...rest\n};\n\n({});\n({ a } = x);";

const CLASSES: &str = "(class extends Base {\n\t#count = 0;\n\tstatic label;\n\n\tconstructor(x) {\n\t\tthis.#count = x;\n\t}\n\n\tget count() {\n\t\treturn this.#count;\n\t}\n\n\tstatic ['key']() {}\n});";

const CONTROL: &str = "if (a === 1) {\n\ta++;\n} else {\n\tdebugger;\n}\n\nif (x) {\n\tif (y) z;\n} else w;\n\nfor (let i = 0; i < 10; ++i) {}\n\nfor await (const item of items) {\n\tuse(item);\n}\n\nouter: do {\n\t!x;\n} while (cond);\n\nthrow new Error('oops');\n\nafter_empty;";

const EXPRESSIONS: &str = "(a || b) ?? c;\n(a + b) * (c - d);\na - (b - c);\n(-a) ** 2;\n- -a;\ntypeof a;\ntest ? 'yes' : 'no';\n\ntest\n\t? consequent_function_name(argument)\n\t: alternate_function_name(argument);\n\n(a, b);\nf()?.[x];\n(a + b).c;\na.b.c.d;\na[0] += 1;\nnew (make())();\n(() => {})();\n(function () {})();";

const EXPORTS: &str = "export default function Component($$anchor) {\n\tvar x = 1;\n}\n\nexport default () => x;";

