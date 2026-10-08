//! `leadingComments`, as `get_comment_handlers().add_comments` attaches them while parsing.
//! Only needed for `svelte-ignore` comments in JS, so this only runs when there are some.

use oxc_ast::AstKind;

use super::Analyzer;
use super::nodes::{self, P};
use crate::ast::{Ast, Attr, Chunk, Expr, Node};
use crate::js::{CommentCtx, JsComment};

/// Attach comments for every JS AST of the component (if any comment is a `svelte-ignore`)
pub fn attach_all<'s>(an: &mut Analyzer<'s>) {
    let root = an.root;
    if !root.comments.iter().any(|c| c.value.contains("svelte-ignore")) {
        return;
    }
    for script in [&root.instance, &root.module].into_iter().flatten() {
        let program = P::Js(AstKind::Program(&script.content.program));
        attach(an, program, script.content.comments.index, script.content.comments.upto);
    }
    let mut exprs: Vec<(P<'s>, CommentCtx)> = Vec::new();
    collect_template(an.ast, P::Fragment(root.fragment), &mut exprs);
    for (p, ctx) in exprs {
        attach(an, p, ctx.index, ctx.upto);
    }
}

fn collect_expr<'s>(e: &'s Expr<'s>, out: &mut Vec<(P<'s>, CommentCtx)>) {
    if let Expr::Js(js) = e {
        if let Some(ctx) = js.comments {
            out.push((nodes::template_expr(e), ctx));
        }
    }
}

fn collect_template<'s>(ast: &'s Ast<'s>, p: P<'s>, out: &mut Vec<(P<'s>, CommentCtx)>) {
    match p {
        P::Fragment(f) => {
            for &n in &ast.fragments[f].nodes {
                collect_template(ast, P::Node(n), out);
            }
        }
        P::Node(n) => {
            match &ast.nodes[n] {
                Node::ExpressionTag { expression, .. } | Node::HtmlTag { expression, .. } | Node::RenderTag { expression, .. } => {
                    collect_expr(expression, out)
                }
                Node::ConstTag { init, .. } => collect_expr(init, out),
                Node::DeclarationTag { declaration: crate::ast::Declaration::Js(stmt), .. } => {
                    out.push((nodes::statement(&stmt.stmt), stmt.comments));
                }
                Node::IfBlock { test, .. } => collect_expr(test, out),
                Node::EachBlock { expression, key, .. } => {
                    collect_expr(expression, out);
                    if let Some(k) = key {
                        collect_expr(k, out);
                    }
                }
                Node::AwaitBlock { expression, .. } | Node::KeyBlock { expression, .. } => collect_expr(expression, out),
                Node::Element(el) => {
                    for a in &el.attributes {
                        match a {
                            Attr::Attribute { value, .. } | Attr::StyleDirective { value, .. } => {
                                for c in super::utils::chunks(value) {
                                    if let Chunk::Expression { expression, .. } = c {
                                        collect_expr(expression, out);
                                    }
                                }
                            }
                            Attr::Spread { expression, .. } | Attr::Attach { expression, .. } => collect_expr(expression, out),
                            Attr::Directive { expression: Some(e), .. } => collect_expr(e, out),
                            _ => {}
                        }
                    }
                    if let Some(t) = &el.tag {
                        collect_expr(t, out);
                    }
                    if let Some(e) = &el.expression {
                        collect_expr(e, out);
                    }
                }
                _ => {}
            }
            for child in template_fragments(ast, n) {
                collect_template(ast, P::Fragment(child), out);
            }
        }
        _ => {}
    }
}

fn template_fragments(ast: &Ast, n: usize) -> Vec<usize> {
    match &ast.nodes[n] {
        Node::IfBlock { consequent, alternate, .. } => std::iter::once(*consequent).chain(*alternate).collect(),
        Node::EachBlock { body, fallback, .. } => std::iter::once(*body).chain(*fallback).collect(),
        Node::AwaitBlock { pending, then, catch, .. } => [pending, then, catch].into_iter().flatten().copied().collect(),
        Node::KeyBlock { fragment, .. } => vec![*fragment],
        Node::SnippetBlock { body, .. } => vec![*body],
        Node::Element(el) => vec![el.fragment],
        _ => Vec::new(),
    }
}

struct Attacher<'a, 's> {
    queue: std::collections::VecDeque<&'s JsComment>,
    source: &'s str,
    out: &'a mut rustc_hash::FxHashMap<usize, Vec<(usize, &'s str)>>,
    ast: &'s Ast<'s>,
}

fn attach<'s>(an: &mut Analyzer<'s>, root: P<'s>, index: u32, upto: u32) {
    let all = &an.root.comments[..upto as usize];
    let queue: std::collections::VecDeque<&'s JsComment> = all.iter().filter(|c| c.start >= index as usize).collect();
    if queue.is_empty() {
        return;
    }
    let mut a = Attacher { queue, source: an.source, out: &mut an.leading_comments, ast: an.ast };
    let mut path = Vec::new();
    a.visit(root, &mut path);
}

impl<'s> Attacher<'_, 's> {
    fn visit(&mut self, node: P<'s>, path: &mut Vec<P<'s>>) {
        let Some((mut start, end)) = node.span(self.ast) else {
            // removed TS statements and the like
            return;
        };
        if let P::Js(AstKind::Program(_)) = node {
            // acorn's Program starts at 0 (the source is padded up to the script)
            start = 0;
        }
        while let Some(c) = self.queue.front() {
            if c.start < start {
                let c = self.queue.pop_front().unwrap();
                self.out.entry(node.key()).or_default().push((c.start, c.value.as_str()));
            } else {
                break;
            }
        }

        let children = nodes::children(node, self.ast);
        path.push(node);
        for &c in &children {
            self.visit(c, path);
        }
        path.pop();

        let Some(first) = self.queue.front() else { return };
        let parent = path.last().copied();
        let parent_end = parent.and_then(|p| p.span(self.ast)).map(|s| s.1);
        if parent.is_none() || Some(end) != parent_end {
            let is_last_in_body = match parent {
                Some(parent) => {
                    let list_parent = matches!(
                        parent,
                        P::Js(
                            AstKind::BlockStatement(_)
                                | AstKind::FunctionBody(_)
                                | AstKind::Program(_)
                                | AstKind::ArrayExpression(_)
                                | AstKind::ObjectExpression(_)
                        )
                    );
                    list_parent && nodes::children(parent, self.ast).last().is_some_and(|l| l.is(node))
                }
                None => false,
            };
            if is_last_in_body {
                while let Some(c) = self.queue.front() {
                    if parent_end.is_some_and(|pe| c.start >= pe) {
                        break;
                    }
                    self.queue.pop_front();
                }
            } else if end <= first.start {
                let slice = self.source.get(end..first.start).unwrap_or("");
                if slice.bytes().all(|b| matches!(b, b',' | b')' | b' ' | b'\t')) {
                    self.queue.pop_front();
                }
            }
        }
    }
}
