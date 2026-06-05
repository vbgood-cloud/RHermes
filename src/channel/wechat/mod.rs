//! 微信个号 iLink Bot 通道
//!
//! 通过 iLink Bot API 扫码登录微信个号，实现消息收发。
//!
//! ## 配置
//! ```toml
//! [channels.wechat]
//! enabled = true
//! poll_interval_secs = 2
//! token_path = "home/wechat_token.txt"
//! proxy = "http://127.0.0.1:7890"   # 可选
//! ```

use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use rand::Rng;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc};

use crate::channel::{Channel, InboundMessage};
use crate::core::Config;

// ---------------------------------------------------------------------------
// 常量
// ---------------------------------------------------------------------------

/// iLink Bot API 基础地址
const API_BASE: &str = "https://ilinkai.weixin.qq.com";

// ---------------------------------------------------------------------------
// API 请求/响应结构
// ---------------------------------------------------------------------------

/// 获取二维码响应
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct QrCodeResponse {
    qrcode: Option<String>,
    qrcode_img_content: Option<String>,
}

/// 登录状态响应
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct QrCodeStatusResponse {
    status: Option<String>,
    bot_token: Option<String>,
    ilink_bot_id: Option<String>,
    ilink_user_id: Option<String>,
}

/// 拉取消息请求
#[derive(Debug, Serialize)]
struct GetUpdatesRequest {
    get_updates_buf: String,
}

/// 拉取消息响应
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct GetUpdatesResponse {
    ret: Option<i32>,
    errcode: Option<i32>,
    msgs: Option<Vec<WeChatMessage>>,
    get_updates_buf: Option<String>,
}

/// 微信消息
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct WeChatMessage {
    message_type: i32,
    from_user_id: String,
    to_user_id: String,
    context_token: Option<String>,
    item_list: Option<Vec<MessageItem>>,
}

/// 消息内容项
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct MessageItem {
    #[serde(rename = "type")]
    item_type: i32,
    text_item: Option<TextItem>,
}

/// 文本内容
#[derive(Debug, Deserialize, Serialize)]
#[allow(dead_code)]
struct TextItem {
    text: String,
}

/// 发送消息请求
#[derive(Debug, Serialize)]
struct SendMessageRequest {
    msg: SendMessageBody,
}

/// 发送消息体
#[derive(Debug, Serialize)]
struct SendMessageBody {
    from_user_id: String,
    to_user_id: String,
    client_id: String,
    message_type: i32,
    message_state: i32,
    item_list: Vec<SendMessageItem>,
    context_token: String,
}

/// 发送消息内容项
#[derive(Debug, Serialize)]
struct SendMessageItem {
    #[serde(rename = "type")]
    item_type: i32,
    text_item: TextItem,
}

// ---------------------------------------------------------------------------
// 辅助函数
// ---------------------------------------------------------------------------

/// 生成 base64 编码的随机数（用于 X-WECHAT-UIN 请求头）
fn generate_uin() -> String {
    let mut rng = rand::rngs::OsRng;
    let mut bytes = [0u8; 16];
    rng.fill(&mut bytes);
    BASE64.encode(bytes)
}

/// 生成客户端 ID（格式: wcc-{timestamp}-{hex}）
fn generate_client_id() -> String {
    let timestamp = chrono::Utc::now().timestamp_millis();
    let mut rng = rand::rngs::OsRng;
    let hex_val: u64 = rng.r#gen();
    format!("wcc-{}-{:016x}", timestamp, hex_val)
}

/// 按最大长度拆分文本（微信单条消息限制）
fn split_message(text: &str, max_len: usize) -> Vec<String> {
    if text.len() <= max_len {
        return vec![text.to_string()];
    }
    let mut chunks = Vec::new();
    let mut remaining = text;
    while !remaining.is_empty() {
        let mut split_at = max_len.min(remaining.len());
        while !remaining.is_char_boundary(split_at) {
            split_at -= 1;
        }
        if split_at == 0 {
            split_at = remaining.len();
        }
        chunks.push(remaining[..split_at].to_string());
        remaining = &remaining[split_at..];
    }
    chunks
}

// ---------------------------------------------------------------------------
// WeChatChannel
// ---------------------------------------------------------------------------

