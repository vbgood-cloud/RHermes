//! 教师仪表板 — Web 界面
//!
//! 教师通过浏览器查看学生学习数据、管理课程。
//! 使用 axum + 嵌入式 HTML。

use std::path::Path;
use std::sync::Arc;

use axum::{
    extract::State,
    response::{Html, Json},
    routing::get,
    Router,
};
use serde::Serialize;

use crate::edu::store::EduStore;

/// 教师仪表板服务器
pub struct TeacherDashboard {
    port: u16,
    db_path: std::path::PathBuf,
}

#[derive(Clone)]
struct DashboardState {
    db_path: std::path::PathBuf,
}

#[derive(Serialize)]
struct StudentSummary {
    student_no: String,
    name: String,
    journal_count: usize,
    avg_quality: f64,
    avg_reflection: f64,
    total_tokens: i64,
}

impl TeacherDashboard {
    pub fn new(port: u16, db_path: &Path) -> Self {
        Self {
            port,
            db_path: db_path.to_path_buf(),
        }
    }

    /// 启动仪表板 Web 服务器
    pub async fn run(&self) {
        let port = self.port;
        let state = DashboardState {
            db_path: self.db_path.clone(),
        };

        let app = Router::new()
            .route("/", get(dashboard))
            .route("/api/students", get(list_students))
            .route("/api/courses", get(list_courses))
            .with_state(state);

        let addr = format!("0.0.0.0:{port}");
        tracing::info!("教师仪表板启动: http://localhost:{port}");

        let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
        axum::serve(listener, app).await.unwrap();
    }
}

async fn dashboard() -> Html<&'static str> {
    Html(DASHBOARD_HTML)
}

async fn list_courses(
    State(state): State<DashboardState>,
) -> Json<serde_json::Value> {
    let store = match EduStore::open(&state.db_path) {
        Ok(s) => s,
        Err(e) => return Json(serde_json::json!({ "error": e.to_string() })),
    };

    let courses = store.list_courses_by_teacher(1).unwrap_or_default();
    Json(serde_json::json!({ "courses": courses }))
}

async fn list_students(
    State(state): State<DashboardState>,
) -> Json<serde_json::Value> {
    let store = match EduStore::open(&state.db_path) {
        Ok(s) => s,
        Err(e) => return Json(serde_json::json!({ "error": e.to_string() })),
    };

    // 获取所有学生的学习数据（简化版：用 journal 统计）
    let journals = store.get_student_journal(0, None).unwrap_or_default();

    let summaries: Vec<StudentSummary> = journals
        .iter()
        .fold(
            std::collections::HashMap::<i64, StudentSummary>::new(),
            |mut acc, j| {
                let entry = acc.entry(j.student_id).or_insert_with(|| {
                    StudentSummary {
                        student_no: format!("#{}", j.student_id),
                        name: format!("学生{}", j.student_id),
                        journal_count: 0,
                        avg_quality: 0.0,
                        avg_reflection: 0.0,
                        total_tokens: 0,
                    }
                });
                entry.journal_count += 1;
                entry.avg_quality += j.quality_score;
                entry.avg_reflection += j.reflection_depth;
                entry.total_tokens += j.token_usage;
                acc
            },
        )
        .into_values()
        .map(|mut s| {
            if s.journal_count > 0 {
                s.avg_quality /= s.journal_count as f64;
                s.avg_reflection /= s.journal_count as f64;
            }
            s
        })
        .collect();

    Json(serde_json::json!({ "students": summaries }))
}

const DASHBOARD_HTML: &str = r#"<!DOCTYPE html>
<html lang="zh">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>RHermes 教师仪表板</title>
<style>
* { margin: 0; padding: 0; box-sizing: border-box; }
body { font-family: -apple-system, sans-serif; background: #f0f2f5; }
.header { background: #722ed1; color: white; padding: 16px 24px; font-size: 20px; font-weight: bold; }
.container { padding: 24px; max-width: 1200px; margin: 0 auto; }
.card { background: white; border-radius: 8px; padding: 20px; margin-bottom: 16px; box-shadow: 0 1px 3px rgba(0,0,0,0.1); }
.card h2 { font-size: 16px; color: #333; margin-bottom: 12px; }
.stat { display: inline-block; margin-right: 32px; text-align: center; }
.stat .num { font-size: 28px; font-weight: bold; color: #722ed1; }
.stat .label { font-size: 12px; color: #999; }
table { width: 100%; border-collapse: collapse; }
th, td { text-align: left; padding: 8px 12px; border-bottom: 1px solid #f0f0f0; font-size: 14px; }
th { color: #999; font-weight: normal; }
.loading { color: #999; text-align: center; padding: 40px; }
</style>
</head>
<body>
<div class="header">🎓 RHermes 教师仪表板</div>
<div class="container">
  <div class="card">
    <h2>📊 总览</h2>
    <div class="stat"><div class="num" id="studentCount">-</div><div class="label">学生数</div></div>
    <div class="stat"><div class="num" id="courseCount">-</div><div class="label">课程数</div></div>
    <div class="stat"><div class="num" id="journalCount">-</div><div class="label">学习记录</div></div>
  </div>

  <div class="card">
    <h2>📚 课程</h2>
    <div id="courses"><div class="loading">加载中...</div></div>
  </div>

  <div class="card">
    <h2>👥 学生学习数据</h2>
    <table>
      <thead><tr><th>学号</th><th>姓名</th><th>学习次数</th><th>平均提问质量</th><th>平均反思深度</th><th>Token 用量</th></tr></thead>
      <tbody id="students"><tr><td colspan="6" class="loading">加载中...</td></tr></tbody>
    </table>
  </div>
</div>
<script>
async function loadData() {
  // 加载课程
  try {
    const resp = await fetch('/api/courses');
    const data = await resp.json();
    const courses = data.courses || [];
    document.getElementById('courseCount').textContent = courses.length;
    const html = courses.length ? courses.map(c =>
      `<div style="padding:4px 0">📘 ${c.course_code} ${c.name}</div>`
    ).join('') : '<div class="loading">暂无课程</div>';
    document.getElementById('courses').innerHTML = html;
  } catch(e) {}

  // 加载学生
  try {
    const resp = await fetch('/api/students');
    const data = await resp.json();
    const students = data.students || [];
    document.getElementById('studentCount').textContent = students.length;
    document.getElementById('journalCount').textContent = students.reduce((s, st) => s + st.journal_count, 0);
    const html = students.length ? students.map(s =>
      `<tr><td>${s.student_no}</td><td>${s.name}</td><td>${s.journal_count}</td><td>${(s.avg_quality*100).toFixed(0)}%</td><td>${(s.avg_reflection*100).toFixed(0)}%</td><td>${s.total_tokens}</td></tr>`
    ).join('') : '<tr><td colspan="6" class="loading">暂无数据</td></tr>';
    document.getElementById('students').innerHTML = html;
  } catch(e) {}
}
loadData();
setInterval(loadData, 5000);
</script>
</body>
</html>"#;
