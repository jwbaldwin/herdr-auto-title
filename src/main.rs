mod policy;
mod runtime;

fn main() {
    if let Err(error) = runtime::run_from_env() {
        eprintln!("herdr-auto-title: {error}");
        std::process::exit(1);
    }
}
