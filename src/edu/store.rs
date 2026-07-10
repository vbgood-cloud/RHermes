//! EduStore — 教育数据存储层
//!
//! 基于 SQLite，管理教师/课程/班级/课次/学生/选课/学习日志/认证会话。
//! 密码使用 argon2 哈希。

use std::path::Path;

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// 错误类型
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum EduError {
    Open(rusqlite::Error),
    Execute(rusqlite::Error),
    Query(rusqlite::Error),
    NotFound(String),
    Auth(String),
    Argon(String),
}

impl std::fmt::Display for EduError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EduError::Open(e) => write!(f, "数据库打开失败: {e}"),
            EduError::Execute(e) => write!(f, "数据库执行失败: {e}"),
            EduError::Query(e) => write!(f, "数据库查询失败: {e}"),
            EduError::NotFound(msg) => write!(f, "未找到: {msg}"),
            EduError::Auth(msg) => write!(f, "认证失败: {msg}"),
            EduError::Argon(msg) => write!(f, "密码哈希错误: {msg}"),
        }
    }
}

impl From<rusqlite::Error> for EduError {
    fn from(e: rusqlite::Error) -> Self {
        match e {
            rusqlite::Error::QueryReturnedNoRows => EduError::NotFound("记录不存在".into()),
            _ => EduError::Execute(e),
        }
    }
}

impl std::error::Error for EduError {}

