//! Prints the adapter table the Network view shows, as JSON, once or repeatedly:
//!   cargo run -p proclens-app --example netsnap            one reading
//!   cargo run -p proclens-app --example netsnap -- 5 1000  five readings, 1000 ms apart
//! Used to compare the counters with Windows' own tools (Get-NetAdapterStatistics, netstat -e).

fn main() {
    let args: Vec<u64> = std::env::args().skip(1).filter_map(|a| a.parse().ok()).collect();
    let (count, gap) = (args.first().copied().unwrap_or(1), args.get(1).copied().unwrap_or(1000));
    for i in 0..count {
        if i > 0 {
            std::thread::sleep(std::time::Duration::from_millis(gap));
        }
        match proclens_app_lib::network::snapshot() {
            Ok(s) => println!("{}", serde_json::to_string(&s).unwrap()),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
    }
    let t = proclens_app_lib::network::selftest();
    eprintln!("selftest: ok={} {}", t.ok, t.detail);
}
