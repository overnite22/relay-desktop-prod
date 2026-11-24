//! Disposable SSH + SFTP server for manual tests of the "Fichiers (SFTP)" tab:
//! `cargo run -p relay-desktop-core --example sftp_test_server -- <folder> [127.0.0.1:2222]`
//! (user `test`, password `secret`).
#[path = "../tests/support/sftp_server.rs"]
mod sftp_server;

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let root = std::path::PathBuf::from(args.next().expect("dossier servi"));
    let addr = args.next().unwrap_or_else(|| "127.0.0.1:2222".into());
    let port = sftp_server::serve(root.clone(), &addr).await;
    println!("SFTP de test sur le port {port}, dossier {}", root.display());
    std::future::pending::<()>().await;
}