/// 微信个号 iLink Bot 通道
pub struct WeChatChannel {
    config: Arc<Config>,
    client: Client,
    /// 最近收到的 context_token（按 chat_id 缓存，用于回复时关联对话）
    context_tokens: Mutex<HashMap<String, String>>,
}

impl WeChatChannel {
    /// 创建新的微信个号通道
    pub fn new(config: &Config) -> Self {
        let wechat = &config.channels.wechat;

        let mut client_builder = Client::builder()
            .timeout(Duration::from_secs(15));

        // 配置代理
        if let Some(ref proxy_url) = wechat.proxy {
            if let Ok(proxy) = reqwest::Proxy::all(proxy_url) {
                client_builder = client_builder.proxy(proxy);
            } else {
                tracing::warn!("WeChat: 代理地址无效: {}", proxy_url);
            }
        }

        let client = client_builder
            .build()
            .expect("创建 HTTP 客户端失败");

        Self {
            config: Arc::new(config.clone()),
            client,
            context_tokens: Mutex::new(HashMap::new()),
        }
    }

    /// 构建通用请求头（每次调用生成新的 UIN，避免 API 侧缓存失效）
    fn build_headers(&self, bot_token: &str) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", bot_token).parse().unwrap(),
        );
        headers.insert(
            "AuthorizationType",
            "ilink_bot_token".parse().unwrap(),
        );
        headers.insert(
            "X-WECHAT-UIN",
            generate_uin().parse().unwrap(),
        );
        headers.insert(
            "iLink-App-ClientVersion",
            "1".parse().unwrap(),
        );
        headers
    }

    /// 获取登录二维码
    ///
    /// 返回 (二维码文本, BMP 图片字节)
    /// 二维码文本可能是 URL（用于显示/扫码）或 hash（用于 API 调用）
    async fn fetch_qrcode(&self) -> Result<(String, Vec<u8>), String> {
        let url = format!("{}/ilink/bot/get_bot_qrcode?bot_type=3", API_BASE);

        let resp = self
            .client
            .get(&url)
            .header("X-WECHAT-UIN", &generate_uin())
            .send()
            .await
            .map_err(|e| format!("获取二维码失败: {e}"))?;

        let result: QrCodeResponse = resp
            .json()
            .await
            .map_err(|e| format!("二维码响应解析失败: {e}"))?;

        let qrcode = result.qrcode.ok_or_else(|| "二维码响应中缺少 qrcode 字段".to_string())?;
        let img_content = result.qrcode_img_content.ok_or_else(|| "二维码响应中缺少图片数据".to_string())?;

        tracing::info!("WeChat: 请用微信扫描二维码或访问链接登录");
        tracing::info!("WeChat: 二维码链接: {}", img_content);

        // 如果返回的是 URL 而非 base64 图片，生成二维码图片并返回 URL
        if img_content.starts_with("http://") || img_content.starts_with("https://") {
            let png_data = generate_qr_png(&img_content)
                .map_err(|e| format!("QR 码图片生成失败: {e}"))?;
            // 返回 URL 作为第一元素（用于 ASCII 二维码和显示）
            // `qrcode` hash 保存在 `img_content` URL 中（作为 query 参数）
            return Ok((img_content, png_data));
        }

        // 否则：解码 base64 图片数据
        let mut img_b64 = img_content;
        if let Some(pos) = img_b64.find(',') {
            img_b64 = img_b64[pos + 1..].to_string();
        }
        let img_b64_cleaned: String = img_b64.chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '+' || *c == '/' || *c == '=')
            .collect();

        let img_data = BASE64
            .decode(img_b64_cleaned.as_bytes())
            .map_err(|e| format!("二维码图片 base64 解码失败: {e}"))?;

        Ok((qrcode, img_data))
    }

    /// 从二维码文本中提取 qrcode hash（用于登录状态查询）
    /// - 如果是 URL 格式，解析 `qrcode=` 参数
    /// - 如果是纯 hash，直接返回
    fn extract_qrcode_param(text: &str) -> String {
        if text.starts_with("http://") || text.starts_with("https://") {
            if let Some(pos) = text.find("qrcode=") {
                let val = &text[pos + 7..];
                // 取到 & 或结尾
                if let Some(amp) = val.find('&') {
                    return val[..amp].to_string();
                }
                return val.to_string();
            }
        }
        text.to_string()
    }

    /// 轮询扫码登录状态
    async fn poll_login_status(&self, qrcode: &str) -> Result<QrCodeStatusResponse, String> {
        let url = format!(
            "{}/ilink/bot/get_qrcode_status?qrcode={}",
            API_BASE, qrcode
        );

        let resp = self
            .client
            .get(&url)
            .header("X-WECHAT-UIN", &generate_uin())
            .header("iLink-App-ClientVersion", "1")
            .timeout(Duration::from_secs(35))
            .send()
            .await
            .map_err(|e| format!("查询登录状态失败: {e}"))?;

        let result: QrCodeStatusResponse = resp
            .json()
            .await
            .map_err(|e| format!("登录状态响应解析失败: {e}"))?;

        Ok(result)
    }

    /// 拉取消息
    async fn get_updates(&self, sync_buf: &str, bot_token: &str) -> Result<GetUpdatesResponse, String> {
        let url = format!("{}/ilink/bot/getupdates", API_BASE);

        let body = GetUpdatesRequest {
            get_updates_buf: sync_buf.to_string(),
        };

        let resp = self
            .client
            .post(&url)
            .headers(self.build_headers(bot_token))
            .json(&body)
            .timeout(Duration::from_secs(35))
            .send()
            .await
            .map_err(|e| format!("拉取消息失败: {e}"))?;

        let result: GetUpdatesResponse = resp
            .json()
            .await
            .map_err(|e| format!("消息响应解析失败: {e}"))?;

        // 检查错误码
        if let Some(errcode) = result.errcode {
            if errcode != 0 {
                // -1005 通常表示 token 过期
                return Err(format!("API 返回错误码: {}", errcode));
            }
        }

        Ok(result)
    }

    /// 发送文本消息
    async fn send_message_api(
        &self,
        chat_id: &str,
        text: &str,
        context_token: &str,
        bot_token: &str,
    ) -> Result<(), String> {
        let url = format!("{}/ilink/bot/sendmessage", API_BASE);
        let client_id = generate_client_id();

        let body = SendMessageRequest {
            msg: SendMessageBody {
                from_user_id: String::new(),
                to_user_id: chat_id.to_string(),
                client_id,
                message_type: 2,
                message_state: 2,
                item_list: vec![SendMessageItem {
                    item_type: 1,
                    text_item: TextItem { text: text.to_string() },
                }],
                context_token: context_token.to_string(),
            },
        };

        let resp = self
            .client
            .post(&url)
            .headers(self.build_headers(bot_token))
            .json(&body)
            .timeout(Duration::from_secs(15))
            .send()
            .await
            .map_err(|e| format!("发送消息失败: {e}"))?;

        let result: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("发送消息响应解析失败: {e}"))?;

        // 检查是否有错误
        if let Some(errcode) = result.get("errcode").and_then(|v| v.as_i64()) {
            if errcode != 0 {
                return Err(format!("发送消息错误码: {} ({:?})", errcode, result.get("errmsg")));
            }
        }

        tracing::debug!("WeChat: 已发送到 {} ({} 字符)", chat_id, text.len());
        Ok(())
    }

    /// 尝试从 token_path 文件读取 bot_token
    fn load_token(&self) -> Option<String> {
        let path = &self.config.channels.wechat.token_path;
        if path.is_empty() {
            return None;
        }
        match std::fs::read_to_string(path) {
            Ok(content) => {
                let token = content.trim().to_string();
                if token.is_empty() {
                    None
                } else {
                    Some(token)
                }
            }
            Err(_) => None,
        }
    }

    /// 保存 bot_token 到 token_path 文件
    fn save_token(&self, token: &str) {
        let path = &self.config.channels.wechat.token_path;
        if path.is_empty() {
            return;
        }
        if let Err(e) = std::fs::write(path, token) {
            tracing::warn!("WeChat: 保存 token 失败: {e}");
        }
    }

    /// 清除保存的 token 文件
    fn clear_token(&self) {
        let path = &self.config.channels.wechat.token_path;
        if path.is_empty() {
            return;
        }
        let _ = std::fs::remove_file(path);
    }

    /// 执行扫码登录流程
    async fn login_flow(&self) -> Result<String, String> {
        tracing::info!("WeChat: 正在获取登录二维码...");

        loop {
            // 1. 获取二维码
            let (qr_display, img_data) = self.fetch_qrcode().await?;
            let qrcode_hash = Self::extract_qrcode_param(&qr_display);

            // 2. 保存二维码图片
            let qr_path = "wechat_qrcode.png";
            let mut file = std::fs::File::create(qr_path)
                .map_err(|e| format!("创建二维码文件失败: {e}"))?;
            file.write_all(&img_data)
                .map_err(|e| format!("写入二维码文件失败: {e}"))?;
            drop(file);

            println!("\n========================================================");
            println!("  微信个号扫码登录");
            if qr_display.starts_with("http") {
                println!("  扫码链接: {}", qr_display);
            }
            println!("  二维码已保存至: {}", qr_path);
            println!("========================================================");

            // 在控制台显示 ASCII 二维码（二维码更新时同步刷新）
            let qr_lines = crate::tui::render_ascii_qr(&qr_display);
            for line in &qr_lines {
                let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                println!("{text}");
            }
            println!();

            // 3. 轮询等待扫码
            let poll_interval = Duration::from_secs(2);
            let mut poll_failures: u32 = 0;
            loop {
                tokio::time::sleep(poll_interval).await;

                // 连续失败 5 次后重新获取二维码
                if poll_failures >= 5 {
                    tracing::warn!("WeChat: 连续 {} 次查询失败，重新获取二维码...", poll_failures);
                    break;
                }

                match self.poll_login_status(&qrcode_hash).await {
                    Ok(status) => {
                        poll_failures = 0;
                        match status.status.as_deref() {
                            Some(s) if s.eq_ignore_ascii_case("Confirmed") => {
                                let token = status.bot_token.ok_or_else(|| {
                                    "扫码确认后未返回 bot_token".to_string()
                                })?;
                                tracing::info!("WeChat: 扫码登录成功");
                                return Ok(token);
                            }
                            Some(s) if s.eq_ignore_ascii_case("Expired") => {
                                tracing::warn!("WeChat: 二维码已过期，重新获取...");
                                break; // 跳出内层循环，重新获取二维码
                            }
                            Some(s) if s.eq_ignore_ascii_case("Waiting") || s.eq_ignore_ascii_case("Scanned") => {
                                tracing::info!("WeChat: 已扫码，请在手机上确认登录...");
                            }
                            Some(s) => {
                                tracing::debug!("WeChat: 登录状态: {}", s);
                            }
                            None => {
                                tracing::debug!("WeChat: 登录状态为空");
                            }
                        }
                    }
                    Err(e) => {
                        poll_failures += 1;
                        tracing::warn!("WeChat: 查询登录状态失败 (第{poll_failures}次): {e}");
                    }
                }
            }
        }
    }
}

