//! End-to-end DAP session against the real `zz-dap` binary: launch a
//! fixture, break, inspect the stack and a variable, continue to
//! termination. Drives framed stdio like an editor would.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

struct Session {
    child: std::process::Child,
    reader: BufReader<std::process::ChildStdout>,
    seq: u64,
    received: Vec<serde_json::Value>,
}

impl Session {
    fn spawn(fixture: &std::path::Path) -> Self {
        let bin = env!("CARGO_BIN_EXE_zz-dap");
        let mut child = Command::new(bin)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn zz-dap");
        let reader = BufReader::new(child.stdout.take().expect("stdout"));
        let mut session = Self {
            child,
            reader,
            seq: 0,
            received: Vec::new(),
        };
        session.request("initialize", serde_json::json!({ "adapterID": "zz" }));
        session.expect_event("initialized");
        session.request(
            "launch",
            serde_json::json!({ "program": fixture.to_str().unwrap() }),
        );
        session
    }

    fn send(&mut self, msg: &serde_json::Value) {
        let body = serde_json::to_string(msg).unwrap();
        let stdin = self.child.stdin.as_mut().expect("stdin");
        write!(stdin, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        stdin.flush().unwrap();
    }

    fn recv(&mut self) -> serde_json::Value {
        // Skip any events, stashing them for later assertions.
        loop {
            let msg = read_message(&mut self.reader).expect("message");
            if msg.get("type").and_then(|t| t.as_str()) == Some("event") {
                self.received.push(msg);
                continue;
            }
            return msg;
        }
    }

    fn request(&mut self, command: &str, arguments: serde_json::Value) -> serde_json::Value {
        self.seq += 1;
        let seq = self.seq;
        self.send(&serde_json::json!({
            "seq": seq,
            "type": "request",
            "command": command,
            "arguments": arguments,
        }));
        let resp = self.recv();
        assert_eq!(resp["request_seq"], seq, "response seq for {command}");
        assert_eq!(resp["command"], command);
        resp
    }

    fn expect_event(&mut self, name: &str) -> serde_json::Value {
        // Drain stashed first.
        if let Some(i) = self
            .received
            .iter()
            .position(|m| m.get("event").and_then(|e| e.as_str()) == Some(name))
        {
            return self.received.remove(i);
        }
        loop {
            let msg = read_message(&mut self.reader).expect("message");
            if msg.get("event").and_then(|e| e.as_str()) == Some(name) {
                return msg;
            }
            if msg.get("type").and_then(|t| t.as_str()) == Some("event") {
                self.received.push(msg);
            }
        }
    }

    fn finish(mut self) {
        let _ = self.child.stdin.take();
        let status = self.child.wait().expect("exit");
        assert!(status.success(), "adapter exited cleanly");
    }
}

fn read_message<R: BufRead>(reader: &mut R) -> Option<serde_json::Value> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim();
        if line.is_empty() {
            break;
        }
        if let Some(rest) = line.strip_prefix("Content-Length:") {
            content_length = rest.trim().parse().ok();
        }
    }
    let mut buf = vec![0u8; content_length?];
    reader.read_exact(&mut buf).ok()?;
    serde_json::from_slice(&buf).ok()
}

fn fixture(name: &str, content: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("zz-dap-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, content).unwrap();
    path
}

#[test]
fn full_debug_session() {
    let prog = fixture("prog.zz", "a := 10\nb := a + 5\nc := b * 2\nc\n");
    let mut s = Session::spawn(&prog);

    let resp = s.request(
        "setBreakpoints",
        serde_json::json!({
            "source": { "path": prog.to_str().unwrap() },
            "breakpoints": [{ "line": 2 }],
        }),
    );
    assert!(resp["success"].as_bool().unwrap());
    assert_eq!(resp["body"]["breakpoints"][0]["line"], 2);

    s.request("configurationDone", serde_json::json!({}));
    let stopped = s.expect_event("stopped");
    assert_eq!(stopped["body"]["reason"], "breakpoint");
    assert_eq!(stopped["body"]["threadId"], 1);

    let resp = s.request("threads", serde_json::json!({}));
    assert_eq!(resp["body"]["threads"][0]["name"], "main");

    let resp = s.request("stackTrace", serde_json::json!({ "threadId": 1 }));
    let frames = resp["body"]["stackFrames"].as_array().unwrap();
    assert!(!frames.is_empty());
    assert_eq!(frames[0]["line"], 2);

    let frame_id = frames[0]["id"].as_u64().unwrap();
    let resp = s.request("scopes", serde_json::json!({ "frameId": frame_id }));
    let scopes = resp["body"]["scopes"].as_array().unwrap();
    assert_eq!(scopes[0]["name"], "Locals");
    let var_ref = scopes[0]["variablesReference"].as_u64().unwrap();

    let resp = s.request(
        "variables",
        serde_json::json!({ "variablesReference": var_ref }),
    );
    let vars = resp["body"]["variables"].as_array().unwrap();
    let a = vars.iter().find(|v| v["name"] == "a").expect("var a");
    assert_eq!(a["value"], "10");
    assert_eq!(a["type"], "int");

    let resp = s.request("continue", serde_json::json!({ "threadId": 1 }));
    assert!(resp["success"].as_bool().unwrap());
    s.expect_event("terminated");

    s.request("disconnect", serde_json::json!({ "restart": false }));
    s.finish();
}

#[test]
fn step_over_advances_line() {
    let prog = fixture("step.zz", "a := 10\nb := a + 5\nc := b * 2\nc\n");
    let mut s = Session::spawn(&prog);

    s.request(
        "setBreakpoints",
        serde_json::json!({
            "source": { "path": prog.to_str().unwrap() },
            "breakpoints": [{ "line": 2 }],
        }),
    );
    s.request("configurationDone", serde_json::json!({}));
    let stopped = s.expect_event("stopped");
    assert_eq!(stopped["body"]["reason"], "breakpoint");

    let resp = s.request("next", serde_json::json!({ "threadId": 1 }));
    assert!(resp["success"].as_bool().unwrap());
    let stopped = s.expect_event("stopped");
    assert_eq!(stopped["body"]["reason"], "step");

    let resp = s.request("stackTrace", serde_json::json!({ "threadId": 1 }));
    let frames = resp["body"]["stackFrames"].as_array().unwrap();
    assert_eq!(frames[0]["line"], 3);

    let resp = s.request("continue", serde_json::json!({ "threadId": 1 }));
    assert!(resp["success"].as_bool().unwrap());
    s.expect_event("terminated");

    s.request("disconnect", serde_json::json!({ "restart": false }));
    s.finish();
}
