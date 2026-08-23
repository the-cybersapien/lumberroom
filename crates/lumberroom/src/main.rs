//! Entry point. Current-thread runtime: a CLI making one request at a time has no use for a
//! worker pool, and it keeps the binary's startup cost off the bootstrap latency budget.

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(lumberroom::run(argv).await);
}
