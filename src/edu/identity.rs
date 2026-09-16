//! edu 身份密钥的**持久化**：每位老师 / 每位学生各一套凭据。
//!
//! ## 为什么需要它
//!
//! iroh 的 `EndpointId` 就是身份公钥。若每次启动都用 `Endpoint::builder().bind()`
//! 的默认行为（随机生成），会带来三个连锁问题：
//!
//! 1. **白名单失效** —— 老师的授权表按 `EndpointId` 记名，学生一换设备/一重启
//!    就变成新节点，老师那边仍是旧身份，学生被自己的白名单拒之门外；
//! 2. **离网直连失灵** —— `EndpointAddr`（id + 地址）每次变，对端地址簿全部作废；
//! 3. **复现困难** —— 出题、排障时无法用固定身份复现。
//!
//! 因此把私钥落盘：`<home>/edu_identities/<owner>.key`（64 位十六进制的 32 字节种子）。
//!
//! ## 命名与隔离
//!
//! `owner` 取「本机在 edu 里的账号」—— 学生用学号，老师用工号。
//! 于是**同一台机器上可以并排存在多位老师的凭据**（`t001.key` / `t002.key`），
//! 各自独立、互不覆盖，正好对上「每位老师一套凭据」。
//!
//! ⚠️ **文件已存在但内容损坏时绝不重新生成** —— 那等于悄悄换了身份，
//! 后果是白名单里凭空多出一个陌生人、原本的自己被踢出。宁可报错让用户处理。

use std::fs;
use std::path::{Path, PathBuf};

use iroh::SecretKey;

/// 身份密钥目录（相对 `home/`）
pub const IDENTITY_DIR: &str = "edu_identities";

/// 身份目录的完整路径
pub fn identity_dir(home: &Path) -> PathBuf {
    home.join(IDENTITY_DIR)
}

/// 把账号清洗成安全文件名。
///
/// 学号/工号本应是 `[A-Za-z0-9_-]`，但配置是人手写的 —— 出现空格、斜杠、
/// 中文都可能，而它要当文件名用（`..` 之类还会变成路径穿越）。
///
/// ⚠️ **不能把所有非法字符一律塌缩成 `_`**：那样 `张三` 和 `李四` 都会变成 `__`，
/// 于是两位老师共用同一个密钥文件 —— 「每位老师一套凭据」当场破产。
/// 故：安全名原样保留（便于人工识别）；一旦发生替换，就追加**内容指纹**兜底，
/// 保证不同账号得到不同文件名。
fn sanitize(owner: &str) -> String {
    let trimmed = owner.trim();
    if trimmed.is_empty() {
        return "default".to_string();
    }

    let cleaned: String = trimmed
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();

    // 未被替换（纯 ASCII 安全名）→ 直接用，人和脚本都好认
    if cleaned == trimmed {
        return cleaned;
    }

    // 发生过替换 → 追加短指纹，避免不同账号塌缩到同一文件名
    let digest = blake3::hash(trimmed.as_bytes()).to_hex().to_string();
    format!("{}-{}", cleaned, &digest[..8])
}

/// 某账号的密钥文件路径
pub fn key_path(dir: &Path, owner: &str) -> PathBuf {
    dir.join(format!("{}.key", sanitize(owner)))
}

/// 从文件读取私钥。
///
/// 接受两种写法：64 位 hex（本模块写出的格式），或裸 32 字节二进制。
pub fn read_key_file(path: &Path) -> anyhow::Result<SecretKey> {
    let raw = fs::read(path)
        .map_err(|e| anyhow::anyhow!("读取身份文件 {} 失败：{e}", path.display()))?;

    // 优先按 hex 解析（容忍首尾空白/换行）
    if let Ok(text) = std::str::from_utf8(&raw) {
        let trimmed = text.trim();
        if trimmed.len() == 64 {
            if let Ok(bytes) = hex::decode(trimmed) {
                if let Ok(arr) = <[u8; 32]>::try_from(bytes.as_slice()) {
                    return Ok(SecretKey::from_bytes(&arr));
                }
            }
        }
    }

    // 退回裸二进制
    if raw.len() == 32 {
        let arr: [u8; 32] = raw.as_slice().try_into().expect("长度已判");
        return Ok(SecretKey::from_bytes(&arr));
    }

    anyhow::bail!(
        "身份文件 {} 内容损坏（既不是 64 位 hex，也不是 32 字节）—— 拒绝自动重建，请人工处理",
        path.display()
    )
}

/// 把私钥以 hex 写入 `path`（自动建目录）。
pub fn write_key_file(path: &Path, key: &SecretKey) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| anyhow::anyhow!("创建身份目录 {} 失败：{e}", parent.display()))?;
    }
    fs::write(path, hex::encode(key.to_bytes()))
        .map_err(|e| anyhow::anyhow!("写入身份文件 {} 失败：{e}", path.display()))?;
    Ok(())
}

/// 读取或新建：文件存在则读，不存在则生成并落盘。
///
/// 这是「每位老师一套凭据」的入口 —— 第二次启动拿到的还是同一把钥匙。
pub fn load_or_create(dir: &Path, owner: &str) -> anyhow::Result<SecretKey> {
    let path = key_path(dir, owner);
    if path.exists() {
        let key = read_key_file(&path)?;
        tracing::debug!("已加载身份 {} → {}", path.display(), fingerprint(&key));
        return Ok(key);
    }
    let key = SecretKey::generate();
    write_key_file(&path, &key)?;
    tracing::info!("新建身份 {} → {}", path.display(), fingerprint(&key));
    Ok(key)
}

