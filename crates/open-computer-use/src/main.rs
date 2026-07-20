use open_computer_use::cli;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) = cli::run(std::env::args().skip(1)).await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
