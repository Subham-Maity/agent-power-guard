use embed_manifest::manifest::ExecutionLevel;
use embed_manifest::{embed_manifest, new_manifest};

fn main() {
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        // Embed UAC Administrator execution level
        embed_manifest(
            new_manifest("AgentPowerGuard.App")
                .requested_execution_level(ExecutionLevel::RequireAdministrator),
        )
        .expect("unable to embed application manifest");

        // Embed Windows application icon into the PE executable
        if std::path::Path::new("assets/app.ico").exists() {
            let mut res = winres::WindowsResource::new();
            res.set_icon("assets/app.ico");
            res.set("ProductName", "Agent Power Guard");
            res.set("FileDescription", "Agent Power Guard Watchdog");
            let _ = res.compile();
        }
    }
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/app.ico");
}
