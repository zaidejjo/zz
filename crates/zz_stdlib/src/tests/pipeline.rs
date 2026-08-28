use super::run;
use zz_runtime::Value;

#[test]
fn pipe_with_stdlib() {
    let v =
        run("import std.str\nimport std.vec\n\"a,b,c\" |> str.split(\",\") |> vec.len()").unwrap();
    assert_eq!(v, Value::Int(3));
}
