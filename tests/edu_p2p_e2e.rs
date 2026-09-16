//! 教育版去中心化 P2P —— **离线端到端测试**（不依赖外网、不需要中继）
//!
//! 这是 `docs/edu-p2p-design.md` P1–P4 的验收测试，覆盖 5 条关键链路：
//!
//! | # | 链路 | 断言要点 |
//! |---|---|---|
//! | 1 | 认证链 `/class-auth/1.0` | 密码正确 → 拿到含 TopicId 的票据 |
//! | 2 | 群组通信 gossip | 老师广播 → 学生收到 |
//! | 3 | 越权拦截 | 未认证节点连 `/class-app/1.0` 被 Hook 拒绝 |
//! | 4 | 签名白名单 | 老师广播 → 学生本地 `AuthRegistry` 生效 |
//! | 5 | R3 组合撤销 | 被撤销者拿不到新 Topic、收不到新老师广播；剩余成员正常轮换 |
//!
//! ## 为什么能离线跑
//!
//! 用 `presets::Minimal`（无中继、无 DNS 发现）+ `MemoryLookup`（手工地址簿），
//! 两个节点都绑在本机 loopback 上直连。生产用 `presets::N0`（n0 Relay + DNS）。
//!
//! ## 运行
//!
//! ```bash
//! RH_SKIP_WINRESOURCE=1 cargo test --test edu_p2p_e2e -- --nocapture
//! ```

use std::time::Duration;

use tokio::time::{timeout, Instant};

use rhermes::core::{Config, EduServeSection, EduStudentConfig, EduTeacherConfig, EduTeacherPeer};
use rhermes::edu::authz::AuthRegistry;
use rhermes::edu::gossip::SectionMsg;
use rhermes::edu::net::{client, P2pNode};
use rhermes::edu::runtime::{Enrollment, SectionEvent, SectionKey, StudentRuntime, TeacherRuntime};
use rhermes::edu::store::EduStore;
use rhermes::edu::{client_app, identity, serve};

/// 初始化日志（`RUST_LOG=info` 可见入班 / 白名单 / 拒绝等关键事件）
static INIT_TRACING: std::sync::Once = std::sync::Once::new();
fn init_tracing() {
    INIT_TRACING.call_once(|| {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_test_writer()
            .try_init();
    });
}

// ---------------------------------------------------------------------------
// 测试夹具
// ---------------------------------------------------------------------------

struct Fixture {
    _tmp: tempfile::TempDir,
    db_path: std::path::PathBuf,
    section_id: i64,
}

/// 建一个最小教务场景：1 老师 / 1 课程 / 1 教学班 / 2 学生（均未绑定设备）
fn setup_db() -> Fixture {
    let tmp = tempfile::tempdir().expect("临时目录");
    let db_path = tmp.path().join("edu.db");
    let section_id;
    {
        let store = EduStore::open(&db_path).expect("打开库");
        let t = store.create_teacher("张老师", "tpw").expect("建老师");
        let c = store
            .create_course("CS101", "Python 编程基础", t.id)
            .expect("建课程");
        let s = store.create_class("计算机2301", c.id).expect("建班级");
        store
            .create_student("2024001", "张三", "pw1", Some(s.id))
            .expect("建学生1");
        store
            .create_student("2024002", "李四", "pw2", Some(s.id))
            .expect("建学生2");
        section_id = s.id;
    }
    Fixture {
        _tmp: tmp,
        db_path,
        section_id,
    }
}

