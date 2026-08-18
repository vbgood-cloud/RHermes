#!/usr/bin/env python3
"""RHermes Codegraph 生成器 v1.0
扫描 src/ 生成:
1. docs/codegraph.md — 模块依赖图(Mermaid) + 类型定义 + pub API + 统计
2. docs/codegraph.json — 机器可读的模块/依赖/符号数据
"""
import os, re, json
from collections import defaultdict

ROOT = os.path.expanduser("~/lab/RHermes")
SRC = os.path.join(ROOT, "src")

# ── 1. 收集所有 .rs 文件 ──
rs_files = []
for dirpath, dirs, files in os.walk(SRC):
    dirs.sort()
    for f in sorted(files):
        if f.endswith(".rs"):
            full = os.path.join(dirpath, f)
            rel = os.path.relpath(full, SRC).replace(os.sep, "/")
            rs_files.append((rel, full))
rs_files.sort()

# ── 2. 每文件：行数 / mod 声明 / use crate 依赖 / pub 项 ──
info = {}
for rel, full in rs_files:
    src = open(full, encoding="utf-8", errors="replace").read()
    lines = src.count("\n") + 1

    # 内部模块依赖：use crate::xxx / use super:: 父级 / pub mod
    deps = set()
    for m in re.finditer(r"use\s+crate::([a-z_]+)", src):
        deps.add(m.group(1))
    for m in re.finditer(r"use\s+super::super::([a-z_]+)", src):
        deps.add(m.group(1))
    # mod 声明的子模块也算依赖（父子关系）
    mods = re.findall(r"(?:pub\s+)?mod\s+([a-z_]+)\s*;", src)

    # pub 项统计
    pub_fns = len(re.findall(r"(?:pub\s+)(?:async\s+)?fn\s+", src))
    pub_structs = re.findall(r"pub\s+(?:struct|enum)\s+([A-Z]\w+)", src)
    pub_traits = re.findall(r"pub\s+trait\s+(\w+)", src)
    tests = len(re.findall(r"#\[(?:tokio::)?test\]", src))

    info[rel] = {
        "lines": lines,
        "deps": sorted(d for d in deps if d != rel.split("/")[0] or "/" in rel),
        "mods": mods,
        "pub_fns": pub_fns,
        "pub_structs": pub_structs,
        "pub_traits": pub_traits,
        "tests": tests,
    }

# ── 3. 顶层模块聚合 ──
top = defaultdict(lambda: {"files": 0, "lines": 0, "fns": 0, "tests": 0, "traits": [], "structs": []})
for rel, d in info.items():
    t = rel.split("/")[0].replace(".rs", "")
    top[t]["files"] += 1
    top[t]["lines"] += d["lines"]
    top[t]["fns"] += d["pub_fns"]
    top[t]["tests"] += d["tests"]
    top[t]["traits"].extend(d["pub_traits"])
    top[t]["structs"].extend(d["pub_structs"])

# ── 4. 顶层模块间依赖（文件级 deps 映射到顶层）──
top_deps = defaultdict(set)
for rel, d in info.items():
    src_top = rel.split("/")[0].replace(".rs", "")
    for dep in d["deps"]:
        if dep in top and dep != src_top:
            top_deps[src_top].add(dep)

# ── 5. 输出 JSON ──
graph = {
    "version": "0.7.0",
    "generated": "2026-08-18",
    "totals": {
        "files": len(rs_files),
        "lines": sum(d["lines"] for d in info.values()),
        "pub_fns": sum(d["pub_fns"] for d in info.values()),
        "tests": sum(d["tests"] for d in info.values()),
        "traits": sorted({t for v in top.values() for t in v["traits"]}),
    },
    "modules": {t: {"files": v["files"], "lines": v["lines"], "pub_fns": v["fns"],
                     "tests": v["tests"], "traits": v["traits"], "deps": sorted(top_deps.get(t, []))}
                for t, v in sorted(top.items())},
    "files": info,
}
with open(os.path.join(ROOT, "docs/codegraph.json"), "w", encoding="utf-8") as f:
    json.dump(graph, f, ensure_ascii=False, indent=2)

