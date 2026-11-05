// The triple this helper is built for: the machine it runs on, because
// `cargo run` builds it without `--target`. The helper compares it with the
// triple Tauri bundles for, so it never labels a worker for the wrong system.
fn main() {
    let target = std::env::var("TARGET").expect("cargo sets TARGET for build scripts");
    println!("cargo:rustc-env=LUNAEPIX_HOST_TRIPLE={target}");
}
