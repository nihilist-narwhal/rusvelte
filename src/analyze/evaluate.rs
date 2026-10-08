//! `scope.evaluate(expression)`: Svelte's constant evaluation (the `Evaluation` class in
//! `phases/scope.js`), which the transform uses to inline static values and to skip
//! reactivity for values that can't change.
//!
//! JS values are modelled by [`Val`]. Operations follow JS semantics for primitives; where
//! the JS would throw (e.g. `1n + 1`, `'a' in 'b'`), the compiler itself crashes, and here
//! the value becomes `Unknown` instead.

use oxc_ast::AstKind;
use oxc_ast::ast::*;
use oxc_ecmascript::{StringToNumber, ToInt32, ToUint32};

use super::nodes::{self, P};
use super::scope::{self, BindingId, Kind, ScopeId, Scopes};
use crate::ast::{Ast, Expr, Node};

/// A JS value, or one of the symbols Svelte uses for "some value of this type"
#[derive(Clone, Debug)]
pub enum Val {
    Unknown,
    Number,
    String,
    Function,
    Undefined,
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    /// a BigInt (its decimal digits)
    BigInt(i128),
    /// a RegExp object: identity (the literal's address) and `toString()`
    RegExp(usize, String),
}

impl Val {
    fn is_symbol(&self) -> bool {
        matches!(self, Val::Unknown | Val::Number | Val::String | Val::Function)
    }

    /// SameValueZero, which `Set` uses
    fn same_value_zero(&self, other: &Val) -> bool {
        match (self, other) {
            (Val::Unknown, Val::Unknown)
            | (Val::Number, Val::Number)
            | (Val::String, Val::String)
            | (Val::Function, Val::Function)
            | (Val::Undefined, Val::Undefined)
            | (Val::Null, Val::Null) => true,
            (Val::Bool(a), Val::Bool(b)) => a == b,
            (Val::Num(a), Val::Num(b)) => a == b || (a.is_nan() && b.is_nan()),
            (Val::Str(a), Val::Str(b)) => a == b,
            (Val::BigInt(a), Val::BigInt(b)) => a == b,
            (Val::RegExp(a, _), Val::RegExp(b, _)) => a == b,
            _ => false,
        }
    }

    /// `String(value)`
    pub fn to_js_string(&self) -> Option<String> {
        Some(match self {
            Val::Undefined => "undefined".into(),
            Val::Null => "null".into(),
            Val::Bool(b) => b.to_string(),
            Val::Num(n) => number_to_string(*n),
            Val::Str(s) => s.clone(),
            Val::BigInt(n) => n.to_string(),
            Val::RegExp(_, s) => s.clone(),
            _ => return None,
        })
    }

    fn to_boolean(&self) -> bool {
        match self {
            Val::Undefined | Val::Null => false,
            Val::Bool(b) => *b,
            Val::Num(n) => !(*n == 0.0 || n.is_nan()),
            Val::Str(s) => !s.is_empty(),
            Val::BigInt(n) => *n != 0,
            _ => true,
        }
    }

    /// `ToNumber` (None where it throws, i.e. for BigInts)
    fn to_number(&self) -> Option<f64> {
        Some(match self {
            Val::Undefined => f64::NAN,
            Val::Null => 0.0,
            Val::Bool(b) => f64::from(u8::from(*b)),
            Val::Num(n) => *n,
            Val::Str(s) => s.as_str().string_to_number(),
            Val::RegExp(_, s) => s.as_str().string_to_number(),
            _ => return None,
        })
    }

    /// `ToPrimitive` (objects become their string)
    fn to_primitive(&self) -> Val {
        match self {
            Val::RegExp(_, s) => Val::Str(s.clone()),
            v => v.clone(),
        }
    }

    fn type_of(&self) -> &'static str {
        match self {
            Val::Undefined => "undefined",
            Val::Null | Val::RegExp(..) => "object",
            Val::Bool(_) => "boolean",
            Val::Num(_) => "number",
            Val::Str(_) => "string",
            Val::BigInt(_) => "bigint",
            _ => "undefined",
        }
    }
}

