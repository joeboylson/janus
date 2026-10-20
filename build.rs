// Stamps the build time into the binary, so `janus --version` tells two
// builds of the same version apart.
fn main() {
    let out = std::process::Command::new("date").args(["-u", "+%Y-%m-%d %H:%M UTC"]).output();
    let built = out.ok().and_then(|o| String::from_utf8(o.stdout).ok()).unwrap_or_default();
    println!("cargo:rustc-env=JANUS_BUILT={}", built.trim());
    println!("cargo:rerun-if-changed=src");
}
