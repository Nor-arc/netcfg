fn main() {
    // A build stamp so a stale wheel is easy to spot: netcfg.__build__
    let stamp = std::process::Command::new("git").args(["rev-parse", "--short", "HEAD"]).output().ok()
        .filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into());
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    println!("cargo:rustc-env=NETCFG_BUILD={stamp}.{t}");
    println!("cargo:rerun-if-changed=build.rs");
}