/// 学生侧完整入场：建节点 → `/class-auth` 认证 → 凭票据入班。
async fn enroll(
    username: &str,
    password: &str,
    teacher_addr: iroh::EndpointAddr,
) -> StudentRuntime {
    // ① 学生自己的节点（**必须复用同一身份**入班，否则老师白名单里那把钥匙对不上）
    let node = P2pNode::student_offline(AuthRegistry::new())
        .await
        .expect("建学生节点");

    // ② 走认证协议换取票据
    let resp = client::authenticate(&node.endpoint, teacher_addr.clone(), username, password)
        .await
        .expect("认证请求应完成");
    assert!(resp.ok, "认证应成功：{}", resp.message);
    assert!(!resp.tickets.is_empty(), "应下发至少一个教学班票据");
    assert!(
        !resp.tickets[0].topic_id.is_empty(),
        "票据里必须带 TopicId"
    );

    // ③ 凭票据入班（复用同一节点）
    StudentRuntime::from_node(node, resp.tickets, teacher_addr)
        .await
        .expect("入班失败")
}

/// 在事件流里等待第一个匹配的事件（带总超时）。
async fn wait_for<T>(
    rt: &mut StudentRuntime,
    secs: u64,
    mut pred: impl FnMut(&SectionEvent) -> Option<T>,
) -> Option<T> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return None;
        }
        match timeout(remaining, rt.next_event()).await {
            Ok(Some(ev)) => {
                if let Some(v) = pred(&ev) {
                    return Some(v);
                }
            }
            _ => return None,
        }
    }
}

