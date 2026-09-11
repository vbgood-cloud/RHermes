//! 教务系统对接（可选）—— 对应 `docs/edu-p2p-design.md` §7。
//!
//! **定位**：教务系统只提供**权威名单**（谁教哪个班、谁选了哪个班），
//! 认证与通信仍由 Iroh P2P 层完成。因此同步是**只读**的：只回填课程/教学班/成员，
//! 绝不触碰 `gossip_seed` / `topic_epoch` / `endpoint_id` / `authorized`。
//!
//! 两个实现：
//! - [`HttpRegistrar`]：教务侧暴露只读 REST JSON 接口（推荐）；
//! - [`CsvRegistrar`]：离线场景，教务导出 CSV/Excel 后本地导入。
//!
//! 安全红线（§5.4 的推论）：如果同步覆盖了 `topic_epoch`，全班 Topic 会静默改变，
//! 所有已入群学生瞬间掉线 —— 因此 `upsert_section_by_key` 对已存在的教学班**一个列都不写**。

use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::edu::model::MemberRole;
use crate::edu::store::EduStore;

// ---------------------------------------------------------------------------
// DTO（教务侧契约）
// ---------------------------------------------------------------------------

/// 教务侧的「教学班」描述。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SectionSpec {
    /// 教务侧唯一教学班号
    pub section_no: String,
    pub course_code: String,
    pub course_name: String,
    /// 任课老师工号
    pub teacher_no: String,
    pub teacher_name: String,
    /// 学期，如 "2026-2027-1"
    pub term: String,
    /// 教学班名，如 "数据结构-信工班"
    pub section_name: String,
}

/// 教务侧的「学生」描述（名单一行）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StudentSpec {
    pub student_no: String,
    pub name: String,
    /// 行政班级，如 "信工2201"
    pub admin_class: String,
}

// ---------------------------------------------------------------------------
// 抽象接口
// ---------------------------------------------------------------------------

#[async_trait]
pub trait RegistrarClient: Send + Sync {
    /// 老师本学期开设的教学班
    async fn teacher_sections(&self, teacher_no: &str, term: &str)
    -> anyhow::Result<Vec<SectionSpec>>;

    /// 教学班选课名单（含行政班级）
    async fn section_roster(&self, section_no: &str) -> anyhow::Result<Vec<StudentSpec>>;

    /// 学生本学期已选的教学班
    async fn student_sections(&self, student_no: &str, term: &str)
    -> anyhow::Result<Vec<SectionSpec>>;
}

// ---------------------------------------------------------------------------
// 实现 ①：REST
// ---------------------------------------------------------------------------

/// 教务 REST 契约（建议教务侧提供）：
///
/// | 方法 | 路径 | 返回 |
/// |------|------|------|
/// | GET | `/api/registrar/teachers/{teacher_no}/sections?term=` | `[SectionSpec]` |
/// | GET | `/api/registrar/sections/{section_no}/roster` | `[StudentSpec]` |
/// | GET | `/api/registrar/students/{student_no}/sections?term=` | `[SectionSpec]` |
#[derive(Debug, Clone)]
pub struct HttpRegistrar {
    base_url: String,
    token: String,
    client: reqwest::Client,
}

impl HttpRegistrar {
    pub fn new(base_url: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            token: token.into(),
            client: reqwest::Client::new(),
        }
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> anyhow::Result<T> {
        let resp = self
            .client
            .get(url)
            .bearer_auth(&self.token)
            .send()
            .await?
            .error_for_status()?;
        Ok(resp.json::<T>().await?)
    }
}

#[async_trait]
impl RegistrarClient for HttpRegistrar {
    async fn teacher_sections(
        &self,
        teacher_no: &str,
        term: &str,
    ) -> anyhow::Result<Vec<SectionSpec>> {
        let url = format!(
            "{}/api/registrar/teachers/{teacher_no}/sections?term={term}",
            self.base_url
        );
        self.get_json(&url).await
    }

    async fn section_roster(&self, section_no: &str) -> anyhow::Result<Vec<StudentSpec>> {
        let url = format!(
            "{}/api/registrar/sections/{section_no}/roster",
            self.base_url
        );
        self.get_json(&url).await
    }

    async fn student_sections(
        &self,
        student_no: &str,
        term: &str,
    ) -> anyhow::Result<Vec<SectionSpec>> {
        let url = format!(
            "{}/api/registrar/students/{student_no}/sections?term={term}",
            self.base_url
        );
        self.get_json(&url).await
    }
}

