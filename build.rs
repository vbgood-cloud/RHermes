//! 构建脚本 — 嵌入 Windows 资源（图标 + 版本信息）

fn main() {
    // 在 Windows 上嵌入图标和版本信息
    if std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() == "windows" {
        let mut res = winresource::WindowsResource::new();
        // 使用绝对路径，确保 rc.exe 能找到图标文件
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        let icon_path = std::path::Path::new(&manifest_dir)
            .join("resources")
            .join("icon.ico");
        res.set_icon(icon_path.to_str().unwrap())
            .set("LegalCopyright", "MIT")
            .set("FileDescription", "RHermes - Rust AI Agent");
        res.compile().expect("Failed to compile resources");
    }
}
