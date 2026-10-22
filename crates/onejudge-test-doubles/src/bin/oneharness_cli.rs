//! The released oneharness CLI shell, without replacing its parser or engine.
//! Only the harness configured by the journey is a test double.
fn main() {
    oneharness_core::io::app_server::serve_sweeper_if_requested();
    let cli = oneharness::Cli::parse_or_exit();
    std::process::exit(oneharness::dispatch(cli));
}
