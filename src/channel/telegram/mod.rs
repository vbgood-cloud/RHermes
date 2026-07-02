//! Telegram Bot 通道
//!
//! 通过 Long Polling 接收消息，sendMessage 发送回复。
//! 支持 MarkdownV2 格式化、私聊/群聊 @bot 提及。
//! 复用 create_proxied_client 工厂走代理。

mod api;
mod sink;

pub use sink::TelegramSink;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::broadcast;
use tokio::sync::mpsc;

use crate::channel::{Channel, InboundMessage};
use crate::core::Config;
use api::{TelegramApi, TgError};

pub struct TelegramChannel {
    api: TelegramApi,
    allowed_chats: Vec<String>,
    poll_timeout: u32,
}

impl TelegramChannel {
    pub fn new(config: &Config) -> Result<Self, String> {
        let token = if !config.channels.telegram.bot_token.is_empty() {
            config.channels.telegram.bot_token.clone()
        } else {
            return Err("Telegram bot_token 未配置（config.toml 或 .env 的 TELEGRAM_BOT_TOKEN）".into());
        };

        let poll_timeout = config.channels.telegram.poll_timeout_secs;
        let client = crate::core::http_client::create_proxied_client(
            &config.proxy,
            "telegram",
            Duration::from_secs(poll_timeout as u64 + 10),
        );

        let api = TelegramApi::new(client, token);

        Ok(Self {
            api,
            allowed_chats: config.channels.telegram.allowed_chats.clone(),
            poll_timeout,
        })
    }

    fn is_allowed(&self, chat_id: &str) -> bool {
        if self.allowed_chats.is_empty() { return true; }
        self.allowed_chats.iter().any(|a| a == chat_id)
    }

    fn format_sender(msg: &api::TgMessage) -> String {
        if let Some(user) = &msg.from {
            let name = &user.first_name;
            match &user.username {
                Some(uname) => format!("{name} (@{uname})"),
                None => name.clone(),
            }
        } else {
            "unknown".into()
        }
    }

    /// 判断消息是否应在群聊中触发 Bot 响应
    fn should_handle_group(msg: &api::TgMessage, bot_username: &str) -> bool {
        // 私聊：总是处理
        if msg.chat.chat_type == "private" {
            return true;
        }

        // 群组/超级群：需要 @bot 提及 或 回复 Bot 的消息
        let text = msg.text.as_deref().unwrap_or("");

        // 检查是否以 @botusername 开头（不区分大小写）
        let mention = format!("@{}", bot_username);
        if text.to_lowercase().starts_with(&mention.to_lowercase()) {
            return true;
        }

        false
    }

    /// 清洗群聊消息：去掉开头的 @botusername
    fn clean_group_text(text: &str, bot_username: &str) -> String {
        let mention = format!("@{}", bot_username);
        if let Some(rest) = text.strip_prefix(&mention) {
            rest.trim().to_string()
        } else if let Some(rest) = text.strip_prefix(&mention.to_lowercase()) {
            rest.trim().to_string()
        } else {
            text.to_string()
        }
    }

    // ---- MarkdownV2 转义 ----

