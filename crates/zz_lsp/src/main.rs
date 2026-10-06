use tower_lsp::{LspService, Server};
use zz_lsp::server::Backend;

const VERSION: &str = env!("CARGO_PKG_VERSION");

const USAGE: &str = "usage: zz-lsp [--version] [--help]   (reads LSP on stdin, writes LSP on stdout)\n";

#[tokio::main]
async fn main() {
    // CLI flags short-circuit before the LSP serve loop: without this the
    // flag bytes feed the LSP parser and answer with a JSON-RPC parse
    // error, hiding which server build an editor spawned (#255).
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--version" | "-V" => {
                println!("zz-lsp {VERSION}");
                return;
            }
            "--help" | "-h" => {
                print!("{USAGE}");
                return;
            }
            _ => {}
        }
    }
    env_logger::init();

    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(Backend::new);
    Server::new(stdin, stdout, socket)
        .concurrency_level(4)
        .serve(service)
        .await;
}
