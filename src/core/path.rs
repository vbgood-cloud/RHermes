/// RHermes 路径管理器
///
/// 核心职责：启动时检测部署模式，统一管理所有文件路径。
///
/// ## 两种模式
///
/// ### 可移动模式（Portable Mode）
/// 当 `<可执行程序目录>/home/` 存在时触发。
/// 所有配置/记忆/技能/会话都保存在 `home/` 目录中。
/// 适用于：U盘、云同步文件夹、Docker volume、CI/CD 挂载点。
///
/// ### 传统模式（Traditional Mode）
/// 无 `home/` 目录时自动降级。
/// 使用系统标准配置目录：
///   - Linux:   $XDG_CONFIG_HOME/rhermes  (~/.config/rhermes)
///   - macOS:   ~/Library/Application Support/rhermes
///   - Windows: %APPDATA%/rhermes

use std::path::{Path, PathBuf};
use std::fs;

// ---------------------------------------------------------------------------
// 部署模式枚举
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum DeploymentMode {
    /// 可移动模式：数据根目录 = 可执行文件旁的 `home/` 目录
    Portable(PathBuf),
    /// 传统模式：数据根目录 = 系统标准配置目录
    Traditional(PathBuf),
}

impl DeploymentMode {
    /// 返回人类可读的模式名称
    pub fn name(&self) -> &str {
        match self {
            Self::Portable(_) => "portable",
            Self::Traditional(_) => "traditional",
        }
    }
}

// ---------------------------------------------------------------------------
// PathManager
// ---------------------------------------------------------------------------

/// 路径管理器 —— 所有文件系统操作的唯一路径来源
///
/// # 不变式
/// - `data_root` 在初始化后**永不改变**
/// - 所有子路径都是 `data_root` 的相对路径拼接
/// - `ensure_dirs()` 在首次使用时惰性创建目录
#[derive(Debug, Clone)]
pub struct PathManager {
    mode: DeploymentMode,
    data_root: PathBuf,
    /// 可执行文件所在目录（config.toml 和 .env 放在这里）
    exe_dir: PathBuf,
}

impl PathManager {
    /// 检测部署模式并初始化 PathManager
    ///
    /// ## 检测流程
    /// 1. 获取当前可执行文件的路径 (`std::env::current_exe()`)
    /// 2. 取其父目录作为 `exe_dir`
    /// 3. 检查 `exe_dir/home/` 是否存在且为目录
    /// 4. 是 → 可移动模式；否 → 传统模式
    pub fn detect() -> Self {
        let exe_path = std::env::current_exe()
            .expect("无法获取可执行文件路径");
        let exe_dir = exe_path.parent()
            .expect("无法获取可执行文件所在目录")
            .to_path_buf();
        let home_candidate = exe_dir.join("home");

        if home_candidate.is_dir() {
            tracing::info!(
                "检测到 home/ 目录，使用可移动模式: {}",
                home_candidate.display()
            );
            Self {
                mode: DeploymentMode::Portable(home_candidate.clone()),
                data_root: home_candidate,
                exe_dir,
            }
        } else {
            let data_root = Self::default_data_root();
            tracing::info!(
                "未检测到 home/ 目录，使用传统模式: {}",
                data_root.display()
            );
            Self {
                mode: DeploymentMode::Traditional(data_root.clone()),
                data_root,
                exe_dir,
            }
        }
    }

    /// 使用指定的 exe_dir 创建 PathManager（用于测试或覆盖）
    /// `exe_dir` 是配置文件和 .env 的位置，`home/` 子目录是数据根目录
    #[allow(dead_code)]
    pub fn with_root(exe_dir: PathBuf) -> Self {
        let home_candidate = exe_dir.join("home");
        if home_candidate.is_dir() {
            Self {
                mode: DeploymentMode::Portable(home_candidate.clone()),
                data_root: home_candidate,
                exe_dir,
            }
        } else {
            Self {
                mode: DeploymentMode::Traditional(exe_dir.clone()),
                data_root: exe_dir.clone(),
                exe_dir,
            }
        }
    }

    /// 返回当前部署模式
    pub fn mode(&self) -> &DeploymentMode {
        &self.mode
    }

    /// 返回数据根目录
    pub fn data_root(&self) -> &Path {
        &self.data_root
    }

    // ---- 子路径访问器 ----

    /// 主配置文件路径（与可执行文件同目录）
    #[allow(dead_code)]
    pub fn config_path(&self) -> PathBuf {
        self.exe_dir.join("config.toml")
    }

