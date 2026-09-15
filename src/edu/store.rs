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
    Db(String),
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
            EduError::Db(msg) => write!(f, "数据库错误: {msg}"),
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
// 新增数据模型
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Assignment {
    pub id: i64,
    pub course_id: i64,
    pub title: String,
    pub description: String,
    pub lesson_num: Option<i64>,
    pub due_date: String,
    pub allowed_mode: String,
    pub max_attempts: i64,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassPublish {
    pub id: i64,
    pub class_id: i64,
    pub course_id: i64,
    pub content_type: String,  // "lesson" / "assignment"
    pub content_id: i64,
    pub published_at: String,
    pub published_by: String,
}

/// 按头（班级）的课程参数覆盖记录
///
/// 课程（Course）作为模板，每个头（Class）导入后可独立修改参数。
/// 每个字段为 `None` 时继承课程模板值，为 `Some` 时该头独立。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassCourseOverride {
    pub class_id: i64,
    pub course_id: i64,
    /// NULL = 继承模板
    pub tools_whitelist_override: Option<String>,
    pub description_override: Option<String>,
    pub allowed_modes_override: Option<String>,
    pub imported_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Submission {
    pub id: i64,
    pub assignment_id: i64,
    pub student_id: i64,
    pub content: String,
    pub file_path: String,
    pub submitted_at: String,
    pub ai_score: f64,
    pub ai_feedback: String,
    pub teacher_score: f64,
    pub teacher_feedback: String,
    pub evaluated_at: String,
    pub status: String,
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
            
            -- 课次表（课程级共享资源，不再绑定班级）
            CREATE TABLE IF NOT EXISTS edu_lessons_v2 (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                course_id INTEGER NOT NULL REFERENCES edu_courses(id),
                lesson_num INTEGER NOT NULL,
                topic TEXT DEFAULT '',
                mode_override TEXT DEFAULT '',
                created_at TEXT NOT NULL,
                UNIQUE(course_id, lesson_num)
            );
            
            -- 作业表
            CREATE TABLE IF NOT EXISTS edu_assignments (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                course_id INTEGER NOT NULL REFERENCES edu_courses(id),
                title TEXT NOT NULL,
                description TEXT DEFAULT '',
                lesson_num INTEGER,
                due_date TEXT DEFAULT '',
                allowed_mode TEXT DEFAULT 'explore',
                max_attempts INTEGER DEFAULT 3,
                created_at TEXT NOT NULL
            );
            
            -- 班级发布状态表
            CREATE TABLE IF NOT EXISTS edu_class_publish (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                class_id INTEGER NOT NULL REFERENCES edu_classes(id),
                course_id INTEGER NOT NULL REFERENCES edu_courses(id),
                content_type TEXT NOT NULL,
                content_id INTEGER NOT NULL,
                published_at TEXT NOT NULL,
                published_by TEXT DEFAULT 'manual',
                UNIQUE(class_id, content_type, content_id)
            );
            
            -- 作业提交表
            CREATE TABLE IF NOT EXISTS edu_submissions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                assignment_id INTEGER NOT NULL REFERENCES edu_assignments(id),
                student_id INTEGER NOT NULL REFERENCES edu_students(id),
                content TEXT DEFAULT '',
                file_path TEXT DEFAULT '',
                submitted_at TEXT NOT NULL,
                ai_score REAL DEFAULT 0,
                ai_feedback TEXT DEFAULT '',
                teacher_score REAL DEFAULT -1,
                teacher_feedback TEXT DEFAULT '',
                evaluated_at TEXT DEFAULT '',
                status TEXT DEFAULT 'submitted',
                UNIQUE(assignment_id, student_id)
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
            );
            -- 按头（班级）的课程参数覆盖表
            --
            -- 课程（edu_courses）作为模板：tools_whitelist / description / allowed_modes 全局共享。
            -- 每个头导入课程后可以独立修改这些参数。NULL = 继承模板值，非 NULL = 该头独立覆盖。
            CREATE TABLE IF NOT EXISTS edu_class_course_overrides (
                class_id INTEGER NOT NULL REFERENCES edu_classes(id),
                course_id INTEGER NOT NULL REFERENCES edu_courses(id),
                tools_whitelist_override TEXT,      -- NULL=继承模板，否则该头独立
                description_override TEXT,          -- NULL=继承模板
                allowed_modes_override TEXT,        -- NULL=继承模板
                imported_at TEXT NOT NULL,          -- 课程导入到该头的时间
                PRIMARY KEY(class_id, course_id)
            );"
        )?;

        let store = Self { db };
        // 幂等迁移：把 edu_classes 语义升级为「教学班(Section)」，并建立成员/设备绑定
        store.migrate()?;
        Ok(store)
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
        // 决策 1B：教学班必须自带「任课老师 + 独立 Topic 种子」，
        // 否则迁移之后新建的班会没有 Topic（群组通信直接不可用）。
        self.db.execute(
            "INSERT INTO edu_classes
                (name, course_id, teacher_id, term, gossip_seed, topic_epoch, created_at)
             VALUES (?1, ?2,
                     (SELECT teacher_id FROM edu_courses WHERE id = ?2),
                     '', randomblob(32), 0, ?3)",
            params![name, course_id, now],
        )?;
        let id = self.db.last_insert_rowid();
        // 老师自动成为该班成员 —— 与迁移回填保持同一不变量
        self.db.execute(
            "INSERT OR IGNORE INTO edu_section_members
                (section_id, user_id, role, username, display_name, admin_class, authorized, joined_at)
             SELECT ?1, t.id, 'teacher', t.name, t.name, '', 0, ?2
             FROM edu_teachers t
             WHERE t.id = (SELECT teacher_id FROM edu_courses WHERE id = ?3)",
            params![id, now, course_id],
        )?;
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
// 按头（班级）的课程参数覆盖 CRUD
// ---------------------------------------------------------------------------

impl EduStore {
    /// 导入课程到头：创建覆盖记录（所有字段初始为 NULL = 继承模板）。
    ///
    /// 若已导入过（记录已存在）则不重复创建，返回现有记录。
    pub fn import_course_to_class(
        &self,
        class_id: i64,
        course_id: i64,
    ) -> Result<ClassCourseOverride, EduError> {
        let now = Self::now();
        // INSERT OR IGNORE：已导入则跳过，保持已有覆盖不变
        self.db.execute(
            "INSERT OR IGNORE INTO edu_class_course_overrides
             (class_id, course_id, tools_whitelist_override, description_override, allowed_modes_override, imported_at)
             VALUES (?1, ?2, NULL, NULL, NULL, ?3)",
            params![class_id, course_id, now],
        )?;
        // 回读记录
        self.get_class_course_override(class_id, course_id)?
            .ok_or_else(|| EduError::Db("导入后回读覆盖记录失败".into()))
    }

