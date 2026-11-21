//! Lists the sample rates: period, block span and coverage. `cargo run -p rfpm --example rates`

fn main() {
    for rate in rfpm::SampleRate::all() {
        println!("{rate}");
    }
}
