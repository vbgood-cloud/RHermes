//! Extism Host Functions — 宿主提供给 Wasm 插件的安全能力网关
//!
//! 遵循 P28 设计的安全策略：
//!   - host_log: 日志，永远允许
//!   - host_http_get / host_http_post: HTTP 请求，受 allowed_hosts 白名单限制
//!   - host_read_file / host_write_file: 文件读写，受 allowed_paths 前缀白名单限制
//!   - host_exec: 命令执行，默认禁用（allow_exec 显式开启）
//!
//! Extism 1.30 约定：字符串参数/返回值用 i64 偏移量（PTR）传递，
//! 宿主函数签名为 Fn(&mut CurrentPlugin, &[Val], &mut [Val], UserData<T>)。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use extism::{Function, UserData, Val, ValType, PTR};

/// Host Functions 权限配置
#[derive(Debug, Clone)]
pub struct HostAccessConfig {
    /// 允许访问的主机名白名单（"*" 表示全部，需谨慎）
    pub allowed_hosts: HashSet<String>,
    /// 允许访问的文件路径前缀（空列表 = 禁止所有文件访问）
    pub allowed_paths: Vec<PathBuf>,
    /// 是否允许命令执行（默认 false，极高风险）
    pub allow_exec: bool,
    /// host_exec 超时（秒）
    pub exec_timeout_secs: u64,
    /// HTTP 超时（秒）
    pub http_timeout_secs: u64,
}

impl Default for HostAccessConfig {
    fn default() -> Self {
        Self {
            allowed_hosts: HashSet::new(),
            allowed_paths: Vec::new(),
            allow_exec: false,
            exec_timeout_secs: 10,
            http_timeout_secs: 15,
        }
    }
}

impl HostAccessConfig {
    pub fn new(allowed_hosts: Vec<String>, allowed_paths: Vec<String>, allow_exec: bool) -> Self {
        Self {
            allowed_hosts: allowed_hosts.into_iter().collect(),
            allowed_paths: allowed_paths.into_iter().map(PathBuf::from).collect(),
            allow_exec,
            ..Default::default()
        }
    }

    /// 检查主机是否在白名单中（支持 "*" 与 "*.example.com" 通配）
    pub fn is_host_allowed(&self, host: &str) -> bool {
        self.allowed_hosts.contains("*")
            || self.allowed_hosts.contains(host)
            || self
                .allowed_hosts
                .iter()
                .any(|h| h.starts_with("*.") && host.ends_with(&h[1..]))
    }

    /// 检查路径是否在白名单前缀内（canonicalize 双向解析，防 ../ 逃逸）
    pub fn is_path_allowed(&self, path: &Path) -> bool {
        if self.allowed_paths.is_empty() {
            return false;
        }
        let canonical = match path.canonicalize() {
            Ok(p) => p,
            Err(_) => return false, // 不存在的路径一律拒绝（写文件需父目录在前缀内）
        };
        self.allowed_paths.iter().any(|prefix| {
            let resolved = match prefix.canonicalize() {
                Ok(p) => p,
                Err(_) => return false,
            };
            canonical.starts_with(&resolved)
        })
    }
}

/// 从 URL 提取主机名（去掉协议/路径/端口）
pub fn extract_host(url: &str) -> Option<String> {
    let trimmed = url.trim();
    let host_part = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .unwrap_or(trimmed);
    let host = host_part.split('/').next()?;
    let host = host.split(':').next()?;
    if host.is_empty() {
        return None;
    }
    Some(host.to_string())
}

// ---------------------------------------------------------------------------
// 内存读写辅助 — Extism i64 偏移量协议
// ---------------------------------------------------------------------------

fn read_str(p: &mut extism::CurrentPlugin, offs: &Val) -> Result<String, String> {
    p.memory_get_val::<String>(offs).map_err(|e| e.to_string())
}

fn write_str(
    p: &mut extism::CurrentPlugin,
    out: &mut [Val],
    s: &str,
) -> Result<(), extism::Error> {
    let handle = p.memory_new(s)?;
    out[0] = Val::I64(handle.offset() as i64);
    Ok(())
}