// ---------------------------------------------------------------------------
// 实现 ②：CSV（离线）
// ---------------------------------------------------------------------------

/// 离线导入。
///
/// - `sections_csv` 表头：`section_no,course_code,course_name,teacher_no,teacher_name,term,section_name`
/// - `roster_dir/<section_no>.csv` 表头：`student_no,name,admin_class`
///
/// 过滤规则（宽松但安全）：仅当「行内该列非空」且与查询条件不一致时才跳过。
/// 这样既支持「一个老师一个文件」（school 列留空），也支持「全校一个文件」。
#[derive(Debug, Clone)]
pub struct CsvRegistrar {
    sections_csv: PathBuf,
    roster_dir: PathBuf,
}

impl CsvRegistrar {
    pub fn new(sections_csv: impl Into<PathBuf>, roster_dir: impl Into<PathBuf>) -> Self {
        Self {
            sections_csv: sections_csv.into(),
            roster_dir: roster_dir.into(),
        }
    }

    /// 按表头下标取值（缺列为空串），避免列顺序变化导致错位
    fn col<'a>(header: &[String], row: &'a [String], name: &str) -> &'a str {
        header
            .iter()
            .position(|h| h == name)
            .and_then(|i| row.get(i))
            .map(|s| s.as_str())
            .unwrap_or("")
    }

    fn read_csv(path: &std::path::Path) -> anyhow::Result<(Vec<String>, Vec<Vec<String>>)> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("读取 {} 失败: {e}", path.display()))?;
        let mut rows = parse_csv(&text);
        if rows.is_empty() {
            anyhow::bail!("{} 是空文件", path.display());
        }
        let header = rows.remove(0);
        Ok((header, rows))
    }
}

#[async_trait]
impl RegistrarClient for CsvRegistrar {
    async fn teacher_sections(
        &self,
        teacher_no: &str,
        term: &str,
    ) -> anyhow::Result<Vec<SectionSpec>> {
        let (header, rows) = Self::read_csv(&self.sections_csv)?;
        let mut out = Vec::new();
        for row in rows {
            let spec = SectionSpec {
                section_no: Self::col(&header, &row, "section_no").to_string(),
                course_code: Self::col(&header, &row, "course_code").to_string(),
                course_name: Self::col(&header, &row, "course_name").to_string(),
                teacher_no: Self::col(&header, &row, "teacher_no").to_string(),
                teacher_name: Self::col(&header, &row, "teacher_name").to_string(),
                term: Self::col(&header, &row, "term").to_string(),
                section_name: Self::col(&header, &row, "section_name").to_string(),
            };
            if spec.section_name.is_empty() || spec.course_code.is_empty() {
                continue;
            }
            if !term.is_empty() && !spec.term.is_empty() && spec.term != term {
                continue;
            }
            if !teacher_no.is_empty()
                && !spec.teacher_no.is_empty()
                && spec.teacher_no != teacher_no
            {
                continue;
            }
            out.push(spec);
        }
        Ok(out)
    }

    async fn section_roster(&self, section_no: &str) -> anyhow::Result<Vec<StudentSpec>> {
        let path = self.roster_dir.join(format!("{section_no}.csv"));
        if !path.exists() {
            return Ok(Vec::new());
        }
        let (header, rows) = Self::read_csv(&path)?;
        let mut out = Vec::new();
        for row in rows {
            let spec = StudentSpec {
                student_no: Self::col(&header, &row, "student_no").to_string(),
                name: Self::col(&header, &row, "name").to_string(),
                admin_class: Self::col(&header, &row, "admin_class").to_string(),
            };
            if spec.student_no.is_empty() {
                continue;
            }
            out.push(spec);
        }
        Ok(out)
    }

    async fn student_sections(
        &self,
        student_no: &str,
        term: &str,
    ) -> anyhow::Result<Vec<SectionSpec>> {
        // 复用教学班表：CSV 场景下用「名单文件里含该学号」判定学生所属教学班
        let (header, rows) = Self::read_csv(&self.sections_csv)?;
        let mut out = Vec::new();
        for row in rows {
            let spec = SectionSpec {
                section_no: Self::col(&header, &row, "section_no").to_string(),
                course_code: Self::col(&header, &row, "course_code").to_string(),
                course_name: Self::col(&header, &row, "course_name").to_string(),
                teacher_no: Self::col(&header, &row, "teacher_no").to_string(),
                teacher_name: Self::col(&header, &row, "teacher_name").to_string(),
                term: Self::col(&header, &row, "term").to_string(),
                section_name: Self::col(&header, &row, "section_name").to_string(),
            };
            if spec.section_no.is_empty() || spec.section_name.is_empty() {
                continue;
            }
            if !term.is_empty() && !spec.term.is_empty() && spec.term != term {
                continue;
            }
            // 该学号必须出现在该班名单里
            let roster = self.section_roster(&spec.section_no).await?;
            if roster.iter().any(|s| s.student_no == student_no) {
                out.push(spec);
            }
        }
        Ok(out)
    }
}

