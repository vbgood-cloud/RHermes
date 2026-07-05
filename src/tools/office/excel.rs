//! Excel 读写工具
//!
//! - read_excel: 使用 calamine 读取 .xlsx/.xls 文件
//! - write_excel: 使用 rust_xlsxwriter 写入 .xlsx 文件

use async_trait::async_trait;
use serde_json::Value;

use crate::tools::office::check_workspace;
use crate::tools::{ParamDef, ParamType, Tool, ToolError};

// ---- read_excel ----

/// 读取 Excel 文件（.xlsx/.xls），返回 CSV 格式的表格数据
pub struct ReadExcel;

#[async_trait]
impl Tool for ReadExcel {
    fn name(&self) -> String {
        "read_excel".into()
    }
    fn description(&self) -> String {
        "读取 Excel 文件(.xlsx/.xls)，返回表格数据。可指定工作表名称".into()
    }
    fn parallel_safe(&self) -> bool {
        true
    }
    fn parameters(&self) -> Vec<ParamDef> {
        vec![
            ParamDef::required("path", ParamType::String, "Excel 文件路径"),
            ParamDef::optional("sheet", ParamType::String, "工作表名称（不指定则读第一个）"),
            ParamDef::optional("max_rows", ParamType::Integer, "最多返回的行数（默认 500）"),
        ]
    }
    async fn execute(&self, args: Value) -> Result<String, ToolError> {
        let path = crate::tools::get_string_arg(&args, "path")?;
        let sheet = crate::tools::get_optional_string(&args, "sheet");
        let max_rows = args
            .get("max_rows")
            .and_then(|v| v.as_u64())
            .unwrap_or(500) as usize;

        let abs = check_workspace(&path).map_err(ToolError::ExecutionFailed)?;

        // spawn_blocking: calamine 是同步 API
        let result = tokio::task::spawn_blocking(move || -> Result<String, String> {
            use calamine::{open_workbook, Reader, Xlsx};

            let mut workbook: Xlsx<_> = open_workbook(&abs)
                .map_err(|e| format!("打开 Excel 失败: {e}"))?;

            // 确定要读取的 sheet
            let sheet_names = workbook.sheet_names();
            if sheet_names.is_empty() {
                return Err("Excel 文件中没有工作表".into());
            }

            let target_sheet = if let Some(s) = &sheet {
                if sheet_names.iter().any(|n| n == s) {
                    s.clone()
                } else {
                    return Err(format!(
                        "工作表 '{}' 不存在，可用: {}",
                        s,
                        sheet_names.join(", ")
                    ));
                }
            } else {
                sheet_names[0].clone()
            };

            let range = workbook
                .worksheet_range(&target_sheet)
                .map_err(|e| format!("读取工作表失败: {e}"))?;

            let height = range.height();
            let width = range.width();
            if height == 0 || width == 0 {
                return Ok(format!("📊 Sheet \"{target_sheet}\" 为空"));
            }

            // 转为 CSV
            let limited_rows = height.min(max_rows);
            let mut csv = String::with_capacity(limited_rows * width * 8);
            for (row_idx, row) in range.rows().take(limited_rows).enumerate() {
                for (col_idx, cell) in row.iter().enumerate() {
                    if col_idx > 0 {
                        csv.push(',');
                    }
                    // 转义 CSV 中的逗号和引号
                    let cell_str = format_cell(cell);
                    if cell_str.contains(',') || cell_str.contains('"') || cell_str.contains('\n') {
                        csv.push('"');
                        csv.push_str(&cell_str.replace('"', "\"\""));
                        csv.push('"');
                    } else {
                        csv.push_str(&cell_str);
                    }
                }
                csv.push('\n');
            }

            let truncated = if height > limited_rows {
                format!("\n（截断，共 {height} 行，显示前 {limited_rows} 行）")
            } else {
                String::new()
            };

            Ok(format!(
                "📊 Sheet \"{target_sheet}\" ({height} 行 × {width} 列){truncated}:\n{csv}"
            ))
        })
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("Excel 线程崩溃: {e}")))?
        .map_err(ToolError::ExecutionFailed)?;

        Ok(result)
    }
}

/// 将 calamine Data 格式化为字符串
fn format_cell(data: &calamine::Data) -> String {
    use calamine::DataType;
    if data.is_empty() {
        String::new()
    } else if let Some(s) = data.get_string() {
        s.to_string()
    } else if let Some(i) = data.get_int() {
        i.to_string()
    } else if let Some(f) = data.get_float() {
        // 整数浮点不显示小数点
        if f.fract() == 0.0 && f.abs() < 1e15 {
            format!("{}", f as i64)
        } else {
            format!("{f}")
        }
    } else if let Some(b) = data.get_bool() {
        b.to_string()
    } else {
        data.as_string().unwrap_or_default()
    }
}

// ---- write_excel ----

/// 将 JSON 二维数组写入 Excel 文件
pub struct WriteExcel;