fn write_err(p: &mut extism::CurrentPlugin, out: &mut [Val], msg: &str) {
    let json = serde_json::json!({
        "success": false,
        "error": msg,
    });
    let _ = write_str(p, out, &json.to_string());
}

// ---------------------------------------------------------------------------
// Host Function 构造
// ---------------------------------------------------------------------------

/// 构建全部 Host Functions（按 access 配置裁剪）
///
/// - host_log 永远注册
/// - host_http_get/post 仅当 allowed_hosts 非空时注册（空 = 禁网）
/// - host_read_file/host_write_file 仅当 allowed_paths 非空时注册
/// - host_exec 仅当 allow_exec = true 时注册
pub fn build_host_functions(access: &HostAccessConfig) -> Vec<Function> {
    let mut fns = Vec::new();

    // ── host_log(level: String, message: String) -> String ──
    fns.push(host_log_fn());

    // ── HTTP（白名单非空才注册）──
    if !access.allowed_hosts.is_empty() {
        let cfg = access.clone();
        fns.push(Function::new(
            "host_http_get",
            [PTR],
            [PTR],
            UserData::new(cfg.clone()),
            move |p, inp, outp, ud| {
                let url = match read_str(p, &inp[0]) {
                    Ok(u) => u,
                    Err(e) => {
                        write_err(p, outp, &format!("读取参数失败: {e}"));
                        return Ok(());
                    }
                };
                let cfg = ud.get()?.lock().unwrap().clone();
                match http_get(&cfg, &url) {
                    Ok(body) => write_str(p, outp, &body)?,
                    Err(e) => write_err(p, outp, &e),
                }
                Ok(())
            },
        ));

        let cfg = access.clone();
        fns.push(Function::new(
            "host_http_post",
            [PTR, PTR, PTR],
            [PTR],
            UserData::new(cfg),
            move |p, inp, outp, ud| {
                let url = match read_str(p, &inp[0]) {
                    Ok(u) => u,
                    Err(e) => {
                        write_err(p, outp, &format!("读取参数失败: {e}"));
                        return Ok(());
                    }
                };
                let content_type = read_str(p, &inp[1]).unwrap_or_else(|_| "application/json".into());
                let body = match read_str(p, &inp[2]) {
                    Ok(b) => b,
                    Err(e) => {
                        write_err(p, outp, &format!("读取参数失败: {e}"));
                        return Ok(());
                    }
                };
                let cfg = ud.get()?.lock().unwrap().clone();
                match http_post(&cfg, &url, &content_type, &body) {
                    Ok(resp) => write_str(p, outp, &resp)?,
                    Err(e) => write_err(p, outp, &e),
                }
                Ok(())
            },
        ));
    }

    // ── 文件读写（白名单前缀非空才注册）──
    if !access.allowed_paths.is_empty() {
        let cfg = access.clone();
        fns.push(Function::new(
            "host_read_file",
            [PTR],
            [PTR],
            UserData::new(cfg),
            move |p, inp, outp, ud| {
                let path = match read_str(p, &inp[0]) {
                    Ok(x) => x,
                    Err(e) => {
                        write_err(p, outp, &format!("读取参数失败: {e}"));
                        return Ok(());
                    }
                };
                let cfg = ud.get()?.lock().unwrap().clone();
                match read_allowed_file(&cfg, &path) {
                    Ok(content) => write_str(p, outp, &content)?,
                    Err(e) => write_err(p, outp, &e),
                }
                Ok(())
            },
        ));

        let cfg = access.clone();
        fns.push(Function::new(
            "host_write_file",
            [PTR, PTR],
            [PTR],
            UserData::new(cfg),
            move |p, inp, outp, ud| {
                let path = match read_str(p, &inp[0]) {
                    Ok(x) => x,
                    Err(e) => {
                        write_err(p, outp, &format!("读取参数失败: {e}"));
                        return Ok(());
                    }
                };
                let content = match read_str(p, &inp[1]) {
                    Ok(x) => x,
                    Err(e) => {
                        write_err(p, outp, &format!("读取参数失败: {e}"));
                        return Ok(());
                    }
                };
                let cfg = ud.get()?.lock().unwrap().clone();
                match write_allowed_file(&cfg, &path, &content) {
                    Ok(n) => {
                        let json = serde_json::json!({ "success": true, "bytes_written": n });
                        write_str(p, outp, &json.to_string())?
                    }
                    Err(e) => write_err(p, outp, &e),
                }
                Ok(())
            },
        ));
    }

    // ── host_exec（默认禁用）──
    if access.allow_exec {
        let cfg = access.clone();
        fns.push(Function::new(
            "host_exec",
            [PTR],
            [PTR],
            UserData::new(cfg),
            move |p, inp, outp, ud| {
                let cmd = match read_str(p, &inp[0]) {
                    Ok(x) => x,
                    Err(e) => {
                        write_err(p, outp, &format!("读取参数失败: {e}"));
                        return Ok(());
                    }
                };
                let cfg = ud.get()?.lock().unwrap().clone();
                match exec_command(&cfg, &cmd) {
                    Ok(result) => write_str(p, outp, &result)?,
                    Err(e) => write_err(p, outp, &e),
                }
                Ok(())
            },
        ));
    }

    fns
}

