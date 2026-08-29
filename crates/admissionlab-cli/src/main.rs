#![forbid(unsafe_code)]

mod output;

fn main() {
    // `false` is a placeholder: Task 0.5 wires this up to a real
    // `--verbose` flag once argument parsing exists. This module does not
    // parse arguments itself (see `output` module docs).
    output::init_tracing(false);
    tracing::info!("admissionlab starting");
}
