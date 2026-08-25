use tower_lsp::{LspService, Server};
use zz_lsp::server::Backend;

#[tokio::main]
async fn main() {
    env_logger::init();

    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(Backend::new);
    Server::new(stdin, stdout, socket)
        .concurrency_level(4)
        .serve(service)
        .await;
}
