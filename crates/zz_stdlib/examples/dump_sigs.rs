//! Dump every stdlib signature as TSV for `scripts/gen-stdlib-ref.py`.
//!
//! One line per function: `name \t generics(comma) \t params(name: type,...; `=`
//! marks defaulted) \t return`. Types render through the checker's `Display`,
//! so docs can never drift from what `zz check` enforces.

fn main() {
    let mut fns: Vec<_> = zz_stdlib::stdlib_funcs().into_iter().collect();
    fns.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, sig) in &fns {
        let params: Vec<String> = sig
            .params
            .iter()
            .zip(sig.has_default.iter().chain(std::iter::repeat(&false)))
            .map(|((pname, ptype), is_default)| {
                if *is_default {
                    format!("{pname}: {ptype} = …")
                } else {
                    format!("{pname}: {ptype}")
                }
            })
            .collect();
        println!(
            "{}\t{}\t{}\t{}",
            name,
            sig.generics.join(","),
            params.join(", "),
            sig.ret
        );
    }
    let mut consts: Vec<_> = zz_stdlib::stdlib_consts().into_iter().collect();
    consts.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, value) in &consts {
        println!("{name}\t\tvalue = {value}\tfloat");
    }
}
