//! Scoped variable environments for the interpreter.
//!
//! Scopes form a linked list; each scope shares its parent by reference
//! (`Rc<RefCell>`), so assignments inside a block or loop propagate to the
//! enclosing scope instead of being lost on a copy.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::value::Value;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Env {
    vars: HashMap<String, Value>,
    parent: Option<Rc<RefCell<Env>>>,
}

impl Env {
    pub fn new() -> Self {
        Env::default()
    }

    /// Create a child scope sharing `parent` by reference. Assignments in
    /// the child are visible to the parent and vice versa.
    pub fn with_parent(parent: &Rc<RefCell<Env>>) -> Rc<RefCell<Env>> {
        Rc::new(RefCell::new(Env {
            vars: HashMap::new(),
            parent: Some(Rc::clone(parent)),
        }))
    }

    /// Define a binding in this scope. Phase 0: redefinition shadows.
    pub fn define(&mut self, name: &str, value: Value) {
        self.vars.insert(name.to_string(), value);
    }

    pub fn get(&self, name: &str) -> Option<Value> {
        if let Some(v) = self.vars.get(name) {
            Some(v.clone())
        } else {
            self.parent.as_deref().and_then(|p| p.borrow().get(name))
        }
    }

    /// Mutable access to a binding in this scope (no parent fallthrough).
    pub fn get_mut(&mut self, name: &str) -> Option<&mut Value> {
        self.vars.get_mut(name)
    }

    /// Copy all local bindings from `child` into this scope. Used for
    /// or-pattern matching: alternatives are tried in a throwaway child
    /// scope so failed alternatives leave no bindings behind; on success
    /// the winning alternative's bindings are merged here.
    pub fn absorb_locals(&mut self, child: &Rc<RefCell<Env>>) {
        let vars = child.borrow().vars.clone();
        for (k, v) in vars {
            self.vars.insert(k, v);
        }
    }

    /// The parent scope, if any (used by the VM to leave a scope).
    pub fn parent_rc(&self) -> Option<Rc<RefCell<Env>>> {
        self.parent.clone()
    }

    /// Assign to a binding, walking up the scope chain. Returns `false` when
    /// the name is not bound anywhere.
    pub fn assign(&mut self, name: &str, value: Value) -> bool {
        if self.vars.contains_key(name) {
            self.vars.insert(name.to_string(), value);
            return true;
        }
        match &self.parent {
            Some(p) => p.borrow_mut().assign(name, value),
            None => false,
        }
    }

    /// Shape fingerprint of the scope chain: per-scope `(identity, binding
    /// count)` leaf→root. The *set* of visible names can only change when a
    /// binding is added (count changes) or a scope is replaced (identity
    /// changes), so snapshots can cache name-filter decisions against this
    /// key while always cloning values fresh.
    pub fn chain_shape(env: &Rc<RefCell<Env>>) -> Vec<(usize, usize)> {
        let mut shape = Vec::new();
        let mut cur: Option<Rc<RefCell<Env>>> = Some(Rc::clone(env));
        while let Some(rc) = cur {
            let (key, parent, len) = {
                let borrowed = rc.borrow();
                (
                    Rc::as_ptr(&rc) as *const () as usize,
                    borrowed.parent.clone(),
                    borrowed.vars.len(),
                )
            };
            shape.push((key, len));
            cur = parent;
        }
        shape
    }