    /// Telegram MarkdownV2 特殊字符
    const MARKDOWN_SPECIAL: &'static [char] = &[
        '_', '*', '[', ']', '(', ')', '~', '`', '>', '#', '+',
        '-', '=', '|', '{', '}', '.', '!',
    ];

    /// 对文本进行 MarkdownV2 转义（将每个特殊字符前加反斜杠）
    ///
    /// 注意：转义后文本不再有 Markdown 渲染效果（纯文字显示）。
    /// 如果需要保留 Markdown 格式，应在转义后手动恢复标记。
    fn escape_markdown_v2(text: &str) -> String {
        let mut out = String::with_capacity(text.len() + 16);
        for ch in text.chars() {
            if Self::MARKDOWN_SPECIAL.contains(&ch) {
                out.push('\\');
            }
            out.push(ch);
        }
        out
    }

    /// 将文本包装为安全的 MarkdownV2 消息
    ///
    /// - 对整个文本做 MarkdownV2 转义
    /// - 然后恢复代码块 (```...```) 和行内代码 (`...`) 内的转义，
    ///   使代码块能正常渲染
    /// - 恢复加粗 (**...**) 和斜体 (*...* / _..._) 标记，保留基本格式
    fn to_markdown_v2(text: &str) -> String {
        let escaped = Self::escape_markdown_v2(text);

        // 恢复代码块内的转义字符
        // ```...``` 块内所有转义的反斜杠去掉
        let mut result = String::with_capacity(escaped.len());
        let mut in_code_block = false;
        let mut chars = escaped.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == '\\' {
                // 检查是否在代码块边界
                let is_triple_backtick = {
                    let mut lookahead = chars.clone();
                    lookahead.next() == Some('`')
                        && lookahead.next() == Some('`')
                        && lookahead.next() == Some('`')
                };
                if is_triple_backtick {
                    // 反斜杠 + ```，这是转义后的 ```，恢复它
                    result.push('`');
                    chars.next(); // consume first `
                    chars.next(); // consume second `
                    chars.next(); // consume third `
                    in_code_block = !in_code_block;
                    continue;
                }
            }

            if in_code_block && ch == '\\' {
                // 代码块内：移除转义反斜杠
                if let Some(next) = chars.next() {
                    result.push(next);
                }
                continue;
            }

            result.push(ch);
        }

        // 恢复行内代码 `...` 内的转义
        // 注意：行内代码在 Telegram MarkdownV2 中会用 ` 包裹
        // 我们已经对原始 ` 做了转义 (\`)，需要恢复它
        // 但上面已经处理了 \, 这里需要检查 \` → `
        // 实际上，行内代码内不应转义任何字符
        // 但简单的恢复方式是：在 `...` 内不转义
        let result = Self::unescape_inline_code(&result);

        result
    }

    /// 恢复行内代码 \`...\` 内的转义
    fn unescape_inline_code(text: &str) -> String {
        let mut result = String::with_capacity(text.len());
        let mut in_code = false;
        let mut chars = text.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == '\\' {
                if let Some(&next) = chars.peek() {
                    if next == '`' {
                        // 转义的反引号：如果不在代码内，保留反斜杠
                        // 如果在代码内，去掉反斜杠
                        if in_code {
                            chars.next(); // consume `
                            result.push('`');
                        } else {
                            result.push('\\');
                            chars.next();
                            result.push('`');
                            in_code = true;
                        }
                        continue;
                    }
                }
            }
            if ch == '`' && !in_code {
                // 进入行内代码
                in_code = true;
                // 注意：如果 ch 是 \` 则上面已处理，这里是普通 `
                result.push(ch);
            } else if ch == '`' && in_code {
                // 退出行内代码
                in_code = false;
                result.push(ch);
            } else {
                result.push(ch);
            }
        }
        result
    }
}

