//! What one CoreWLAN association read costs, which is where `MIN_INTERVAL_MS`
//! comes from. The floor was a round number in the source before it was a
//! measurement, and a sampling interval asserted without evidence is worse than
//! none: it looks like a criterion.
//!
//! `cargo run --release -p lanplay-radio-sample --example read-cost`
//!
//! Two runs on the development machine, 2000 reads each:
//!
//! ```text
//! n=2000 p50=3185.5us p99=4591.9us max=7553.2us
//! n=2000 p50=3161.1us p99=5404.8us max=15495.8us
//! ```

const READS: usize = 2000;

fn main() {
    let mut us: Vec<f64> = Vec::with_capacity(READS);
    for _ in 0..READS {
        let at = std::time::Instant::now();
        // The return value is deliberately dropped: the cost of the read is the
        // measurement, and formatting the association would be measured too.
        let _ = lanplay_capabilities::wifi::association();
        us.push(at.elapsed().as_secs_f64() * 1e6);
    }
    us.sort_by(f64::total_cmp);
    let at = |q: f64| us[((us.len() - 1) as f64 * q) as usize];
    println!(
        "n={} p50={:.1}us p99={:.1}us max={:.1}us",
        us.len(),
        at(0.50),
        at(0.99),
        at(1.0)
    );
}
