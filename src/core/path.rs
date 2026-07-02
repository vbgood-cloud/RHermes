/// RHermes 路径管理器
///
/// 核心职责：管理所有文件路径。
///
/// ## 可移动模式（Portable Mode）
/// 所有配置/记忆/技能/会话都保存在可执行文件旁的 `home/` 目录中。
/// 适用于：U盘、云同步文件夹、Docker volume、CI/CD 挂载点。

use std::path::{Path, PathBuf};
use std::fs;

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
    data_root: PathBuf,
    /// 可执行文件所在目录（config.toml 和 .env 放在这里）
    exe_dir: PathBuf,
}

impl PathManager {
    /// 检测 home/ 目录并初始化 PathManager
    ///
    /// ## 检测流程
    /// 1. 获取当前可执行文件的路径 (`std::env::current_exe()`)
    /// 2. 取其父目录作为 `exe_dir`
    /// 3. 取 `exe_dir/home/` 作为数据根目录
    pub fn detect() -> Self {
        let exe_path = std::env::current_exe()
            .expect("无法获取可执行文件路径");
        let exe_dir = exe_path.parent()
            .expect("无法获取可执行文件所在目录")
            .to_path_buf();
        let home_dir = exe_dir.join("home");

        tracing::info!(
            "可移动模式 · 数据目录: {}",
            home_dir.display()
        );

        Self {
            data_root: home_dir,
            exe_dir,
        }
    }

    /// 使用指定的 exe_dir 创建 PathManager（用于测试或覆盖）
    /// `exe_dir` 是配置文件和 .env 的位置，`home/` 子目录是数据根目录
    #[allow(dead_code)]
    pub fn with_root(exe_dir: PathBuf) -> Self {
        let home_dir = exe_dir.join("home");
        Self {
            data_root: home_dir,
            exe_dir,
        }
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

        let pm = PathManager::with_root(exe_dir.clone());
        assert_eq!(pm.data_root(), home_dir);
    }

    #[test]
    fn test_sub_paths() {
        let tmp = setup_temp_dir();
        let root = tmp.path().join("rhermes_data");
        fs::create_dir_all(&root).unwrap();

        let pm = PathManager::with_root(root.clone());

        assert_eq!(pm.config_path(), root.join("config.toml"));
        assert_eq!(pm.memory_db_path(), root.join("home").join("memory.db"));
        assert_eq!(pm.skills_dir(), root.join("home").join("skills"));
        assert_eq!(pm.sessions_dir(), root.join("home").join("sessions"));
        assert_eq!(pm.logs_dir(), root.join("home").join("logs"));
        assert_eq!(pm.cache_dir(), root.join("home").join("cache"));
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
