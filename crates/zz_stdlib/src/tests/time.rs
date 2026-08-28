use super::run;
use zz_runtime::Value;

#[test]
fn time_now_ms() {
    let v = run("import std.time\ntime.now_ms()").unwrap();
    match v {
        Value::Int(ms) => assert!(ms > 0, "now_ms should be positive: {ms}"),
        other => panic!("expected int, got {other}"),
    }
}

#[test]
fn time_sleep_ms() {
    let start = std::time::Instant::now();
    let v = run("import std.time\ntime.sleep_ms(20)").unwrap();
    assert_eq!(v, Value::Unit);
    assert!(
        start.elapsed().as_millis() >= 15,
        "sleep returned too early: {:?}",
        start.elapsed()
    );
}