# ── 6. 输出 Markdown（Mermaid 图）──
md = []
md.append("# RHermes Codegraph\n")
md.append("> v0.7.0 · 87 .rs · ~34,500 行 · 自动生成（`python3 docs/codegraph_gen.py` 或询问 Agent 重新生成）\n")

total = graph["totals"]
md.append("## 总览\n")
md.append("| 指标 | 值 |")
md.append("|------|-----|")
md.append(f"| 源文件 | {total['files']} |")
md.append(f"| 总行数 | {total['lines']:,} |")
md.append(f"| pub fn | {total['pub_fns']} |")
md.append(f"| 单元测试 | {total['tests']} |")
md.append(f"| 核心 trait | {' · '.join(total['traits'][:12])} |\n")

md.append("## 模块依赖图\n")
md.append("```mermaid")
md.append("graph TD")
# 节点带规模标签
for t, v in sorted(top.items()):
    label = f"{t}<br/>{v['files']}f/{v['lines']//1000}k行"
    md.append(f"    {t}[\"{label}\"]")
md.append("")
for t in sorted(top_deps):
    for dep in sorted(top_deps[t]):
        md.append(f"    {t} --> {dep}")
md.append("```\n")

md.append("## 模块清单\n")
md.append("| 模块 | 文件 | 行数 | pub fn | 测试 | traits | 依赖 |")
md.append("|------|-----:|-----:|-------:|-----:|--------|------|")
for t, v in sorted(top.items(), key=lambda x: -x[1]["lines"]):
    tr = ", ".join(v["traits"]) if v["traits"] else "—"
    deps_list = sorted(top_deps.get(t, []))
    dp = ", ".join(deps_list) if deps_list else "—"
    md.append(f"| `{t}/` | {v['files']} | {v['lines']:,} | {v['fns']} | {v['tests']} | {tr} | {dp} |")
md.append("")

md.append("## 核心 trait 索引\n")
trait_loc = defaultdict(list)
for rel, d in info.items():
    for t in d["pub_traits"]:
        trait_loc[t].append(rel)
for t in sorted(trait_loc):
    locs = ", ".join(f"`{l}`" for l in sorted(trait_loc[t])[:3])
    md.append(f"- **`{t}`** — {locs}")
md.append("")

md.append("## 关键类型（跨模块 pub struct/enum，按模块）\n")
for t, v in sorted(top.items(), key=lambda x: -x[1]["lines"]):
    if v["structs"]:
        structs = ", ".join(f"`{s}`" for s in v["structs"][:10])
        more = f" (+{len(v['structs'])-10})" if len(v["structs"]) > 10 else ""
        md.append(f"- **{t}/**: {structs}{more}")
md.append("")

md.append("## 大文件 Top 15\n")
md.append("| 文件 | 行数 | pub fn | 测试 |")
md.append("|------|-----:|-------:|-----:|")
for rel, d in sorted(info.items(), key=lambda x: -x[1]["lines"])[:15]:
    md.append(f"| `{rel}` | {d['lines']:,} | {d['pub_fns']} | {d['tests']} |")
md.append("")

with open(os.path.join(ROOT, "docs/codegraph.md"), "w", encoding="utf-8") as f:
    f.write("\n".join(md))

print(f"codegraph.md + codegraph.json 生成完毕")
print(f"模块 {len(top)} 个 | 文件 {len(rs_files)} | 行 {total['lines']:,} | 测试 {total['tests']}")
for t, v in sorted(top.items(), key=lambda x: -x[1]["lines"]):
    print(f"  {t:14s} {v['files']:3d}f {v['lines']:6,d}行 {v['tests']:3d}测试 deps={sorted(top_deps[t])}")