/// `Number.prototype.toString()`
pub fn number_to_string(n: f64) -> String {
    if n.is_nan() {
        return "NaN".into();
    }
    if n == 0.0 {
        return "0".into();
    }
    if n.is_infinite() {
        return if n > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    ryu_js::Buffer::new().format(n).to_string()
}

#[derive(Debug, Clone)]
pub struct Evaluation {
    pub values: Vec<Val>,
    /// exactly one possible value
    pub is_known: bool,
    pub has_unknown: bool,
    /// known not to be null/undefined
    pub is_defined: bool,
    pub is_string: bool,
    pub is_number: bool,
    pub is_primitive: bool,
    pub is_function: bool,
    /// the last value (the value, when `is_known`)
    pub value: Option<Val>,
}

struct Ctx<'a, 's> {
    scopes: &'a Scopes<'s>,
    ast: &'s Ast<'s>,
    /// expressions being evaluated (`current_evaluations`)
    current: Vec<usize>,
}

fn add(values: &mut Vec<Val>, v: Val) {
    if !values.iter().any(|x| x.same_value_zero(&v)) {
        values.push(v);
    }
}

impl<'s> Scopes<'s> {
    /// `scope.evaluate(expression)`
    pub fn evaluate(&self, ast: &'s Ast<'s>, expression: P<'s>, scope: ScopeId) -> Evaluation {
        let mut cx = Ctx { scopes: self, ast, current: Vec::new() };
        let mut values = Vec::new();
        cx.evaluate(expression, scope, &mut values);
        summarize(values)
    }
}

fn summarize(values: Vec<Val>) -> Evaluation {
    let mut e = Evaluation {
        values: Vec::new(),
        is_known: true,
        has_unknown: false,
        is_defined: true,
        is_string: true,
        is_number: true,
        is_primitive: true,
        is_function: true,
        value: None,
    };
    for v in &values {
        if !matches!(v, Val::String | Val::Str(_)) {
            e.is_string = false;
        }
        if !matches!(v, Val::Number | Val::Num(_)) {
            e.is_number = false;
        }
        if !matches!(v, Val::Function) {
            e.is_function = false;
        }
        if matches!(v, Val::Null | Val::Undefined | Val::Unknown) {
            e.is_defined = false;
        }
        if matches!(v, Val::Unknown) {
            e.has_unknown = true;
            e.is_primitive = false;
        }
    }
    e.value = values.last().cloned();
    if values.len() > 1 || e.value.as_ref().is_some_and(Val::is_symbol) {
        e.is_known = false;
    }
    e.values = values;
    e
}

impl<'a, 's> Ctx<'a, 's> {
    /// An evaluation of its own (`scope.evaluate(x)` without a shared `values`)
    fn eval(&mut self, expression: P<'s>, scope: ScopeId) -> Evaluation {
        let mut values = Vec::new();
        if self.evaluate(expression, scope, &mut values) {
            summarize(values)
        } else {
            // a cycle: JS returns the evaluation in progress, whose values aren't final; it
            // has none yet at the point the cycle is detected
            summarize(Vec::new())
        }
    }

    /// `new Evaluation(scope, expression, values)`; false if `expression` is already being
    /// evaluated (then nothing is added)
    fn evaluate(&mut self, expression: P<'s>, scope: ScopeId, values: &mut Vec<Val>) -> bool {
        let key = expression.key();
        if self.current.contains(&key) {
            return false;
        }
        self.current.push(key);
        self.evaluate_inner(expression, scope, values);
        self.current.pop();
        true
    }

    fn evaluate_inner(&mut self, expression: P<'s>, scope: ScopeId, values: &mut Vec<Val>) {
        use AstKind as K;
        match expression {
            P::TplExpr(Expr::Literal { value, .. }) => add(values, Val::Str(value.clone())),
            P::Js(K::StringLiteral(s)) => add(values, Val::Str(s.value.to_string())),
            P::Js(K::NumericLiteral(n)) => add(values, Val::Num(n.value)),
            P::Js(K::BooleanLiteral(b)) => add(values, Val::Bool(b.value)),
            P::Js(K::NullLiteral(_)) => add(values, Val::Null),
            P::Js(K::BigIntLiteral(b)) => add(values, b.value.parse::<i128>().ok().or_else(unrepresentable).map_or(Val::Unknown, Val::BigInt)),
            P::Js(K::RegExpLiteral(r)) => add(values, Val::RegExp(nodes::addr(&K::RegExpLiteral(r)), format!("/{}/{}", r.regex.pattern.text, r.regex.flags))),
            P::Js(K::IdentifierReference(_)) | P::TplExpr(Expr::Ident { .. }) => self.identifier(expression, scope, values),
            P::Js(K::BinaryExpression(e)) => self.binary(e, scope, values),
            P::Js(K::ConditionalExpression(e)) => {
                let test = self.eval(nodes::expr(&e.test), scope);
                let consequent = self.eval(nodes::expr(&e.consequent), scope);
                let alternate = self.eval(nodes::expr(&e.alternate), scope);
                if test.is_known {
                    let taken = if test.value.as_ref().is_some_and(Val::to_boolean) { consequent } else { alternate };
                    for v in taken.values {
                        add(values, v);
                    }
                } else {
                    for v in consequent.values.into_iter().chain(alternate.values) {
                        add(values, v);
                    }
                }
            }
            P::Js(K::LogicalExpression(e)) => {
                let a = self.eval(nodes::expr(&e.left), scope);
                let b = self.eval(nodes::expr(&e.right), scope);
                let av = a.value.clone().unwrap_or(Val::Undefined);
                if a.is_known {
                    if b.is_known {
                        let bv = b.value.clone().unwrap_or(Val::Undefined);
                        let r = match e.operator {
                            LogicalOperator::And => if av.to_boolean() { bv } else { av },
                            LogicalOperator::Or => if av.to_boolean() { av } else { bv },
                            LogicalOperator::Coalesce => if matches!(av, Val::Null | Val::Undefined) { bv } else { av },
                        };
                        add(values, r);
                        return;
                    }
                    let short = match e.operator {
                        LogicalOperator::And => !av.to_boolean(),
                        LogicalOperator::Or => av.to_boolean(),
                        LogicalOperator::Coalesce => !matches!(av, Val::Null | Val::Undefined),
                    };
                    if short {
                        add(values, av);
                    } else {
                        for v in b.values {
                            add(values, v);
                        }
                    }
                    return;
                }
                for v in a.values.into_iter().chain(b.values) {
                    add(values, v);
                }
            }
            P::Js(K::UnaryExpression(e)) => {
                let arg = self.eval(nodes::expr(&e.argument), scope);
                if arg.is_known {
                    let v = arg.value.clone().unwrap_or(Val::Undefined);
                    add(values, unary(e.operator, &v));
                    return;
                }
                match e.operator {
                    UnaryOperator::LogicalNot | UnaryOperator::Delete => {
                        add(values, Val::Bool(false));
                        add(values, Val::Bool(true));
                    }
                    UnaryOperator::UnaryPlus | UnaryOperator::UnaryNegation | UnaryOperator::BitwiseNot => add(values, Val::Number),
                    UnaryOperator::Typeof => add(values, Val::String),
                    UnaryOperator::Void => add(values, Val::Undefined),
                }
            }
            P::Js(K::CallExpression(c)) => self.call(c, expression, scope, values),
            P::Js(K::TemplateLiteral(t)) => {
                let cooked = |i: usize| t.quasis[i].value.cooked.as_ref().map_or("undefined".to_string(), |c| c.to_string());
                let mut result = cooked(0);
                for (i, e) in t.expressions.iter().enumerate() {
                    let ev = self.eval(nodes::expr(e), scope);
                    if ev.is_known {
                        let s = ev.value.as_ref().and_then(Val::to_js_string).unwrap_or_else(|| "undefined".into());
                        result.push_str(&s);
                        result.push_str(&cooked(i + 1));
                    } else {
                        add(values, Val::String);
                        break;
                    }
                }
                add(values, Val::Str(result));
            }
            P::Js(K::StaticMemberExpression(_) | K::ComputedMemberExpression(_) | K::PrivateFieldExpression(_)) => {
                let keypath = scope::get_global_keypath(self.scopes, expression, scope);
                let constant = match keypath.as_deref() {
                    Some("Math.PI") => Some(std::f64::consts::PI),
                    Some("Math.E") => Some(std::f64::consts::E),
                    Some("Math.LN10") => Some(std::f64::consts::LN_10),
                    Some("Math.LN2") => Some(std::f64::consts::LN_2),
                    Some("Math.LOG10E") => Some(std::f64::consts::LOG10_E),
                    Some("Math.LOG2E") => Some(std::f64::consts::LOG2_E),
                    Some("Math.SQRT2") => Some(std::f64::consts::SQRT_2),
                    Some("Math.SQRT1_2") => Some(std::f64::consts::FRAC_1_SQRT_2),
                    _ => None,
                };
                add(values, constant.map_or(Val::Unknown, Val::Num));
            }
            P::Js(K::ArrowFunctionExpression(_) | K::Function(_)) => add(values, Val::Function),
            _ => add(values, Val::Unknown),
        }
    }

    fn identifier(&mut self, expression: P<'s>, scope: ScopeId, values: &mut Vec<Val>) {
        let Some(id) = scope::ident(expression) else {
            add(values, Val::Unknown);
            return;
        };
        let Some(b) = self.scopes.get(scope, id.name) else {
            add(values, if id.name == "undefined" { Val::Undefined } else { Val::Unknown });
            return;
        };
        let binding: &scope::Binding<'s> = self.scopes.binding(b);
        if let Some(initial @ P::Js(AstKind::CallExpression(_))) = binding.initial {
            if scope::get_rune(self.scopes, Some(initial), scope) == Some("$props.id") {
                add(values, Val::String);
                return;
            }
        }
        let is_prop = matches!(binding.kind, Kind::Prop | Kind::RestProp | Kind::BindableProp);
        if let Some(P::Node(n)) = binding.initial {
            match &self.ast.nodes[n] {
                Node::EachBlock { index: Some(index), .. } if index == id.name => {
                    add(values, Val::Number);
                    return;
                }
                Node::SnippetBlock { .. } => {
                    add(values, Val::Unknown);
                    return;
                }
                _ => {}
            }
        }
        if !binding.updated() && !is_prop {
            if let Some(initial) = binding.initial {
                self.evaluate(initial, binding.scope, values);
                return;
            }
        }
        add(values, Val::Unknown);
        let _ = b as BindingId;
    }

    fn binary(&mut self, e: &'s BinaryExpression<'s>, scope: ScopeId, values: &mut Vec<Val>) {
        let a = self.eval(nodes::expr(&e.left), scope);
        let b = self.eval(nodes::expr(&e.right), scope);
        if a.is_known && b.is_known {
            let av = a.value.clone().unwrap_or(Val::Undefined);
            let bv = b.value.clone().unwrap_or(Val::Undefined);
            add(values, binary(e.operator, &av, &bv).unwrap_or(Val::Unknown));
            return;
        }
        use BinaryOperator as O;
        match e.operator {
            O::Inequality | O::StrictInequality | O::LessThan | O::LessEqualThan | O::GreaterThan | O::GreaterEqualThan | O::Equality | O::StrictEquality | O::In | O::Instanceof => {
                add(values, Val::Bool(true));
                add(values, Val::Bool(false));
            }
            O::Remainder | O::BitwiseAnd | O::Multiplication | O::Exponential | O::Subtraction | O::Division | O::ShiftLeft | O::ShiftRight | O::ShiftRightZeroFill | O::BitwiseXOR | O::BitwiseOR => add(values, Val::Number),
            O::Addition => {
                if a.is_string || b.is_string {
                    add(values, Val::String);
                } else if a.is_number && b.is_number {
                    add(values, Val::Number);
                } else {
                    add(values, Val::String);
                    add(values, Val::Number);
                }
            }
        }
    }

    fn call(&mut self, c: &'s CallExpression<'s>, expression: P<'s>, scope: ScopeId, values: &mut Vec<Val>) {
        let callee = nodes::expr(&c.callee);
        let Some(keypath) = scope::get_global_keypath(self.scopes, callee, scope) else {
            add(values, Val::Unknown);
            return;
        };
        if super::utils::is_rune(&keypath).is_some() {
            let arg = c.arguments.first();
            match keypath.as_str() {
                "$state" | "$state.raw" | "$derived" => match arg {
                    Some(a) => {
                        self.evaluate(nodes::argument(a), scope, values);
                    }
                    None => add(values, Val::Undefined),
                },
                "$props.id" => add(values, Val::String),
                "$effect.tracking" => {
                    add(values, Val::Bool(false));
                    add(values, Val::Bool(true));
                }
                "$derived.by" => match arg.map(nodes::argument) {
                    Some(P::Js(AstKind::ArrowFunctionExpression(f))) if f.body.as_expression().is_some() => {
                        self.evaluate(nodes::expr(f.body.as_expression().unwrap()), scope, values);
                    }
                    _ => add(values, Val::Unknown),
                },
                _ => add(values, Val::Unknown),
            }
            return;
        }
        let _ = expression;
        if let Some((ty, f)) = global_fn(&keypath) {
            if c.arguments.iter().all(|a| !matches!(a, Argument::SpreadElement(_))) {
                let args: Vec<Evaluation> = c.arguments.iter().map(|a| self.eval(nodes::argument(a), scope)).collect();
                match f {
                    Some(f) if args.iter().all(|e| e.is_known) => {
                        let vals: Vec<Val> = args.into_iter().map(|e| e.value.unwrap_or(Val::Undefined)).collect();
                        add(values, f(&vals).unwrap_or(Val::Unknown));
                    }
                    _ => add(values, ty),
                }
                return;
            }
        }
        add(values, Val::Unknown);
    }
}

fn unary(op: UnaryOperator, v: &Val) -> Val {
    match op {
        UnaryOperator::LogicalNot => Val::Bool(!v.to_boolean()),
        UnaryOperator::Delete => Val::Bool(true),
        UnaryOperator::Void => Val::Undefined,
        UnaryOperator::Typeof => Val::Str(v.type_of().into()),
        UnaryOperator::UnaryNegation => match v {
            Val::BigInt(n) => n.checked_neg().or_else(unrepresentable).map_or(Val::Unknown, Val::BigInt),
            v => v.to_number().map_or(Val::Unknown, |n| Val::Num(-n)),
        },
        UnaryOperator::UnaryPlus => v.to_number().map_or(Val::Unknown, Val::Num),
        UnaryOperator::BitwiseNot => match v {
            Val::BigInt(n) => Val::BigInt(!n),
            v => v.to_number().map_or(Val::Unknown, |n| Val::Num(f64::from(!n.to_int_32()))),
        },
    }
}

fn loose_equals(a: &Val, b: &Val) -> bool {
    match (a, b) {
        (Val::Undefined | Val::Null, Val::Undefined | Val::Null) => true,
        (Val::Undefined | Val::Null, _) | (_, Val::Undefined | Val::Null) => false,
        (Val::Num(_), Val::Num(_)) | (Val::Str(_), Val::Str(_)) | (Val::Bool(_), Val::Bool(_)) | (Val::BigInt(_), Val::BigInt(_)) => strict_equals(a, b),
        (Val::RegExp(x, _), Val::RegExp(y, _)) => x == y,
        (Val::Bool(_), _) => loose_equals(&Val::Num(a.to_number().unwrap_or(f64::NAN)), b),
        (_, Val::Bool(_)) => loose_equals(a, &Val::Num(b.to_number().unwrap_or(f64::NAN))),
        (Val::RegExp(..), _) => loose_equals(&a.to_primitive(), b),
        (_, Val::RegExp(..)) => loose_equals(a, &b.to_primitive()),
        (Val::Num(n), Val::Str(s)) | (Val::Str(s), Val::Num(n)) => *n == s.as_str().string_to_number(),
        (Val::BigInt(n), Val::Num(m)) | (Val::Num(m), Val::BigInt(n)) => compare_bigint_number(*n, *m) == Some(std::cmp::Ordering::Equal),
        (Val::BigInt(n), Val::Str(s)) | (Val::Str(s), Val::BigInt(n)) => string_to_bigint(s) == Some(*n),
        _ => false,
    }
}

fn strict_equals(a: &Val, b: &Val) -> bool {
    match (a, b) {
        (Val::Num(x), Val::Num(y)) => x == y,
        _ => a.same_value_zero(b) && !matches!((a, b), (Val::Num(x), _) if x.is_nan()),
    }
}

/// Abstract relational comparison `a < b` (None for undefined)
thread_local! {
    /// Set when a constant the JS compiler would fold exactly can't be represented here (a
    /// BigInt beyond 128 bits, a string with a lone surrogate): `transform::compile` then
    /// declines the component, so the JS compiler handles it and the output stays identical
    static UNREPRESENTABLE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Note an unrepresentable constant (see `UNREPRESENTABLE`); returns `None` for `?`
fn unrepresentable<T>() -> Option<T> {
    UNREPRESENTABLE.with(|u| u.set(true));
    None
}

/// Clears the flag before a compilation and reads it after
pub fn take_unrepresentable() -> bool {
    UNREPRESENTABLE.with(|u| u.replace(false))
}

/// A BigInt compared with a number exactly (not through a lossy conversion); `None` for NaN
fn compare_bigint_number(x: i128, y: f64) -> Option<std::cmp::Ordering> {
    use std::cmp::Ordering;
    if y.is_nan() {
        return None;
    }
    const LIMIT: f64 = 170141183460469231731687303715884105728.0; // 2^127
    if y >= LIMIT {
        return Some(Ordering::Less);
    }
    if y < -LIMIT {
        return Some(Ordering::Greater);
    }
    // |y| < 2^127, so its integer part converts exactly
    let t = y.trunc();
    match x.cmp(&(t as i128)) {
        Ordering::Equal => Some(0f64.partial_cmp(&(y - t))?),
        o => Some(o),
    }
}

/// `StringToBigInt` of a string compared with a BigInt
enum StringBigInt {
    /// a SyntaxError: comparisons are `undefined`, equality false
    Invalid,
    Value(i128),
    /// valid, but beyond 128 bits (`true` if negative)
    Huge(bool),
}

fn string_bigint(s: &str) -> StringBigInt {
    match string_to_bigint(s) {
        Some(v) => StringBigInt::Value(v),
        None => {
            // valid but too large? (the same syntax check, without the range)
            let t = crate::analyze::utils::js_trim(s);
            let negative = t.starts_with('-');
            let digits = t.strip_prefix(['+', '-']).unwrap_or(t);
            let radix_digits = [("0x", 16), ("0X", 16), ("0o", 8), ("0O", 8), ("0b", 2), ("0B", 2)]
                .iter()
                .find_map(|&(p, radix)| t.strip_prefix(p).map(|d| (d, radix)));
            let valid = match radix_digits {
                Some((d, radix)) => !d.is_empty() && d.chars().all(|c| c.is_digit(radix)),
                None => !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()),
            };
            if valid { StringBigInt::Huge(negative && radix_digits.is_none()) } else { StringBigInt::Invalid }
        }
    }
}

/// `StringToBigInt`: `None` where it's a SyntaxError (or beyond what's represented)
fn string_to_bigint(s: &str) -> Option<i128> {
    let s = crate::analyze::utils::js_trim(s);
    if s.is_empty() {
        return Some(0);
    }
    for (prefix, radix) in [("0x", 16), ("0X", 16), ("0o", 8), ("0O", 8), ("0b", 2), ("0B", 2)] {
        if let Some(digits) = s.strip_prefix(prefix) {
            return if digits.is_empty() || digits.starts_with(['+', '-']) { None } else { i128::from_str_radix(digits, radix).ok() };
        }
    }
    let digits = s.strip_prefix(['+', '-']).unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<i128>().ok()
}

fn less_than(a: &Val, b: &Val) -> Option<bool> {
    let (a, b) = (a.to_primitive(), b.to_primitive());
    if let (Val::Str(x), Val::Str(y)) = (&a, &b) {
        return Some(x.encode_utf16().lt(y.encode_utf16()));
    }
    match (&a, &b) {
        (Val::BigInt(x), Val::BigInt(y)) => return Some(x < y),
        (Val::BigInt(x), Val::Str(y)) => {
            return match string_bigint(y) {
                StringBigInt::Invalid => None,
                StringBigInt::Value(y) => Some(*x < y),
                StringBigInt::Huge(negative) => Some(!negative),
            };
        }
        (Val::Str(x), Val::BigInt(y)) => {
            return match string_bigint(x) {
                StringBigInt::Invalid => None,
                StringBigInt::Value(x) => Some(x < *y),
                StringBigInt::Huge(negative) => Some(negative),
            };
        }
        (Val::BigInt(x), _) => {
            let y = b.to_number()?;
            return compare_bigint_number(*x, y).map(|o| o == std::cmp::Ordering::Less);
        }
        (_, Val::BigInt(y)) => {
            let x = a.to_number()?;
            return compare_bigint_number(*y, x).map(|o| o == std::cmp::Ordering::Greater);
        }
        _ => {}
    }
    let (x, y) = (a.to_number()?, b.to_number()?);
    if x.is_nan() || y.is_nan() { None } else { Some(x < y) }
}

/// BigInt `x << n` (None, noting it, when the result doesn't fit)
fn bigint_shl(x: i128, n: u128) -> Option<i128> {
    if x == 0 {
        return Some(0);
    }
    let r = u32::try_from(n).ok().and_then(|n| x.checked_shl(n));
    // `checked_shl` only checks the shift amount: bits shifted out are an overflow too
    match r {
        Some(r) if r >> n == x => Some(r),
        _ => unrepresentable(),
    }
}

/// BigInt `x >> n`: rounds towards negative infinity, like an arithmetic shift
fn bigint_shr(x: i128, n: u128) -> i128 {
    if n >= 128 { if x < 0 { -1 } else { 0 } } else { x >> n }
}

/// `binary[operator](left, right)` (None where the JS throws)
fn binary(op: BinaryOperator, a: &Val, b: &Val) -> Option<Val> {
    use BinaryOperator as O;
    Some(match op {
        O::Equality => Val::Bool(loose_equals(a, b)),
        O::Inequality => Val::Bool(!loose_equals(a, b)),
        O::StrictEquality => Val::Bool(strict_equals(a, b)),
        O::StrictInequality => Val::Bool(!strict_equals(a, b)),
        O::LessThan => Val::Bool(less_than(a, b) == Some(true)),
        O::GreaterThan => Val::Bool(less_than(b, a) == Some(true)),
        O::LessEqualThan => Val::Bool(less_than(b, a) == Some(false)),
        O::GreaterEqualThan => Val::Bool(less_than(a, b) == Some(false)),
        O::In | O::Instanceof => return None,
        O::Addition => {
            let (pa, pb) = (a.to_primitive(), b.to_primitive());
            if matches!(pa, Val::Str(_)) || matches!(pb, Val::Str(_)) {
                let mut s = pa.to_js_string()?;
                s.push_str(&pb.to_js_string()?);
                Val::Str(s)
            } else {
                match (&pa, &pb) {
                    (Val::BigInt(x), Val::BigInt(y)) => Val::BigInt(x.checked_add(*y).or_else(unrepresentable)?),
                    (Val::BigInt(_), _) | (_, Val::BigInt(_)) => return None,
                    _ => Val::Num(pa.to_number()? + pb.to_number()?),
                }
            }
        }
        _ => {
            if let (Val::BigInt(x), Val::BigInt(y)) = (a, b) {
                let (x, y) = (*x, *y);
                return Some(Val::BigInt(match op {
                    O::Subtraction => x.checked_sub(y).or_else(unrepresentable)?,
                    O::Multiplication => x.checked_mul(y).or_else(unrepresentable)?,
                    // dividing by zero throws; `MIN / -1` overflows, `MIN % -1` is 0
                    O::Division | O::Remainder if y == 0 => return None,
                    O::Division => x.checked_div(y).or_else(unrepresentable)?,
                    O::Remainder => x.checked_rem(y).unwrap_or(0),
                    // a negative exponent throws
                    O::Exponential if y < 0 => return None,
                    O::Exponential => match x {
                        0 | 1 => if y == 0 { 1 } else { x },
                        -1 => if y % 2 == 0 { 1 } else { -1 },
                        _ => match u32::try_from(y) {
                            Ok(e) => x.checked_pow(e).or_else(unrepresentable)?,
                            Err(_) => return unrepresentable(),
                        },
                    },
                    O::BitwiseAnd => x & y,
                    O::BitwiseOR => x | y,
                    O::BitwiseXOR => x ^ y,
                    // a negative shift amount shifts the other way
                    O::ShiftLeft if y >= 0 => bigint_shl(x, y.unsigned_abs())?,
                    O::ShiftLeft => bigint_shr(x, y.unsigned_abs()),
                    O::ShiftRight if y >= 0 => bigint_shr(x, y.unsigned_abs()),
                    O::ShiftRight => bigint_shl(x, y.unsigned_abs())?,
                    _ => return None,
                }));
            }
            if matches!(a, Val::BigInt(_)) || matches!(b, Val::BigInt(_)) {
                return None;
            }
            let (x, y) = (a.to_primitive().to_number()?, b.to_primitive().to_number()?);
            Val::Num(match op {
                O::Subtraction => x - y,
                O::Multiplication => x * y,
                O::Division => x / y,
                O::Remainder => x % y,
                O::Exponential => js_pow(x, y),
                O::BitwiseAnd => f64::from(x.to_int_32() & y.to_int_32()),
                O::BitwiseOR => f64::from(x.to_int_32() | y.to_int_32()),
                O::BitwiseXOR => f64::from(x.to_int_32() ^ y.to_int_32()),
                O::ShiftLeft => f64::from(x.to_int_32().wrapping_shl(y.to_uint_32() & 31)),
                O::ShiftRight => f64::from(x.to_int_32().wrapping_shr(y.to_uint_32() & 31)),
                O::ShiftRightZeroFill => f64::from(x.to_uint_32().wrapping_shr(y.to_uint_32() & 31)),
                _ => return None,
            })
        }
    })
}

/// `Math.pow`/`**`, where NaN exponents and `±1 ** ±Infinity` are NaN
fn js_pow(x: f64, y: f64) -> f64 {
    if y.is_nan() || (x.abs() == 1.0 && y.is_infinite()) {
        return f64::NAN;
    }
    x.powf(y)
}

type GlobalFn = fn(&[Val]) -> Option<Val>;

fn num_arg(args: &[Val], i: usize) -> Option<f64> {
    args.get(i).unwrap_or(&Val::Undefined).to_primitive().to_number()
}

fn math1(args: &[Val], f: fn(f64) -> f64) -> Option<Val> {
    Some(Val::Num(f(num_arg(args, 0)?)))
}

/// `Math.round`: rounds half up, keeping -0 for -0.5 <= x < 0
/// `Math.round`: compares the fraction exactly (`x + 0.5` can itself round)
fn js_round(x: f64) -> f64 {
    if !x.is_finite() || x == 0.0 {
        return x;
    }
    let floor = x.floor();
    // exact: within 1 of each other, or `x` is already an integer (|x| >= 2^52)
    let fraction = x - floor;
    if fraction == 0.0 {
        return x;
    }
    let r = if fraction >= 0.5 { floor + 1.0 } else { floor };
    if r == 0.0 && x < 0.0 { -0.0 } else { r }
}

fn js_sign(x: f64) -> f64 {
    if x.is_nan() || x == 0.0 { x } else { x.signum() }
}

/// `Math.f16round`
fn f16round(x: f64) -> f64 {
    if !x.is_finite() || x == 0.0 {
        return x;
    }
    // round to the nearest half-precision value (ties to even)
    let a = x.abs();
    if a >= 65520.0 {
        return f64::INFINITY.copysign(x);
    }
    let (exp, step) = if a < 6.103_515_625e-5 {
        (0, 2f64.powi(-24))
    } else {
        let e = a.log2().floor() as i32;
        (e, 2f64.powi(e - 10))
    };
    let _ = exp;
    let q = a / step;
    let r = q.round();
    let r = if (q - q.floor() - 0.5).abs() < f64::EPSILON && r % 2.0 != 0.0 { r - 1.0 } else { r };
    (r * step).copysign(x)
}

/// `parseFloat`
fn parse_float(s: &str) -> f64 {
    let t = s.trim_start_matches(crate::analyze::utils::is_js_whitespace);
    for prefix in ["Infinity", "+Infinity"] {
        if t.starts_with(prefix) {
            return f64::INFINITY;
        }
    }
    if t.starts_with("-Infinity") {
        return f64::NEG_INFINITY;
    }
    let bytes = t.as_bytes();
    let mut i = 0;
    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
        i += 1;
    }
    let digits_start = i;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i < bytes.len() && bytes[i] == b'.' {
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
    }
    if i == digits_start || (i == digits_start + 1 && bytes[digits_start] == b'.') {
        return f64::NAN;
    }
    if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
        let mut j = i + 1;
        if j < bytes.len() && (bytes[j] == b'+' || bytes[j] == b'-') {
            j += 1;
        }
        let exp_start = j;
        while j < bytes.len() && bytes[j].is_ascii_digit() {
            j += 1;
        }
        if j > exp_start {
            i = j;
        }
    }
    t[..i].parse::<f64>().unwrap_or(f64::NAN)
}

