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
}