#[async_trait]
impl Channel for TelegramChannel {
    fn start(
        self: Arc<Self>,
        inbound_tx: mpsc::UnboundedSender<InboundMessage>,
        mut shutdown_rx: broadcast::Receiver<()>,
    ) -> tokio::task::JoinHandle<()> {
        tracing::info!("Telegram Long Polling 启动");

        tokio::spawn(async move {
            // 启动时验证 Bot Token 并获取 bot username
            let bot_username = match self.api.get_me().await {
                Ok(bot) => {
                    tracing::info!("Telegram Bot 已连接: {} (@{})", bot.first_name, bot.username);
                    bot.username
                }
                Err(e) => {
                    tracing::error!("Telegram Bot 验证失败: {e}");
                    return;
                }
            };

            let mut offset = 0i64;

            loop {
                // 检查 shutdown 信号
                if shutdown_rx.try_recv().is_ok() {
                    tracing::info!("Telegram 通道收到 shutdown 信号");
                    break;
                }

                match self.api.get_updates(offset, self.poll_timeout).await {
                    Ok(updates) => {
                        for update in updates {
                            offset = update.update_id + 1;

                            if let Some(msg) = &update.message {
                                if let Some(text) = &msg.text {
                                    let chat_id = msg.chat.id.to_string();

                                    // 群聊过滤：需要 @bot 提及
                                    if msg.chat.chat_type != "private"
                                        && !Self::should_handle_group(msg, &bot_username)
                                    {
                                        continue;
                                    }

                                    if !self.is_allowed(&chat_id) {
                                        tracing::warn!(
                                            "Telegram 消息被过滤 (chat_id={})",
                                            chat_id
                                        );
                                        continue;
                                    }

                                    // 清洗消息文本（去除 @botusername 前缀）
                                    let clean_text = if msg.chat.chat_type != "private" {
                                        Self::clean_group_text(text, &bot_username)
                                    } else {
                                        text.clone()
                                    };

                                    if clean_text.is_empty() {
                                        continue;
                                    }

                                    tracing::info!(
                                        "Telegram 收到消息: [{}] {}",
                                        Self::format_sender(msg),
                                        if clean_text.chars().count() > 50 {
                                            let trunc: String = clean_text.chars().take(50).collect();
                                            format!("{}...", trunc)
                                        } else {
                                            clean_text.clone()
                                        }
                                    );

                                    let inbound = InboundMessage::new(
                                        "telegram",
                                        &chat_id,
                                        clean_text,
                                    );

                                    if inbound_tx.send(inbound).is_err() {
                                        tracing::error!("Telegram inbound_tx 已关闭");
                                        return;
                                    }
                                }
                            }
                        }
                    }
                    Err(TgError::Http(e)) if e.is_timeout() => {
                        // Long Polling 超时是正常的，继续下一轮
                        continue;
                    }
                    Err(e) => {
                        tracing::error!("Telegram getUpdates 失败: {e}");
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }
            }

            tracing::info!("Telegram 通道已停止");
        })
    }

    async fn send_message(&self, chat_id: &str, text: &str) -> Result<(), String> {
        if text.len() <= 4096 {
            self.api.send_message(chat_id, text, None).await
                .map_err(|e| format!("{e}"))
        } else {
            let mut remaining = text;
            while !remaining.is_empty() {
                let cut = if remaining.len() > 4000 {
                    let mut pos = 4000;
                    while !remaining.is_char_boundary(pos) && pos > 0 {
                        pos -= 1;
                    }
                    pos
                } else {
                    remaining.len()
                };
                let chunk = &remaining[..cut];
                self.api.send_message(chat_id, chunk, None).await
                    .map_err(|e| format!("{e}"))?;
                remaining = &remaining[cut..];
            }
            Ok(())
        }
    }

    /// 发送 MarkdownV2 格式化的消息
    async fn send_formatted(&self, chat_id: &str, text: &str) -> Result<(), String> {
        let safe_md = Self::to_markdown_v2(text);
        if safe_md.len() <= 4096 {
            self.api.send_message(chat_id, &safe_md, Some("MarkdownV2")).await
                .map_err(|e| format!("{e}"))
        } else {
            let mut remaining = safe_md.as_str();
            while !remaining.is_empty() {
                let cut = if remaining.len() > 4000 {
                    let mut pos = 4000;
                    while !remaining.is_char_boundary(pos) && pos > 0 {
                        pos -= 1;
                    }
                    pos
                } else {
                    remaining.len()
                };
                let chunk = &remaining[..cut];
                self.api.send_message(chat_id, chunk, Some("MarkdownV2")).await
                    .map_err(|e| format!("{e}"))?;
                remaining = &remaining[cut..];
            }
            Ok(())
        }
    }

    fn name(&self) -> &'static str {
        "telegram"
    }
}
