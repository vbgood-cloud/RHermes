//! 图谱布局 — 无依赖手写分层布局
//!
//! 1. compute_layers: Kahn 拓扑分层（基础在下，进阶在上）
//! 2. compute_positions: 层内水平排布 + 居中对齐 → (x, y) 坐标供 SVG 渲染

use std::collections::{HashMap, HashSet, VecDeque};

use super::store::GraphSnapshot;

/// Kahn 分层：layer(n) = max(layer(前置)) + 1，无入边的节点 layer = 0。
/// 环中节点按"最大层级截断"处理（不 panic）。
pub fn compute_layers(snapshot: &GraphSnapshot) -> HashMap<String, i64> {
    let names: HashSet<&str> = snapshot.nodes.iter().map(|n| n.name.as_str()).collect();
    // 只统计有效边
    let edges: Vec<(&str, &str)> = snapshot
        .edges
        .iter()
        .filter(|e| names.contains(e.from.as_str()) && names.contains(e.to.as_str()))
        .map(|e| (e.from.as_str(), e.to.as_str()))
        .collect();

    // 入度表
    let mut indegree: HashMap<&str, usize> = names.iter().map(|n| (*n, 0usize)).collect();
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    for (from, to) in &edges {
        adj.entry(from).or_default().push(to);
        *indegree.entry(to).or_insert(0) += 1;
    }

    let mut layers: HashMap<&str, i64> = HashMap::new();
    let mut queue: VecDeque<&str> = indegree.iter().filter(|(_, d)| **d == 0).map(|(n, _)| *n).collect();
    for n in &queue {
        layers.insert(n, 0);
    }

    while let Some(cur) = queue.pop_front() {
        let cur_layer = layers[&cur];
        if let Some(nexts) = adj.get(cur) {
            for &next in nexts {
                let d = indegree.get_mut(next).unwrap();
                *d -= 1;
                let new_layer = cur_layer + 1;
                let e = layers.entry(next).or_insert(0);
                if new_layer > *e {
                    *e = new_layer;
                }
                if *d == 0 {
                    queue.push_back(next);
                }
            }
        }
    }

    // 环中节点（未分层）：放到已分层最大层 + 1
    let max_layer = layers.values().copied().max().unwrap_or(0);
    for n in &names {
        layers.entry(n).or_insert(max_layer + 1);
    }

    layers.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

/// SVG 布局坐标：每层一行，层内均分水平位置并整体居中。
/// 返回 name → (x, y)（SVG 像素坐标）
pub fn compute_positions(snapshot: &GraphSnapshot) -> HashMap<String, (f64, f64)> {
    // 按层分组（保持 snapshot 的稳定顺序）
    let mut by_layer: Vec<(i64, Vec<&str>)> = Vec::new();
    for node in &snapshot.nodes {
        if let Some(slot) = by_layer.iter_mut().find(|(l, _)| *l == node.layer) {
            slot.1.push(&node.name);
        } else {
            by_layer.push((node.layer, vec![&node.name]));
        }
    }
    by_layer.sort_by_key(|(l, _)| *l);

    const NODE_W: f64 = 170.0; // 节点框宽
    const GAP_X: f64 = 34.0;   // 水平间距
    const GAP_Y: f64 = 110.0;  // 层间距

    let max_count = by_layer.iter().map(|(_, v)| v.len()).max().unwrap_or(1).max(1);
    let total_w = max_count as f64 * NODE_W + (max_count - 1) as f64 * GAP_X;
    let margin_x = 40.0;
    let margin_y = 70.0;

    let mut pos = HashMap::new();
    for (row, (_, names)) in by_layer.iter().enumerate() {
        let n = names.len() as f64;
        let row_w = n * NODE_W + (n - 1.0) * GAP_X;
        let start_x = margin_x + (total_w - row_w) / 2.0; // 居中
        let y = margin_y + row as f64 * GAP_Y;
        for (i, name) in names.iter().enumerate() {
            pos.insert(name.to_string(), (start_x + i as f64 * (NODE_W + GAP_X), y));
        }
    }
    pos
}

/// 画布尺寸（宽, 高）
pub fn canvas_size(snapshot: &GraphSnapshot, positions: &HashMap<String, (f64, f64)>) -> (f64, f64) {
    let max_x = positions.values().map(|(x, _)| x + 170.0 + 40.0).fold(0.0_f64, f64::max);
    let max_y = positions.values().map(|(_, y)| y + 60.0 + 60.0).fold(0.0_f64, f64::max);
    let _ = snapshot;
    (max_x.max(600.0), max_y.max(320.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::store::{EdgeRow, NodeRow};

    fn snap(nodes: &[(&str, i64)], edges: &[(&str, &str)]) -> GraphSnapshot {
        GraphSnapshot {
            topic: "t".into(),
            nodes: nodes
                .iter()
                .map(|(n, l)| NodeRow {
                    id: 0, name: n.to_string(), summary: String::new(),
                    layer: *l, mastery: 0, review_count: 0, quiz_count: 0,
                })
                .collect(),
            edges: edges
                .iter()
                .map(|(f, t)| EdgeRow { from: f.to_string(), to: t.to_string(), relation: "依赖".into() })
                .collect(),
        }
    }

    #[test]
    fn test_chain_layers() {
        let s = snap(&[("a", 0), ("b", 0), ("c", 0)], &[("a", "b"), ("b", "c")]);
        let l = compute_layers(&s);
        assert_eq!(l["a"], 0);
        assert_eq!(l["b"], 1);
        assert_eq!(l["c"], 2);
    }

    #[test]
    fn test_diamond() {
        let s = snap(&[("a", 0), ("b", 0), ("c", 0), ("d", 0)], &[("a", "b"), ("a", "c"), ("b", "d"), ("c", "d")]);
        let l = compute_layers(&s);
        assert_eq!(l["a"], 0);
        assert_eq!(l["b"], 1);
        assert_eq!(l["c"], 1);
        assert_eq!(l["d"], 2);
    }

    #[test]
    fn test_cycle_no_panic() {
        let s = snap(&[("a", 0), ("b", 0)], &[("a", "b"), ("b", "a")]);
        let l = compute_layers(&s); // 不 panic 即通过
        assert!(l.contains_key("a"));
        assert!(l.contains_key("b"));
    }

    #[test]
    fn test_isolated_nodes_layer0() {
        let s = snap(&[("孤立", 0), ("a", 0), ("b", 0)], &[("a", "b")]);
        let l = compute_layers(&s);
        assert_eq!(l["孤立"], 0);
    }

    #[test]
    fn test_positions_centered() {
        let s = snap(&[("a", 0), ("b", 0), ("c", 1)], &[("a", "c"), ("b", "c")]);
        let p = compute_positions(&s);
        // 同层两节点水平排开
        assert!(p["a"].0 < p["b"].0);
        assert_eq!(p["a"].1, p["b"].1);
        // 高层节点 y 更小（SVG 顶部）或按行递增——本实现按行递增
        assert!(p["c"].1 > p["a"].1);
        // 全部坐标为正
        for (x, y) in p.values() {
            assert!(*x > 0.0 && *y > 0.0);
        }
    }

    #[test]
    fn test_canvas() {
        let s = snap(&[("a", 0)], &[]);
        let p = compute_positions(&s);
        let (w, h) = canvas_size(&s, &p);
        assert!(w >= 600.0 && h >= 320.0);
    }
}