    /// 读取某个头的课程覆盖记录（None = 该头未导入此课程）
    pub fn get_class_course_override(
        &self,
        class_id: i64,
        course_id: i64,
    ) -> Result<Option<ClassCourseOverride>, EduError> {
        let result = self
            .db
            .prepare(
                "SELECT class_id, course_id, tools_whitelist_override, description_override,
                        allowed_modes_override, imported_at
                 FROM edu_class_course_overrides WHERE class_id = ?1 AND course_id = ?2",
            )?
            .query_row(params![class_id, course_id], |row| {
                Ok(ClassCourseOverride {
                    class_id: row.get(0)?,
                    course_id: row.get(1)?,
                    tools_whitelist_override: row.get(2)?,
                    description_override: row.get(3)?,
                    allowed_modes_override: row.get(4)?,
                    imported_at: row.get(5)?,
                })
            })
            .ok();
        Ok(result)
    }

    /// 更新某个头的课程参数覆盖（只更新提供的字段，None 字段保持不变）。
    ///
    /// 若该头未导入课程，返回错误。
    pub fn update_class_course_override(
        &self,
        class_id: i64,
        course_id: i64,
        tools_whitelist: Option<&str>,
        description: Option<&str>,
        allowed_modes: Option<&str>,
    ) -> Result<(), EduError> {
        // 动态拼接 SET 子句：只更新提供的字段
        let mut sets: Vec<&str> = Vec::new();
        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if let Some(v) = tools_whitelist {
            sets.push("tools_whitelist_override = ?");
            params_vec.push(Box::new(v.to_string()));
        }
        if let Some(v) = description {
            sets.push("description_override = ?");
            params_vec.push(Box::new(v.to_string()));
        }
        if let Some(v) = allowed_modes {
            sets.push("allowed_modes_override = ?");
            params_vec.push(Box::new(v.to_string()));
        }

        if sets.is_empty() {
            return Ok(()); // 没有要更新的字段
        }

        let sql = format!(
            "UPDATE edu_class_course_overrides SET {} WHERE class_id = ? AND course_id = ?",
            sets.join(", ")
        );
        params_vec.push(Box::new(class_id));
        params_vec.push(Box::new(course_id));

        let affected = self.db.execute(&sql, rusqlite::params_from_iter(params_vec))?;
        if affected == 0 {
            return Err(EduError::NotFound(format!(
                "头(class_id={class_id})未导入课程(course_id={course_id})，请先 /import"
            )));
        }
        Ok(())
    }