/// `parseInt`
fn parse_int(s: &str, radix: Option<f64>) -> f64 {
    let t = s.trim_start_matches(crate::analyze::utils::is_js_whitespace);
    let (neg, mut t) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let mut r = radix.map_or(0, |r| r.to_int_32());
    let mut strip_prefix = true;
    if r != 0 {
        if !(2..=36).contains(&r) {
            return f64::NAN;
        }
        if r != 16 {
            strip_prefix = false;
        }
    } else {
        r = 10;
    }
    if strip_prefix && (t.starts_with("0x") || t.starts_with("0X")) {
        t = &t[2..];
        r = 16;
    }
    let end = t.char_indices().find(|&(_, c)| c.to_digit(r as u32).is_none()).map_or(t.len(), |(i, _)| i);
    let digits = &t[..end];
    if digits.is_empty() {
        return f64::NAN;
    }
    // exactly, then rounded once: base 10 through Rust's correctly rounded parser, other bases
    // through a 128-bit integer
    let n = if r == 10 {
        match digits.parse::<f64>() {
            Ok(n) => n,
            Err(_) => return unrepresentable().unwrap_or(f64::NAN),
        }
    } else {
        let mut v: u128 = 0;
        for c in digits.chars() {
            match v.checked_mul(r as u128).and_then(|v| v.checked_add(c.to_digit(r as u32).unwrap() as u128)) {
                Some(next) => v = next,
                None => return unrepresentable().unwrap_or(f64::NAN),
            }
        }
        // powers of two round correctly from the exact value; other bases only stay exact below
        // 2^53 (beyond it, engines accumulate in floating point and their rounding varies)
        if !(r as u32).is_power_of_two() && v > (1u128 << 53) {
            return unrepresentable().unwrap_or(f64::NAN);
        }
        v as f64
    };
    if neg { -n } else { n }
}