// ---------------------------------------------------------------------------
// 主验收测试
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn edu_p2p_revocation_end_to_end() {
    init_tracing();
    let fx = setup_db();

    // ===== 0. 老师上线（离网）=====
    let mut teacher = TeacherRuntime::start_offline(fx.db_path.clone())
        .await
        .expect("老师节点启动");
    let teacher_addr = teacher.node.endpoint.addr();
    let teacher_id = teacher.node.endpoint.id();
    // 学生端的**全局班键** = (签发老师, 该老师库内的班 id)
    let key = SectionKey::new(teacher_id, fx.section_id);
    assert!(
        !teacher_addr.addrs.is_empty(),
        "离网老师也应至少有 loopback 直连地址"
    );

    // 老师先入班：建会话 + 广播首版白名单（此时只有老师自己）
    teacher
        .publish_allowlist(fx.section_id)
        .await
        .expect("首版白名单广播");

    // ===== 1. 认证链：两名学生拿票据入班 =====
    let mut s1 = enroll("2024001", "pw1", teacher_addr.clone()).await;
    let mut s2 = enroll("2024002", "pw2", teacher_addr.clone()).await;
    let s1_id = s1.node.endpoint.id();
    let s2_id = s2.node.endpoint.id();
    assert_ne!(s1_id, s2_id, "两个学生必须有不同身份");

    // 两名学生都已绑定设备 → 重新广播白名单（现在应含 2 名学生）
    let signed = teacher
        .publish_allowlist(fx.section_id)
        .await
        .expect("二版白名单广播");
    assert_eq!(signed.student_count(), 2, "白名单应含 2 名学生");
    assert_eq!(signed.epoch, 0, "尚未轮换，纪元应为 0");

    // ===== 4. 签名白名单：学生本地生效 =====
    let applied = wait_for(&mut s1, 15, |e| match e {
        SectionEvent::AllowlistApplied { members, epoch, .. } => Some((*members, *epoch)),
        _ => None,
    })
    .await
    .expect("学生应在超时前收到白名单");
    assert_eq!(applied, (3, 0), "白名单成员数应为 老师+2 学生");
    assert!(
        s1.registry.is_authorized(&teacher_id).await,
        "老师应被写入学生本地白名单"
    );
    assert!(
        s1.registry.is_authorized(&s2_id).await,
        "同学应被写入学生本地白名单"
    );

    // 记录轮换前的**发送端** Topic 指纹 —— 5d 用它断言轮换确实换掉了发送端（缺陷 E）
    let topic_before = s2
        .session_topic(&key)
        .await
        .expect("学生 2 应有可用会话 Topic");

    // ===== 2. 群组通信：老师广播 → 学生收到 =====
    teacher
        .announce(fx.section_id, "轮换前公告", "第一版 Topic 上的通知")
        .await
        .expect("老师广播失败");
    let got = wait_for(&mut s1, 15, |e| match e {
        SectionEvent::Message {
            msg: SectionMsg::Announce { title, .. },
            ..
        } if title == "轮换前公告" => {
            Some(())
        }
        _ => None,
    })
    .await;
    assert!(got.is_some(), "学生 1 应收到轮换前公告");

    // ===== 3. 越权拦截：未认证节点不得进入 /class-app =====
    let outsider = P2pNode::student_offline(AuthRegistry::new())
        .await
        .expect("建旁观者节点");
    let denied = timeout(
        Duration::from_secs(10),
        client::who_am_i(&outsider.endpoint, teacher_addr.clone()),
    )
    .await;
    assert!(
        matches!(denied, Ok(Err(_))),
        "未认证节点必须被 Hook 拒绝，实测 = {denied:?}"
    );

    // ===== 5. R3 组合撤销 =====
    let outcome = teacher
        .revoke_member(fx.section_id, "2024001", "端到端测试撤销")
        .await
        .expect("撤销失败");
    assert_eq!(outcome.old_epoch, 0);
    assert_eq!(outcome.new_epoch, 1, "撤销必须轮换 Topic 纪元");
    assert_eq!(
        outcome.revoked_endpoint.as_deref(),
        Some(s1_id.to_string().as_str()),
        "撤销结果应记录被撤销者绑定的 EndpointId"
    );
    assert_eq!(
        outcome.allowlist.student_count(),
        1,
        "轮换后的新白名单只剩李四"
    );
    assert!(
        !outcome.allowlist.contains(&s1_id),
        "新白名单不得含被撤销者"
    );

    // 5a. DB 侧：被撤销者已无有效教学班
    {
        let store = EduStore::open(&fx.db_path).expect("重开库");
        assert!(
            store.sections_for_student("2024001").unwrap().is_empty(),
            "被撤销者不应再查出教学班"
        );
        assert_eq!(
            store.sections_for_student("2024002").unwrap().len(),
            1,
            "其余学生不受影响"
        );
    }

    // 5b. 被撤销者「密码仍然正确」，但不得再拿到任何票据（认证卡口）
    {
        let probe = P2pNode::student_offline(AuthRegistry::new())
            .await
            .expect("建探测节点");
        let r = client::authenticate(&probe.endpoint, teacher_addr.clone(), "2024001", "pw1")
            .await
            .expect("认证请求应完成");
        assert!(!r.ok, "被撤销者不得通过认证");
        assert!(r.tickets.is_empty(), "被撤销者不得拿到票据（=拿不到新 Topic）");
    }

    // 5c. 被撤销者走旧 EndpointId 刷新票据 → 被 Hook 拒绝
    let refresh = timeout(
        Duration::from_secs(10),
        client::refresh_tickets(&s1.node.endpoint, teacher_addr.clone()),
    )
    .await;
    assert!(
        matches!(refresh, Ok(Err(_))),
        "被撤销者不得刷新票据，实测 = {refresh:?}"
    );

    // 5d. 剩余成员应收到轮换通知 → 自动切到新 Topic
    let rotated = wait_for(&mut s2, 20, |e| match e {
        SectionEvent::TopicRotated { new_epoch, .. } => Some(*new_epoch),
        _ => None,
    })
    .await;
    assert_eq!(rotated, Some(1), "剩余成员应轮换到纪元 1");

    // 5d-2. 🔴 缺陷 E 回归：轮换必须同时替换**发送端**，而不只是接收端。
    //
    //       旧实现里接收循环把新会话丢给了 `_s`，只换了收件箱 `rx`；
    //       `StudentRuntime` 内保存的 `SectionSession`（持有 GossipSender）仍指向旧
    //       Topic。用户可见后果：轮换后学生"发得出"却全班收不到，而滞留在旧 Topic 的
    //       被撤销者反而能收到 —— 既是投递失败，也是撤销后的信息泄露。
    let topic_after = s2
        .session_topic(&key)
        .await
        .expect("学生 2 轮换后仍应有会话 Topic");
    assert_ne!(
        topic_before, topic_after,
        "轮换后学生的发送端 Topic 必须改变（缺陷 E：只换接收端不换发送端）"
    );

    // 5e. 被撤销者**不会**切到新 Topic（他刷新不到票据）——
    //     注意：旧 Topic 上的 `TopicRotate` 广播他确实收得到，但只是"纪元变了"的暗号，
    //     没有新 TopicId，且刷新票据被拒 → 永远停留在作废的旧 Topic。
    let leaked = wait_for(&mut s1, 4, |e| match e {
        SectionEvent::TopicRotated { .. } => Some(()),
        _ => None,
    })
    .await;
    assert!(leaked.is_none(), "被撤销者不得随轮换进入新 Topic");

    // ===== 6. 终局验证：新 Topic 上的广播只有合法成员收得到 =====
    teacher
        .announce(fx.section_id, "轮换后公告", "只有本班合法成员可见")
        .await
        .expect("轮换后广播失败");

    let ok = wait_for(&mut s2, 15, |e| match e {
        SectionEvent::Message {
            msg: SectionMsg::Announce { title, .. },
            ..
        } if title == "轮换后公告" => {
            Some(())
        }
        _ => None,
    })
    .await;
    assert!(ok.is_some(), "合法成员应在新 Topic 收到公告");

    let leaked = wait_for(&mut s1, 5, |e| match e {
        SectionEvent::Message {
            msg: SectionMsg::Announce { title, .. },
            ..
        } if title == "轮换后公告" => {
            Some(())
        }
        _ => None,
    })
    .await;
    assert!(
        leaked.is_none(),
        "🔴 撤销失效：被撤销者竟在新 Topic 收到了公告"
    );

    // 收尾
    s2.shutdown().await.ok();
    s1.shutdown().await.ok();
    teacher.shutdown().await.ok();
}

