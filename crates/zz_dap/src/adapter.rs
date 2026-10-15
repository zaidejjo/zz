//! Session state machine: program loading, the exec thread, and the
//! request handlers behind each DAP request.
//!
//! Threading (actor model): the transport thread never touches runtime
//! types (`Vm`/`Interp` are `!Send`). The exec thread is spawned at
//! launch and owns the running program for life; everything crossing
//! the boundary is plain data — commands in, events and inspection
//! results out. Breakpoint updates flow through the shared session
//! lock and take effect on the next executed op, running or parked.

use std::sync::{mpsc, Arc, Mutex};

use serde_json::json;
use zz_runtime::debug::{DebugSession, StopReason};
use zz_runtime::eval::Interp;
use zz_runtime::value::{Value, YieldReason};
use zz_runtime::vm::{Compiler, Vm};

use crate::protocol::*;

const MAIN_THREAD_ID: u64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExecStatus {
    Idle,
    Running,
    Parked,
    Done,
}

/// Plain-data snapshot of one parked frame for the transport thread.
#[derive(Debug, Clone)]
struct FrameView {
    func: String,
    path: String,
    line: u32,
}

/// Plain-data variable for the transport thread.
#[derive(Debug, Clone)]
struct VarView {
    name: String,
    value: String,
    typ: &'static str,
}

pub enum WorkerEvent {
    Stopped { reason: &'static str },
    Terminated,
}

enum WorkerCmd {
    SetBreakpoints { path: String, lines: Vec<u32> },
    Start,
    Resume,
    Step(zz_runtime::debug::StepMode),
    Inspect(InspectReq, mpsc::Sender<InspectResp>),
    Disconnect,
}

enum InspectReq {
    Frames,
    Vars { frame_idx: usize },
}

enum InspectResp {
    Frames(Vec<FrameView>),
    Vars(Vec<VarView>),
}

/// The adapter: transport-side handles plus the channels to the exec
/// thread. Created per connection (one process serves one session).
pub struct Adapter {
    cmd_tx: Option<mpsc::Sender<WorkerCmd>>,
    event_rx: Option<mpsc::Receiver<WorkerEvent>>,
    exec_handle: Option<std::thread::JoinHandle<()>>,
    seq: u64,
    launched: bool,
    parked: bool,
}

impl Adapter {
    pub fn new() -> Self {
        Self {
            cmd_tx: None,
            event_rx: None,
            exec_handle: None,
            seq: 1,
            launched: false,
            parked: false,
        }
    }

    /// Next outbound message sequence number.
    pub fn next_seq(&mut self) -> u64 {
        let seq = self.seq;
        self.seq += 1;
        seq
    }

    /// Take an event the exec thread posted, if any.
    pub fn poll_event(&self) -> Option<WorkerEvent> {
        self.event_rx.as_ref().and_then(|rx| rx.try_recv().ok())
    }

    /// Handle one client request. Returns the response; run-control
    /// requests additionally poke the exec thread.
    pub fn handle(&mut self, req: &Request) -> Response {
        let seq = self.next_seq();
        match req.command.as_str() {
            "initialize" => {
                let body = json!({ "supportsConfigurationDoneRequest": true });
                Response::ok(seq, req.seq, "initialize", Some(body))
            }
            "launch" => self.on_launch(seq, req),
            "setBreakpoints" => self.on_set_breakpoints(seq, req),
            "configurationDone" => self.on_configuration_done(seq, req),
            "threads" => {
                let body = json!({ "threads": [{ "id": MAIN_THREAD_ID, "name": "main" }] });
                Response::ok(seq, req.seq, "threads", Some(body))
            }
            "stackTrace" => self.on_stack_trace(seq, req),
            "scopes" => self.on_scopes(seq, req),
            "variables" => self.on_variables(seq, req),
            "continue" => self.on_run_control(seq, req, "continue", WorkerCmd::Resume),
            "next" => self.on_run_control(
                seq,
                req,
                "next",
                WorkerCmd::Step(zz_runtime::debug::StepMode::Over),
            ),
            "stepIn" => self.on_run_control(
                seq,
                req,
                "stepIn",
                WorkerCmd::Step(zz_runtime::debug::StepMode::Into),
            ),
            "stepOut" => self.on_run_control(
                seq,
                req,
                "stepOut",
                WorkerCmd::Step(zz_runtime::debug::StepMode::Out),
            ),
            "disconnect" => self.on_disconnect(seq, req),
            other => Response::err(
                seq,
                req.seq,
                other,
                format!("unsupported command `{other}`"),
            ),
        }
    }

