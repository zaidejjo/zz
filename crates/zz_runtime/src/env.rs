//! Scoped variable environments for the interpreter.
//!
//! Scopes form a linked list of [`EnvLink`]s — either owned (`Rc<RefCell>`
//! scopes, single-threaded use) or frozen (`Arc`-shared immutable maps).
//! Snapshots share frozen ancestors by pointer instead of deep-cloning
//! values: freezing is copy-on-write (writers detach), so sharing is
//! exactly as correct as cloning, at O(1) per spawn.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use crate::value::Value;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Env {
    vars: HashMap<String, Value>,
    parent: Option<EnvLink>,
    /// Mutation counter: bumped on every write (`define`, `assign`,
    /// `absorb_locals`). Snapshot caches key on `(link id, version)` —
    /// any value change invalidates, so frozen copies are never stale.
    /// Per-scope (never global): worker-side writes touch only worker
    /// scopes and can never invalidate the spawner's cache entries.
    version: std::cell::Cell<u64>,
}

/// An immutable scope snapshot, shareable across threads. Created by
/// freezing an owned chain (see [`EnvLink::frozen_scope`]): the map is
/// cloned once, then read freely with no borrow flags and no locks.
/// The parent chain is frozen-only (`Arc`-linked), so a frozen scope is
/// statically `Send + Sync` — the compiler itself proves the
/// cross-thread sharing sound (values cross under the existing
/// `Send for Value` discipline).
#[derive(Debug, Clone, PartialEq)]
pub struct FrozenScope {
    vars: HashMap<String, Value>,
    parent: Option<Arc<FrozenScope>>,
}

impl FrozenScope {
    /// Read a binding, leaf-first (plain map reads, no borrow flags).
    fn get(&self, name: &str) -> Option<Value> {
        if let Some(v) = self.vars.get(name) {
            Some(v.clone())
        } else {
            self.parent.as_ref().and_then(|p| p.get(name))
        }
    }
}

/// A scope-chain link: owned (single-threaded, mutable) or frozen
/// (shared, immutable). Same method surface either way; frozen links
/// detach (clone-on-write) when mutation is attempted.
#[derive(Debug, Clone, PartialEq)]
pub enum EnvLink {
    Owned(Rc<RefCell<Env>>),
    Frozen(Arc<FrozenScope>),
}

impl Env {
    pub fn new() -> Self {
        Env::default()
    }

    /// Create a child scope sharing `parent` by reference. Assignments in
    /// the child are visible to the parent and vice versa.
    pub fn with_parent(parent: &EnvLink) -> EnvLink {
        EnvLink::Owned(Rc::new(RefCell::new(Env {
            vars: HashMap::new(),
            parent: Some(parent.clone()),
            version: std::cell::Cell::new(0),
        })))
    }

    /// Current mutation version of this scope (see `version` docs).
    pub fn version(&self) -> u64 {
        self.version.get()
    }

    /// Define a binding in this scope. Phase 0: redefinition shadows.
    /// Bumps the mutation version.
    pub fn define(&mut self, name: &str, value: Value) {
        self.vars.insert(name.to_string(), value);
        self.bump();
    }

    /// Bump the mutation version (new or replaced binding).
    fn bump(&self) {
        // `Cell`: no borrow flags involved; `&self` suffices.
        self.version.set(self.version.get().wrapping_add(1));
    }

    pub fn get(&self, name: &str) -> Option<Value> {
        if let Some(v) = self.vars.get(name) {
            Some(v.clone())
        } else {
            self.parent.as_ref().and_then(|p| p.get(name))
        }
    }

    /// Mutable access to a binding in this scope (no parent fallthrough).
    pub fn get_mut(&mut self, name: &str) -> Option<&mut Value> {
        self.vars.get_mut(name)
    }

    /// Direct read from this scope only (no parent walk): for cached
    /// resolutions, where the holder is already known.
    pub fn get_local(&self, name: &str) -> Option<Value> {
        self.vars.get(name).cloned()
    }