// ---------------------------------------------------------------------------
// 多老师 / 多课程：一个学生同时选修多位老师的课
// ---------------------------------------------------------------------------

/// 为某位老师建一份独立教务库：1 老师 / 1 课程 / 1 教学班 / 1 学生（未绑定设备）
fn setup_teacher_db(db: &std::path::Path, code: &str, course: &str, class: &str) -> i64 {
    let store = EduStore::open(db).expect("打开库");
    let t = store.create_teacher("张老师", "tpw").expect("建老师");
    let c = store.create_course(code, course, t.id).expect("建课程");
    let s = store.create_class(class, c.id).expect("建班级");
    store
        .create_student("2024001", "张三", "pw", Some(s.id))
        .expect("建学生");
    s.id
}

/// 持续收集事件，直到闭包判定「够了」或超时（避免依赖事件到达顺序）。
async fn collect_until<T>(
    rt: &mut StudentRuntime,
    budget: Duration,
    mut done: impl FnMut(&SectionEvent, &mut Vec<T>) -> bool,
) -> Vec<T> {
    let mut acc: Vec<T> = Vec::new();
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        match timeout(Duration::from_secs(5), rt.next_event()).await {
            Ok(Some(ev)) => {
                if done(&ev, &mut acc) {
                    break;
                }
            }
            // 所有会话关闭
            Ok(None) => break,
            // 本轮等待超时 → 继续等（只要总预算未耗尽）
            Err(_) => {}
        }
    }
    acc
}