    /// The `initialized` event, sent after the initialize response.
    pub fn initialized_event(&mut self) -> Event {
        Event {
            seq: self.next_seq(),
            typ: "event",
            event: "initialized",
            body: None,
        }
    }

    pub fn stopped_event(&mut self, reason: &'static str) -> Event {
        Event {
            seq: self.next_seq(),
            typ: "event",
            event: "stopped",
            body: Some(json!({ "reason": reason, "threadId": MAIN_THREAD_ID })),
        }
    }

    pub fn terminated_event(&mut self) -> Event {
        Event {
            seq: self.next_seq(),
            typ: "event",
            event: "terminated",
            body: None,
        }
    }

    fn send_cmd(&self, cmd: WorkerCmd) -> bool {
        self.cmd_tx
            .as_ref()
            .map(|tx| tx.send(cmd).is_ok())
            .unwrap_or(false)
    }

    fn on_launch(&mut self, seq: u64, req: &Request) -> Response {
        let args: LaunchArgs = match req
            .arguments
            .clone()
            .and_then(|v| serde_json::from_value(v).ok())
        {
            Some(a) => a,
            None => return Response::err(seq, req.seq, "launch", "missing `program`".into()),
        };
        let source = match std::fs::read_to_string(&args.program) {
            Ok(s) => s,
            Err(e) => {
                return Response::err(
                    seq,
                    req.seq,
                    "launch",
                    format!("cannot read {}: {e}", args.program),
                )
            }
        };
        if let Err(e) = check_parses(&args.program, &source) {
            return Response::err(seq, req.seq, "launch", e);
        }
        // The exec thread owns all runtime state from birth (it is
        // !Send); the source travels as plain text.
        let (cmd_tx, cmd_rx) = mpsc::channel::<WorkerCmd>();
        let (event_tx, event_rx) = mpsc::channel::<WorkerEvent>();
        self.cmd_tx = Some(cmd_tx);
        self.event_rx = Some(event_rx);
        let program = args.program.clone();
        let handle = std::thread::spawn(move || exec_main(program, source, cmd_rx, event_tx));
        self.exec_handle = Some(handle);
        self.launched = true;
        Response::ok(seq, req.seq, "launch", None)
    }

    fn on_set_breakpoints(&mut self, seq: u64, req: &Request) -> Response {
        let args: SetBreakpointsArgs = match req
            .arguments
            .clone()
            .and_then(|v| serde_json::from_value(v).ok())
        {
            Some(a) => a,
            None => {
                return Response::err(seq, req.seq, "setBreakpoints", "bad arguments".into());
            }
        };
        if !self.launched {
            return Response::err(seq, req.seq, "setBreakpoints", "no program launched".into());
        }
        let mut path = args.source.path.unwrap_or_default();
        if let Some(stripped) = path.strip_prefix("file://") {
            path = stripped.to_string();
        }
        let lines: Vec<u32> = args.breakpoints.iter().map(|b| b.line).collect();
        let shown: Vec<serde_json::Value> = lines
            .iter()
            .map(|line| json!({ "verified": true, "line": line }))
            .collect();
        self.send_cmd(WorkerCmd::SetBreakpoints { path, lines });
        Response::ok(
            seq,
            req.seq,
            "setBreakpoints",
            Some(json!({ "breakpoints": shown })),
        )
    }