/// 极简 CSV 解析：支持 CRLF、末尾空行、双引号包裹与 `""` 转义、UTF-8 BOM 剥离。
///
/// 教务导出的中文姓名/班名不会含逗号，故不做完整 RFC 4180 实现，够用即可。
pub fn parse_csv(text: &str) -> Vec<Vec<String>> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut rows = Vec::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        rows.push(parse_csv_line(line));
    }
    rows
}

fn parse_csv_line(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                if in_quotes && chars.peek() == Some(&'"') {
                    cur.push('"');
                    chars.next();
                } else {
                    in_quotes = !in_quotes;
                }
            }
            ',' if !in_quotes => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

// ---------------------------------------------------------------------------
// 同步执行
// ---------------------------------------------------------------------------

/// 同步结果统计。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SyncReport {
    pub sections_created: usize,
    pub sections_existing: usize,
    pub members_added: usize,
    pub members_existing: usize,
    /// 单条失败不中断整体同步（例如某班名单接口 404）
    pub errors: Vec<String>,
}

impl SyncReport {
    pub fn summary(&self) -> String {
        let mut s = format!(
            "教学班：新增 {} / 已存在 {}\n成员：新增 {} / 已存在 {}",
            self.sections_created, self.sections_existing, self.members_added, self.members_existing
        );
        if !self.errors.is_empty() {
            s.push_str(&format!("\n⚠️ {} 处错误：", self.errors.len()));
            for e in &self.errors {
                s.push_str(&format!("\n  - {e}"));
            }
        }
        s
    }
}

/// 老师侧同步：拉取「我本学期教学班 + 各班名单」并落库。
///
/// 幂等：重复执行不会重复建班、不会重置 Topic、不会作废已授权设备。
pub async fn sync_teacher_sections(
    store: &EduStore,
    client: &dyn RegistrarClient,
    teacher_no: &str,
    teacher_name: &str,
    term: &str,
    default_password: &str,
) -> anyhow::Result<SyncReport> {
    // ① 老师本体：按姓名 upsert（教务有工号，本地表只有姓名）
    let teacher = match store.find_teacher_by_name(teacher_name)? {
        Some(t) => t,
        None => store.create_teacher(teacher_name, default_password)?,
    };

    // ② 教学班
    let specs = client.teacher_sections(teacher_no, term).await?;
    let mut report = SyncReport::default();

    for spec in &specs {
        let course_id =
            store.upsert_course_by_code(&spec.course_code, &spec.course_name, teacher.id)?;
        let section_term = if spec.term.is_empty() { term } else { &spec.term };
        let (section_id, created) =
            store.upsert_section_by_key(course_id, teacher.id, &spec.section_name, section_term)?;
        if created {
            report.sections_created += 1;
        } else {
            report.sections_existing += 1;
        }

        // 老师自己的成员行（不覆盖既有授权状态）
        store.upsert_section_member(
            section_id,
            teacher.id,
            "teacher",
            teacher_name,
            teacher_name,
            "",
        )?;

        // ③ 名单
        let known: std::collections::HashSet<String> = store
            .list_section_members(section_id)?
            .into_iter()
            .filter(|m| m.role == MemberRole::Student)
            .map(|m| m.username)
            .collect();

        match client.section_roster(&spec.section_no).await {
            Ok(roster) => {
                for st in roster {
                    let student = match store.get_student(&st.student_no)? {
                        Some(s) => s,
                        None => store.create_student(
                            &st.student_no,
                            &st.name,
                            default_password,
                            Some(section_id),
                        )?,
                    };
                    let is_new = !known.contains(&st.student_no);
                    store.upsert_section_member(
                        section_id,
                        student.id,
                        "student",
                        &st.student_no,
                        &st.name,
                        &st.admin_class,
                    )?;
                    if is_new {
                        report.members_added += 1;
                    } else {
                        report.members_existing += 1;
                    }
                }
            }
            Err(e) => report
                .errors
                .push(format!("教学班 {} 名单拉取失败: {e}", spec.section_no)),
        }
    }

    Ok(report)
}

