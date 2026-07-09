fn main() {
    // Earliest Rust entry: stamp process start for the fallback cold-start basis
    // (the Linux path prefers the true exec time from /proc). Zero cost — one
    // OnceLock write. See jetty_app::perf.
    jetty_app::perf::mark_process_start();
    jetty_app::run();
}
