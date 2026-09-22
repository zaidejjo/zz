use zz_fmt::FmtConfig;
use std::fs;
fn main() {
    let src = "// header\nx := 1 // trailing\n/// doc line\ny := 2\n";
    let formatted = zz_fmt::format_source(&src, &FmtConfig::default()).unwrap();
    print!("{}", formatted);
}