/// 学生侧同步：返回「教务说我在这个班」的列表，并标注本地是否已入班。
///
/// 学生端**不落库**（无权限建班），仅用于提示「你还有 N 个班待加入」。
pub async fn student_pending_sections(
    store: &EduStore,
    client: &dyn RegistrarClient,
    student_no: &str,
    term: &str,
) -> anyhow::Result<Vec<(SectionSpec, bool)>> {
    let specs = client.student_sections(student_no, term).await?;

    // 本地已入班的教学班名（教务的 `section_no` 与本地 `edu_classes.id` 不同源，
    // 只能按「教学班名」对齐 —— §7.3 契约里教务侧也应保证名称唯一）
    let joined_names: std::collections::HashSet<String> = store
        .sections_for_student(student_no)?
        .into_iter()
        .map(|r| r.section_name)
        .collect();

    Ok(specs
        .into_iter()
        .map(|s| {
            let is_joined = joined_names.contains(&s.section_name);
            (s, is_joined)
        })
        .collect())
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ── CSV 解析 ──

    #[test]
    fn test_parse_csv_basic() {
        let rows = parse_csv("a,b,c\n1,2,3\n");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], vec!["a", "b", "c"]);
        assert_eq!(rows[1], vec!["1", "2", "3"]);
    }

    #[test]
    fn test_parse_csv_crlf_bom_quotes() {
        let text = "\u{feff}name,class\r\n\"张,三\",信工2201\r\n\"李\"\"四\",信工2202\r\n\r\n";
        let rows = parse_csv(text);
        assert_eq!(rows.len(), 3, "应忽略 BOM 与末尾空行");
        assert_eq!(rows[0], vec!["name", "class"]);
        assert_eq!(rows[1], vec!["张,三", "信工2201"], "引号内逗号不应切分");
        assert_eq!(rows[2], vec!["李\"四", "信工2202"], "双引号转义");
    }

    #[tokio::test]
    async fn test_csv_registrar_roster_and_filter() {
        let dir = tempfile::tempdir().unwrap();
        let sections = dir.path().join("sections.csv");
        std::fs::write(
            &sections,
            "section_no,course_code,course_name,teacher_no,teacher_name,term,section_name\n\
             S1,CS201,数据结构,T1,张老师,2026-1,数据结构-信工班\n\
             S2,CS201,数据结构,T2,李老师,2026-1,数据结构-电气班\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("S1.csv"),
            "student_no,name,admin_class\n2024001,张三,信工2201\n2024002,李四,信工2202\n",
        )
        .unwrap();

        let r = CsvRegistrar::new(&sections, dir.path());

        let mine = r.teacher_sections("T1", "2026-1").await.unwrap();
        assert_eq!(mine.len(), 1, "只应返回 T1 自己的班");
        assert_eq!(mine[0].section_name, "数据结构-信工班");

        // teacher_no 留空 = 不过滤老师（一师一文件场景）
        let all = r.teacher_sections("", "2026-1").await.unwrap();
        assert_eq!(all.len(), 2);

        let roster = r.section_roster("S1").await.unwrap();
        assert_eq!(roster.len(), 2);
        assert_eq!(roster[0].admin_class, "信工2201");

        // 不存在的班 → 空名单而非报错
        let none = r.section_roster("S9").await.unwrap();
        assert!(none.is_empty());
    }

    // ── 同步（用假 Registrar，不触网） ──

    struct FakeRegistrar {
        sections: Vec<SectionSpec>,
        rosters: std::collections::HashMap<String, Vec<StudentSpec>>,
    }

    #[async_trait]
    impl RegistrarClient for FakeRegistrar {
        async fn teacher_sections(
            &self,
            _t: &str,
            _term: &str,
        ) -> anyhow::Result<Vec<SectionSpec>> {
            Ok(self.sections.clone())
        }
        async fn section_roster(&self, section_no: &str) -> anyhow::Result<Vec<StudentSpec>> {
            Ok(self.rosters.get(section_no).cloned().unwrap_or_default())
        }
        async fn student_sections(
            &self,
            _s: &str,
            _term: &str,
        ) -> anyhow::Result<Vec<SectionSpec>> {
            Ok(self.sections.clone())
        }
    }

    fn spec(section_no: &str, name: &str) -> SectionSpec {
        SectionSpec {
            section_no: section_no.into(),
            course_code: "CS201".into(),
            course_name: "数据结构".into(),
            teacher_no: "T1".into(),
            teacher_name: "张老师".into(),
            term: "2026-1".into(),
            section_name: name.into(),
        }
    }

    #[tokio::test]
    async fn test_sync_is_idempotent_and_preserves_topic_seed() {
        let dir = tempfile::tempdir().unwrap();
        let store = EduStore::open(dir.path().join("edu.db")).unwrap();

        let mut rosters = std::collections::HashMap::new();
        rosters.insert(
            "S1".to_string(),
            vec![
                StudentSpec {
                    student_no: "2024001".into(),
                    name: "张三".into(),
                    admin_class: "信工2201".into(),
                },
                StudentSpec {
                    student_no: "2024002".into(),
                    name: "李四".into(),
                    admin_class: "信工2202".into(),
                },
            ],
        );
        let client = FakeRegistrar {
            sections: vec![spec("S1", "数据结构-信工班")],
            rosters,
        };

        // 第一次同步：建课 + 建班 + 建成员
        let r1 = sync_teacher_sections(&store, &client, "T1", "张老师", "2026-1", "123456")
            .await
            .unwrap();
        assert_eq!(r1.sections_created, 1);
        assert_eq!(r1.sections_existing, 0);
        assert_eq!(r1.members_added, 2, "两名学生应被加入");
        assert!(r1.errors.is_empty(), "不应有错误: {:?}", r1.errors);

        // 记录 Topic 种子与 epoch
        let sec = store.get_section(1).unwrap().expect("教学班应存在");
        let seed_before = sec.gossip_seed.clone();
        let epoch_before = sec.topic_epoch;
        assert_eq!(seed_before.len(), 32, "种子应为 32 字节");
        let topic_before = sec_topic(&store, 1);

        // 第二次同步：全部幂等，且**不覆盖** Topic 种子/纪元
        let r2 = sync_teacher_sections(&store, &client, "T1", "张老师", "2026-1", "123456")
            .await
            .unwrap();
        assert_eq!(r2.sections_created, 0, "不应重复建班");
        assert_eq!(r2.sections_existing, 1);
        assert_eq!(r2.members_added, 0, "不应重复加人");
        assert_eq!(r2.members_existing, 2);

        let sec2 = store.get_section(1).unwrap().unwrap();
        assert_eq!(sec2.gossip_seed, seed_before, "同步绝不能改 Topic 种子");
        assert_eq!(sec2.topic_epoch, epoch_before, "同步绝不能改 Topic 纪元");
        assert_eq!(sec_topic(&store, 1), topic_before, "整班 Topic 必须保持不变");
    }

    fn sec_topic(store: &EduStore, id: i64) -> [u8; 32] {
        let s = store.get_section(id).unwrap().unwrap();
        crate::edu::gossip::derive_topic(&s.gossip_seed, s.topic_epoch)
    }

    #[tokio::test]
    async fn test_sync_teacher_member_row_exists() {
        let dir = tempfile::tempdir().unwrap();
        let store = EduStore::open(dir.path().join("edu.db")).unwrap();
        let client = FakeRegistrar {
            sections: vec![spec("S1", "数据结构-信工班")],
            rosters: Default::default(),
        };
        sync_teacher_sections(&store, &client, "T1", "张老师", "2026-1", "123456")
            .await
            .unwrap();

        let members = store.list_section_members(1).unwrap();
        let teachers: Vec<_> = members
            .iter()
            .filter(|m| m.role == MemberRole::Teacher)
            .collect();
        assert_eq!(teachers.len(), 1, "应有一行老师成员");
        assert_eq!(teachers[0].username, "张老师");
    }

    #[tokio::test]
    async fn test_student_pending_sections() {
        let dir = tempfile::tempdir().unwrap();
        let store = EduStore::open(dir.path().join("edu.db")).unwrap();
        let client = FakeRegistrar {
            sections: vec![spec("S1", "数据结构-信工班")],
            rosters: Default::default(),
        };
        // 本地无任何教学班 → 全部待加入
        let pending = student_pending_sections(&store, &client, "2024001", "2026-1")
            .await
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert!(!pending[0].1, "本地未入班应为 false");
        assert_eq!(pending[0].0.section_name, "数据结构-信工班");
    }

    #[test]
    fn test_sync_report_summary() {
        let r = SyncReport {
            sections_created: 2,
            sections_existing: 1,
            members_added: 30,
            members_existing: 5,
            errors: vec!["x".into()],
        };
        let s = r.summary();
        assert!(s.contains("新增 2"));
        assert!(s.contains("30"));
        assert!(s.contains("1 处错误"));
    }
}
