use tower_lsp_server::{LspService, Server};
use wake_css_lsp::Backend;

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("--version") => {
            println!("{}", env!("CARGO_PKG_VERSION"));
            return;
        }
        Some("--stdio") | None => {}
        Some(argument) => {
            eprintln!(
                "unknown argument: {argument}\nusage: wake-css-language-server [--stdio|--version]"
            );
            std::process::exit(2);
        }
    }
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let (service, socket) = LspService::new(Backend::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}