/// 带显式私钥：`hex` 非空则以其为准（并写盘覆盖），否则退化为 [`load_or_create`]。
///
/// 供配置文件 `secret_key = "..."` 使用 —— 便于把身份固定在配置里做演示/复现。
pub fn load_or_import(dir: &Path, owner: &str, hex_str: &str) -> anyhow::Result<SecretKey> {
    let hex_str = hex_str.trim();
    if hex_str.is_empty() {
        return load_or_create(dir, owner);
    }
    let bytes = hex::decode(hex_str)
        .map_err(|e| anyhow::anyhow!("edu 身份私钥不是合法 hex（长度应为 64）：{e}"))?;
    let arr: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("edu 身份私钥长度错误：期望 32 字节，实得 {}", bytes.len()))?;
    let key = SecretKey::from_bytes(&arr);

    let path = key_path(dir, owner);
    write_key_file(&path, &key)?;
    Ok(key)
}

/// 短指纹（前 12 位 hex），仅用于日志与终端展示。
pub fn fingerprint(key: &SecretKey) -> String {
    hex::encode(key.public().as_bytes())[..12].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_owner_ascii_passthrough() {
        // 纯 ASCII 安全名原样保留，方便人工识别
        assert_eq!(sanitize("2024001"), "2024001");
        assert_eq!(sanitize("t-001"), "t-001");
        assert_eq!(sanitize(" t_001 "), "t_001");
        assert_eq!(sanitize(""), "default");
        assert_eq!(sanitize("   "), "default");
    }

    #[test]
    fn test_sanitize_owner_never_collapses_distinct_names() {
        // 🔴 关键：非法字符若一律塌缩成 `_`，`张三` / `李四` / `..` 会全部变成同一个
        //    文件名 → 两位老师共用一套凭据。追加指纹后必须彼此可区分。
        let a = sanitize("张三");
        let b = sanitize("李四");
        let c = sanitize("..");
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_ne!(b, c);

        // 前缀保留可读性，后缀是 8 位指纹
        assert!(a.starts_with("__-"), "应保留清洗后的前缀：{a}");
        assert_eq!(a.len(), 11, "`__-` + 8 位指纹：{a}");

        // 同一个账号必须稳定
        assert_eq!(sanitize("张三"), sanitize(" 张三 "));
    }

    #[test]
    fn test_sanitize_owner_handles_path_traversal_chars() {
        let s = sanitize("../etc/passwd");
        assert!(!s.contains('/'), "文件名不得含路径分隔符：{s}");
        assert!(!s.starts_with('.'), "不得以点开头（避免隐藏文件/相对路径）：{s}");
    }

    #[test]
    fn test_load_or_create_is_stable() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = identity_dir(tmp.path());

        let a = load_or_create(&dir, "2024001").unwrap();
        let b = load_or_create(&dir, "2024001").unwrap();
        // 同一账号第二次必须拿到同一把钥匙
        assert_eq!(a.to_bytes(), b.to_bytes());
        assert_eq!(a.public().as_bytes(), b.public().as_bytes());

        // 文件确实落盘了
        assert!(key_path(&dir, "2024001").exists());
    }

    #[test]
    fn test_multiple_owners_are_isolated() {
        // 每位老师一套凭据 —— 同机并存互不影响
        let tmp = tempfile::tempdir().unwrap();
        let dir = identity_dir(tmp.path());

        let t1 = load_or_create(&dir, "t001").unwrap();
        let t2 = load_or_create(&dir, "t002").unwrap();
        assert_ne!(t1.to_bytes(), t2.to_bytes());
        assert_ne!(t1.public().as_bytes(), t2.public().as_bytes());

        // 再次读取不得互相污染
        assert_eq!(load_or_create(&dir, "t001").unwrap().to_bytes(), t1.to_bytes());
        assert_eq!(load_or_create(&dir, "t002").unwrap().to_bytes(), t2.to_bytes());
    }

    #[test]
    fn test_import_explicit_hex() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = identity_dir(tmp.path());

        let seed = [7u8; 32];
        let expect = SecretKey::from_bytes(&seed);
        let hex_str = hex::encode(seed);

        let got = load_or_import(&dir, "t001", &hex_str).unwrap();
        assert_eq!(got.to_bytes(), expect.to_bytes());

        // 之后即使不带 hex，也应读回同一身份
        let again = load_or_create(&dir, "t001").unwrap();
        assert_eq!(again.to_bytes(), expect.to_bytes());
    }

    #[test]
    fn test_corrupt_file_refuses_to_regenerate() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = identity_dir(tmp.path());
        let path = key_path(&dir, "t001");
        fs::create_dir_all(&dir).unwrap();
        fs::write(&path, b"not-a-key").unwrap();

        let err = load_or_create(&dir, "t001").unwrap_err().to_string();
        assert!(err.contains("损坏"), "应报损坏而不是重建：{err}");
        // 且不得覆盖原文件
        assert_eq!(fs::read(&path).unwrap(), b"not-a-key");
    }

    #[test]
    fn test_fingerprint_is_12_hex() {
        let key = SecretKey::generate();
        let fp = fingerprint(&key);
        assert_eq!(fp.len(), 12);
        assert!(fp.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
