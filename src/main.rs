use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    brain_shell::run().await
}
