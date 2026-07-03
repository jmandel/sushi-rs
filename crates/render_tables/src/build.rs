//! A small builder over `render_xhtml::XhtmlNode` that reproduces the fhir-core
//! `XhtmlNode` fluent API (`addTag`, `style`, `setAttribute`, `tx`, `nbsp`,
//! `colspan`, `img`, `input`, `br`, `addChildNodes`) *and* defers attribute
//! ordering to Java-HashMap order (see `hashorder`).
//!
//! Design: attributes are collected in call order in a `Vec`. When the node is
//! finalized (`build`), the attribute keys are reordered with
//! `hashorder::hashmap_order` and written into the XhtmlNode's insertion-ordered
//! attribute map in that order, so `render_xhtml`'s composer (which emits in the
//! map's iteration order) produces Java-HashMap-order bytes.
//!
//! `style(...)` mirrors XhtmlNode.style (XhtmlNode.java:754): append `"; "` +
//! value to an existing `style` attribute, else set it.

use render_xhtml::{NodeType, XhtmlNode};

use crate::hashorder::hashmap_order;

/// U+00A0 NBSP, as `XhtmlNode.NBSP`.
pub const NBSP: char = '\u{00A0}';

/// A mutable element builder. Not a 1:1 of XhtmlNode — it is the *element*
/// case; text/comment children are pushed as finished XhtmlNodes.
pub struct Elem {
    name: String,
    /// (key, value) in call order. value None == Java null attribute.
    attrs: Vec<(String, Option<String>)>,
    children: Vec<XhtmlNode>,
}

impl Elem {
    pub fn new(name: impl Into<String>) -> Elem {
        Elem {
            name: name.into(),
            attrs: Vec::new(),
            children: Vec::new(),
        }
    }

    /// XhtmlNode.setAttribute / attribute — put (insert or update in place).
    pub fn set_attr(&mut self, name: &str, value: impl Into<String>) -> &mut Elem {
        let v = value.into();
        if let Some(slot) = self.attrs.iter_mut().find(|(k, _)| k == name) {
            slot.1 = Some(v);
        } else {
            self.attrs.push((name.to_string(), Some(v)));
        }
        self
    }

    pub fn has_attr(&self, name: &str) -> bool {
        self.attrs.iter().any(|(k, _)| k == name)
    }

    fn get_attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .and_then(|(_, v)| v.as_deref())
    }

    /// XhtmlNode.style (XhtmlNode.java:754).
    pub fn style(&mut self, style: &str) -> &mut Elem {
        if let Some(existing) = self.get_attr("style") {
            let merged = format!("{}; {}", existing, style);
            self.set_attr("style", merged);
        } else {
            self.set_attr("style", style.to_string());
        }
        self
    }

    /// Append a finished child node.
    pub fn push(&mut self, node: XhtmlNode) -> &mut Elem {
        self.children.push(node);
        self
    }

    /// XhtmlNode.addText — append a text node.
    pub fn text(&mut self, content: impl Into<String>) -> &mut Elem {
        let mut t = XhtmlNode::new(NodeType::Text);
        t.set_content(content.into());
        self.children.push(t);
        self
    }

    /// XhtmlNode.tx (same as addText for our purposes).
    pub fn tx(&mut self, content: impl Into<String>) -> &mut Elem {
        self.text(content)
    }

    /// XhtmlNode.nbsp — addText(NBSP).
    pub fn nbsp(&mut self) -> &mut Elem {
        self.text(NBSP.to_string())
    }

    /// Append a sub-element built elsewhere.
    pub fn push_elem(&mut self, e: Elem) -> &mut Elem {
        self.children.push(e.build());
        self
    }

    /// Number of children so far.
    pub fn child_count(&self) -> usize {
        self.children.len()
    }

    /// Finalize into an XhtmlNode with attributes in Java-HashMap order.
    pub fn build(self) -> XhtmlNode {
        let mut node = XhtmlNode::new(NodeType::Element);
        node.set_name(self.name);
        let keys: Vec<String> = self.attrs.iter().map(|(k, _)| k.clone()).collect();
        let ordered = hashmap_order(&keys);
        for key in ordered {
            let (_, v) = self.attrs.iter().find(|(k, _)| *k == key).unwrap();
            node.put_attribute_opt(key, v.clone());
        }
        for c in self.children {
            node.add_child_node(c);
        }
        node
    }
}

/// A plain text node.
pub fn text_node(content: impl Into<String>) -> XhtmlNode {
    let mut t = XhtmlNode::new(NodeType::Text);
    t.set_content(content.into());
    t
}