    /// 长期记忆数据库路径（SQLite + FTS5）
    #[allow(dead_code)]
    pub fn memory_db_path(&self) -> PathBuf {
        self.data_root.join("memory.db")
    }

    /// 技能目录
    pub fn skills_dir(&self) -> PathBuf {
        self.data_root.join("skills")
    }

    /// 会话归档目录
    pub fn sessions_dir(&self) -> PathBuf {
        self.data_root.join("sessions")
    }

    /// 运行日志目录
    pub fn logs_dir(&self) -> PathBuf {
        self.data_root.join("logs")
    }

    /// 临时缓存目录（可安全删除）
    pub fn cache_dir(&self) -> PathBuf {
        self.data_root.join("cache")
    }

    /// 用户工作目录
    pub fn workspace_dir(&self) -> PathBuf {
        self.data_root.join("workspace")
    }

    // ---- 目录创建 ----

    /// 确保所有标准子目录存在
    /// 在首次使用时调用，惰性创建
    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        let dirs = [
            self.data_root().to_path_buf(),
            self.skills_dir(),
            self.sessions_dir(),
            self.logs_dir(),
            self.cache_dir(),
            self.workspace_dir(),
        ];
        for dir in &dirs {
            fs::create_dir_all(dir)?;
        }
        tracing::debug!("所有标准目录已就绪");
        Ok(())
    }

    // ---- 辅助方法 ----

    /// 获取系统的标准配置目录作为传统模式 data_root
    fn default_data_root() -> PathBuf {
        #[cfg(target_os = "linux")]
        {
            // 优先使用 XDG_CONFIG_HOME，否则 ~/.config
            let base = dirs::config_dir()
                .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".config"));
            base.join("rhermes")
        }
        #[cfg(target_os = "macos")]
        {
            let base = dirs::config_dir()
                .unwrap_or_else(|| {
                    dirs::home_dir()
                        .unwrap_or_default()
                        .join("Library")
                        .join("Application Support")
                });
            base.join("rhermes")
        }
        #[cfg(target_os = "windows")]
        {
            let base = dirs::config_dir()
                .unwrap_or_else(|| {
                    dirs::data_dir()
                        .unwrap_or_default()
                });
            base.join("rhermes")
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            dirs::home_dir()
                .unwrap_or_default()
                .join(".rhermes")
        }
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn setup_temp_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("创建临时目录失败")
    }

    #[test]
    fn test_portable_mode_detection() {
        let tmp = setup_temp_dir();
        let exe_dir = tmp.path().join("bin");
        fs::create_dir_all(&exe_dir).unwrap();
        let home_dir = exe_dir.join("home");
        fs::create_dir_all(&home_dir).unwrap();

        // 模拟可执行文件在 bin/ 下
        let pm = PathManager::with_root(exe_dir.clone());
        assert_eq!(pm.mode().name(), "portable");
        assert_eq!(pm.data_root(), home_dir);
    }

    #[test]
    fn test_traditional_mode_fallback() {
        let tmp = setup_temp_dir();
        let exe_dir = tmp.path().join("bin");
        fs::create_dir_all(&exe_dir).unwrap();

        // 没有 home/ 目录
        let pm = PathManager::with_root(exe_dir.clone());
        assert_eq!(pm.mode().name(), "traditional");
        assert_eq!(pm.data_root(), exe_dir);
    }

    #[test]
    fn test_sub_paths() {
        let tmp = setup_temp_dir();
        let root = tmp.path().join("rhermes_data");
        fs::create_dir_all(&root).unwrap();

        let pm = PathManager::with_root(root.clone());

        assert_eq!(pm.config_path(), root.join("config.toml"));
        assert_eq!(pm.memory_db_path(), root.join("memory.db"));
        assert_eq!(pm.skills_dir(), root.join("skills"));
        assert_eq!(pm.sessions_dir(), root.join("sessions"));
        assert_eq!(pm.logs_dir(), root.join("logs"));
        assert_eq!(pm.cache_dir(), root.join("cache"));
    }

    #[test]
    fn test_ensure_dirs() {
        let tmp = setup_temp_dir();
        let root = tmp.path().join("rhermes_data");
        fs::create_dir_all(&root).unwrap();

        let pm = PathManager::with_root(root.clone());
        pm.ensure_dirs().unwrap();

        assert!(pm.skills_dir().exists());
        assert!(pm.sessions_dir().exists());
        assert!(pm.logs_dir().exists());
        assert!(pm.cache_dir().exists());
    }
}
