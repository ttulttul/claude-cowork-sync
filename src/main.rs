use std::process::ExitCode;

fn main() -> ExitCode {
    if let Err(error) = cowork_merge_rs::cli::run(std::env::args_os()) {
        eprintln!("error: {error:#}");
        return ExitCode::from(1);
    }
    ExitCode::from(0)
}
