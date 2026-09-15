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

use rhermes::edu::authz::AuthRegistry;
use rhermes::edu::gossip::SectionMsg;
use rhermes::edu::net::{client, P2pNode};
use rhermes::edu::runtime::{SectionEvent, StudentRuntime, TeacherRuntime};
use rhermes::edu::store::EduStore;

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

    // ===== 2. 群组通信：老师广播 → 学生收到 =====
    teacher
        .announce(fx.section_id, "轮换前公告", "第一版 Topic 上的通知")
        .await
        .expect("老师广播失败");
    let got = wait_for(&mut s1, 15, |e| match e {
        SectionEvent::Message(SectionMsg::Announce { title, .. }) if title == "轮换前公告" => {
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
        SectionEvent::Message(SectionMsg::Announce { title, .. }) if title == "轮换后公告" => {
            Some(())
        }
        _ => None,
    })
    .await;
    assert!(ok.is_some(), "合法成员应在新 Topic 收到公告");

    let leaked = wait_for(&mut s1, 5, |e| match e {
        SectionEvent::Message(SectionMsg::Announce { title, .. }) if title == "轮换后公告" => {
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
