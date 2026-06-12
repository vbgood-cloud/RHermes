# RHermes WASM 插件目录

基于 [Extism](https://extism.org/) 的 WASM 插件系统。

## 插件约定

每个 `.wasm` 文件必须导出 4 个函数：

| 函数 | 签名 | 说明 |
|------|------|------|
| `info_name` | `(&str) -> &str` | 工具名称 |
| `info_description` | `(&str) -> &str` | 工具描述 |
| `info_parameters` | `(&str) -> &str` | JSON 参数定义 |
| `execute` | `(&str) -> &str` | JSON 入参 → 结果字符串 |

## 示例插件

### hello — 打招呼

```bash
cd plugins/example_hello
cargo build --target wasm32-unknown-unknown --release
cp target/wasm32-unknown-unknown/release/example_hello.wasm ../plugins/
```

### 用其他语言编写

Extism 支持 Rust / Go / JavaScript / Python / C 等语言的 PDK，
参考 https://extism.org/docs/write-a-plugin
