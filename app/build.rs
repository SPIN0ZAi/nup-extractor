fn main() {
    println!("cargo:rerun-if-changed=../app_icon.ico");
    println!("cargo:rerun-if-changed=app.manifest");
    println!("cargo:rerun-if-changed=../static/index.html");
    println!("cargo:rerun-if-changed=../static/styles.css");
    println!("cargo:rerun-if-changed=../static/ipc.js");
    println!("cargo:rerun-if-changed=../static/app.js");

    println!("cargo:rerun-if-env-changed=NUP_RELEASE_VERSION");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let raw_version = std::env::var("NUP_RELEASE_VERSION")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
        let product_version = raw_version.trim().trim_start_matches(['v', 'V']);
        let parts: Vec<&str> = product_version
            .split('.')
            .filter(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
            .collect();
        let file_version = match parts.len() {
            0 => "1.0.11.0".to_string(),
            1 => format!("{}.0.0.0", parts[0]),
            2 => format!("{}.{}.0.0", parts[0], parts[1]),
            3 => format!("{}.{}.{}.0", parts[0], parts[1], parts[2]),
            _ => format!("{}.{}.{}.{}", parts[0], parts[1], parts[2], parts[3]),
        };

        let mut res = winresource::WindowsResource::new();
        res.set_icon("../app_icon.ico");
        res.set_manifest_file("app.manifest");
        res.set("ProductName", "NUP Extractor");
        res.set(
            "FileDescription",
            "High-performance Nintendo Switch clean-room container extraction utility",
        );
        res.set("CompanyName", "SPIN0ZAi");
        res.set(
            "LegalCopyright",
            "Copyright (c) 2026 SPIN0ZAi, a55uka (NUP spec)",
        );
        res.set("ProductVersion", product_version);
        res.set("FileVersion", &file_version);
        res.compile().expect("Failed to compile Windows resources");
    }
}