#[async_trait]
impl Channel for WeChatChannel {
    /// 返回登录二维码（二维码文本, PNG 字节）
    async fn login_qrcode(&self) -> Option<(String, Vec<u8>)> {
        // 如果已有有效 token，跳过扫码
        if self.load_token().is_some() {
            tracing::info!("WeChat: 已有有效 token，跳过扫码");
            return None;
        }

        match self.fetch_qrcode().await {
            Ok((text, img_data)) => {
                tracing::info!("WeChat: 获取二维码成功");
                Some((text, img_data))
            }
            Err(e) => {
                tracing::warn!("WeChat: 获取二维码失败: {e}");
                None
            }
        }
    }

    fn start(
        self: Arc<Self>,
        inbound_tx: mpsc::UnboundedSender<InboundMessage>,
        mut shutdown_rx: broadcast::Receiver<()>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            tracing::info!("WeChatChannel: 已启动");

            // 1. 获取 bot_token：优先从文件加载，否则执行扫码登录
            let bot_token = match self.load_token() {
                Some(token) => {
                    tracing::info!("WeChat: 从文件加载 bot_token 成功");
                    token
                }
                None => {
                    tracing::info!("WeChat: 未找到已保存的 token，启动扫码登录流程");
                    match self.login_flow().await {
                        Ok(token) => {
                            self.save_token(&token);
                            token
                        }
                        Err(e) => {
                            tracing::error!("WeChat: 扫码登录失败: {e}");
                            return;
                        }
                    }
                }
            };

            // 2. 启动消息轮询
            let poll_interval = Duration::from_secs(self.config.channels.wechat.poll_interval_secs);
            let mut sync_buf = String::new();
            let mut current_token = bot_token;

            loop {
                tokio::select! {
                    _ = shutdown_rx.recv() => {
                        tracing::info!("WeChatChannel: 收到关闭信号");
                        break;
                    }
                    _ = tokio::time::sleep(poll_interval) => {
                        // 拉取消息
                        match self.get_updates(&sync_buf, &current_token).await {
                            Ok(resp) => {
                                // 更新 sync buffer
                                if let Some(buf) = resp.get_updates_buf {
                                    if !buf.is_empty() {
                                        sync_buf = buf;
                                    }
                                }

                                // 处理消息
                                if let Some(msgs) = resp.msgs {
                                    for msg in msgs {
                                        // 跳过非文本消息
                                        if msg.message_type != 1 {
                                            continue;
                                        }

                                        // 提取文本内容
                                        let text = msg.item_list
                                            .and_then(|items| {
                                                items.into_iter()
                                                    .find(|item| item.item_type == 1)
                                                    .and_then(|item| item.text_item.map(|t| t.text))
                                            })
                                            .unwrap_or_default();

                                        if text.is_empty() {
                                            continue;
                                        }

                                        // 构建入站消息
                                        let chat_id = msg.from_user_id.clone();
                                        let mut inbound = InboundMessage::new(
                                            "wechat",
                                            &chat_id,
                                            &text,
                                        );

                                        // 存储 context_token（用于后续回复关联）
                                        if let Some(ctx_token) = msg.context_token.clone() {
                                            inbound.metadata.insert("context_token".to_string(), ctx_token.clone());
                                            if let Ok(mut map) = self.context_tokens.lock() {
                                                map.insert(chat_id.clone(), ctx_token);
                                            }
                                        }

                                        if inbound_tx.send(inbound).is_err() {
                                            tracing::warn!("WeChat: inbound_tx 已关闭");
                                            break;
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::warn!("WeChat: 消息轮询失败: {e}");

                                // 如果 token 过期（返回 -1005 或其他错误），清除 token 重新扫码
                                if e.contains("错误码") || e.contains("token") || e.contains("401") {
                                    tracing::warn!("WeChat: token 可能已过期，清除并重新登录");
                                    self.clear_token();

                                    // 重新扫码
                                    match self.login_flow().await {
                                        Ok(new_token) => {
                                            self.save_token(&new_token);
                                            current_token = new_token;
                                            sync_buf.clear();
                                            tracing::info!("WeChat: 重新登录成功");
                                        }
                                        Err(login_err) => {
                                            tracing::error!("WeChat: 重新登录失败: {login_err}");
                                            // 等待一段时间再重试
                                            tokio::time::sleep(Duration::from_secs(30)).await;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            tracing::info!("WeChatChannel: 已停止");
        })
    }

    async fn send_message(&self, chat_id: &str, text: &str) -> Result<(), String> {
        let bot_token = self.load_token().ok_or_else(|| {
            "WeChat: 未登录，无法发送消息".to_string()
        })?;

        // 获取缓存的 context_token
        let ctx_token = self.context_tokens.lock()
            .ok()
            .and_then(|map| map.get(chat_id).cloned())
            .unwrap_or_default();

        tracing::info!("WeChat: 正在回复 {}: {:.100} (ctx_token={})", chat_id, text, ctx_token.len());

        // 将长消息分片发送
        for (i, chunk) in split_message(text, 4000).iter().enumerate() {
            self.send_message_api(chat_id, chunk, &ctx_token, &bot_token).await
                .map_err(|e| format!("WeChat 发送第{}片失败: {}", i + 1, e))?;
        }

        tracing::info!("WeChat: 回复成功 ({} 字符)", text.len());
        Ok(())
    }

    fn name(&self) -> &'static str {
        "wechat"
    }
}

/// 简单 URL 编码（只编码必要字符）
fn urlencoding(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(byte as char);
            }
            _ => {
                result.push_str(&format!("%{:02X}", byte));
            }
        }
    }
    result
}

/// 将 QR 码文本编码为 BMP 图片字节
///
/// BMP 格式简单、无需压缩、所有图片查看器都支持。
/// 每个 QR 模块缩放为 `scale` 像素，带白色边框。
fn generate_qr_png(text: &str) -> Result<Vec<u8>, String> {
    let qr = qrcodegen::QrCode::encode_text(text, qrcodegen::QrCodeEcc::Medium)
        .map_err(|e| format!("QR 码编码失败: {:?}", e))?;

    let module_size = qr.size() as u32;
    let scale = 6u32;         // 每个模块 6×6 像素
    let border = 6u32;        // 白色边框 6 像素
    let dim = module_size * scale + border * 2;

    // ---- BMP 编码 ----
    // 每行补零到 4 字节对齐（BMP 要求）
    let row_size = (dim * 3 + 3) / 4 * 4;  // 24-bit BGR, 每行字节数（4 对齐）
    let pixel_offset = 14 + 40; // file header + info header
    let file_size = pixel_offset + row_size * dim;

    let mut bmp: Vec<u8> = Vec::with_capacity(file_size as usize);

    // BITMAPFILEHEADER (14 bytes)
    bmp.extend(b"BM");
    bmp.extend(&(file_size as u32).to_le_bytes());  // file size
    bmp.extend(&[0u8; 4]);                           // reserved
    bmp.extend(&(pixel_offset as u32).to_le_bytes()); // pixel offset

    // BITMAPINFOHEADER (40 bytes)
    bmp.extend(&(40u32).to_le_bytes());              // header size
    bmp.extend(&dim.to_le_bytes());                  // width
    bmp.extend(&dim.to_le_bytes());                  // height (positive = bottom-up)
    bmp.extend(&(1u16).to_le_bytes());               // planes
    bmp.extend(&(24u16).to_le_bytes());              // bit count (24-bit BGR)
    bmp.extend(&[0u8; 24]);                          // compression=0, size, resolution, colors

    // 像素数据（BMP 是 bottom-up, 所以从最后一行开始）
    for y in (0..dim).rev() {
        for x in 0..dim {
            // 判断是否在边框内
            let in_border = x < border || x >= dim - border || y < border || y >= dim - border;
            // 判断模块
            let in_module = if in_border {
                false
            } else {
                let mx = (x - border) / scale;
                let my = (y - border) / scale;
                qr.get_module(mx as i32, my as i32)
            };

            if in_module {
                // 黑色 (BGR: 0, 0, 0)
                bmp.extend(&[0u8, 0, 0]);
            } else {
                // 白色 (BGR: 255, 255, 255)
                bmp.extend(&[255u8, 255, 255]);
            }
        }
        // 行对齐填充
        let padding = row_size - dim * 3;
        if padding > 0 {
            bmp.extend(vec![0u8; padding as usize]);
        }
    }

    Ok(bmp)
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_qr_png_returns_valid_bmp() {
        let bmp = generate_qr_png("https://example.com/login").unwrap();
        // BMP 文件头以 "BM" 开头
        assert_eq!(&bmp[..2], b"BM", "BMP 应以 BM 开头");
        // 文件大小
        let file_size = u32::from_le_bytes([bmp[2], bmp[3], bmp[4], bmp[5]]);
        assert_eq!(file_size as usize, bmp.len(), "BMP 文件大小应匹配");
        // 像素偏移
        let pixel_offset = u32::from_le_bytes([bmp[10], bmp[11], bmp[12], bmp[13]]);
        assert_eq!(pixel_offset, 54, "BMP 24-bit 无压缩像素偏移应为 54");
    }

    #[test]
    fn test_generate_qr_png_non_empty() {
        let bmp = generate_qr_png("test-qr-data").unwrap();
        assert!(bmp.len() > 100, "BMP 文件应大于 100 字节");
    }

    #[test]
    fn test_urlencoding() {
        let encoded = urlencoding("hello world");
        assert_eq!(encoded, "hello%20world");
    }

    #[test]
    fn test_generate_uin() {
        let uin = generate_uin();
        assert!(!uin.is_empty());
    }

    #[test]
    fn test_generate_client_id() {
        let id = generate_client_id();
        assert!(id.starts_with("wcc-"));
    }

    #[test]
    fn test_split_message() {
        let chunks = split_message("hello world", 4000);
        assert_eq!(chunks.len(), 1);
    }
}