/// 🔴 缺陷 H 回归：**一个学生同时选修多位老师的课**。
///
/// 旧实现把「老师」当成全局单值：`teacher_from_tickets` 只取首票的 bootstrap、
/// `connect_at` 一个地址全票共用、`apply_signed_allowlist` 只认单一期望签发者 ——
/// 第二位老师的白名单会被「签发者不符」直接拒绝，第二个班永远进不去。
#[tokio::test]
async fn edu_p2p_multi_teacher_end_to_end() {
    init_tracing();

    // ===== 两位老师，各自独立教务库（现实即如此）=====
    let tmp_a = tempfile::tempdir().expect("临时目录A");
    let tmp_b = tempfile::tempdir().expect("临时目录B");
    let db_a = tmp_a.path().join("edu.db");
    let db_b = tmp_b.path().join("edu.db");

    let sec_a = setup_teacher_db(&db_a, "CS101", "Python 编程基础", "计算机2301");
    let sec_b = setup_teacher_db(&db_b, "MA201", "高等数学", "电信2302");
    // ⚠️ 两位老师各自独立建库 → 教学班主键**必然重复**（都从 1 开始）。
    //    这正是学生端必须用 (老师, 班 id) 组合键的原因：
    //    单用 section_id 做键，第二个老师的班会直接覆盖第一个。
    assert_eq!(
        sec_a, sec_b,
        "独立建库的两位老师，班 id 相同 —— 单靠 section_id 无法区分"
    );

    let mut teacher_a = TeacherRuntime::start_offline(db_a.clone())
        .await
        .expect("老师A 启动");
    let mut teacher_b = TeacherRuntime::start_offline(db_b.clone())
        .await
        .expect("老师B 启动");
    teacher_a
        .publish_allowlist(sec_a)
        .await
        .expect("A 首版白名单广播");
    teacher_b
        .publish_allowlist(sec_b)
        .await
        .expect("B 首版白名单广播");

    let addr_a = teacher_a.node.endpoint.addr();
    let addr_b = teacher_b.node.endpoint.addr();
    let id_a = teacher_a.node.node_id();
    let id_b = teacher_b.node.node_id();
    assert_ne!(id_a, id_b, "两位老师必须是不同身份");
    let key_a = SectionKey::new(id_a, sec_a);
    let key_b = SectionKey::new(id_b, sec_b);
    assert_ne!(key_a, key_b, "班 id 相同但老师不同 ⇒ 必须是两个不同的班键");

    // ===== 学生：**同一把钥匙**分别向两位老师认证 =====
    let node = P2pNode::student_offline(AuthRegistry::new())
        .await
        .expect("建学生节点");
    let student_id = node.endpoint.id();

    let ra = client::authenticate(&node.endpoint, addr_a.clone(), "2024001", "pw")
        .await
        .expect("认证A 应完成");
    assert!(ra.ok, "认证 A 应成功：{}", ra.message);
    let rb = client::authenticate(&node.endpoint, addr_b.clone(), "2024001", "pw")
        .await
        .expect("认证B 应完成");
    assert!(rb.ok, "认证 B 应成功：{}", rb.message);
    assert_eq!(
        ra.tickets[0].bootstrap[0],
        id_a.to_string(),
        "A 的票必须由 A 签发"
    );
    assert_eq!(
        rb.tickets[0].bootstrap[0],
        id_b.to_string(),
        "B 的票必须由 B 签发"
    );

    // ===== 一次入班：两位老师、两个班 =====
    let mut stu = StudentRuntime::from_node_multi(
        node,
        vec![
            Enrollment {
                teacher_addr: addr_a.clone(),
                tickets: ra.tickets,
            },
            Enrollment {
                teacher_addr: addr_b.clone(),
                tickets: rb.tickets,
            },
        ],
    )
    .await
    .expect("多老师入班失败");

    assert_eq!(stu.sections().len(), 2, "应同时接入两个班");
    assert_eq!(
        stu.teacher_of(&key_a).map(|a| a.id),
        Some(id_a),
        "A 班归 A 老师"
    );
    assert_eq!(
        stu.teacher_of(&key_b).map(|a| a.id),
        Some(id_b),
        "B 班归 B 老师"
    );
    assert_ne!(
        stu.session_topic(&key_a).await,
        stu.session_topic(&key_b).await,
        "两个班必须是彼此独立的 Topic"
    );

    // 两位老师的白名单都必须被接受（旧实现会拒掉第二位）
    assert!(
        stu.registry.is_authorized(&id_a).await,
        "老师A 应写入学生本地白名单"
    );
    assert!(
        stu.registry.is_authorized(&id_b).await,
        "🔴 缺陷 H：老师B 也必须在白名单（旧实现报「签发者不符」直接拒绝）"
    );
    let b = stu.registry.binding(&student_id).await.expect("学生应有绑定");
    assert_eq!(
        b.sections.len(),
        2,
        "本地绑定应同时含两位老师的班（跨老师也必须并存，缺陷 F）"
    );

    // ===== 两位老师各自再广播一次白名单（此时已含学生）=====
    let sa = teacher_a
        .publish_allowlist(sec_a)
        .await
        .expect("A 二版白名单");
    let sb = teacher_b
        .publish_allowlist(sec_b)
        .await
        .expect("B 二版白名单");
    assert_eq!(sa.student_count(), 1, "A 班白名单应含 1 名学生");
    assert_eq!(sb.student_count(), 1, "B 班白名单应含 1 名学生");

    let applied = collect_until(&mut stu, Duration::from_secs(20), |ev, acc| {
        if let SectionEvent::AllowlistApplied { key, .. } = ev {
            if !acc.contains(key) {
                acc.push(key.clone());
            }
        }
        acc.len() >= 2
    })
    .await;
    assert!(
        applied.contains(&key_a) && applied.contains(&key_b),
        "两班白名单都应生效，实测 = {applied:?}"
    );

    // ===== 两班公告互不串台，且都带正确的 section_id =====
    teacher_a
        .announce(sec_a, "A班公告", "只属于 A 班")
        .await
        .expect("A 广播");
    teacher_b
        .announce(sec_b, "B班公告", "只属于 B 班")
        .await
        .expect("B 广播");

    let got = collect_until(&mut stu, Duration::from_secs(20), |ev, acc| {
        if let SectionEvent::Message {
            key,
            msg: SectionMsg::Announce { title, .. },
        } = ev
        {
            acc.push((key.clone(), title.clone()));
        }
        acc.len() >= 2
    })
    .await;
    assert!(
        got.contains(&(key_a.clone(), "A班公告".to_string())),
        "A 班公告应带 key_a，实测 = {got:?}"
    );
    assert!(
        got.contains(&(key_b.clone(), "B班公告".to_string())),
        "B 班公告应带 key_b，实测 = {got:?}"
    );

    // ===== 学生发言：两个班各自可发，互不影响 =====
    for (k, text) in [(&key_a, "同学们好"), (&key_b, "老师好")] {
        stu.broadcast(
            k,
            &SectionMsg::Chat {
                from: "2024001".to_string(),
                display_name: "张三".to_string(),
                text: text.to_string(),
                ts: String::new(),
            },
        )
        .await
        .expect("学生发言应成功");
    }

    stu.shutdown().await.ok();
    teacher_a.shutdown().await.ok();
    teacher_b.shutdown().await.ok();
}