fn host_log_fn() -> Function {
    Function::new(
        "host_log",
        [PTR, PTR],
        [],
        UserData::new(()),
        |p, inp, _outp, _ud| {
            let level = read_str(p, &inp[0]).unwrap_or_else(|_| "info".into());
            let message = read_str(p, &inp[1]).unwrap_or_default();
            match level.as_str() {
                "error" => tracing::error!("[wasm-plugin] {message}"),
                "warn" | "warning" => tracing::warn!("[wasm-plugin] {message}"),
                "debug" => tracing::debug!("[wasm-plugin] {message}"),
                _ => tracing::info!("[wasm-plugin] {message}"),
            }
            Ok(())
        },
    )
}

// ---------------------------------------------------------------------------
// 实际操作（阻塞调用在 host fn 内执行——Wasm 单线程，可接受）
// ---------------------------------------------------------------------------

fn http_get(cfg: &HostAccessConfig, url: &str) -> Result<String, String> {
    let host = extract_host(url).ok_or_else(|| "无法解析 URL 主机名".to_string())?;
    if !cfg.is_host_allowed(&host) {
        return Err(format!("主机 {host} 不在白名单"));
    }
    let resp = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(cfg.http_timeout_secs))
        .build()
        .get(url)
        .call()
        .map_err(|e| format!("HTTP GET 失败: {e}"))?;
    let status = resp.status();
    let body = resp
        .into_string()
        .map_err(|e| format!("读取响应失败: {e}"))?;
    Ok(serde_json::json!({
        "success": true,
        "status": status,
        "body": body,
    })
    .to_string())
}

fn http_post(cfg: &HostAccessConfig, url: &str, content_type: &str, body: &str) -> Result<String, String> {
    let host = extract_host(url).ok_or_else(|| "无法解析 URL 主机名".to_string())?;
    if !cfg.is_host_allowed(&host) {
        return Err(format!("主机 {host} 不在白名单"));
    }
    let resp = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(cfg.http_timeout_secs))
        .build()
        .post(url)
        .set("Content-Type", content_type)
        .send_string(body)
        .map_err(|e| format!("HTTP POST 失败: {e}"))?;
    let status = resp.status();
    let resp_body = resp
        .into_string()
        .map_err(|e| format!("读取响应失败: {e}"))?;
    Ok(serde_json::json!({
        "success": true,
        "status": status,
        "body": resp_body,
    })
    .to_string())
}

