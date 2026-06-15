use std::io;
use std::process::ExitCode;

fn main() -> ExitCode {
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(error) => {
            eprintln!("clear-launcher: failed to determine current directory: {error}");
            return ExitCode::FAILURE;
        }
    };

    let env_get = |key: &str| std::env::var(key).ok();
    let mut stdout = io::stdout().lock();

    match clear_launcher::execute(std::env::args().skip(1), &cwd, &env_get, &mut stdout) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("clear-launcher: {error:#}");
            ExitCode::FAILURE
        }
    }
}
