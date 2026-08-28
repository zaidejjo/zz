//! Struct registration and checking.

use crate::checker::Checker;
use zz_frontend::ast::Stmt;

impl Checker {
    pub(crate) fn collect_struct(&mut self, stmt: &Stmt) {
        let (name, fields) = match stmt {
            Stmt::Struct { name, fields, .. } => (name, fields),
            _ => unreachable!(),
        };
        let gens = self.current_generics.clone();
        let sig_fields = fields
            .iter()
            .map(|(fname, fty)| (fname.name.clone(), self.ast_to_type(fty, &gens)))
            .collect();
        let full_name = name.join(".");
        self.structs
            .insert(full_name, crate::checker::StructSig { fields: sig_fields });
    }
}