    fn on_configuration_done(&mut self, seq: u64, req: &Request) -> Response {
        if !self.launched {
            return Response::err(seq, req.seq, "configurationDone", "nothing launched".into());
        }
        self.send_cmd(WorkerCmd::Start);
        Response::ok(seq, req.seq, "configurationDone", None)
    }

    fn inspect(&self, req: InspectReq) -> Option<InspectResp> {
        let (tx, rx) = mpsc::channel::<InspectResp>();
        if !self.send_cmd(WorkerCmd::Inspect(req, tx)) {
            return None;
        }
        rx.recv_timeout(std::time::Duration::from_secs(5)).ok()
    }

    fn parked(&self) -> bool {
        self.parked
    }

    /// Record a stop/resume so inspection requests gate correctly.
    pub fn note_stopped(&mut self) {
        self.parked = true;
    }

    pub fn note_running(&mut self) {
        self.parked = false;
    }

    fn on_stack_trace(&mut self, seq: u64, req: &Request) -> Response {
        let args: StackTraceArgs = match req
            .arguments
            .clone()
            .and_then(|v| serde_json::from_value(v).ok())
        {
            Some(a) => a,
            None => return Response::err(seq, req.seq, "stackTrace", "bad arguments".into()),
        };
        if args.thread_id != MAIN_THREAD_ID {
            return Response::err(seq, req.seq, "stackTrace", "unknown thread".into());
        }
        if !self.parked() {
            return Response::err(seq, req.seq, "stackTrace", "not stopped".into());
        }
        match self.inspect(InspectReq::Frames) {
            Some(InspectResp::Frames(frames)) => {
                let wire: Vec<serde_json::Value> = frames
                    .iter()
                    .enumerate()
                    .map(|(i, f)| {
                        json!({
                            "id": i as u64,
                            "name": f.func,
                            "line": f.line,
                            "column": 1,
                            "source": { "path": f.path },
                        })
                    })
                    .collect();
                Response::ok(
                    seq,
                    req.seq,
                    "stackTrace",
                    Some(json!({ "stackFrames": wire, "totalFrames": wire.len() })),
                )
            }
            _ => Response::err(seq, req.seq, "stackTrace", "not stopped".into()),
        }
    }

    fn on_scopes(&mut self, seq: u64, req: &Request) -> Response {
        if !self.parked() {
            return Response::err(seq, req.seq, "scopes", "not stopped".into());
        }
        let args: ScopesArgs = match req
            .arguments
            .clone()
            .and_then(|v| serde_json::from_value(v).ok())
        {
            Some(a) => a,
            None => return Response::err(seq, req.seq, "scopes", "bad arguments".into()),
        };
        let body = json!({ "scopes": [{
            "name": "Locals",
            "variablesReference": 1000 + args.frame_id,
            "expensive": false,
        }] });
        Response::ok(seq, req.seq, "scopes", Some(body))
    }

    fn on_variables(&mut self, seq: u64, req: &Request) -> Response {
        if !self.parked() {
            return Response::err(seq, req.seq, "variables", "not stopped".into());
        }
        let args: VariablesArgs = match req
            .arguments
            .clone()
            .and_then(|v| serde_json::from_value(v).ok())
        {
            Some(a) => a,
            None => return Response::err(seq, req.seq, "variables", "bad arguments".into()),
        };
        let frame_idx = args.variables_reference.saturating_sub(1000) as usize;
        match self.inspect(InspectReq::Vars { frame_idx }) {
            Some(InspectResp::Vars(vars)) => {
                let wire: Vec<serde_json::Value> = vars
                    .iter()
                    .map(|v| {
                        json!({
                            "name": v.name,
                            "value": v.value,
                            "type": v.typ,
                            "variablesReference": 0,
                        })
                    })
                    .collect();
                Response::ok(
                    seq,
                    req.seq,
                    "variables",
                    Some(json!({ "variables": wire })),
                )
            }
            _ => Response::err(seq, req.seq, "variables", "not stopped".into()),
        }
    }

