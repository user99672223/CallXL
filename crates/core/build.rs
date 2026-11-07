//! Bakes APP_SECRET (CLAUDE.md §5) into the binary from env CALLXL_APP_SECRET.
//! Without it a random DEV secret is generated and a loud warning is printed.

fn main() {
    println!("cargo:rerun-if-env-changed=CALLXL_APP_SECRET");
    let secret = match std::env::var("CALLXL_APP_SECRET") {
        Ok(s) if !s.is_empty() => s,
        _ => {
            println!(
                "cargo:warning=CALLXL_APP_SECRET not set — using a random DEV secret; peers built separately will not see each other's cloud sessions"
            );
            let bytes: [u8; 24] = rand::random();
            format!("dev-{}", hex::encode(bytes))
        }
    };
    println!("cargo:rustc-env=CALLXL_BUILD_APP_SECRET={secret}");
}
