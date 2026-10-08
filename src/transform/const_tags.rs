//! `sort_const_tags(nodes, state)` (`3-transform/utils.js`), shared by the client and server
//! transforms: in legacy mode, a fragment's `{@const}` tags are put in dependency order (as
//! Svelte 4 did), and a cycle between them is the `const_tag_cycle` error.

use crate::analyze::scope::BindingId;
use crate::ast::NodeId;

/// A `{@const}` tag: the bindings it declares and the bindings its initializer references
pub struct ConstTag {
    pub node: NodeId,
    pub bindings: Vec<BindingId>,
    pub deps: Vec<BindingId>,
}

/// The tags in topological order followed by the other nodes, or the tag a cycle starts at
/// with the cycle's bindings. `tag_of` describes a node if it's a `{@const}` tag.
pub fn sort(nodes: &[NodeId], mut tag_of: impl FnMut(NodeId) -> Option<ConstTag>) -> Result<Vec<NodeId>, (NodeId, Vec<BindingId>)> {
    let mut other = Vec::new();
    // `tags`: a Map from binding to tag (insertion order)
    let mut tags: Vec<ConstTag> = Vec::new();
    let mut by_binding: indexmap::IndexMap<BindingId, usize> = indexmap::IndexMap::new();
    for &n in nodes {
        match tag_of(n) {
            Some(tag) => {
                let i = tags.len();
                for &b in &tag.bindings {
                    by_binding.insert(b, i);
                }
                tags.push(tag);
            }
            None => other.push(n),
        }
    }
    if by_binding.is_empty() {
        return Ok(nodes.to_vec());
    }

    let mut edges = Vec::new();
    for (&id, &t) in &by_binding {
        for &dep in &tags[t].deps {
            if by_binding.contains_key(&dep) {
                edges.push((id, dep));
            }
        }
    }
    if let Some(cycle) = crate::analyze::visit::check_graph_for_cycles(&edges) {
        return Err((tags[by_binding[&cycle[0]]].node, cycle));
    }

    let mut sorted: Vec<NodeId> = Vec::new();
    fn add(t: usize, tags: &[ConstTag], by_binding: &indexmap::IndexMap<BindingId, usize>, sorted: &mut Vec<NodeId>) {
        if sorted.contains(&tags[t].node) {
            return;
        }
        for dep in &tags[t].deps {
            if let Some(&d) = by_binding.get(dep) {
                add(d, tags, by_binding, sorted);
            }
        }
        sorted.push(tags[t].node);
    }
    // `for (const tag of tags.values())`
    for &t in by_binding.values() {
        add(t, &tags, &by_binding, &mut sorted);
    }
    sorted.extend(other);
    Ok(sorted)
}
