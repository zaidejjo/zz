//! Scoped variable environments for the interpreter.

use std::collections::HashMap;

use crate::value::Value;

#[derive(Debug, Default)]
pub struct Env {
    vars: HashMap<String, Value>,
    parent: Option<Box<Env>>,
}

impl Env {
    pub fn new() -> Self {
        Env::default()
    }

    /// Create a child scope; lookups fall through to the parent.
    pub fn child(&self) -> Env {
        Env {
            vars: HashMap::new(),
            parent: Some(Box::new(self.clone())),
        }
    }

    /// Define a binding in this scope. Phase 0: redefinition shadows.
    pub fn define(&mut self, name: &str, value: Value) {
        self.vars.insert(name.to_string(), value);
    }

    pub fn get(&self, name: &str) -> Option<Value> {
        if let Some(v) = self.vars.get(name) {
            Some(v.clone())
        } else {
            self.parent.as_deref().and_then(|p| p.get(name))
        }
    }
}

impl Clone for Env {
    fn clone(&self) -> Self {
        Env {
            vars: self.vars.clone(),
            parent: self.parent.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoped_lookup_falls_through() {
        let mut outer = Env::new();
        outer.define("a", Value::Int(1));
        let mut inner = outer.child();
        inner.define("b", Value::Int(2));
        assert_eq!(inner.get("a"), Some(Value::Int(1)));
        assert_eq!(inner.get("b"), Some(Value::Int(2)));
        // Parent doesn't see child bindings.
        assert_eq!(outer.get("b"), None);
    }
}
