//! RHermes WASM 插件示例 — 打招呼工具
//!
//! 约定：每个插件必须导出 4 个函数（info_name / info_description /
//!        info_parameters / execute），均以 &str 入参出参。
//!
//! 编译: cargo build --target wasm32-unknown-unknown --release
//! 部署: cp target/wasm32-unknown-unknown/release/example_hello.wasm ../plugins/

use extism_pdk::*;
use serde_json::Value;

/// 工具名称（返回给 Agent）
#[plugin_fn]
pub fn info_name(_: String) -> FnResult<String> {
    Ok("hello".into())
}

/// 工具描述
#[plugin_fn]
pub fn info_description(_: String) -> FnResult<String> {
    Ok("打招呼工具，输入名字返回问候语".into())
}

/// 参数定义（JSON 格式的 ParamDef 数组）
#[plugin_fn]
pub fn info_parameters(_: String) -> FnResult<String> {
    Ok(r#"[
        {
            "name": "who",
            "type": "string",
            "description": "要问候的人或事物",
            "required": true
        }
    ]"#
    .into())
}

/// 实际执行逻辑
#[plugin_fn]
pub fn execute(args: String) -> FnResult<String> {
    let input: Value = serde_json::from_str(&args)
        .map_err(|e| WithReturnCode(e.to_string(), 1))?;

    let who = input["who"].as_str().unwrap_or("世界");
    Ok(format!("你好，{}！🦀 来自 WASM 插件的问候", who))
}
