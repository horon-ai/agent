//! Entry point: the binary only forwards to [`horon_tui::run`] and exits with
//! its code, so everything that can be tested lives in the library.

fn main() {
    std::process::exit(horon_tui::run(std::env::args_os()));
}