// ---------------------------------------------------------------------------
// 配置驱动的多老师链路（「每位老师一套凭据」+「多位老师写在配置文件」）
// ---------------------------------------------------------------------------

/// 老师地址里第一条 IP 直连地址（离网测试用）
fn first_ip(addrs: &iroh::EndpointAddr) -> std::net::SocketAddr {
    *addrs
        .ip_addrs()
        .next()
        .expect("离网节点必须至少有 loopback 直连地址")
}

fn write_config(dir: &std::path::Path, cfg: &Config) -> std::path::PathBuf {
    std::fs::create_dir_all(dir).expect("建配置目录");
    let path = dir.join("config.toml");
    cfg.save(&path).expect("写配置");
    path
}

/// 端到端验证「一位学生选多位老师的课」在**真实配置文件**下成立：
///
/// 1. 每位老师一套**持久化凭据**（`home/edu_identities/<工号>.key`）；
/// 2. 师端按 `[[edu.teacher.serve]]` 解析出要托管的班；
/// 3. 学生按 `[[edu.student.teachers]]` 同时接入两位老师，**同一把钥匙**；
/// 4. 两班公告互不串台。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn edu_p2p_config_driven_multi_teacher() {
    init_tracing();

    let tmp_a = tempfile::tempdir().expect("临时目录A");
    let tmp_b = tempfile::tempdir().expect("临时目录B");
    let tmp_stu = tempfile::tempdir().expect("临时目录学生");

    let home_a = tmp_a.path().join("home");
    let home_b = tmp_b.path().join("home");
    let home_stu = tmp_stu.path().join("home");
    for h in [&home_a, &home_b, &home_stu] {
        std::fs::create_dir_all(h).expect("建 home");
    }

    // ===== 1. 两位老师各自的教务库（各自独立 → 班 id 必然重复）=====
    let db_a = home_a.join("edu.db");
    let db_b = home_b.join("edu.db");
    let sec_a = setup_teacher_db(&db_a, "CS101", "Python 编程基础", "计算机2301");
    let sec_b = setup_teacher_db(&db_b, "MA201", "高等数学", "电信2302");
    assert_eq!(sec_a, sec_b, "独立建库的老师，班 id 相同");

    // ===== 2. 每位老师一套持久化凭据 =====
    let key_a = identity::load_or_create(&identity::identity_dir(&home_a), "t001").expect("A 身份");
    let key_b = identity::load_or_create(&identity::identity_dir(&home_b), "t002").expect("B 身份");
    assert_ne!(key_a.to_bytes(), key_b.to_bytes(), "两位老师必须不同身份");
    // 同一工号重复载入必须稳定（第二次启动仍是同一个人）
    let again = identity::load_or_create(&identity::identity_dir(&home_a), "t001").unwrap();
    assert_eq!(again.to_bytes(), key_a.to_bytes(), "身份必须稳定");

    let mut teacher_a = TeacherRuntime::start_with_key(db_a.clone(), true, Some(key_a.clone()))
        .await
        .expect("老师A 启动");
    let mut teacher_b = TeacherRuntime::start_with_key(db_b.clone(), true, Some(key_b.clone()))
        .await
        .expect("老师B 启动");
    assert_eq!(
        teacher_a.node.node_id(),
        key_a.public(),
        "注入的私钥决定 EndpointId（每位老师一套凭据的落点）"
    );

    // ===== 3. 师端：按配置解析出要托管的班 =====
    let store_a = EduStore::open(&db_a).unwrap();
    let store_b = EduStore::open(&db_b).unwrap();
    let picked_a = serve::pick_teacher(&store_a, "张老师").expect("认出老师A");
    let picked_b = serve::pick_teacher(&store_b, "张老师").expect("认出老师B");
    let wanted_a = vec![EduServeSection {
        course: "CS101".into(),
        class: "计算机2301".into(),
        term: String::new(),
    }];
    let want_b = vec![EduServeSection {
        course: "MA201".into(),
        class: "电信2302".into(),
        term: String::new(),
    }];
    let sched_a = serve::resolve_sections(&store_a, picked_a.id, &wanted_a).expect("解析 A 托管项");
    let sched_b = serve::resolve_sections(&store_b, picked_b.id, &want_b).expect("解析 B 托管项");
    assert_eq!(sched_a.len(), 1);
    assert_eq!(sched_a[0].section_id, sec_a);
    assert_eq!(sched_a[0].course_code, "CS101");
    assert_eq!(sched_b.len(), 1);
    assert_eq!(sched_b[0].section_id, sec_b);

    // 师端入班（建会话 + 首版白名单）
    teacher_a.publish_allowlist(sec_a).await.expect("A 白名单");
    teacher_b.publish_allowlist(sec_b).await.expect("B 白名单");

    let addr_a = teacher_a.node.endpoint_addr();
    let addr_b = teacher_b.node.endpoint_addr();
    let ip_a = first_ip(&addr_a);
    let ip_b = first_ip(&addr_b);

    // ===== 4. 学生：两位老师写进配置文件 =====
    let cfg_stu = Config {
        edu: rhermes::core::EduConfig {
            enabled: true,
            role: "student".into(),
            student: EduStudentConfig {
                student_no: "2024001".into(),
                display_name: "张三".into(),
                secret_key: String::new(),
                teachers: vec![
                    EduTeacherPeer {
                        teacher: key_a.public().to_string(),
                        addr: ip_a.to_string(),
                        password: "pw".into(),
                        offline: true,
                        ..Default::default()
                    },
                    EduTeacherPeer {
                        teacher: key_b.public().to_string(),
                        addr: ip_b.to_string(),
                        password: "pw".into(),
                        offline: true,
                        ..Default::default()
                    },
                ],
            },
            ..Default::default()
        },
        ..Default::default()
    };
    let stu_config = write_config(tmp_stu.path(), &cfg_stu);

    // 从磁盘重新读回 —— 验证 TOML 往返不丢字段
    let reloaded = Config::load(&stu_config).expect("读回学生配置");
    assert_eq!(reloaded.edu.student.teachers.len(), 2, "两位老师必须都留在配置里");

    // ===== 5. 按配置一次接入两位老师 =====
    let sess = client_app::join_from_config(&reloaded, &home_stu, true, false)
        .await
        .expect("按配置接入应成功");

    let key_a_sec = SectionKey::new(key_a.public(), sec_a);
    let key_b_sec = SectionKey::new(key_b.public(), sec_b);

    assert_eq!(sess.joined.len(), 2, "应同时接入两个班");
    assert!(sess.failures.is_empty(), "不应有老师接入失败：{:?}", sess.failures);
    assert_eq!(sess.rt.sections().len(), 2, "运行时应有 2 个班会话");
    assert!(
        sess.joined.contains(&client_app::Joined {
            key: key_a_sec.clone(),
            course_code: "CS101".into(),
            course_name: "Python 编程基础".into(),
            class_name: "计算机2301".into(),
        }),
        "A 班标签应由票据直接得出"
    );
    assert!(sess.joined.iter().any(|j| j.key == key_b_sec));

    // 学生身份持久化：节点身份 == 身份文件里的公钥
    let stu_key =
        identity::load_or_create(&identity::identity_dir(&home_stu), "2024001").expect("学生身份");
    assert_eq!(
        sess.rt.node.endpoint.id(),
        stu_key.public(),
        "客户端必须复用持久化身份（否则老师白名单里那把钥匙对不上）"
    );

    // ===== 6. 两班公告互不串台 =====
    let mut sess = sess;
    teacher_a
        .announce(sec_a, "A班公告", "只属于 A 班")
        .await
        .expect("A 广播");
    teacher_b
        .announce(sec_b, "B班公告", "只属于 B 班")
        .await
        .expect("B 广播");

    let got = collect_until(&mut sess.rt, Duration::from_secs(20), |ev, acc| {
        if let SectionEvent::Message {
            key,
            msg: SectionMsg::Announce { title, .. },
        } = ev
        {
            acc.push((key.clone(), title.clone()));
        }
        acc.len() >= 2
    })
    .await;
    assert!(
        got.contains(&(key_a_sec.clone(), "A班公告".to_string())),
        "A 班公告应带 A 的班键，实测 = {got:?}"
    );
    assert!(
        got.contains(&(key_b_sec.clone(), "B班公告".to_string())),
        "B 班公告应带 B 的班键，实测 = {got:?}"
    );

    sess.rt.shutdown().await.ok();
    teacher_a.shutdown().await.ok();
    teacher_b.shutdown().await.ok();
}
