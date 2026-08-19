// Stamps the short commit into the binary so the shell can show it
// bottom-center, like the web app's build-time __COMMIT_HASH__.
fn main() {
    println!("cargo:rerun-if-changed=../.git/HEAD");
    if let Ok(out) = std::process::Command::new("git")
        .args(["rev-parse", "--short=6", "HEAD"])
        .output()
    {
        if out.status.success() {
            let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !sha.is_empty() {
                println!("cargo:rustc-env=GOMPASS_COMMIT={sha}");
            }
        }
    }
}