    /// Sorted names bound in this scope only (no parent walk): for
    /// spawn keep-cache validation of fresh per-iteration leaves.
    pub fn local_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.vars.keys().cloned().collect();
        names.sort();
        names
    }

    /// Shape fingerprint of one chain link's ancestors: per-scope
    /// `(identity, binding count, mutation version)` from the given scope
    /// upward. Lets caches key on the *parent* chain while the leaf varies
    /// per loop iteration. The version covers value replacement (`assign`
    /// changes no counts but must still invalidate frozen copies).
    pub fn chain_shape_from(env: &EnvLink) -> Vec<(usize, usize, u64)> {
        let mut shape = Vec::new();
        let mut cur: Option<EnvLink> = Some(env.clone());
        while let Some(link) = cur {
            let (key, parent, len, version) = match &link {
                EnvLink::Owned(rc) => {
                    let borrowed = rc.borrow();
                    (
                        Rc::as_ptr(rc) as *const () as usize,
                        borrowed.parent.clone(),
                        borrowed.vars.len(),
                        borrowed.version(),
                    )
                }
                // Frozen scopes never change: version is fixed at freeze
                // (always 0 — the field exists only for key uniformity).
                EnvLink::Frozen(f) => (
                    Arc::as_ptr(f) as *const () as usize,
                    f.parent.clone().map(EnvLink::Frozen),
                    f.vars.len(),
                    0,
                ),
            };
            shape.push((key, len, version));
            cur = parent;
        }
        shape
    }

    /// Copy all local bindings from `child` into this scope. Used for
    /// or-pattern matching: alternatives are tried in a throwaway child
    /// scope so failed alternatives leave no bindings behind; on success
    /// the winning alternative's bindings are merged here.
    /// Bumps the mutation version when anything was merged.
    pub fn absorb_locals(&mut self, child: &EnvLink) {
        let vars = match child {
            EnvLink::Owned(rc) => rc.borrow().vars.clone(),
            EnvLink::Frozen(f) => f.vars.clone(),
        };
        if vars.is_empty() {
            return;
        }
        for (k, v) in vars {
            self.vars.insert(k, v);
        }
        self.bump();
    }

    /// The parent scope, if any (used by the VM to leave a scope).
    pub fn parent_rc(&self) -> Option<EnvLink> {
        self.parent.clone()
    }

    /// Assign to a binding, walking up the scope chain. Returns `false` when
    /// the name is not bound anywhere. Bumps the version of the scope that
    /// actually receives the write.
    pub fn assign(&mut self, name: &str, value: Value) -> bool {
        if self.vars.contains_key(name) {
            self.vars.insert(name.to_string(), value);
            self.bump();
            return true;
        }
        // Recurse through the parent link without holding any borrow
        // across the call; write the (possibly detached) link back so a
        // frozen ancestor's copy-on-write splice persists.
        match self.parent.clone() {
            Some(mut p) => {
                let ok = p.assign(name, value);
                self.parent = Some(p);
                ok
            }
            None => false,
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
        while let Some(link) = cur {
            let parent_opt = match &link {
                EnvLink::Owned(rc) => {
                    let env = rc.borrow();
                    chain.push(env.vars.clone());
                    env.parent.clone()
                }
                EnvLink::Frozen(f) => {
                    chain.push(f.vars.clone());
                    f.parent.clone().map(EnvLink::Frozen)
                }
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

/// Link-level operations shared by owned and frozen scopes (see
/// [`EnvLink`] docs). Reads work identically either way; writes detach
/// frozen links (copy-on-write) so sharing stays sound.
impl Default for EnvLink {
    fn default() -> Self {
        Self::new()
    }
}

impl EnvLink {
    /// Fresh owned root scope.
    pub fn new() -> Self {
        EnvLink::Owned(Rc::new(RefCell::new(Env::new())))
    }

    /// Stable identity for cache keys: the allocation address. Frozen
    /// `Arc`s stay alive while pinned by caches/workers, so reuse-after-
    /// free false hits are impossible (same discipline as the old `Rc`
    /// keys).
    pub fn id(&self) -> usize {
        match self {
            EnvLink::Owned(rc) => Rc::as_ptr(rc) as *const () as usize,
            EnvLink::Frozen(f) => Arc::as_ptr(f) as *const () as usize,
        }
    }

    /// Pointer equality between links.
    pub fn ptr_eq(a: &EnvLink, b: &EnvLink) -> bool {
        match (a, b) {
            (EnvLink::Owned(x), EnvLink::Owned(y)) => Rc::ptr_eq(x, y),
            (EnvLink::Frozen(x), EnvLink::Frozen(y)) => Arc::ptr_eq(x, y),
            _ => false,
        }
    }

    pub fn is_frozen(&self) -> bool {
        matches!(self, EnvLink::Frozen(_))
    }

    /// Read a binding, leaf-first (owned borrows, frozen reads direct).
    pub fn get(&self, name: &str) -> Option<Value> {
        match self {
            EnvLink::Owned(rc) => rc.borrow().get(name),
            EnvLink::Frozen(f) => f.get(name),
        }
    }

    /// Read from this scope only (no parent walk).
    pub fn get_local(&self, name: &str) -> Option<Value> {
        match self {
            EnvLink::Owned(rc) => rc.borrow().get_local(name),
            EnvLink::Frozen(f) => f.vars.get(name).cloned(),
        }
    }

    /// Sorted names bound in this scope only (no parent walk).
    pub fn local_names(&self) -> Vec<String> {
        let mut names: Vec<String> = match self {
            EnvLink::Owned(rc) => rc.borrow().vars.keys().cloned().collect(),
            EnvLink::Frozen(f) => f.vars.keys().cloned().collect(),
        };
        names.sort();
        names
    }

    /// Reset for shell-pool reuse: clear bindings (retain table
    /// capacity), drop the parent link, bump the version (pooled shells
    /// reuse addresses — the version change defeats any stale shape-key
    /// match). Only meaningful on owned links; frozen links detach first
    /// (defense only — pooled shells are always owned by construction).
    pub fn reset_shell(&mut self) {
        if self.is_frozen() {
            *self = self.detached_owned();
        }
        match self {
            EnvLink::Owned(rc) => {
                let mut borrowed = rc.borrow_mut();
                borrowed.vars.clear();
                borrowed.parent = None;
                borrowed.bump();
            }
            EnvLink::Frozen(_) => unreachable!("reset_shell detaches frozen links first"),
        }
    }

    /// This scope's binding count (for shape keys).
    pub fn len(&self) -> usize {
        match self {
            EnvLink::Owned(rc) => rc.borrow().vars.len(),
            EnvLink::Frozen(f) => f.vars.len(),
        }
    }

    /// Whether this scope binds nothing.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Parent link, if any.
    pub fn parent_link(&self) -> Option<EnvLink> {
        match self {
            EnvLink::Owned(rc) => rc.borrow().parent.clone(),
            EnvLink::Frozen(f) => f.parent.clone().map(EnvLink::Frozen),
        }
    }

    /// Replace the parent link (used when assembling worker chains:
    /// fresh owned leaf + frozen shared parent). Detaches frozen links
    /// first so the write always lands in private storage.
    pub fn set_parent(&mut self, parent: Option<EnvLink>) {
        if self.is_frozen() {
            *self = self.detached_owned();
        }
        match self {
            EnvLink::Owned(rc) => rc.borrow_mut().parent = parent,
            EnvLink::Frozen(_) => unreachable!("set_parent detaches frozen links first"),
        }
    }

    /// Define a binding in this scope. On a frozen link this detaches
    /// first (clone-on-write): the link becomes an owned scope carrying
    /// a copy of the frozen map, then defines normally. Spawner chains
    /// never contain frozen links, so this path runs only for worker
    /// chains sharing frozen ancestors.
    pub fn define(&mut self, name: &str, value: Value) {
        if self.is_frozen() {
            *self = self.detached_owned();
        }
        match self {
            EnvLink::Owned(rc) => rc.borrow_mut().define(name, value),
            EnvLink::Frozen(_) => unreachable!("define detaches frozen links first"),
        }
    }

    /// Mutable access to a binding in this scope (no parent fallthrough).
    /// Detaches frozen links first (see `define`). No callers in the hot
    /// path; kept for API completeness.
    pub fn get_mut(&mut self, name: &str) -> Option<std::cell::RefMut<'_, Value>> {
        if self.is_frozen() {
            *self = self.detached_owned();
        }
        match self {
            EnvLink::Owned(rc) => {
                std::cell::RefMut::filter_map(rc.borrow_mut(), |env| env.get_mut(name)).ok()
            }
            EnvLink::Frozen(_) => unreachable!("get_mut detaches frozen links first"),
        }
    }

    /// Assign to a binding, leaf-first. Frozen holders detach (a private
    /// owned copy is spliced at the same position), so a worker assigning
    /// to a shared ancestor never disturbs the spawner or sibling
    /// workers. Returns `false` when unbound anywhere.
    ///
    /// Implemented as a rebuilding recursion: each level returns its
    /// (possibly detached) replacement link, and owned levels splice it
    /// back into their parent slot — so a detach deep in a frozen chain
    /// propagates all the way to the caller's link.
    pub fn assign(&mut self, name: &str, value: Value) -> bool {
        let (ok, replacement) = Self::assign_rec(self, name, value);
        if ok {
            *self = replacement;
        }
        ok
    }

    /// Rebuilding assign recursion (see [`EnvLink::assign`]).
    fn assign_rec(link: &EnvLink, name: &str, value: Value) -> (bool, EnvLink) {
        match link {
            EnvLink::Owned(rc) => {
                if rc.borrow().vars.contains_key(name) {
                    rc.borrow_mut().define(name, value);
                    (true, link.clone())
                } else {
                    let parent = rc.borrow().parent.clone();
                    match parent {
                        Some(p) => {
                            let (ok, np) = Self::assign_rec(&p, name, value);
                            if ok {
                                rc.borrow_mut().parent = Some(np);
                            }
                            (ok, link.clone())
                        }
                        None => (false, link.clone()),
                    }
                }
            }
            EnvLink::Frozen(f) => {
                if f.vars.contains_key(name) {
                    // Detach: private owned copy carrying the write,
                    // spliced in by the caller.
                    let mut owned = Env {
                        vars: f.vars.clone(),
                        parent: f.parent.clone().map(EnvLink::Frozen),
                        version: std::cell::Cell::new(0),
                    };
                    owned.define(name, value);
                    (true, EnvLink::Owned(Rc::new(RefCell::new(owned))))
                } else {
                    match &f.parent {
                        Some(fp) => {
                            let wrapped = EnvLink::Frozen(Arc::clone(fp));
                            let (ok, np) = Self::assign_rec(&wrapped, name, value);
                            (ok, if ok { np } else { link.clone() })
                        }
                        None => (false, link.clone()),
                    }
                }
            }
        }
    }

    /// Merge a child's locals into this scope (or-pattern bindings).
    /// Detaches frozen links first (see `define`).
    pub fn absorb_locals(&mut self, child: &EnvLink) {
        if self.is_frozen() {
            *self = self.detached_owned();
        }
        match (self, child) {
            (EnvLink::Owned(rc), EnvLink::Owned(crc)) => {
                let vars = crc.borrow().vars.clone();
                if !vars.is_empty() {
                    let mut this = rc.borrow_mut();
                    for (k, v) in vars {
                        this.vars.insert(k, v);
                    }
                    this.bump();
                }
            }
            (EnvLink::Owned(rc), EnvLink::Frozen(f)) => {
                let mut this = rc.borrow_mut();
                if !f.vars.is_empty() {
                    for (k, v) in f.vars.iter() {
                        this.define(k, v.clone());
                    }
                }
            }
            (EnvLink::Frozen(_), _) => unreachable!("absorb_locals detaches first"),
        }
    }

    /// Clone this scope's map into a fresh owned link with the same
    /// parent (copy-on-write detach). Values are shared by `Clone`
    /// (same as snapshot clones — `Value: Clone` is the transfer
    /// discipline everywhere).
    fn detached_owned(&self) -> EnvLink {
        let (vars, parent) = match self {
            EnvLink::Owned(rc) => {
                let borrowed = rc.borrow();
                (borrowed.vars.clone(), borrowed.parent.clone())
            }
            EnvLink::Frozen(f) => (f.vars.clone(), f.parent.clone().map(EnvLink::Frozen)),
        };
        EnvLink::Owned(Rc::new(RefCell::new(Env {
            vars,
            parent,
            version: std::cell::Cell::new(0),
        })))
    }

    /// Freeze an owned chain into a shared-frozen root: every owned scope
    /// becomes an immutable `Arc` map; already-frozen links are reused by
    /// pointer (no re-clone). The spawner's chain is untouched — the
    /// frozen copy is independent, so later spawner writes can never
    /// disturb workers holding it (point-in-time semantics, exactly like
    /// the old deep-clone snapshots).
    pub fn frozen_scope(link: &EnvLink) -> Arc<FrozenScope> {
        match link {
            EnvLink::Frozen(f) => Arc::clone(f),
            EnvLink::Owned(rc) => {
                let (vars, parent) = {
                    let borrowed = rc.borrow();
                    (borrowed.vars.clone(), borrowed.parent.clone())
                };
                Arc::new(FrozenScope {
                    vars,
                    parent: parent.as_ref().map(Self::frozen_scope),
                })
            }
        }
    }

    /// [`frozen_scope`](Self::frozen_scope) as a link (for chain slots).
    pub fn frozen_view(link: &EnvLink) -> EnvLink {
        EnvLink::Frozen(Self::frozen_scope(link))
    }

    /// Flatten the chain root→leaf (both link kinds).
    pub fn flatten(&self) -> HashMap<String, Value> {
        match self {
            EnvLink::Owned(rc) => rc.borrow().flatten(),
            EnvLink::Frozen(f) => {
                let mut chain: Vec<HashMap<String, Value>> = Vec::new();
                chain.push(f.vars.clone());
                let mut cur: Option<EnvLink> = f.parent.clone().map(EnvLink::Frozen);
                while let Some(link) = cur {
                    match &link {
                        EnvLink::Owned(rc) => {
                            let env = rc.borrow();
                            chain.push(env.vars.clone());
                            cur = env.parent.clone();
                        }
                        EnvLink::Frozen(ff) => {
                            chain.push(ff.vars.clone());
                            cur = ff.parent.clone().map(EnvLink::Frozen);
                        }
                    }
                }
                let mut flat = HashMap::with_capacity(chain.iter().map(|l| l.len()).sum());
                for mut layer in chain.into_iter().rev() {
                    for (k, v) in layer.drain() {
                        flat.insert(k, v);
                    }
                }
                flat
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoped_lookup_falls_through() {
        let mut outer = EnvLink::new();
        outer.define("a", Value::Int(1));
        let mut inner = Env::with_parent(&outer);
        inner.define("b", Value::Int(2));
        assert_eq!(inner.get("a"), Some(Value::Int(1)));
        assert_eq!(inner.get("b"), Some(Value::Int(2)));
        // Parent doesn't see child bindings.
        assert_eq!(outer.get("b"), None);
    }

    #[test]
    fn assignment_propagates_to_parent() {
        let mut outer = EnvLink::new();
        outer.define("a", Value::Int(1));
        let mut inner = Env::with_parent(&outer);
        assert!(inner.assign("a", Value::Int(99)));
        assert_eq!(outer.get("a"), Some(Value::Int(99)));
    }

    #[test]
    fn flatten_collects_root_to_leaf() {
        let mut root = EnvLink::new();
        root.define("a", Value::Int(1));
        root.define("b", Value::Int(10));
        let mut mid = Env::with_parent(&root);
        mid.define("b", Value::Int(2));
        mid.define("c", Value::Int(3));
        let mut leaf = Env::with_parent(&mid);
        leaf.define("c", Value::Int(30));
        leaf.define("d", Value::Int(4));
        let flat = leaf.flatten();
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

    #[test]
    fn frozen_view_shares_reads() {
        let mut root = EnvLink::new();
        root.define("a", Value::Int(1));
        let frozen = EnvLink::frozen_view(&root);
        assert!(frozen.is_frozen());
        // Original keeps working; frozen sees the point-in-time state.
        root.define("b", Value::Int(2));
        assert_eq!(frozen.get("a"), Some(Value::Int(1)));
        assert_eq!(frozen.get("b"), None);
        assert_eq!(root.get("b"), Some(Value::Int(2)));
    }

    #[test]
    fn frozen_assign_detaches() {
        let mut root = EnvLink::new();
        root.define("a", Value::Int(1));
        let mut frozen = EnvLink::frozen_view(&root);
        // Writing through a frozen link detaches: the frozen view's
        // siblings (and the spawner) never observe it.
        assert!(frozen.assign("a", Value::Int(99)));
        assert_eq!(frozen.get("a"), Some(Value::Int(99)));
        assert_eq!(root.get("a"), Some(Value::Int(1)));
        // And the spawner assigning later doesn't disturb the worker.
        root.assign("a", Value::Int(7));
        assert_eq!(frozen.get("a"), Some(Value::Int(99)));
        assert_eq!(root.get("a"), Some(Value::Int(7)));
    }

    #[test]
    fn frozen_view_reuses_frozen_links() {
        let mut root = EnvLink::new();
        root.define("a", Value::Int(1));
        let frozen = EnvLink::frozen_view(&root);
        // Re-freezing an already-frozen link reuses the pointer (no
        // re-clone): identity is preserved.
        let again = EnvLink::frozen_view(&frozen);
        assert!(EnvLink::ptr_eq(&frozen, &again));
    }

    #[test]
    fn frozen_links_are_sync() {
        // Compile-time proof of the thread-safety claim: frozen scopes
        // (the only scope data that crosses threads) are Send + Sync.
        // Owned links stay thread-local — sharing them would be a
        // compile error, which is exactly the guarantee we want.
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<FrozenScope>();
    }
}