    fn on_run_control(&mut self, seq: u64, req: &Request, name: &str, cmd: WorkerCmd) -> Response {
        if !self.launched {
            return Response::err(seq, req.seq, name, "nothing launched".into());
        }
        self.send_cmd(cmd);
        self.note_running();
        if name == "continue" {
            Response::ok(
                seq,
                req.seq,
                name,
                Some(json!({ "allThreadsContinued": true })),
            )
        } else {
            Response::ok(seq, req.seq, name, None)
        }
    }

    fn on_disconnect(&mut self, seq: u64, req: &Request) -> Response {
        if let Some(tx) = self.cmd_tx.take() {
            let _ = tx.send(WorkerCmd::Disconnect);
        }
        if let Some(handle) = self.exec_handle.take() {
            let _ = handle.join();
        }
        self.launched = false;
        Response::ok(seq, req.seq, "disconnect", None)
    }
}

fn check_parses(program: &str, source: &str) -> Result<(), String> {
    let parsed = zz_frontend::parse(source);
    if parsed.errors.is_empty() {
        Ok(())
    } else {
        Err(format!("parse errors in {program}: {:?}", parsed.errors))
    }
}

fn value_type_name(value: &Value) -> &'static str {
    match value {
        Value::Int(_) => "int",
        Value::Float(_) => "float",
        Value::Bool(_) => "bool",
        Value::Str(_) => "str",
        Value::Unit => "unit",
        _ => "value",
    }
}

/// The exec thread: builds everything locally, runs, and parks.
/// Inspection requests are answered inline whenever parked (or idle);
/// run-control commands drive the execution loop.
fn exec_main(
    program_path: String,
    source: String,
    cmd_rx: mpsc::Receiver<WorkerCmd>,
    event_tx: mpsc::Sender<WorkerEvent>,
) {
    let parsed = zz_frontend::parse(&source);
    let mut chunk = Compiler::compile_program(&parsed.program);
    zz_runtime::coverage::stamp_chunk_tree(
        &mut chunk,
        &std::sync::Arc::from(program_path.as_str()),
    );
    let chunk = std::sync::Arc::new(chunk);

    let mut session = DebugSession::new();
    session.add_source(program_path.clone(), &source);
    let session = Arc::new(std::sync::Mutex::new(session));

    let interp = Interp::with_natives(zz_stdlib::natives::stdlib_natives());
    let mut vm = Vm::new();
    vm.debug = Some(session.clone());

    let mut prog = ExecProg {
        vm,
        interp,
        chunk,
        session,
        program_path,
        started: false,
        status: ExecStatus::Idle,
    };

    loop {
        match cmd_rx.recv() {
            Ok(WorkerCmd::Disconnect) | Err(_) => return,
            Ok(cmd) => {
                if exec_command(&mut prog, cmd, &event_tx) {
                    return;
                }
            }
        }
    }
}

struct ExecProg {
    vm: Vm,
    interp: Interp,
    chunk: std::sync::Arc<zz_runtime::vm::Chunk>,
    session: Arc<Mutex<DebugSession>>,
    program_path: String,
    started: bool,
    status: ExecStatus,
}

/// Handle one command. Returns true when the thread should exit.
fn exec_command(prog: &mut ExecProg, cmd: WorkerCmd, event_tx: &mpsc::Sender<WorkerEvent>) -> bool {
    match cmd {
        WorkerCmd::Disconnect => return true,
        WorkerCmd::SetBreakpoints { path, lines } => {
            let mut session = prog.session.lock().unwrap();
            session.retain_file(&path);
            for line in lines {
                session.add_breakpoint(path.clone(), line);
            }
        }
        WorkerCmd::Start => {
            if prog.status == ExecStatus::Idle {
                prog.status = ExecStatus::Running;
                run_until_park(prog, event_tx);
            }
        }
        WorkerCmd::Resume | WorkerCmd::Step(_) => {
            if prog.status == ExecStatus::Parked {
                if let WorkerCmd::Step(mode) = cmd {
                    arm_step(prog, mode);
                }
                prog.status = ExecStatus::Running;
                run_until_park(prog, event_tx);
            }
        }
        WorkerCmd::Inspect(req, resp) => {
            let _ = resp.send(inspect(prog, req));
        }
    }
    false
}

