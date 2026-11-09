fn main() {
    #[cfg(windows)]
    {
        use embed_manifest::manifest::ExecutionLevel;
        use embed_manifest::{embed_manifest, new_manifest};

        embed_manifest(
            new_manifest("AgentPowerGuard.App")
                .requested_execution_level(ExecutionLevel::RequireAdministrator),
        )
        .expect("unable to embed application manifest");

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