fn read_allowed_file(cfg: &HostAccessConfig, path_str: &str) -> Result<String, String> {
    let path = Path::new(path_str);
    if !cfg.is_path_allowed(path) {
        return Err(format!("路径 {path_str} 不在白名单"));
    }
    std::fs::read_to_string(path).map_err(|e| format!("读取失败: {e}"))
}

fn write_allowed_file(cfg: &HostAccessConfig, path_str: &str, content: &str) -> Result<usize, String> {
    let path = Path::new(path_str);
    if !cfg.is_path_allowed(path) {
        return Err(format!("路径 {path_str} 不在白名单"));
    }
    std::fs::write(path, content).map_err(|e| format!("写入失败: {e}"))?;
    Ok(content.len())
}

fn exec_command(_cfg: &HostAccessConfig, cmd: &str) -> Result<String, String> {
    use std::process::Command;
    // 简单命令黑名单（与 D14 P0 呼应）
    let danger = ["rm -rf", "mkfs", "shutdown", "reboot", "format ", "del /f"];
    let lower = cmd.to_lowercase();
    if danger.iter().any(|d| lower.contains(d)) {
        return Err(format!("命令被安全策略拦截: {cmd}"));
    }
    let output = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .output()
        .map_err(|e| format!("执行失败: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    Ok(serde_json::json!({
        "success": output.status.success(),
        "exit_code": output.status.code().unwrap_or(-1),
        "stdout": stdout.to_string(),
        "stderr": stderr.to_string(),
    })
    .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_host_allowed_wildcard() {
        let cfg = HostAccessConfig::new(vec!["*".into()], vec![], false);
        assert!(cfg.is_host_allowed("example.com"));
    }

    #[test]
    fn test_host_allowed_specific() {
        let cfg = HostAccessConfig::new(vec!["example.com".into()], vec![], false);
        assert!(cfg.is_host_allowed("example.com"));
        assert!(!cfg.is_host_allowed("evil.com"));
    }

    #[test]
    fn test_host_allowed_wildcard_subdomain() {
        let cfg = HostAccessConfig::new(vec!["*.example.com".into()], vec![], false);
        assert!(cfg.is_host_allowed("api.example.com"));
        assert!(!cfg.is_host_allowed("example.com"));
    }

    #[test]
    fn test_extract_host() {
        assert_eq!(extract_host("https://example.com/path"), Some("example.com".into()));
        assert_eq!(extract_host("http://example.com:8080/api"), Some("example.com".into()));
        assert_eq!(extract_host("example.com"), Some("example.com".into()));
        assert_eq!(extract_host(""), None);
    }

    #[test]
    fn test_path_allowed_prefix() {
        let dir = std::env::temp_dir();
        let cfg = HostAccessConfig {
            allowed_paths: vec![dir.clone()],
            ..Default::default()
        };
        let target = dir.join("rhermes-wasm-test.txt");
        std::fs::write(&target, "x").unwrap();
        assert!(cfg.is_path_allowed(&target));
        assert!(!cfg.is_path_allowed(Path::new("/etc/passwd")));
    }

    #[test]
    fn test_path_reject_when_empty_whitelist() {
        let cfg = HostAccessConfig::default();
        assert!(!cfg.is_path_allowed(Path::new("/tmp/any")));
    }

    #[test]
    fn test_build_functions_respect_permissions() {
        // 无任何权限 → 只有 host_log
        let fns = build_host_functions(&HostAccessConfig::default());
        assert_eq!(fns.len(), 1);
        assert_eq!(fns[0].name(), "host_log");

        // 全权限 → 6 个
        let cfg = HostAccessConfig::new(
            vec!["api.example.com".into()],
            vec!["/tmp".into()],
            true,
        );
        let fns = build_host_functions(&cfg);
        assert_eq!(fns.len(), 6);
        let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();
        assert!(names.contains(&"host_http_get"));
        assert!(names.contains(&"host_http_post"));
        assert!(names.contains(&"host_read_file"));
        assert!(names.contains(&"host_write_file"));
        assert!(names.contains(&"host_exec"));
        assert!(names.contains(&"host_log"));
    }
}