// ---------------------------------------------------------------------------
// 数据模型
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Teacher {
    pub id: i64,
    pub name: String,
    pub node_id: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Course {
    pub id: i64,
    pub course_code: String,
    pub name: String,
    pub teacher_id: i64,
    pub description: String,
    pub tools_whitelist: String,
    pub allowed_modes: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Class {
    pub id: i64,
    pub name: String,
    pub course_id: i64,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lesson {
    pub id: i64,
    pub course_id: i64,
    pub class_id: i64,
    pub lesson_num: i64,
    pub topic: String,
    pub mode_override: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Student {
    pub id: i64,
    pub student_no: String,
    pub name: String,
    pub primary_class_id: Option<i64>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearningJournal {
    pub id: i64,
    pub student_id: i64,
    pub course_id: i64,
    pub lesson_num: i64,
    pub topic: String,
    pub tool_calls: String,
    pub reflection: String,
    pub quality_score: f64,
    pub reflection_depth: f64,
    pub token_usage: i64,
    pub duration_secs: i64,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub token: String,
    pub student_id: i64,
    pub current_course_id: Option<i64>,
    pub current_lesson_num: Option<i64>,
    pub expires_at: String,
    pub created_at: String,
}

// ---------------------------------------------------------------------------
// EduStore
// ---------------------------------------------------------------------------

/// 教育数据存储
pub struct EduStore {
    db: Connection,
}

impl EduStore {
    /// 打开/创建教育数据库
    pub fn open(path: impl AsRef<Path>) -> Result<Self, EduError> {
        let db = Connection::open(path.as_ref()).map_err(EduError::Open)?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;

        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS edu_teachers (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                node_id TEXT DEFAULT '',
                password_hash TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS edu_courses (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                course_code TEXT UNIQUE NOT NULL,
                name TEXT NOT NULL,
                teacher_id INTEGER NOT NULL REFERENCES edu_teachers(id),
                description TEXT DEFAULT '',
                tools_whitelist TEXT DEFAULT '[]',
                allowed_modes TEXT DEFAULT '[\"explore\",\"scaffold\"]',
                created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS edu_classes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                course_id INTEGER NOT NULL REFERENCES edu_courses(id),
                created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS edu_lessons (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                course_id INTEGER NOT NULL REFERENCES edu_courses(id),
                class_id INTEGER NOT NULL REFERENCES edu_classes(id),
                lesson_num INTEGER NOT NULL,
                topic TEXT DEFAULT '',
                mode_override TEXT DEFAULT '',
                created_at TEXT NOT NULL,
                UNIQUE(course_id, class_id, lesson_num)
            );
            CREATE TABLE IF NOT EXISTS edu_students (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                student_no TEXT UNIQUE NOT NULL,
                name TEXT NOT NULL,
                password_hash TEXT NOT NULL,
                primary_class_id INTEGER REFERENCES edu_classes(id),
                created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS edu_enrollments (
                student_id INTEGER NOT NULL REFERENCES edu_students(id),
                course_id INTEGER NOT NULL REFERENCES edu_courses(id),
                enrolled_at TEXT NOT NULL,
                PRIMARY KEY(student_id, course_id)
            );
            CREATE TABLE IF NOT EXISTS edu_learning_journal (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                student_id INTEGER NOT NULL REFERENCES edu_students(id),
                course_id INTEGER NOT NULL REFERENCES edu_courses(id),
                lesson_num INTEGER NOT NULL,
                topic TEXT DEFAULT '',
                tool_calls TEXT DEFAULT '[]',
                reflection TEXT DEFAULT '',
                quality_score REAL DEFAULT 0,
                reflection_depth REAL DEFAULT 0,
                token_usage INTEGER DEFAULT 0,
                duration_secs INTEGER DEFAULT 0,
                created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS edu_sessions (
                token TEXT PRIMARY KEY,
                student_id INTEGER NOT NULL REFERENCES edu_students(id),
                current_course_id INTEGER,
                current_lesson_num INTEGER,
                expires_at TEXT NOT NULL,
                created_at TEXT NOT NULL
            );"
        )?;

        Ok(Self { db })
    }

    fn now() -> String {
        chrono::Utc::now().to_rfc3339()
    }
}

// ---------------------------------------------------------------------------
// 密码哈希
// ---------------------------------------------------------------------------

/// 使用 argon2 哈希密码
pub fn hash_password(plain: &str) -> Result<String, EduError> {
    use argon2::{
        password_hash::{rand_core::OsRng, PasswordHasher, SaltString},
        Argon2,
    };
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    argon2
        .hash_password(plain.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| EduError::Argon(e.to_string()))
}

/// 验证密码
pub fn verify_password(plain: &str, hash: &str) -> Result<bool, EduError> {
    use argon2::{
        password_hash::{PasswordHash, PasswordVerifier},
        Argon2,
    };
    let parsed = PasswordHash::new(hash).map_err(|e| EduError::Argon(e.to_string()))?;
    Ok(Argon2::default()
        .verify_password(plain.as_bytes(), &parsed)
        .is_ok())
}

// ---------------------------------------------------------------------------
// 教师 CRUD
// ---------------------------------------------------------------------------

impl EduStore {
    /// 创建教师
    pub fn create_teacher(&self, name: &str, password: &str) -> Result<Teacher, EduError> {
        let hash = hash_password(password)?;
        let now = Self::now();
        self.db.execute(
            "INSERT INTO edu_teachers (name, node_id, password_hash, created_at) VALUES (?1, '', ?2, ?3)",
            params![name, hash, now],
        )?;
        let id = self.db.last_insert_rowid();
        Ok(Teacher {
            id,
            name: name.to_string(),
            node_id: String::new(),
            created_at: now,
        })
    }

    /// 获取教师
    pub fn get_teacher(&self, id: i64) -> Result<Option<Teacher>, EduError> {
        let mut stmt = self.db.prepare(
            "SELECT id, name, node_id, created_at FROM edu_teachers WHERE id = ?1",
        )?;
        let result = stmt
            .query_row(params![id], |row| {
                Ok(Teacher {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    node_id: row.get(2)?,
                    created_at: row.get(3)?,
                })
            })
            .ok();
        Ok(result)
    }

    /// 验证教师密码
    pub fn verify_teacher_password(&self, name: &str, password: &str) -> Result<bool, EduError> {
        let mut stmt = self
            .db
            .prepare("SELECT password_hash FROM edu_teachers WHERE name = ?1")?;
        let hash: String = stmt
            .query_row(params![name], |row| row.get(0))
            .map_err(|_| EduError::NotFound(format!("教师 '{name}' 不存在")))?;
        verify_password(password, &hash)
    }
}

// ---------------------------------------------------------------------------
// 课程 CRUD
// ---------------------------------------------------------------------------

impl EduStore {
    /// 创建课程
    pub fn create_course(
        &self,
        course_code: &str,
        name: &str,
        teacher_id: i64,
    ) -> Result<Course, EduError> {
        let now = Self::now();
        self.db.execute(
            "INSERT INTO edu_courses (course_code, name, teacher_id, description, tools_whitelist, allowed_modes, created_at)
             VALUES (?1, ?2, ?3, '', '[]', '[\"explore\",\"scaffold\"]', ?4)",
            params![course_code, name, teacher_id, now],
        )?;
        let id = self.db.last_insert_rowid();
        Ok(Course {
            id,
            course_code: course_code.to_string(),
            name: name.to_string(),
            teacher_id,
            description: String::new(),
            tools_whitelist: "[]".to_string(),
            allowed_modes: "[\"explore\",\"scaffold\"]".to_string(),
            created_at: now,
        })
    }

    /// 获取课程
    pub fn get_course(&self, course_code: &str) -> Result<Option<Course>, EduError> {
        let mut stmt = self.db.prepare(
            "SELECT id, course_code, name, teacher_id, description, tools_whitelist, allowed_modes, created_at
             FROM edu_courses WHERE course_code = ?1",
        )?;
        let result = stmt
            .query_row(params![course_code], |row| {
                Ok(Course {
                    id: row.get(0)?,
                    course_code: row.get(1)?,
                    name: row.get(2)?,
                    teacher_id: row.get(3)?,
                    description: row.get(4)?,
                    tools_whitelist: row.get(5)?,
                    allowed_modes: row.get(6)?,
                    created_at: row.get(7)?,
                })
            })
            .ok();
        Ok(result)
    }

    /// 列出教师的课程
    pub fn list_courses_by_teacher(&self, teacher_id: i64) -> Result<Vec<Course>, EduError> {
        let mut stmt = self.db.prepare(
            "SELECT id, course_code, name, teacher_id, description, tools_whitelist, allowed_modes, created_at
             FROM edu_courses WHERE teacher_id = ?1 ORDER BY created_at",
        )?;
        let courses = stmt
            .query_map(params![teacher_id], |row| {
                Ok(Course {
                    id: row.get(0)?,
                    course_code: row.get(1)?,
                    name: row.get(2)?,
                    teacher_id: row.get(3)?,
                    description: row.get(4)?,
                    tools_whitelist: row.get(5)?,
                    allowed_modes: row.get(6)?,
                    created_at: row.get(7)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(courses)
    }
}

// ---------------------------------------------------------------------------
// 班级 CRUD
// ---------------------------------------------------------------------------

impl EduStore {
    pub fn create_class(&self, name: &str, course_id: i64) -> Result<Class, EduError> {
        let now = Self::now();
        self.db.execute(
            "INSERT INTO edu_classes (name, course_id, created_at) VALUES (?1, ?2, ?3)",
            params![name, course_id, now],
        )?;
        let id = self.db.last_insert_rowid();
        Ok(Class {
            id,
            name: name.to_string(),
            course_id,
            created_at: now,
        })
    }

    pub fn get_classes_by_course(&self, course_id: i64) -> Result<Vec<Class>, EduError> {
        let mut stmt = self
            .db
            .prepare("SELECT id, name, course_id, created_at FROM edu_classes WHERE course_id = ?1")?;
        let classes = stmt
            .query_map(params![course_id], |row| {
                Ok(Class {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    course_id: row.get(2)?,
                    created_at: row.get(3)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(classes)
    }
}

// ---------------------------------------------------------------------------
// 课次 CRUD
// ---------------------------------------------------------------------------

impl EduStore {
    pub fn create_lesson(
        &self,
        course_id: i64,
        class_id: i64,
        lesson_num: i64,
        topic: &str,
    ) -> Result<Lesson, EduError> {
        let now = Self::now();
        self.db.execute(
            "INSERT INTO edu_lessons (course_id, class_id, lesson_num, topic, mode_override, created_at)
             VALUES (?1, ?2, ?3, ?4, '', ?5)",
            params![course_id, class_id, lesson_num, topic, now],
        )?;
        let id = self.db.last_insert_rowid();
        Ok(Lesson {
            id,
            course_id,
            class_id,
            lesson_num,
            topic: topic.to_string(),
            mode_override: String::new(),
            created_at: now,
        })
    }

    pub fn get_lessons(&self, course_id: i64, class_id: i64) -> Result<Vec<Lesson>, EduError> {
        let mut stmt = self.db.prepare(
            "SELECT id, course_id, class_id, lesson_num, topic, mode_override, created_at
             FROM edu_lessons WHERE course_id = ?1 AND class_id = ?2 ORDER BY lesson_num",
        )?;
        let lessons = stmt
            .query_map(params![course_id, class_id], |row| {
                Ok(Lesson {
                    id: row.get(0)?,
                    course_id: row.get(1)?,
                    class_id: row.get(2)?,
                    lesson_num: row.get(3)?,
                    topic: row.get(4)?,
                    mode_override: row.get(5)?,
                    created_at: row.get(6)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(lessons)
    }
}

// ---------------------------------------------------------------------------
// 学生 CRUD
// ---------------------------------------------------------------------------

impl EduStore {
    pub fn create_student(
        &self,
        student_no: &str,
        name: &str,
        password: &str,
        primary_class_id: Option<i64>,
    ) -> Result<Student, EduError> {
        let hash = hash_password(password)?;
        let now = Self::now();
        self.db.execute(
            "INSERT INTO edu_students (student_no, name, password_hash, primary_class_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![student_no, name, hash, primary_class_id, now],
        )?;
        let id = self.db.last_insert_rowid();
        Ok(Student {
            id,
            student_no: student_no.to_string(),
            name: name.to_string(),
            primary_class_id,
            created_at: now,
        })
    }

    pub fn get_student(&self, student_no: &str) -> Result<Option<Student>, EduError> {
        let mut stmt = self.db.prepare(
            "SELECT id, student_no, name, primary_class_id, created_at FROM edu_students WHERE student_no = ?1",
        )?;
        let result = stmt
            .query_row(params![student_no], |row| {
                Ok(Student {
                    id: row.get(0)?,
                    student_no: row.get(1)?,
                    name: row.get(2)?,
                    primary_class_id: row.get(3)?,
                    created_at: row.get(4)?,
                })
            })
            .ok();
        Ok(result)
    }

    pub fn verify_student_password(
        &self,
        student_no: &str,
        password: &str,
    ) -> Result<bool, EduError> {
        let mut stmt = self
            .db
            .prepare("SELECT password_hash FROM edu_students WHERE student_no = ?1")?;
        let hash: String = stmt
            .query_row(params![student_no], |row| row.get(0))
            .map_err(|_| EduError::NotFound(format!("学号 '{student_no}' 不存在")))?;
        verify_password(password, &hash)
    }
}

// ---------------------------------------------------------------------------
// 选课
// ---------------------------------------------------------------------------

impl EduStore {
    pub fn enroll(&self, student_id: i64, course_id: i64) -> Result<(), EduError> {
        let now = Self::now();
        self.db.execute(
            "INSERT OR IGNORE INTO edu_enrollments (student_id, course_id, enrolled_at) VALUES (?1, ?2, ?3)",
            params![student_id, course_id, now],
        )?;
        Ok(())
    }

    pub fn get_student_courses(&self, student_id: i64) -> Result<Vec<Course>, EduError> {
        let mut stmt = self.db.prepare(
            "SELECT c.id, c.course_code, c.name, c.teacher_id, c.description, c.tools_whitelist, c.allowed_modes, c.created_at
             FROM edu_courses c
             INNER JOIN edu_enrollments e ON c.id = e.course_id
             WHERE e.student_id = ?1 ORDER BY c.created_at",
        )?;
        let courses = stmt
            .query_map(params![student_id], |row| {
                Ok(Course {
                    id: row.get(0)?,
                    course_code: row.get(1)?,
                    name: row.get(2)?,
                    teacher_id: row.get(3)?,
                    description: row.get(4)?,
                    tools_whitelist: row.get(5)?,
                    allowed_modes: row.get(6)?,
                    created_at: row.get(7)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(courses)
    }
}

// ---------------------------------------------------------------------------
// 学习日志
// ---------------------------------------------------------------------------

impl EduStore {
    pub fn write_journal(
        &self,
        student_id: i64,
        course_id: i64,
        lesson_num: i64,
        topic: &str,
        tool_calls: &str,
        reflection: &str,
        quality_score: f64,
        reflection_depth: f64,
        token_usage: i64,
        duration_secs: i64,
    ) -> Result<i64, EduError> {
        let now = Self::now();
        self.db.execute(
            "INSERT INTO edu_learning_journal
             (student_id, course_id, lesson_num, topic, tool_calls, reflection, quality_score, reflection_depth, token_usage, duration_secs, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![student_id, course_id, lesson_num, topic, tool_calls, reflection, quality_score, reflection_depth, token_usage, duration_secs, now],
        )?;
        Ok(self.db.last_insert_rowid())
    }

    pub fn get_student_journal(
        &self,
        student_id: i64,
        course_id: Option<i64>,
    ) -> Result<Vec<LearningJournal>, EduError> {
        let (sql, params_vec): (String, Vec<Box<dyn rusqlite::ToSql>>) = if let Some(cid) = course_id {
            (
                "SELECT id, student_id, course_id, lesson_num, topic, tool_calls, reflection, quality_score, reflection_depth, token_usage, duration_secs, created_at
                 FROM edu_learning_journal WHERE student_id = ?1 AND course_id = ?2 ORDER BY created_at"
                    .to_string(),
                vec![Box::new(student_id), Box::new(cid)],
            )
        } else {
            (
                "SELECT id, student_id, course_id, lesson_num, topic, tool_calls, reflection, quality_score, reflection_depth, token_usage, duration_secs, created_at
                 FROM edu_learning_journal WHERE student_id = ?1 ORDER BY created_at"
                    .to_string(),
                vec![Box::new(student_id)],
            )
        };
        let mut stmt = self.db.prepare(&sql)?;
        let param_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|p| p.as_ref()).collect();
        let journals = stmt
            .query_map(rusqlite::params_from_iter(param_refs.iter()), |row| {
                Ok(LearningJournal {
                    id: row.get(0)?,
                    student_id: row.get(1)?,
                    course_id: row.get(2)?,
                    lesson_num: row.get(3)?,
                    topic: row.get(4)?,
                    tool_calls: row.get(5)?,
                    reflection: row.get(6)?,
                    quality_score: row.get(7)?,
                    reflection_depth: row.get(8)?,
                    token_usage: row.get(9)?,
                    duration_secs: row.get(10)?,
                    created_at: row.get(11)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(journals)
    }
}

// ---------------------------------------------------------------------------
// 认证会话
// ---------------------------------------------------------------------------

impl EduStore {
    pub fn create_session(
        &self,
        student_id: i64,
        expires_at: &str,
    ) -> Result<String, EduError> {
        let token = generate_token();
        let now = Self::now();
        self.db.execute(
            "INSERT INTO edu_sessions (token, student_id, current_course_id, current_lesson_num, expires_at, created_at)
             VALUES (?1, ?2, NULL, NULL, ?3, ?4)",
            params![token, student_id, expires_at, now],
        )?;
        Ok(token)
    }

    pub fn validate_session(&self, token: &str) -> Result<Option<SessionInfo>, EduError> {
        let mut stmt = self.db.prepare(
            "SELECT token, student_id, current_course_id, current_lesson_num, expires_at, created_at
             FROM edu_sessions WHERE token = ?1",
        )?;
        let result = stmt
            .query_row(params![token], |row| {
                Ok(SessionInfo {
                    token: row.get(0)?,
                    student_id: row.get(1)?,
                    current_course_id: row.get(2)?,
                    current_lesson_num: row.get(3)?,
                    expires_at: row.get(4)?,
                    created_at: row.get(5)?,
                })
            })
            .ok();

        // 检查过期
        if let Some(ref session) = result {
            let now = chrono::Utc::now();
            if let Ok(expires) = chrono::DateTime::parse_from_rfc3339(&session.expires_at) {
                if now > expires.with_timezone(&now.timezone()) {
                    // 过期了，删除
                    let _ = self.db.execute(
                        "DELETE FROM edu_sessions WHERE token = ?1",
                        params![token],
                    );
                    return Ok(None);
                }
            }
        }

        Ok(result)
    }

    pub fn update_session_course(
        &self,
        token: &str,
        course_id: Option<i64>,
        lesson_num: Option<i64>,
    ) -> Result<(), EduError> {
        self.db.execute(
            "UPDATE edu_sessions SET current_course_id = ?1, current_lesson_num = ?2 WHERE token = ?3",
            params![course_id, lesson_num, token],
        )?;
        Ok(())
    }
}

/// 生成随机 32 字节 hex token
fn generate_token() -> String {
    use rand::RngCore;
    let mut buf = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut buf);
    buf.iter().map(|b| format!("{:02x}", b)).collect()
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_store() -> (tempfile::TempDir, EduStore) {
        let tmp = tempfile::tempdir().unwrap();
        let store = EduStore::open(tmp.path().join("edu.db")).unwrap();
        (tmp, store)
    }

    #[test]
    fn test_password_hash_verify() {
        let hash = hash_password("abc123").unwrap();
        assert!(verify_password("abc123", &hash).unwrap());
        assert!(!verify_password("wrong", &hash).unwrap());
    }

    #[test]
    fn test_teacher_crud() {
        let (_tmp, store) = setup_store();

        // 创建
        let teacher = store.create_teacher("张老师", "password123").unwrap();
        assert!(teacher.id > 0);

        // 查询
        let found = store.get_teacher(teacher.id).unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "张老师");

        // 验证密码
        assert!(store.verify_teacher_password("张老师", "password123").unwrap());
        assert!(!store.verify_teacher_password("张老师", "wrong").unwrap());
    }

    #[test]
    fn test_course_crud() {
        let (_tmp, store) = setup_store();
        let teacher = store.create_teacher("李老师", "pass").unwrap();

        // 创建课程
        let course = store.create_course("CS101", "Python 编程基础", teacher.id).unwrap();
        assert_eq!(course.course_code, "CS101");

        // 查询
        let found = store.get_course("CS101").unwrap().unwrap();
        assert_eq!(found.name, "Python 编程基础");

        // 按教师列出
        let courses = store.list_courses_by_teacher(teacher.id).unwrap();
        assert_eq!(courses.len(), 1);
    }

    #[test]
    fn test_class_and_lesson() {
        let (_tmp, store) = setup_store();
        let teacher = store.create_teacher("王老师", "pass").unwrap();
        let course = store.create_course("CS201", "数据结构", teacher.id).unwrap();

        // 创建班级
        let class = store.create_class("计算机2301", course.id).unwrap();
        assert_eq!(class.name, "计算机2301");

        // 创建课次
        let lesson1 = store.create_lesson(course.id, class.id, 1, "线性表").unwrap();
        let lesson2 = store.create_lesson(course.id, class.id, 2, "栈和队列").unwrap();

        // 查询课次
        let lessons = store.get_lessons(course.id, class.id).unwrap();
        assert_eq!(lessons.len(), 2);
        assert_eq!(lessons[0].topic, "线性表");
        assert_eq!(lessons[1].topic, "栈和队列");
    }

    #[test]
    fn test_student_enrollment() {
        let (_tmp, store) = setup_store();
        let teacher = store.create_teacher("陈老师", "pass").unwrap();
        let course = store.create_course("CS301", "AI 导论", teacher.id).unwrap();
        let class = store.create_class("信管2301", course.id).unwrap();

        // 创建学生
        let student = store
            .create_student("2024001", "张三", "stu123", Some(class.id))
            .unwrap();
        assert_eq!(student.student_no, "2024001");

        // 选课
        store.enroll(student.id, course.id).unwrap();

        // 查询选课
        let courses = store.get_student_courses(student.id).unwrap();
        assert_eq!(courses.len(), 1);
        assert_eq!(courses[0].course_code, "CS301");
    }

    #[test]
    fn test_student_password() {
        let (_tmp, store) = setup_store();
        store.create_student("2024002", "李四", "mypassword", None).unwrap();

        assert!(store.verify_student_password("2024002", "mypassword").unwrap());
        assert!(!store.verify_student_password("2024002", "wrong").unwrap());
    }

    #[test]
    fn test_learning_journal() {
        let (_tmp, store) = setup_store();
        let teacher = store.create_teacher("老师", "pass").unwrap();
        let course = store.create_course("CS401", "操作系统", teacher.id).unwrap();
        let student = store.create_student("2024003", "王五", "pass", None).unwrap();

        // 写日志
        let journal_id = store
            .write_journal(
                student.id,
                course.id,
                1,
                "进程调度",
                "[{\"tool\":\"read_file\",\"success\":true}]",
                "我学会了进程调度算法",
                0.75,
                0.68,
                1500,
                120,
            )
            .unwrap();
        assert!(journal_id > 0);

        // 查询
        let journals = store.get_student_journal(student.id, Some(course.id)).unwrap();
        assert_eq!(journals.len(), 1);
        assert_eq!(journals[0].topic, "进程调度");
        assert!((journals[0].quality_score - 0.75).abs() < 0.01);

        // 无课程过滤
        let all = store.get_student_journal(student.id, None).unwrap();
        assert_eq!(all.len(), 1);
    }

    #[test]
    fn test_session_create_validate() {
        let (_tmp, store) = setup_store();
        let student = store.create_student("2024004", "赵六", "pass", None).unwrap();

        // 未来时间过期
        let expires = (chrono::Utc::now() + chrono::Duration::hours(24)).to_rfc3339();
        let token = store.create_session(student.id, &expires).unwrap();
        assert!(!token.is_empty());

        // 验证
        let session = store.validate_session(&token).unwrap();
        assert!(session.is_some());
        assert_eq!(session.unwrap().student_id, student.id);

        // 更新课程
        store.update_session_course(&token, Some(1), Some(3)).unwrap();
        let updated = store.validate_session(&token).unwrap().unwrap();
        assert_eq!(updated.current_course_id, Some(1));
        assert_eq!(updated.current_lesson_num, Some(3));
    }

    #[test]
    fn test_session_expired() {
        let (_tmp, store) = setup_store();
        let student = store.create_student("2024005", "钱七", "pass", None).unwrap();

        // 过去时间过期
        let expires = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
        let token = store.create_session(student.id, &expires).unwrap();

        // 验证 → 已过期
        let session = store.validate_session(&token).unwrap();
        assert!(session.is_none());
    }
}
