fn main() {
    // externalBin（dl-cli / dl-server 随安装器分发）在编译期要求文件存在。
    // fresh clone 时先写空占位符保证可编译；真实二进制由 scripts/bundle.ps1
    // 在打包前从 target/release 复制（占位符 sidecar 不可用于分发）。
    let triple = std::env::var("TARGET").unwrap_or_default();
    let exe_suffix = if triple.contains("windows") {
        ".exe"
    } else {
        ""
    };
    std::fs::create_dir_all("binaries").ok();
    for name in ["dl-cli", "dl-server", "yt-dlp", "deno"] {
        let p = format!("binaries/{name}-{triple}{exe_suffix}");
        if !std::path::Path::new(&p).exists() {
            std::fs::write(&p, "").ok();
        }
    }
    tauri_build::build()
}
