//! `zz-dap`: Debug Adapter Protocol server for ZZ, over stdio.
//!
//! Editors launch this binary and speak DAP (`Content-Length` framing):
//! `initialize` → `launch {program}` → `setBreakpoints` →
//! `configurationDone` starts the worker thread; suspend/resume cycles
//! surface as `stopped`/`terminated` events. One thread only (`main`).

mod adapter;
mod protocol;

use std::io::{BufReader, Write};
use std::sync::mpsc;

use crate::adapter::{read_message, write_message, Adapter};
use crate::protocol::Request;

fn main() {
    let mut adapter = Adapter::new();
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    // Reader thread: blocking stdin reads must not stall event
    // delivery. Bounded handoff to the dispatch loop below.
    let (msg_tx, msg_rx) = mpsc::channel::<serde_json::Value>();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdin.lock());
        while let Some(msg) = read_message(&mut reader) {
            if msg_tx.send(msg).is_err() {
                break;
            }
        }
    });

    let mut out = stdout.lock();
    let mut running = true;
    while running {
        // Worker events first (stopped/terminated beat the next request).
        while let Some(event) = adapter.poll_event() {
            match event {
                adapter::WorkerEvent::Stopped { reason } => {
                    adapter.note_stopped();
                    write_message(&mut out, &as_json(adapter.stopped_event(reason)));
                }
                adapter::WorkerEvent::Terminated => {
                    adapter.note_running();
                    write_message(&mut out, &as_json(adapter.terminated_event()));
                }
            }
        }
        match msg_rx.try_recv() {
            Ok(msg) => {
                let req: Request = match serde_json::from_value(msg) {
                    Ok(r) => r,
                    Err(_) => continue,
                };
                let is_disconnect = req.command == "disconnect";
                let resp = adapter.handle(&req);
                write_message(&mut out, &as_json_ref(&resp));
                if req.command == "initialize" {
                    write_message(&mut out, &as_json(adapter.initialized_event()));
                }
                if is_disconnect {
                    // Drain a terminal event the worker may have posted.
                    while let Some(event) = adapter.poll_event() {
                        match event {
                            adapter::WorkerEvent::Stopped { reason } => {
                                write_message(&mut out, &as_json(adapter.stopped_event(reason)));
                            }
                            adapter::WorkerEvent::Terminated => {
                                write_message(&mut out, &as_json(adapter.terminated_event()));
                            }
                        }
                    }
                    running = false;
                }
            }
            Err(mpsc::TryRecvError::Disconnected) => break,
            Err(mpsc::TryRecvError::Empty) => {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
    }
    let _ = out.flush();
}

fn as_json<T: serde::Serialize>(value: T) -> serde_json::Value {
    serde_json::to_value(value).unwrap_or(serde_json::Value::Null)
}

fn as_json_ref<T: serde::Serialize>(value: &T) -> serde_json::Value {
    serde_json::to_value(value).unwrap_or(serde_json::Value::Null)
}