fn string_arg(args: &[Val], i: usize) -> Option<String> {
    args.get(i).unwrap_or(&Val::Undefined).to_js_string()
}

/// `globals[keypath]`: the result type, and the function when it's known
fn global_fn(keypath: &str) -> Option<(Val, Option<GlobalFn>)> {
    let n = |f: GlobalFn| Some((Val::Number, Some(f)));
    match keypath {
        "BigInt" | "Math.random" => Some((Val::Number, None)),
        "Math.min" => n(|a| {
            let mut r = f64::INFINITY;
            for i in 0..a.len() {
                let x = num_arg(a, i)?;
                if x.is_nan() {
                    r = f64::NAN;
                } else if !r.is_nan() && (x < r || (x == 0.0 && r == 0.0 && x.is_sign_negative())) {
                    r = x;
                }
            }
            Some(Val::Num(r))
        }),
        "Math.max" => n(|a| {
            let mut r = f64::NEG_INFINITY;
            for i in 0..a.len() {
                let x = num_arg(a, i)?;
                if x.is_nan() {
                    r = f64::NAN;
                } else if !r.is_nan() && (x > r || (x == 0.0 && r == 0.0 && r.is_sign_negative())) {
                    r = x;
                }
            }
            Some(Val::Num(r))
        }),
        "Math.floor" => n(|a| math1(a, f64::floor)),
        "Math.f16round" => n(|a| math1(a, f16round)),
        "Math.round" => n(|a| math1(a, js_round)),
        "Math.abs" => n(|a| math1(a, f64::abs)),
        "Math.acos" => n(|a| math1(a, f64::acos)),
        "Math.asin" => n(|a| math1(a, f64::asin)),
        "Math.atan" => n(|a| math1(a, f64::atan)),
        "Math.atan2" => n(|a| Some(Val::Num(num_arg(a, 0)?.atan2(num_arg(a, 1)?)))),
        "Math.ceil" => n(|a| math1(a, f64::ceil)),
        "Math.cos" => n(|a| math1(a, f64::cos)),
        "Math.sin" => n(|a| math1(a, f64::sin)),
        "Math.tan" => n(|a| math1(a, f64::tan)),
        "Math.exp" => n(|a| math1(a, f64::exp)),
        "Math.log" => n(|a| math1(a, f64::ln)),
        "Math.pow" => n(|a| Some(Val::Num(js_pow(num_arg(a, 0)?, num_arg(a, 1)?)))),
        "Math.sqrt" => n(|a| math1(a, f64::sqrt)),
        "Math.clz32" => n(|a| Some(Val::Num(f64::from(num_arg(a, 0)?.to_uint_32().leading_zeros())))),
        "Math.imul" => n(|a| Some(Val::Num(f64::from(num_arg(a, 0)?.to_int_32().wrapping_mul(num_arg(a, 1)?.to_int_32()))))),
        "Math.sign" => n(|a| math1(a, js_sign)),
        "Math.log10" => n(|a| math1(a, f64::log10)),
        "Math.log2" => n(|a| math1(a, f64::log2)),
        "Math.log1p" => n(|a| math1(a, f64::ln_1p)),
        "Math.expm1" => n(|a| math1(a, f64::exp_m1)),
        "Math.cosh" => n(|a| math1(a, f64::cosh)),
        "Math.sinh" => n(|a| math1(a, f64::sinh)),
        "Math.tanh" => n(|a| math1(a, f64::tanh)),
        "Math.acosh" => n(|a| math1(a, f64::acosh)),
        "Math.asinh" => n(|a| math1(a, f64::asinh)),
        "Math.atanh" => n(|a| math1(a, f64::atanh)),
        "Math.trunc" => n(|a| math1(a, f64::trunc)),
        "Math.fround" => n(|a| math1(a, |x| f64::from(x as f32))),
        "Math.cbrt" => n(|a| math1(a, f64::cbrt)),
        "Number" => n(|a| match a.first() {
            None => Some(Val::Num(0.0)),
            Some(Val::BigInt(x)) => Some(Val::Num(*x as f64)),
            Some(v) => Some(Val::Num(v.to_primitive().to_number()?)),
        }),
        "Number.isInteger" => n(|a| Some(Val::Bool(matches!(a.first(), Some(Val::Num(x)) if x.is_finite() && x.trunc() == *x)))),
        "Number.isFinite" => n(|a| Some(Val::Bool(matches!(a.first(), Some(Val::Num(x)) if x.is_finite())))),
        "Number.isNaN" => n(|a| Some(Val::Bool(matches!(a.first(), Some(Val::Num(x)) if x.is_nan())))),
        "Number.isSafeInteger" => n(|a| Some(Val::Bool(matches!(a.first(), Some(Val::Num(x)) if x.is_finite() && x.trunc() == *x && x.abs() <= 9007199254740991.0)))),
        "Number.parseFloat" => n(|a| Some(Val::Num(parse_float(&string_arg(a, 0)?)))),
        "Number.parseInt" => n(|a| {
            let radix = match a.get(1) {
                None | Some(Val::Undefined) => None,
                Some(_) => Some(num_arg(a, 1)?),
            };
            Some(Val::Num(parse_int(&string_arg(a, 0)?, radix)))
        }),
        "String" => Some((Val::String, Some(|a: &[Val]| match a.first() {
            None => Some(Val::Str(String::new())),
            Some(v) => Some(Val::Str(v.to_js_string()?)),
        }))),
        "String.fromCharCode" => Some((Val::String, Some(|a: &[Val]| {
            let units: Option<Vec<u16>> = (0..a.len()).map(|i| num_arg(a, i).map(|x| x.to_uint_32() as u16)).collect();
            // a lone surrogate has no Rust string form: not folded
            Some(Val::Str(String::from_utf16(&units?).ok().or_else(unrepresentable)?))
        }))),
        "String.fromCodePoint" => Some((Val::String, Some(|a: &[Val]| {
            let mut s = String::new();
            for i in 0..a.len() {
                let x = num_arg(a, i)?;
                if x.trunc() != x || !(0.0..=1_114_111.0).contains(&x) {
                    return None;
                }
                // surrogate code points have no Rust string form: not folded
                s.push(char::from_u32(x as u32).or_else(unrepresentable)?);
            }
            Some(Val::Str(s))
        }))),
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------
// Evaluating the ESTree nodes the transform builds

use crate::estree::{LiteralValue as ELit, Node as ENode, NodeKind as EK};

impl<'s> Scopes<'s> {
    /// `scope.evaluate(expression)` on an ESTree node of the transform (identifiers fall back
    /// to their bindings' initial values in the analysed tree)
    pub fn evaluate_estree(&self, ast: &'s Ast<'s>, expression: &ENode, scope: ScopeId) -> Evaluation {
        let mut cx = Ctx { scopes: self, ast, current: Vec::new() };
        let mut values = Vec::new();
        cx.evaluate_node(expression, scope, &mut values);
        summarize(values)
    }
}

/// `get_global_keypath` on an ESTree callee
fn estree_keypath(scopes: &Scopes, node: &ENode, scope: ScopeId) -> Option<String> {
    let mut n = node;
    let mut joined = String::new();
    while let EK::MemberExpression(m) = &n.kind {
        if m.computed {
            return None;
        }
        let EK::Identifier(p) = &m.property.kind else { return None };
        joined.insert_str(0, p.name.as_str());
        joined.insert(0, '.');
        n = &m.object;
    }
    if let EK::CallExpression(c) = &n.kind {
        if matches!(c.callee.kind, EK::Identifier(_)) {
            joined.insert_str(0, "()");
            n = &c.callee;
        }
    }
    let EK::Identifier(id) = &n.kind else { return None };
    if scopes.get(scope, id.name.as_str()).is_some() {
        return None;
    }
    Some(format!("{}{}", id.name, joined))
}

impl<'a, 's> Ctx<'a, 's> {
    fn eval_node(&mut self, n: &ENode, scope: ScopeId) -> Evaluation {
        let mut values = Vec::new();
        if self.evaluate_node(n, scope, &mut values) {
            summarize(values)
        } else {
            summarize(Vec::new())
        }
    }

    fn evaluate_node(&mut self, n: &ENode, scope: ScopeId, values: &mut Vec<Val>) -> bool {
        // identity of ESTree nodes: their address, tagged to keep them apart from `P` keys
        let key = (n as *const ENode as usize) | (1 << 63);
        if self.current.contains(&key) {
            return false;
        }
        self.current.push(key);
        self.evaluate_node_inner(n, scope, values);
        self.current.pop();
        true
    }

    fn evaluate_node_inner(&mut self, n: &ENode, scope: ScopeId, values: &mut Vec<Val>) {
        match &n.kind {
            EK::Literal(l) => add(
                values,
                match &l.value {
                    ELit::String(s) => Val::Str(s.to_string()),
                    ELit::Number(x) => Val::Num(*x),
                    ELit::Boolean(b) => Val::Bool(*b),
                    ELit::Null => Val::Null,
                    ELit::BigInt(b) => b.parse::<i128>().ok().or_else(unrepresentable).map_or(Val::Unknown, Val::BigInt),
                    ELit::RegExp(r) => Val::RegExp(n as *const ENode as usize, format!("/{}/{}", r.pattern, r.flags)),
                },
            ),
            EK::Identifier(id) => {
                let Some(b) = self.scopes.get(scope, id.name.as_str()) else {
                    add(values, if id.name == "undefined" { Val::Undefined } else { Val::Unknown });
                    return;
                };
                self.binding_value(b, id.name.as_str(), scope, values);
            }
            EK::BinaryExpression(e) => {
                let a = self.eval_node(&e.left, scope);
                let b = self.eval_node(&e.right, scope);
                if a.is_known && b.is_known {
                    let av = a.value.clone().unwrap_or(Val::Undefined);
                    let bv = b.value.clone().unwrap_or(Val::Undefined);
                    add(values, binary(e.operator, &av, &bv).unwrap_or(Val::Unknown));
                    return;
                }
                use BinaryOperator as O;
                match e.operator {
                    O::Inequality | O::StrictInequality | O::LessThan | O::LessEqualThan | O::GreaterThan | O::GreaterEqualThan | O::Equality | O::StrictEquality | O::In | O::Instanceof => {
                        add(values, Val::Bool(true));
                        add(values, Val::Bool(false));
                    }
                    O::Addition => {
                        if a.is_string || b.is_string {
                            add(values, Val::String);
                        } else if a.is_number && b.is_number {
                            add(values, Val::Number);
                        } else {
                            add(values, Val::String);
                            add(values, Val::Number);
                        }
                    }
                    _ => add(values, Val::Number),
                }
            }
            EK::ConditionalExpression(e) => {
                let test = self.eval_node(&e.test, scope);
                let consequent = self.eval_node(&e.consequent, scope);
                let alternate = self.eval_node(&e.alternate, scope);
                if test.is_known {
                    let taken = if test.value.as_ref().is_some_and(Val::to_boolean) { consequent } else { alternate };
                    for v in taken.values {
                        add(values, v);
                    }
                } else {
                    for v in consequent.values.into_iter().chain(alternate.values) {
                        add(values, v);
                    }
                }
            }
            EK::LogicalExpression(e) => {
                let a = self.eval_node(&e.left, scope);
                let b = self.eval_node(&e.right, scope);
                let av = a.value.clone().unwrap_or(Val::Undefined);
                if a.is_known {
                    if b.is_known {
                        let bv = b.value.clone().unwrap_or(Val::Undefined);
                        let r = match e.operator {
                            LogicalOperator::And => if av.to_boolean() { bv } else { av },
                            LogicalOperator::Or => if av.to_boolean() { av } else { bv },
                            LogicalOperator::Coalesce => if matches!(av, Val::Null | Val::Undefined) { bv } else { av },
                        };
                        add(values, r);
                        return;
                    }
                    let short = match e.operator {
                        LogicalOperator::And => !av.to_boolean(),
                        LogicalOperator::Or => av.to_boolean(),
                        LogicalOperator::Coalesce => !matches!(av, Val::Null | Val::Undefined),
                    };
                    if short {
                        add(values, av);
                    } else {
                        for v in b.values {
                            add(values, v);
                        }
                    }
                    return;
                }
                for v in a.values.into_iter().chain(b.values) {
                    add(values, v);
                }
            }
            EK::UnaryExpression(e) => {
                let arg = self.eval_node(&e.argument, scope);
                if arg.is_known {
                    let v = arg.value.clone().unwrap_or(Val::Undefined);
                    add(values, unary(e.operator, &v));
                    return;
                }
                match e.operator {
                    UnaryOperator::LogicalNot | UnaryOperator::Delete => {
                        add(values, Val::Bool(false));
                        add(values, Val::Bool(true));
                    }
                    UnaryOperator::UnaryPlus | UnaryOperator::UnaryNegation | UnaryOperator::BitwiseNot => add(values, Val::Number),
                    UnaryOperator::Typeof => add(values, Val::String),
                    UnaryOperator::Void => add(values, Val::Undefined),
                }
            }
            EK::CallExpression(c) => {
                let Some(keypath) = estree_keypath(self.scopes, &c.callee, scope) else {
                    add(values, Val::Unknown);
                    return;
                };
                if super::utils::is_rune(&keypath).is_some() {
                    let arg = c.arguments.first();
                    match keypath.as_str() {
                        "$state" | "$state.raw" | "$derived" => match arg {
                            Some(a) => {
                                self.evaluate_node(a, scope, values);
                            }
                            None => add(values, Val::Undefined),
                        },
                        "$props.id" => add(values, Val::String),
                        "$effect.tracking" => {
                            add(values, Val::Bool(false));
                            add(values, Val::Bool(true));
                        }
                        "$derived.by" => match arg.map(|a| &a.kind) {
                            Some(EK::ArrowFunctionExpression(f)) if f.expression => {
                                self.evaluate_node(&f.body, scope, values);
                            }
                            _ => add(values, Val::Unknown),
                        },
                        _ => add(values, Val::Unknown),
                    }
                    return;
                }
                if let Some((ty, f)) = global_fn(&keypath) {
                    if c.arguments.iter().all(|a| !matches!(a.kind, EK::SpreadElement(_))) {
                        let args: Vec<Evaluation> = c.arguments.iter().map(|a| self.eval_node(a, scope)).collect();
                        match f {
                            Some(f) if args.iter().all(|e| e.is_known) => {
                                let vals: Vec<Val> = args.into_iter().map(|e| e.value.unwrap_or(Val::Undefined)).collect();
                                add(values, f(&vals).unwrap_or(Val::Unknown));
                            }
                            _ => add(values, ty),
                        }
                        return;
                    }
                }
                add(values, Val::Unknown);
            }
            EK::TemplateLiteral(t) => {
                let cooked = |i: usize| match &t.quasis[i].kind {
                    EK::TemplateElement(q) => q.cooked.as_ref().map_or("undefined".to_string(), |c| c.to_string()),
                    _ => String::new(),
                };
                let mut result = cooked(0);
                for (i, e) in t.expressions.iter().enumerate() {
                    let ev = self.eval_node(e, scope);
                    if ev.is_known {
                        let s = ev.value.as_ref().and_then(Val::to_js_string).unwrap_or_else(|| "undefined".into());
                        result.push_str(&s);
                        result.push_str(&cooked(i + 1));
                    } else {
                        add(values, Val::String);
                        break;
                    }
                }
                add(values, Val::Str(result));
            }
            EK::MemberExpression(_) => {
                let keypath = estree_keypath(self.scopes, n, scope);
                let constant = match keypath.as_deref() {
                    Some("Math.PI") => Some(std::f64::consts::PI),
                    Some("Math.E") => Some(std::f64::consts::E),
                    Some("Math.LN10") => Some(std::f64::consts::LN_10),
                    Some("Math.LN2") => Some(std::f64::consts::LN_2),
                    Some("Math.LOG10E") => Some(std::f64::consts::LOG10_E),
                    Some("Math.LOG2E") => Some(std::f64::consts::LOG2_E),
                    Some("Math.SQRT2") => Some(std::f64::consts::SQRT_2),
                    Some("Math.SQRT1_2") => Some(std::f64::consts::FRAC_1_SQRT_2),
                    _ => None,
                };
                add(values, constant.map_or(Val::Unknown, Val::Num));
            }
            EK::ArrowFunctionExpression(_) | EK::FunctionExpression(_) => add(values, Val::Function),
            _ => add(values, Val::Unknown),
        }
    }

    /// The `Identifier` case for a binding
    fn binding_value(&mut self, b: BindingId, name: &str, scope: ScopeId, values: &mut Vec<Val>) {
        let binding: &scope::Binding<'s> = self.scopes.binding(b);
        if let Some(initial @ P::Js(AstKind::CallExpression(_))) = binding.initial {
            if scope::get_rune(self.scopes, Some(initial), scope) == Some("$props.id") {
                add(values, Val::String);
                return;
            }
        }
        let is_prop = matches!(binding.kind, Kind::Prop | Kind::RestProp | Kind::BindableProp);
        if let Some(P::Node(n)) = binding.initial {
            match &self.ast.nodes[n] {
                Node::EachBlock { index: Some(index), .. } if index == name => {
                    add(values, Val::Number);
                    return;
                }
                Node::SnippetBlock { .. } => {
                    add(values, Val::Unknown);
                    return;
                }
                _ => {}
            }
        }
        if !binding.updated() && !is_prop {
            if let Some(initial) = binding.initial {
                self.evaluate(initial, binding.scope, values);
                return;
            }
        }
        add(values, Val::Unknown);
    }
}