#[async_trait]
impl Tool for WriteExcel {
    fn name(&self) -> String {
        "write_excel".into()
    }
    fn description(&self) -> String {
        "将 JSON 二维数组数据写入 Excel(.xlsx) 文件。data 参数为数组的数组".into()
    }
    fn parallel_safe(&self) -> bool {
        false
    }
    fn parameters(&self) -> Vec<ParamDef> {
        vec![
            ParamDef::required("path", ParamType::String, "输出文件路径(.xlsx)"),
            ParamDef::required("data", ParamType::Array, "二维数组数据，如 [[\"姓名\",\"年龄\"],[\"张三\",28]]"),
            ParamDef::optional("sheet", ParamType::String, "工作表名称（默认 Sheet1）"),
        ]
    }
    async fn execute(&self, args: Value) -> Result<String, ToolError> {
        let path = crate::tools::get_string_arg(&args, "path")?;
        let sheet_name = crate::tools::get_optional_string(&args, "sheet")
            .unwrap_or_else(|| "Sheet1".into());
        let data = args
            .get("data")
            .ok_or_else(|| ToolError::MissingParam("data".into()))?
            .as_array()
            .ok_or_else(|| ToolError::InvalidParam("data 必须是数组".into()))?
            .clone();

        let abs = check_workspace(&path).map_err(ToolError::ExecutionFailed)?;

        let sheet_name_for_msg = sheet_name.clone();
        let result = tokio::task::spawn_blocking(move || -> Result<(usize, usize), String> {
            use rust_xlsxwriter::{Format, Workbook};

            let mut wb = Workbook::new();
            let ws = wb
                .add_worksheet()
                .set_name(&sheet_name)
                .map_err(|e| format!("设置工作表名失败: {e}"))?;

            let header_format = Format::new().set_bold();
            let mut row_count = 0usize;
            let mut col_count = 0usize;

            for (row_idx, row) in data.iter().enumerate() {
                let row_arr = row.as_array().ok_or_else(|| {
                    format!("data[{row_idx}] 不是数组（每行必须是数组）")
                })?;
                row_count += 1;
                if row_arr.len() > col_count {
                    col_count = row_arr.len();
                }

                for (col_idx, cell) in row_arr.iter().enumerate() {
                    // 第一行用表头格式（加粗）
                    let format = if row_idx == 0 { &header_format } else { &Format::new() };

                    match cell {
                        Value::String(s) => {
                            ws.write_string_with_format(row_idx as u32, col_idx as u16, s, format)
                                .map_err(|e| format!("写入失败: {e}"))?;
                        }
                        Value::Number(n) => {
                            if let Some(i) = n.as_i64() {
                                ws.write_number_with_format(row_idx as u32, col_idx as u16, i as f64, format)
                                    .map_err(|e| format!("写入失败: {e}"))?;
                            } else if let Some(f) = n.as_f64() {
                                ws.write_number_with_format(row_idx as u32, col_idx as u16, f, format)
                                    .map_err(|e| format!("写入失败: {e}"))?;
                            }
                        }
                        Value::Bool(b) => {
                            ws.write_string_with_format(row_idx as u32, col_idx as u16, b.to_string(), format)
                                .map_err(|e| format!("写入失败: {e}"))?;
                        }
                        Value::Null => {}
                        _ => {
                            // 复杂类型序列化为 JSON 字符串
                            let s = cell.to_string();
                            ws.write_string_with_format(row_idx as u32, col_idx as u16, s, format)
                                .map_err(|e| format!("写入失败: {e}"))?;
                        }
                    }
                }
            }

            // 自适应列宽（简单估算）
            for col in 0..col_count as u16 {
                let _ = ws.set_column_width(col, 15);
            }

            wb.save(&abs).map_err(|e| format!("保存 Excel 失败: {e}"))?;

            Ok((row_count, col_count))
        })
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("Excel 写入线程崩溃: {e}")))?
        .map_err(ToolError::ExecutionFailed)?;

        Ok(format!(
            "✅ 已生成 {path} (Sheet: {sheet_name_for_msg}, {rows} 行 × {cols} 列)",
            rows = result.0,
            cols = result.1
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::builtin::GLOBAL_WORKSPACE;

    /// 确保 GLOBAL_WORKSPACE 已初始化（与 dispatcher 测试一致）
    fn ensure_workspace() {
        GLOBAL_WORKSPACE.get_or_init(|| {
            std::env::current_dir()
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_else(|_| ".".to_string())
        });
    }

    #[test]
    fn test_read_write_excel_roundtrip() {
        ensure_workspace();
        let ws = GLOBAL_WORKSPACE.get().unwrap();
        let test_dir = format!("{ws}/target/tmp_office_test");
        std::fs::create_dir_all(&test_dir).unwrap();
        let path_str = format!("{test_dir}/test_excel.xlsx");

        // 先写
        let write_tool = WriteExcel;
        let data = serde_json::json!([
            ["姓名", "年龄", "城市"],
            ["张三", 28, "北京"],
            ["李四", 35, "上海"],
        ]);
        let args = serde_json::json!({
            "path": &path_str,
            "data": data,
            "sheet": "员工"
        });
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(write_tool.execute(args));
        assert!(result.is_ok(), "写入失败: {:?}", result);

        // 再读
        let read_tool = ReadExcel;
        let args = serde_json::json!({ "path": &path_str, "sheet": "员工" });
        let result = rt.block_on(read_tool.execute(args));
        assert!(result.is_ok(), "读取失败: {:?}", result);
        let content = result.unwrap();
        assert!(content.contains("张三"), "内容缺少张三: {content}");
        assert!(content.contains("员工"), "Sheet 名错误: {content}");

        // 清理
        let _ = std::fs::remove_file(&path_str);
        let _ = std::fs::remove_dir(&test_dir);
    }
}