    /// 合并课程模板 + 头覆盖，得到该头实际生效的课程参数。
    ///
    /// 覆盖字段为 NULL 时用模板值；否则用覆盖值。
    /// 若该头未导入（无覆盖记录），则全部用模板值。
    pub fn resolve_course_for_class(
        &self,
        class_id: i64,
        course: &Course,
    ) -> Result<Course, EduError> {
        match self.get_class_course_override(class_id, course.id)? {
            None => Ok(course.clone()),
            Some(ov) => Ok(Course {
                id: course.id,
                course_code: course.course_code.clone(),
                name: course.name.clone(),
                teacher_id: course.teacher_id,
                description: ov.description_override.unwrap_or_else(|| course.description.clone()),
                tools_whitelist: ov
                    .tools_whitelist_override
                    .unwrap_or_else(|| course.tools_whitelist.clone()),
                allowed_modes: ov
                    .allowed_modes_override
                    .unwrap_or_else(|| course.allowed_modes.clone()),
                created_at: course.created_at.clone(),
            }),
        }
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
        // 决策 1B：学生一旦指定教学班，就必须在 edu_section_members 里留下成员行，
        // 否则 `bind_endpoint` / `sections_for_student` / `revoke_member` 全部落空
        // （迁移回填只覆盖老数据，新建学生同样需要）。列取值方式与迁移回填保持一致。
        if primary_class_id.is_some() {
            self.db.execute(
                "INSERT OR IGNORE INTO edu_section_members
                    (section_id, user_id, role, username, display_name, admin_class, authorized, joined_at)
                 SELECT ?1, s.id, 'student', s.student_no, s.name,
                        COALESCE(s.admin_class, ''), 0, ?2
                 FROM edu_students s WHERE s.id = ?3",
                params![primary_class_id, now, id],
            )?;
        }
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

    // =====================================================================
    // 课次 v2（课程级共享资源）
    // =====================================================================

    /// 创建课次（课程级，无班级绑定）
    pub fn create_lesson_v2(&self, course_id: i64, lesson_num: i64, topic: &str) -> Result<i64, EduError> {
        let now = Self::now();
        self.db.execute(
            "INSERT INTO edu_lessons_v2 (course_id, lesson_num, topic, mode_override, created_at)
             VALUES (?1, ?2, ?3, '', ?4)",
            params![course_id, lesson_num, topic, now],
        )?;
        Ok(self.db.last_insert_rowid())
    }

    /// 获取课程的所有课次
    pub fn get_lessons_v2(&self, course_id: i64) -> Result<Vec<Lesson>, EduError> {
        let mut stmt = self.db.prepare(
            "SELECT id, course_id, 0 as class_id, lesson_num, topic, mode_override, created_at
             FROM edu_lessons_v2 WHERE course_id = ?1 ORDER BY lesson_num",
        )?;
        let lessons = stmt
            .query_map(params![course_id], |row| {
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

    /// 修改课次主题（所有班级自动同步）
    pub fn update_lesson_topic(&self, course_id: i64, lesson_num: i64, new_topic: &str) -> Result<(), EduError> {
        self.db.execute(
            "UPDATE edu_lessons_v2 SET topic = ?1 WHERE course_id = ?2 AND lesson_num = ?3",
            params![new_topic, course_id, lesson_num],
        )?;
        Ok(())
    }

    /// 删除课次
    pub fn delete_lesson_v2(&self, course_id: i64, lesson_num: i64) -> Result<bool, EduError> {
        let rows = self.db.execute(
            "DELETE FROM edu_lessons_v2 WHERE course_id = ?1 AND lesson_num = ?2",
            params![course_id, lesson_num],
        )?;
        Ok(rows > 0)
    }

    // =====================================================================
    // 作业
    // =====================================================================

    pub fn create_assignment(&self, course_id: i64, title: &str, description: &str) -> Result<i64, EduError> {
        let now = Self::now();
        self.db.execute(
            "INSERT INTO edu_assignments (course_id, title, description, lesson_num, due_date, allowed_mode, max_attempts, created_at)
             VALUES (?1, ?2, ?3, NULL, '', 'explore', 3, ?4)",
            params![course_id, title, description, now],
        )?;
        Ok(self.db.last_insert_rowid())
    }

    pub fn get_assignments(&self, course_id: i64) -> Result<Vec<Assignment>, EduError> {
        let mut stmt = self.db.prepare(
            "SELECT id, course_id, title, description, lesson_num, due_date, allowed_mode, max_attempts, created_at
             FROM edu_assignments WHERE course_id = ?1 ORDER BY created_at",
        )?;
        let assignments = stmt
            .query_map(params![course_id], |row| {
                Ok(Assignment {
                    id: row.get(0)?,
                    course_id: row.get(1)?,
                    title: row.get(2)?,
                    description: row.get(3)?,
                    lesson_num: row.get(4)?,
                    due_date: row.get(5)?,
                    allowed_mode: row.get(6)?,
                    max_attempts: row.get(7)?,
                    created_at: row.get(8)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(assignments)
    }

    pub fn update_assignment(&self, id: i64, title: Option<&str>, description: Option<&str>, due_date: Option<&str>) -> Result<(), EduError> {
        if let Some(t) = title {
            self.db.execute("UPDATE edu_assignments SET title = ?1 WHERE id = ?2", params![t, id])?;
        }
        if let Some(d) = description {
            self.db.execute("UPDATE edu_assignments SET description = ?1 WHERE id = ?2", params![d, id])?;
        }
        if let Some(dd) = due_date {
            self.db.execute("UPDATE edu_assignments SET due_date = ?1 WHERE id = ?2", params![dd, id])?;
        }
        Ok(())
    }

    // =====================================================================
    // 班级发布
    // =====================================================================

    pub fn publish_to_class(&self, class_id: i64, course_id: i64, content_type: &str, content_id: i64) -> Result<(), EduError> {
        let now = Self::now();
        self.db.execute(
            "INSERT OR IGNORE INTO edu_class_publish (class_id, course_id, content_type, content_id, published_at, published_by)
             VALUES (?1, ?2, ?3, ?4, ?5, 'manual')",
            params![class_id, course_id, content_type, content_id, now],
        )?;
        Ok(())
    }

    pub fn unpublish_from_class(&self, class_id: i64, content_type: &str, content_id: i64) -> Result<bool, EduError> {
        let rows = self.db.execute(
            "DELETE FROM edu_class_publish WHERE class_id = ?1 AND content_type = ?2 AND content_id = ?3",
            params![class_id, content_type, content_id],
        )?;
        Ok(rows > 0)
    }

    pub fn is_published(&self, class_id: i64, content_type: &str, content_id: i64) -> Result<bool, EduError> {
        let count: i64 = self.db.query_row(
            "SELECT COUNT(*) FROM edu_class_publish WHERE class_id = ?1 AND content_type = ?2 AND content_id = ?3",
            params![class_id, content_type, content_id],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    pub fn get_published_lessons(&self, class_id: i64, course_id: i64) -> Result<Vec<i64>, EduError> {
        let mut stmt = self.db.prepare(
            "SELECT content_id FROM edu_class_publish WHERE class_id = ?1 AND course_id = ?2 AND content_type = 'lesson'",
        )?;
        let ids = stmt.query_map(params![class_id, course_id], |row| row.get::<_, i64>(0))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(ids)
    }

    pub fn get_published_assignments(&self, class_id: i64, course_id: i64) -> Result<Vec<i64>, EduError> {
        let mut stmt = self.db.prepare(
            "SELECT content_id FROM edu_class_publish WHERE class_id = ?1 AND course_id = ?2 AND content_type = 'assignment'",
        )?;
        let ids = stmt.query_map(params![class_id, course_id], |row| row.get::<_, i64>(0))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(ids)
    }

    // =====================================================================
    // 作业提交
    // =====================================================================

    pub fn submit_assignment(&self, assignment_id: i64, student_id: i64, content: &str, file_path: &str) -> Result<i64, EduError> {
        let now = Self::now();
        // 覆盖提交（INSERT OR REPLACE）
        self.db.execute(
            "INSERT OR REPLACE INTO edu_submissions (assignment_id, student_id, content, file_path, submitted_at, ai_score, ai_feedback, teacher_score, teacher_feedback, evaluated_at, status)
             VALUES (?1, ?2, ?3, ?4, ?5, 0, '', -1, '', '', 'submitted')",
            params![assignment_id, student_id, content, file_path, now],
        )?;
        Ok(self.db.last_insert_rowid())
    }

    pub fn get_submission(&self, assignment_id: i64, student_id: i64) -> Result<Option<Submission>, EduError> {
        let mut stmt = self.db.prepare(
            "SELECT id, assignment_id, student_id, content, file_path, submitted_at, ai_score, ai_feedback, teacher_score, teacher_feedback, evaluated_at, status
             FROM edu_submissions WHERE assignment_id = ?1 AND student_id = ?2",
        )?;
        let result = stmt
            .query_row(params![assignment_id, student_id], |row| {
                Ok(Submission {
                    id: row.get(0)?,
                    assignment_id: row.get(1)?,
                    student_id: row.get(2)?,
                    content: row.get(3)?,
                    file_path: row.get(4)?,
                    submitted_at: row.get(5)?,
                    ai_score: row.get(6)?,
                    ai_feedback: row.get(7)?,
                    teacher_score: row.get(8)?,
                    teacher_feedback: row.get(9)?,
                    evaluated_at: row.get(10)?,
                    status: row.get(11)?,
                })
            })
            .ok();
        Ok(result)
    }

    pub fn update_submission_score(&self, submission_id: i64, ai_score: Option<f64>, ai_feedback: Option<&str>, teacher_score: Option<f64>, teacher_feedback: Option<&str>) -> Result<(), EduError> {
        let now = Self::now();
        if let Some(s) = ai_score {
            self.db.execute("UPDATE edu_submissions SET ai_score = ?1, ai_feedback = ?2, evaluated_at = ?3, status = 'ai_evaluated' WHERE id = ?4", params![s, ai_feedback.unwrap_or(""), now, submission_id])?;
        }
        if let Some(s) = teacher_score {
            self.db.execute("UPDATE edu_submissions SET teacher_score = ?1, teacher_feedback = ?2, evaluated_at = ?3, status = 'teacher_evaluated' WHERE id = ?4", params![s, teacher_feedback.unwrap_or(""), now, submission_id])?;
        }
        Ok(())
    }
}
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
mod teaching_tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, EduStore) {
        let tmp = tempfile::tempdir().unwrap();
        let store = EduStore::open(tmp.path().join("edu.db")).unwrap();
        // 创建基础数据
        store.create_teacher("老师", "p").unwrap();
        store.create_course("TS101", "测试课程", 1).unwrap();
        store.create_class("测试班", 1).unwrap();
        (tmp, store)
    }

    #[test]
    fn test_lesson_v2_crud() {
        let (_tmp, store) = setup();
        // 创建
        let id = store.create_lesson_v2(1, 1, "第一讲").unwrap();
        assert!(id > 0);
        store.create_lesson_v2(1, 2, "第二讲").unwrap();

        // 查询
        let lessons = store.get_lessons_v2(1).unwrap();
        assert_eq!(lessons.len(), 2);

        // 修改
        store.update_lesson_topic(1, 1, "修改后的第一讲").unwrap();
        let lessons = store.get_lessons_v2(1).unwrap();
        assert_eq!(lessons[0].topic, "修改后的第一讲");

        // 删除
        assert!(store.delete_lesson_v2(1, 2).unwrap());
        let lessons = store.get_lessons_v2(1).unwrap();
        assert_eq!(lessons.len(), 1);
    }

    #[test]
    fn test_assignment_crud() {
        let (_tmp, store) = setup();
        let id = store.create_assignment(1, "作业1", "完成计算器").unwrap();
        assert!(id > 0);

        let assignments = store.get_assignments(1).unwrap();
        assert_eq!(assignments.len(), 1);
        assert_eq!(assignments[0].title, "作业1");

        store.update_assignment(id, Some("修改作业"), Some("新描述"), Some("2026-12-31")).unwrap();
        let assignments = store.get_assignments(1).unwrap();
        assert_eq!(assignments[0].title, "修改作业");
        assert_eq!(assignments[0].description, "新描述");
    }

    #[test]
    fn test_publish_to_class() {
        let (_tmp, store) = setup();
        let lesson_id = store.create_lesson_v2(1, 1, "第一讲").unwrap();
        let assignment_id = store.create_assignment(1, "作业1", "描述").unwrap();

        // 发布课次
        store.publish_to_class(1, 1, "lesson", lesson_id).unwrap();
        assert!(store.is_published(1, "lesson", lesson_id).unwrap());

        // 发布作业
        store.publish_to_class(1, 1, "assignment", assignment_id).unwrap();
        assert!(store.is_published(1, "assignment", assignment_id).unwrap());

        // 查询已发布
        let published_lessons = store.get_published_lessons(1, 1).unwrap();
        assert_eq!(published_lessons.len(), 1);

        let published_assignments = store.get_published_assignments(1, 1).unwrap();
        assert_eq!(published_assignments.len(), 1);

        // 撤回
        assert!(store.unpublish_from_class(1, "lesson", lesson_id).unwrap());
        assert!(!store.is_published(1, "lesson", lesson_id).unwrap());
    }

    #[test]
    fn test_submission_crud() {
        let (_tmp, store) = setup();
        store.create_student("2024001", "张三", "p", Some(1)).unwrap();
        let assignment_id = store.create_assignment(1, "作业1", "描述").unwrap();

        // 提交
        store.submit_assignment(assignment_id, 1, "我的答案", "").unwrap();

        // 查询
        let sub = store.get_submission(assignment_id, 1).unwrap().unwrap();
        assert_eq!(sub.content, "我的答案");
        assert_eq!(sub.status, "submitted");

        // AI 评分
        store.update_submission_score(sub.id, Some(0.75), Some("做得不错"), None, None).unwrap();
        let sub = store.get_submission(assignment_id, 1).unwrap().unwrap();
        assert!((sub.ai_score - 0.75).abs() < 0.01);
        assert_eq!(sub.status, "ai_evaluated");

        // 教师评分
        store.update_submission_score(sub.id, None, None, Some(0.85), Some("很好")).unwrap();
        let sub = store.get_submission(assignment_id, 1).unwrap().unwrap();
        assert!((sub.teacher_score - 0.85).abs() < 0.01);
        assert_eq!(sub.status, "teacher_evaluated");
    }

    #[test]
    fn test_lesson_shared_sync() {
        let (_tmp, store) = setup();
        store.create_class("第二个班", 1).unwrap(); // class_id=2

        // 创建课次（共享）
        let lesson_id = store.create_lesson_v2(1, 1, "变量").unwrap();

        // 两个班都发布
        store.publish_to_class(1, 1, "lesson", lesson_id).unwrap();
        store.publish_to_class(2, 1, "lesson", lesson_id).unwrap();

        // 修改课次主题
        store.update_lesson_topic(1, 1, "变量与类型").unwrap();

        // 两个班看到的都是新主题
        let lessons = store.get_lessons_v2(1).unwrap();
        assert_eq!(lessons[0].topic, "变量与类型");

        // 两个班都能看到
        assert!(store.is_published(1, "lesson", lesson_id).unwrap());
        assert!(store.is_published(2, "lesson", lesson_id).unwrap());
    }
}

// ---------------------------------------------------------------------------
// 教学班（Section）与成员（Member）— 决策 1B「规范重构」
// ---------------------------------------------------------------------------

/// 教学班行（`edu_classes` 经语义升级后即为 Section 表）
#[derive(Debug, Clone)]
pub struct SectionRow {
    pub id: i64,
    pub course_id: i64,
    pub teacher_id: i64,
    pub name: String,
    pub term: String,
    /// 该教学班独立的 Topic 种子（32 字节）
    pub gossip_seed: Vec<u8>,
    pub topic_epoch: u32,
    pub created_at: String,
}

/// 学生视角的教学班（含所属课程代码/名称），用于组装 §4.4 的教学班票据
#[derive(Debug, Clone)]
pub struct StudentSectionRow {
    pub section_id: i64,
    pub section_name: String,
    pub course_code: String,
    pub course_name: String,
    pub gossip_seed: Vec<u8>,
    pub topic_epoch: u32,
}

fn row_to_section(r: &rusqlite::Row<'_>) -> rusqlite::Result<SectionRow> {
    Ok(SectionRow {
        id: r.get(0)?,
        course_id: r.get(1)?,
        teacher_id: r.get::<_, Option<i64>>(2)?.unwrap_or(0),
        name: r.get(3)?,
        term: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
        gossip_seed: r.get::<_, Option<Vec<u8>>>(5)?.unwrap_or_default(),
        topic_epoch: r.get::<_, Option<i64>>(6)?.unwrap_or(0) as u32,
        created_at: r.get(7)?,
    })
}

impl EduStore {
    /// 幂等迁移：`edu_classes` 语义升级为教学班 + 建立成员/设备绑定表。
    ///
    /// 走 `PRAGMA user_version`，与 `/learn` 模块同源套路。
    fn migrate(&self) -> Result<(), EduError> {
        let v: i64 = self.db.query_row("PRAGMA user_version", [], |r| r.get(0))?;

        if v >= 2 {
            return Ok(());
        }

        if v < 1 {
        // ── v1-a：列扩展（SQLite 的 ADD COLUMN 无 IF NOT EXISTS，重复执行会报错，故整段容错） ──
        for sql in [
            "ALTER TABLE edu_classes ADD COLUMN teacher_id INTEGER",
            "ALTER TABLE edu_classes ADD COLUMN term TEXT NOT NULL DEFAULT ''",
            "ALTER TABLE edu_classes ADD COLUMN gossip_seed BLOB",
            "ALTER TABLE edu_classes ADD COLUMN topic_epoch INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE edu_students ADD COLUMN endpoint_id TEXT",
            "ALTER TABLE edu_students ADD COLUMN admin_class TEXT NOT NULL DEFAULT ''",
        ] {
            let _ = self.db.execute(sql, []);
        }

        // 回填任课老师：旧模型里课程绑老师，故教学班的老师 = 其课程的 teacher_id
        let _ = self.db.execute(
            "UPDATE edu_classes SET teacher_id =
                (SELECT c.teacher_id FROM edu_courses c WHERE c.id = edu_classes.course_id)
             WHERE teacher_id IS NULL",
            [],
        );

        // 回填 Topic 种子：每个教学班一个独立 32 字节随机种子
        let _ = self.db.execute(
            "UPDATE edu_classes SET gossip_seed = randomblob(32) WHERE gossip_seed IS NULL",
            [],
        );

        // ── v1-b：成员表（老师/学生统一，含设备绑定与行政班级） ──
        self.db.execute_batch(
            "CREATE TABLE IF NOT EXISTS edu_section_members (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                section_id   INTEGER NOT NULL REFERENCES edu_classes(id) ON DELETE CASCADE,
                user_id      INTEGER NOT NULL,
                role         TEXT NOT NULL,
                username     TEXT NOT NULL,
                display_name TEXT NOT NULL DEFAULT '',
                admin_class  TEXT NOT NULL DEFAULT '',
                endpoint_id  TEXT,
                authorized   INTEGER NOT NULL DEFAULT 0,
                joined_at    TEXT NOT NULL,
                last_seen_at TEXT,
                UNIQUE(section_id, role, username)
             );
             CREATE INDEX IF NOT EXISTS idx_sec_members_section ON edu_section_members(section_id);
             CREATE INDEX IF NOT EXISTS idx_sec_members_endpoint ON edu_section_members(endpoint_id);",
        )?;

        // 回填「老师成员」
        let _ = self.db.execute(
            "INSERT OR IGNORE INTO edu_section_members
                (section_id, user_id, role, username, display_name, admin_class, authorized, joined_at)
             SELECT cl.id, cl.teacher_id, 'teacher',
                    COALESCE(t.name, ''), COALESCE(t.name, ''), '', 0, cl.created_at
             FROM edu_classes cl LEFT JOIN edu_teachers t ON t.id = cl.teacher_id
             WHERE cl.teacher_id IS NOT NULL",
            [],
        );

        // 回填「学生成员」：**只**从 `edu_students.primary_class_id`（该列明确指向
        // edu_classes.id，语义无歧义）。课程级选课 `edu_enrollments` 无法判断
        // 学生属于该课程的**哪个**教学班，故意不做猜测，留给教务同步或手工指派。
        let _ = self.db.execute(
            "INSERT OR IGNORE INTO edu_section_members
                (section_id, user_id, role, username, display_name, admin_class, authorized, joined_at)
             SELECT s.primary_class_id, s.id, 'student', s.student_no, s.name,
                    COALESCE(s.admin_class, ''), 0, s.created_at
             FROM edu_students s
             WHERE s.primary_class_id IS NOT NULL",
            [],
        );

        self.db.execute_batch("PRAGMA user_version = 1")?;
        tracing::info!("edu.db 迁移完成 → user_version=1（教学班 + 成员 + 设备绑定）");
        }

        // ── v2：撤销时间戳 ──
        //
        // ⚠️ 安全必需：没有这一列时，「从未认证」与「已被撤销」在库里**完全无法区分**
        //    （都是 authorized=0 + endpoint_id=NULL）。后果是被撤销的学生只要重新
        //    执行一次 `/class-auth`，`sections_for_student` 仍会把教学班票据发给他，
        //    他就能拿到轮换后的新 Topic —— 撤销形同虚设。
        if v < 2 {
            let _ = self
                .db
                .execute("ALTER TABLE edu_section_members ADD COLUMN revoked_at TEXT", []);
            self.db.execute_batch("PRAGMA user_version = 2")?;
            tracing::info!("edu.db 迁移完成 → user_version=2（撤销时间戳，区分待认证/已撤销）");
        }
        Ok(())
    }

    /// 某老师任教的教学班
    pub fn sections_by_teacher(&self, teacher_id: i64) -> Result<Vec<SectionRow>, EduError> {
        let mut stmt = self.db.prepare(
            "SELECT id, course_id, teacher_id, name, term, gossip_seed, topic_epoch, created_at
             FROM edu_classes WHERE teacher_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![teacher_id], row_to_section)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// 单个教学班
    pub fn get_section(&self, section_id: i64) -> Result<Option<SectionRow>, EduError> {
        let mut stmt = self.db.prepare(
            "SELECT id, course_id, teacher_id, name, term, gossip_seed, topic_epoch, created_at
             FROM edu_classes WHERE id = ?1",
        )?;
        Ok(stmt.query_row(params![section_id], row_to_section).ok())
    }

    /// 学生所属的全部教学班（用于签发教学班票据）。
    ///
    /// 注意：**不**过滤 `authorized` —— 首次认证时成员行已存在但尚未授权，
    /// 票据正是在认证成功那一刻下发的。
    ///
    /// ⚠️ 但**必须**过滤 `revoked_at`：被撤销的成员与「尚未首次认证」的成员在
    /// `authorized`/`endpoint_id` 上完全相同，只有撤销时间戳能区分。不过滤的话，
    /// 被撤销学生重新走一次 `/class-auth` 就能拿到轮换后的新 Topic（安全缺陷）。
    pub fn sections_for_student(
        &self,
        username: &str,
    ) -> Result<Vec<StudentSectionRow>, EduError> {
        let mut stmt = self.db.prepare(
            "SELECT cl.id, cl.name, c.course_code, c.name, cl.gossip_seed, cl.topic_epoch
             FROM edu_section_members m
             JOIN edu_classes cl ON cl.id = m.section_id
             JOIN edu_courses c  ON c.id = cl.course_id
             WHERE m.role = 'student' AND m.username = ?1
               AND m.revoked_at IS NULL
             ORDER BY c.course_code, cl.id",
        )?;
        let rows = stmt.query_map(params![username], |r| {
            Ok(StudentSectionRow {
                section_id: r.get(0)?,
                section_name: r.get(1)?,
                course_code: r.get(2)?,
                course_name: r.get(3)?,
                gossip_seed: r.get::<_, Option<Vec<u8>>>(4)?.unwrap_or_default(),
                topic_epoch: r.get::<_, Option<i64>>(5)?.unwrap_or(0) as u32,
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// 学生的行政班级
    pub fn admin_class_of(&self, username: &str) -> Result<String, EduError> {
        let r = self.db.query_row(
            "SELECT COALESCE(admin_class, '') FROM edu_students WHERE student_no = ?1",
            params![username],
            |r| r.get::<_, String>(0),
        );
        match r {
            Ok(s) => Ok(s),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(String::new()),
            Err(e) => Err(e.into()),
        }
    }

    /// 设备绑定：首次认证成功后把 EndpointId 写入学生与其全部成员行，并标记授权。
    pub fn bind_endpoint(&self, username: &str, endpoint_id: &str) -> Result<(), EduError> {
        self.db.execute(
            "UPDATE edu_students SET endpoint_id = ?1 WHERE student_no = ?2",
            params![endpoint_id, username],
        )?;
        self.db.execute(
            "UPDATE edu_section_members
                SET endpoint_id = ?1, authorized = 1, last_seen_at = ?2
             WHERE role = 'student' AND username = ?3",
            params![endpoint_id, Self::now(), username],
        )?;
        Ok(())
    }

    /// 新增 / 更新成员 —— **不覆盖** `endpoint_id` 与 `authorized`（保护既有授权状态）。
    pub fn upsert_section_member(
        &self,
        section_id: i64,
        user_id: i64,
        role: &str,
        username: &str,
        display_name: &str,
        admin_class: &str,
    ) -> Result<(), EduError> {
        self.db.execute(
            "INSERT OR IGNORE INTO edu_section_members
                (section_id, user_id, role, username, display_name, admin_class, authorized, joined_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7)",
            params![section_id, user_id, role, username, display_name, admin_class, Self::now()],
        )?;
        self.db.execute(
            "UPDATE edu_section_members
                SET user_id = ?1, display_name = ?2, admin_class = ?3
             WHERE section_id = ?4 AND role = ?5 AND username = ?6",
            params![user_id, display_name, admin_class, section_id, role, username],
        )?;
        Ok(())
    }

    /// 教学班成员列表
    pub fn list_section_members(
        &self,
        section_id: i64,
    ) -> Result<Vec<crate::edu::model::Member>, EduError> {
        let mut stmt = self.db.prepare(
            "SELECT id, section_id, user_id, role, username, display_name, admin_class,
                    endpoint_id, authorized, joined_at, last_seen_at
             FROM edu_section_members WHERE section_id = ?1 ORDER BY role, username",
        )?;
        let rows = stmt.query_map(params![section_id], |r| {
            Ok(crate::edu::model::Member {
                id: r.get(0)?,
                section_id: r.get(1)?,
                user_id: r.get(2)?,
                role: crate::edu::model::MemberRole::parse(&r.get::<_, String>(3)?)
                    .unwrap_or(crate::edu::model::MemberRole::Student),
                username: r.get(4)?,
                display_name: r.get(5)?,
                admin_class: r.get::<_, Option<String>>(6)?.unwrap_or_default(),
                endpoint_id: r.get(7)?,
                authorized: r.get::<_, i64>(8)? != 0,
                joined_at: r.get(9)?,
                last_seen_at: r.get(10)?,
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// 撤销成员授权（清 endpoint 绑定 + 取消 authorized）。返回该学生的 EndpointId（若有）。
    pub fn revoke_member(&self, section_id: i64, username: &str) -> Result<Option<String>, EduError> {
        let ep: Option<String> = self
            .db
            .query_row(
                "SELECT endpoint_id FROM edu_section_members
                 WHERE section_id = ?1 AND username = ?2 AND role = 'student'",
                params![section_id, username],
                |r| r.get(0),
            )
            .unwrap_or(None);
        self.db.execute(
            "UPDATE edu_section_members
                SET authorized = 0, endpoint_id = NULL, revoked_at = ?3
             WHERE section_id = ?1 AND username = ?2 AND role = 'student'",
            params![section_id, username, Self::now()],
        )?;
        Ok(ep)
    }

    /// 恢复 / 取消成员授权位（不清 `endpoint_id`）。
    ///
    /// 与 `revoke_member` 的区别：`revoke_member` 是「彻底撤销」（清绑定 + 取消授权 +
    /// 打撤销时间戳），本函数只翻转 `authorized` 标志，用于「先停权、保留设备绑定」
    /// 或「恢复」场景。恢复时一并清除 `revoked_at`，否则该生仍拿不到票据。
    pub fn set_member_authorized(
        &self,
        section_id: i64,
        username: &str,
        authorized: bool,
    ) -> Result<(), EduError> {
        if authorized {
            self.db.execute(
                "UPDATE edu_section_members SET authorized = 1, revoked_at = NULL
                 WHERE section_id = ?1 AND username = ?2 AND role = 'student'",
                params![section_id, username],
            )?;
        } else {
            self.db.execute(
                "UPDATE edu_section_members SET authorized = 0
                 WHERE section_id = ?1 AND username = ?2 AND role = 'student'",
                params![section_id, username],
            )?;
        }
        Ok(())
    }

    /// Topic 轮换：`topic_epoch += 1`，旧 Topic 立即作废（§5.4 治本方案）。返回新纪元。
    pub fn rotate_topic_epoch(&self, section_id: i64) -> Result<u32, EduError> {
        self.db.execute(
            "UPDATE edu_classes SET topic_epoch = topic_epoch + 1 WHERE id = ?1",
            params![section_id],
        )?;
        let e: i64 = self.db.query_row(
            "SELECT topic_epoch FROM edu_classes WHERE id = ?1",
            params![section_id],
            |r| r.get(0),
        )?;
        Ok(e as u32)
    }

    /// 恢复既有授权：进程启动时预热 `AuthRegistry` 用。
    /// 返回 (endpoint_id, username, display_name, admin_class, section_id)。
    pub fn authorized_bindings(
        &self,
    ) -> Result<Vec<(String, String, String, String, i64)>, EduError> {
        let mut stmt = self.db.prepare(
            "SELECT endpoint_id, username, display_name, admin_class, section_id
             FROM edu_section_members
             WHERE role = 'student' AND authorized = 1 AND endpoint_id IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                r.get::<_, i64>(4)?,
            ))
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    // -----------------------------------------------------------------------
    // 教务同步辅助（P4，`src/edu/registrar.rs` 使用）
    // -----------------------------------------------------------------------

    /// 按姓名查老师。
    ///
    /// 教务侧只给「工号 + 姓名」，而 `edu_teachers` 历史表只有 `name` 列，故以姓名对齐。
    /// 工号若要长期比对，应在后续迁移中为 `edu_teachers` 增加 `teacher_no` 列。
    pub fn find_teacher_by_name(&self, name: &str) -> Result<Option<Teacher>, EduError> {
        let r = self.db.query_row(
            "SELECT id, name, node_id, created_at FROM edu_teachers WHERE name = ?1 LIMIT 1",
            params![name],
            |r| {
                Ok(Teacher {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    node_id: r.get(2)?,
                    created_at: r.get(3)?,
                })
            },
        );
        match r {
            Ok(t) => Ok(Some(t)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// 教务同步：课程按 `course_code` upsert。
    ///
    /// 已存在时**只**更新课程名，绝不覆盖 `tools_whitelist` / `allowed_modes`
    /// —— 那是老师在本班自定义过的教学参数，同步不得冲掉。
    pub fn upsert_course_by_code(
        &self,
        code: &str,
        name: &str,
        teacher_id: i64,
    ) -> Result<i64, EduError> {
        if let Some(c) = self.get_course(code)? {
            if !name.is_empty() && c.name != name {
                self.db.execute(
                    "UPDATE edu_courses SET name = ?1 WHERE id = ?2",
                    params![name, c.id],
                )?;
            }
            return Ok(c.id);
        }
        Ok(self.create_course(code, name, teacher_id)?.id)
    }

    /// 教务同步：教学班按业务主键 `(course_id, teacher_id, name, term)` upsert。
    ///
    /// ⚠️ **已存在则原样返回 id，绝不写任何列** —— 覆盖 `gossip_seed` / `topic_epoch`
    /// 会让全班 Topic 静默改变，所有学生瞬间掉线（§5.4）。同步是只读语义。
    ///
    /// 返回 `(section_id, created)`：`created = true` 表示本次新建。
    pub fn upsert_section_by_key(
        &self,
        course_id: i64,
        teacher_id: i64,
        name: &str,
        term: &str,
    ) -> Result<(i64, bool), EduError> {
        let existing: Option<i64> = self
            .db
            .query_row(
                "SELECT id FROM edu_classes
                 WHERE course_id = ?1
                   AND IFNULL(teacher_id, 0) = ?2
                   AND name = ?3
                   AND IFNULL(term, '') = ?4
                 LIMIT 1",
                params![course_id, teacher_id, name, term],
                |r| r.get(0),
            )
            .unwrap_or(None);
        if let Some(id) = existing {
            return Ok((id, false));
        }
        self.db.execute(
            "INSERT INTO edu_classes
                (name, course_id, teacher_id, term, gossip_seed, topic_epoch, created_at)
             VALUES (?1, ?2, ?3, ?4, randomblob(32), 0, ?5)",
            params![name, course_id, teacher_id, term, Self::now()],
        )?;
        Ok((self.db.last_insert_rowid(), true))
    }
}

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
        let _lesson1 = store.create_lesson(course.id, class.id, 1, "线性表").unwrap();
        let _lesson2 = store.create_lesson(course.id, class.id, 2, "栈和队列").unwrap();

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

    #[test]
    fn test_class_course_override_isolation() {
        // 验证核心需求：同一课程导入到两个头（班级）后，
        // 修改头 A 的参数不影响头 B，也不影响课程模板。
        let (_tmp, store) = setup_store();
        let teacher = store.create_teacher("老师", "p").unwrap();
        // create_course 默认 tools_whitelist = "[]"
        let course = store
            .create_course("CS999", "隔离测试", teacher.id)
            .unwrap();

        // 两个头（班级）
        let class_a = store.create_class("头A", course.id).unwrap();
        let class_b = store.create_class("头B", course.id).unwrap();

        // 两个头都导入课程
        store.import_course_to_class(class_a.id, course.id).unwrap();
        store.import_course_to_class(class_b.id, course.id).unwrap();

        // 头 A 修改工具白名单 + 描述
        store
            .update_class_course_override(
                class_a.id,
                course.id,
                Some(r#"["read_file","glob","write_file"]"#),
                Some("头A的专属描述"),
                None,
            )
            .unwrap();

        // 验证：头 A 用覆盖值
        let resolved_a = store.resolve_course_for_class(class_a.id, &course).unwrap();
        assert!(resolved_a.tools_whitelist.contains("write_file"));
        assert_eq!(resolved_a.description, "头A的专属描述");

        // 验证：头 B 仍用模板值（不受头 A 影响）
        let resolved_b = store.resolve_course_for_class(class_b.id, &course).unwrap();
        assert!(!resolved_b.tools_whitelist.contains("write_file"));
        assert_eq!(resolved_b.description, ""); // 模板默认空

        // 验证：课程模板本身未被修改
        let template = store.get_course("CS999").unwrap().unwrap();
        assert!(!template.tools_whitelist.contains("write_file"));
        assert_eq!(template.description, "");

        // 验证：更新未导入的课程应报错
        let class_c = store.create_class("头C", course.id).unwrap();
        assert!(store
            .update_class_course_override(class_c.id, course.id, Some("[]"), None, None)
            .is_err());
    }
}

// ---------------------------------------------------------------------------
// 教学班 / 成员 / 设备绑定（决策 1B 规范重构）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod section_tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, EduStore) {
        let tmp = tempfile::tempdir().unwrap();
        let store = EduStore::open(tmp.path().join("edu.db")).unwrap();
        store.create_teacher("张老师", "pw").unwrap();
        (tmp, store)
    }

    #[test]
    fn test_migration_assigns_teacher_and_gossip_seed() {
        let (_t, store) = setup();
        let course = store.create_course("CS201", "数据结构", 1).unwrap();
        let class = store.create_class("数据结构-信工班", course.id).unwrap();

        let sec = store.get_section(class.id).unwrap().unwrap();
        // 决策 1B：teacher_id 从课程下移到教学班
        assert_eq!(sec.teacher_id, 1, "教学班的任课老师应回填为课程老师");
        assert_eq!(sec.name, "数据结构-信工班");
        assert_eq!(sec.topic_epoch, 0);
        // 每个教学班必须有独立的 32 字节 Topic 种子
        assert_eq!(sec.gossip_seed.len(), 32, "gossip_seed 应为 32 字节随机种子");

        // 老师自动成为该班成员
        let members = store.list_section_members(class.id).unwrap();
        assert!(
            members.iter().any(|m| m.role == crate::edu::model::MemberRole::Teacher),
            "老师应被回填为教学班成员"
        );
    }

    #[test]
    fn test_two_sections_have_distinct_seeds() {
        let (_t, store) = setup();
        let course = store.create_course("CS201", "数据结构", 1).unwrap();
        let a = store.create_class("数据结构-信工班", course.id).unwrap();
        let b = store.create_class("数据结构-电气班", course.id).unwrap();

        let sa = store.get_section(a.id).unwrap().unwrap();
        let sb = store.get_section(b.id).unwrap().unwrap();
        assert_ne!(
            sa.gossip_seed, sb.gossip_seed,
            "同一课程下的不同教学班必须有不同 Topic 种子（Topic 隔离）"
        );
    }

    #[test]
    fn test_student_sections_and_device_binding() {
        let (_t, store) = setup();
        let course = store.create_course("CS201", "数据结构", 1).unwrap();
        let class = store.create_class("数据结构-信工班", course.id).unwrap();
        // 学生挂到该教学班（primary_class_id 明确指向 edu_classes.id）
        store
            .create_student("2024001", "张三", "pw", Some(class.id))
            .unwrap();

        // 认证前：成员行已存在（票据据此签发），但未授权、无 endpoint
        let secs = store.sections_for_student("2024001").unwrap();
        assert_eq!(secs.len(), 1);
        assert_eq!(secs[0].section_id, class.id);
        assert_eq!(secs[0].course_code, "CS201");
        assert_eq!(secs[0].gossip_seed.len(), 32);

        // 认证成功后：设备绑定 + 授权
        store.bind_endpoint("2024001", "AB".repeat(32).as_str()).unwrap();
        let members = store.list_section_members(class.id).unwrap();
        let stu = members
            .iter()
            .find(|m| m.username == "2024001")
            .expect("学生应为教学班成员");
        assert!(stu.authorized, "认证后应标记授权");
        assert!(stu.endpoint_id.is_some(), "认证后应写入设备绑定");

        // 预热恢复
        let bindings = store.authorized_bindings().unwrap();
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].1, "2024001");
    }

    #[test]
    fn test_upsert_member_preserves_authorization() {
        let (_t, store) = setup();
        let course = store.create_course("CS201", "数据结构", 1).unwrap();
        let class = store.create_class("数据结构-信工班", course.id).unwrap();
        store.create_student("2024001", "张三", "pw", Some(class.id)).unwrap();
        store.bind_endpoint("2024001", "CD".repeat(32).as_str()).unwrap();

        // 教务同步再跑一次（只改姓名/行政班）——不得冲掉 endpoint 与 authorized
        store
            .upsert_section_member(class.id, 1, "student", "2024001", "张三丰", "信工2202")
            .unwrap();

        let m = store
            .list_section_members(class.id)
            .unwrap()
            .into_iter()
            .find(|m| m.username == "2024001")
            .unwrap();
        assert!(m.authorized, "重新同步不得重置授权状态");
        assert!(m.endpoint_id.is_some(), "重新同步不得清空设备绑定");
        assert_eq!(m.display_name, "张三丰", "姓名应被更新");
        assert_eq!(m.admin_class, "信工2202", "行政班级应被更新");
    }

    #[test]
    fn test_rotate_topic_epoch_and_revoke() {
        let (_t, store) = setup();
        let course = store.create_course("CS201", "数据结构", 1).unwrap();
        let class = store.create_class("数据结构-信工班", course.id).unwrap();
        store.create_student("2024001", "张三", "pw", Some(class.id)).unwrap();
        store.bind_endpoint("2024001", "EF".repeat(32).as_str()).unwrap();

        // 撤销
        let ep = store.revoke_member(class.id, "2024001").unwrap();
        assert!(ep.is_some(), "撤销时应返回被撤销的 EndpointId");
        let m = store
            .list_section_members(class.id)
            .unwrap()
            .into_iter()
            .find(|m| m.username == "2024001")
            .unwrap();
        assert!(!m.authorized);
        assert!(m.endpoint_id.is_none());
        assert_eq!(store.authorized_bindings().unwrap().len(), 0);

        // 轮换 Topic 纪元 → Topic 变化
        let before = store.get_section(class.id).unwrap().unwrap().topic_epoch;
        let after = store.rotate_topic_epoch(class.id).unwrap();
        assert_eq!(after, before + 1, "轮换后纪元应 +1");
    }
}
