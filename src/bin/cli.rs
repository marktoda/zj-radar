// Native CLI entry point. The real logic lives in the `zj_radar` library's
// `cli` module (gated behind the `cli` feature).
fn main() {
    zj_radar::cli::run();
}