    /// Visit shadowing-resolved `(name, value)` pairs root→leaf without
    /// cloning. Leaf values shadow root values: each name is visited once
    /// (leaf-most). Used by spawn snapshots to filter entries *before*
    /// paying for clones.
    ///
    /// `f` must not retain the refs it receives (copy/clone inside `f`):
    /// each node is borrowed only for its own iteration.
    pub fn visit_flat(env: &Rc<RefCell<Env>>, mut f: impl FnMut(&String, &Value)) {
        // Collect the chain leaf→root (Rc clones keep nodes alive), then
        // walk root→leaf tracking seen names so leaves win.
        let mut chain: Vec<Rc<RefCell<Env>>> = Vec::new();
        let mut cur: Option<Rc<RefCell<Env>>> = Some(Rc::clone(env));
        while let Some(rc) = cur {
            let parent = rc.borrow().parent.clone();
            chain.push(rc);
            cur = parent;
        }
        let mut seen_names: HashMap<String, ()> = HashMap::new();
        // Debug aid (ZZ_SPAWN_PROFILE=1, see spawn profiler): chain shape.
        let profile = std::env::var("ZZ_SPAWN_PROFILE").is_ok();
        let mut depth = 0usize;
        let mut node_entries = 0usize;
        for node in chain.iter().rev() {
            depth += 1;
            let borrowed = node.borrow();
            node_entries += borrowed.vars.len();
            for (k, v) in borrowed.vars.iter() {
                if seen_names.contains_key(k) {
                    continue;
                }
                seen_names.insert(k.clone(), ());
                f(k, v);
            }
        }
        if profile {
            eprintln!("[visit-flat] depth={depth} node_entries={node_entries}");
        }
    }

    /// Flatten the scope chain root→leaf into a `HashMap`. Leaf values shadow
    /// root values, matching normal scope semantics.
    pub fn flatten(&self) -> HashMap<String, Value> {
        // Collect the chain leaf→root, then insert root→leaf so leaf values
        // override. Single clone per entry per layer (no intermediate layer
        // copies).
        let mut chain: Vec<HashMap<String, Value>> = Vec::new();
        // NOTE: cannot borrow across the loop (RefCell), so each layer is
        // cloned once here; shadowed entries are simply overwritten below.
        chain.push(self.vars.clone());
        let mut cur = self.parent.clone();
        while let Some(rc) = cur {
            let parent_opt = {
                let env = rc.borrow();
                chain.push(env.vars.clone());
                env.parent.clone()
            };
            cur = parent_opt;
        }
        let mut flat = HashMap::with_capacity(chain.iter().map(|l| l.len()).sum());
        // Drain root→leaf: each entry moves exactly once; leaf values
        // overwrite root values via plain insert.
        for mut layer in chain.into_iter().rev() {
            for (k, v) in layer.drain() {
                flat.insert(k, v);
            }
        }
        flat
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoped_lookup_falls_through() {
        let outer = Rc::new(RefCell::new(Env::new()));
        outer.borrow_mut().define("a", Value::Int(1));
        let inner = Env::with_parent(&outer);
        inner.borrow_mut().define("b", Value::Int(2));
        assert_eq!(inner.borrow().get("a"), Some(Value::Int(1)));
        assert_eq!(inner.borrow().get("b"), Some(Value::Int(2)));
        // Parent doesn't see child bindings.
        assert_eq!(outer.borrow().get("b"), None);
    }

    #[test]
    fn assignment_propagates_to_parent() {
        let outer = Rc::new(RefCell::new(Env::new()));
        outer.borrow_mut().define("a", Value::Int(1));
        let inner = Env::with_parent(&outer);
        assert!(inner.borrow_mut().assign("a", Value::Int(99)));
        assert_eq!(outer.borrow().get("a"), Some(Value::Int(99)));
    }

    #[test]
    fn flatten_collects_root_to_leaf() {
        let root = Rc::new(RefCell::new(Env::new()));
        root.borrow_mut().define("a", Value::Int(1));
        root.borrow_mut().define("b", Value::Int(10));
        let mid = Env::with_parent(&root);
        mid.borrow_mut().define("b", Value::Int(2));
        mid.borrow_mut().define("c", Value::Int(3));
        let leaf = Env::with_parent(&mid);
        leaf.borrow_mut().define("c", Value::Int(30));
        leaf.borrow_mut().define("d", Value::Int(4));
        let flat = leaf.borrow().flatten();
        assert_eq!(flat.get("a"), Some(&Value::Int(1))); // root only
        assert_eq!(flat.get("b"), Some(&Value::Int(2))); // mid shadows root
        assert_eq!(flat.get("c"), Some(&Value::Int(30))); // leaf shadows mid
        assert_eq!(flat.get("d"), Some(&Value::Int(4))); // leaf only
    }

    #[test]
    fn flatten_empty_env() {
        let env = Env::new();
        assert!(env.flatten().is_empty());
    }
}
