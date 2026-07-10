//! 学生认证模块
//!
//! 学生通过学号+密码与教师服务端认证。

use std::path::Path;

use crate::edu::store::{EduError, EduStore, SessionInfo};

/// 认证结果
#[derive(Debug)]
pub struct AuthResult {
    pub token: String,
    pub student_id: i64,
    pub student_name: String,
}

/// 学生认证
///
/// 验证学号+密码，成功后创建会话并返回 token。
pub fn authenticate(
    store: &EduStore,
    student_no: &str,
    password: &str,
) -> Result<AuthResult, EduError> {
    // 验证密码
    let valid = store.verify_student_password(student_no, password)?;
    if !valid {
        return Err(EduError::Auth("学号或密码错误".into()));
    }

    // 获取学生信息
    let student = store
        .get_student(student_no)?
        .ok_or_else(|| EduError::NotFound(format!("学号 '{student_no}' 不存在")))?;

    // 创建会话（24 小时有效）
    let expires = (chrono::Utc::now() + chrono::Duration::hours(24)).to_rfc3339();
    let token = store.create_session(student.id, &expires)?;

    Ok(AuthResult {
        token,
        student_id: student.id,
        student_name: student.name,
    })
}

/// 验证已有 token 是否有效
pub fn validate_token(store: &EduStore, token: &str) -> Result<Option<SessionInfo>, EduError> {
    store.validate_session(token)
}

/// 学生认证命令交互流程
///
/// 在终端中引导学生输入学号和密码，返回认证结果。
pub fn interactive_auth(db_path: &Path) -> Result<AuthResult, EduError> {
    let store = EduStore::open(db_path)?;

    let student_no: String = dialoguer::Input::new()
        .with_prompt("学号")
        .interact_text()
        .map_err(|e| EduError::Auth(format!("输入错误: {e}")))?;

    let password = dialoguer::Password::new()
        .with_prompt("密码")
        .interact()
        .map_err(|e| EduError::Auth(format!("输入错误: {e}")))?;

    authenticate(&store, &student_no, &password)
}

/// 处理学生认证相关子命令
pub fn handle_auth_command(args: &[String], db_path: &Path) {
    if args.is_empty() {
        // 交互式认证
        match interactive_auth(db_path) {
            Ok(result) => {
                println!("✅ 认证成功！欢迎, {}", result.student_name);
                println!("   Token: {}...", &result.token[..16]);
            }
            Err(EduError::Auth(msg)) => {
                eprintln!("❌ 认证失败: {msg}");
            }
            Err(e) => {
                eprintln!("❌ {e}");
            }
        }
        return;
    }

    match args[0].as_str() {
        "login" => {
            let student_no = args.get(1).cloned().unwrap_or_default();
            let password = args.get(2).cloned().unwrap_or_default();
            if student_no.is_empty() || password.is_empty() {
                eprintln!("用法: rhermes edu auth login <学号> <密码>");
                return;
            }
            let store = match EduStore::open(db_path) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("❌ {e}");
                    return;
                }
            };
            match authenticate(&store, &student_no, &password) {
                Ok(result) => {
                    println!("✅ 认证成功！欢迎, {}", result.student_name);
                    println!("   Token: {}", result.token);
                }
                Err(EduError::Auth(msg)) => eprintln!("❌ {msg}"),
                Err(e) => eprintln!("❌ {e}"),
            }
        }
        "verify" => {
            let token = args.get(1).cloned().unwrap_or_default();
            if token.is_empty() {
                eprintln!("用法: rhermes edu auth verify <token>");
                return;
            }
            let store = match EduStore::open(db_path) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("❌ {e}");
                    return;
                }
            };
            match validate_token(&store, &token) {
                Ok(Some(session)) => {
                    println!("✅ Token 有效 (student_id: {})", session.student_id);
                }
                Ok(None) => println!("❌ Token 无效或已过期"),
                Err(e) => eprintln!("❌ {e}"),
            }
        }
        _ => {
            println!("用法: rhermes edu auth [login <学号> <密码> | verify <token>]");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_with_student() -> (tempfile::TempDir, EduStore) {
        let tmp = tempfile::tempdir().unwrap();
        let store = EduStore::open(tmp.path().join("edu.db")).unwrap();
        store.create_student("2024001", "测试学生", "testpass", None).unwrap();
        (tmp, store)
    }

    #[test]
    fn test_authenticate_success() {
        let (_tmp, store) = setup_with_student();
        let result = authenticate(&store, "2024001", "testpass").unwrap();
        assert!(!result.token.is_empty());
        assert_eq!(result.student_name, "测试学生");
    }

    #[test]
    fn test_authenticate_wrong_password() {
        let (_tmp, store) = setup_with_student();
        let result = authenticate(&store, "2024001", "wrongpass");
        assert!(result.is_err());
        match result.unwrap_err() {
            EduError::Auth(msg) => assert!(msg.contains("密码错误")),
            _ => panic!("应该是 Auth 错误"),
        }
    }

    #[test]
    fn test_authenticate_nonexistent_student() {
        let (_tmp, store) = setup_with_student();
        let result = authenticate(&store, "nonexistent", "pass");
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_token_flow() {
        let (_tmp, store) = setup_with_student();
        let auth = authenticate(&store, "2024001", "testpass").unwrap();

        // 验证 token 有效
        let session = validate_token(&store, &auth.token).unwrap();
        assert!(session.is_some());
        assert_eq!(session.unwrap().student_id, auth.student_id);

        // 无效 token
        let invalid = validate_token(&store, "invalid_token_12345").unwrap();
        assert!(invalid.is_none());
    }

    #[test]
    fn test_token_expiry() {
        let tmp = tempfile::tempdir().unwrap();
        let store = EduStore::open(tmp.path().join("edu.db")).unwrap();
        store.create_student("2024002", "过期测试", "pass", None).unwrap();

        // 直接创建一个已过期的会话
        let expires = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
        let token = store.create_session(1, &expires).unwrap();

        // 验证 → None
        let result = validate_token(&store, &token).unwrap();
        assert!(result.is_none());
    }
}
