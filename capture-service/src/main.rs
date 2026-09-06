//! Capture-service command-line entrypoint.

#[tokio::main]
async fn main() {
    if let Err(error) = louiselm_capture::cli::run().await {
        eprintln!("louiselm-capture: {error}");
        std::process::exit(1);
    }
}