/// Arm a step from the current parked position.
fn arm_step(prog: &mut ExecProg, mode: zz_runtime::debug::StepMode) {
    let depth = prog.vm.debug_frames().len();
    if let Some((func, label, span)) = prog.vm.debug_frames().first() {
        let _ = func;
        if let Some(label) = label {
            let mut session = prog.session.lock().unwrap();
            let line = session.line_for(label, span.start).unwrap_or(1);
            session.start_step(mode, label.clone(), line, depth);
        }
    }
}

/// Run until suspension or completion, then report. The first call
/// pushes the frame (`run_chunk`); every later call continues it
/// (`resume_chunk`) — re-running `run_chunk` would restart the program
/// from op 0 on every continue.
fn run_until_park(prog: &mut ExecProg, event_tx: &mpsc::Sender<WorkerEvent>) {
    let outcome = if !prog.started {
        prog.started = true;
        prog.vm.run_chunk(&prog.chunk, &mut prog.interp)
    } else {
        prog.vm.resume_chunk(&mut prog.interp)
    };
    match outcome {
        Ok(zz_runtime::runtime::Flow::Yield(YieldReason::DebugBreak { .. })) => {
            prog.status = ExecStatus::Parked;
            let reason = match prog.session.lock().unwrap().stop_reason {
                StopReason::Breakpoint => "breakpoint",
                StopReason::Step => "step",
            };
            let _ = event_tx.send(WorkerEvent::Stopped { reason });
        }
        Ok(_) | Err(_) => {
            prog.status = ExecStatus::Done;
            let _ = event_tx.send(WorkerEvent::Terminated);
        }
    }
}

fn inspect(prog: &mut ExecProg, req: InspectReq) -> InspectResp {
    match req {
        InspectReq::Frames => {
            let session = prog.session.lock().unwrap();
            let frames = prog
                .vm
                .debug_frames()
                .into_iter()
                .map(|(func, label, span)| {
                    let (path, line) = match &label {
                        Some(label) => (
                            label.clone(),
                            session.line_for(label, span.start).unwrap_or(1),
                        ),
                        None => (prog.program_path.clone(), 1),
                    };
                    FrameView {
                        func: if func.is_empty() {
                            "main".to_string()
                        } else {
                            func
                        },
                        path,
                        line,
                    }
                })
                .collect();
            InspectResp::Frames(frames)
        }
        InspectReq::Vars { frame_idx } => {
            let vars = prog
                .vm
                .debug_frame_vars(&prog.interp, frame_idx)
                .into_iter()
                .map(|(name, value)| {
                    // `Unit` displays as empty: show `()` so variables never
                    // render blank in the editor.
                    let text = format!("{value}");
                    let text = if text.is_empty() {
                        "()".to_string()
                    } else {
                        text
                    };
                    VarView {
                        name,
                        value: text,
                        typ: value_type_name(&value),
                    }
                })
                .collect();
            InspectResp::Vars(vars)
        }
    }
}

/// Read one DAP message from `reader` (Content-Length framing).
pub fn read_message<R: std::io::BufRead>(reader: &mut R) -> Option<serde_json::Value> {
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
    let len = content_length?;
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).ok()?;
    serde_json::from_slice(&buf).ok()
}

/// Write one DAP message to `writer`.
pub fn write_message<W: std::io::Write>(writer: &mut W, value: &serde_json::Value) {
    let body = serde_json::to_string(value).unwrap_or_default();
    let _ = write!(writer, "Content-Length: {}\r\n\r\n{body}", body.len());
    let _ = writer.flush();
}
