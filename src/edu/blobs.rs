//! 作业文件分发：iroh-blobs（BLAKE3 内容寻址）
//!
//! 对应 `docs/edu-p2p-design.md` §6。
//!
//! 核心策略：**哈希走 gossip，文件走 blobs**
//! - 老师 `add_slice` 得 BLAKE3 哈希 → 生成 `BlobTicket`（含 provider 地址）→ 经该班 Topic 广播通知；
//! - 学生收到通知后凭票据拉取，**下载过程本身即 BLAKE3 流式校验**；
//! - 任一持有者（老师或已下载的同学）都可充当 provider，实现多源下载。
//!
//! ## API 核验（已对 iroh-blobs 0.103.0 源码逐条核对）
//!
//! | 用途 | 真实签名 |
//! |------|----------|
//! | 加入内容 | `Blobs::add_slice(&self, impl AsRef<[u8]>) -> AddProgress<'_>`（`await` 得 `Tag{hash, format}`）|
//! | 读回内容 | `Blobs::get_bytes(&self, impl Into<Hash>) -> ExportBaoResult<Bytes>` |
//! | 下载 | `Downloader::new(&Store, &Endpoint)` + `.download(Hash, impl ContentDiscovery)` |
//! | 票据 | `BlobTicket::new(EndpointAddr, Hash, BlobFormat)`；字段**私有**，取值用 `hash()` / `addr()` / `format()` |
//! | 取 store 视图 | `MemStore: Deref<Target = api::Store>`，`api::Store: Deref<Target = api::blobs::Blobs>` |
//!
//! `Vec<EndpointId>` 自动满足 `ContentDiscovery`（`downloader.rs` 里有 blanket impl）。

use iroh::{Endpoint, EndpointId};
use iroh_blobs::api::downloader::Downloader;
use iroh_blobs::store::mem::MemStore;
use iroh_blobs::ticket::BlobTicket;
use iroh_blobs::Hash;

use super::gossip::SectionSession;

/// 老师发布作业文件，返回内容 BLAKE3 哈希。
pub async fn publish_assignment_file(
    store: &MemStore,
    endpoint: &Endpoint,
    session: &SectionSession,
    assignment_id: i64,
    title: &str,
    file_bytes: &[u8],
    due_date: &str,
    posted_by: &str,
) -> anyhow::Result<Hash> {
    // 1) 内容寻址：加入本地 blob 存储，得到哈希 + 格式
    let tag = store.add_slice(file_bytes).await?;

    // 2) 生成票据 = provider 地址（本机 EndpointAddr）+ 哈希 + 格式
    let ticket = BlobTicket::new(endpoint.addr(), tag.hash, tag.format);

    // 3) 只广播「通知 + 哈希 + 票据」，文件本体不进 gossip
    session
        .broadcast_assignment(
            assignment_id,
            title,
            &tag.hash.to_string(),
            &ticket.to_string(),
            due_date,
            posted_by,
        )
        .await?;

    tracing::info!(
        "[section {}] 作业 {assignment_id} 已发布: {} ({} bytes, blake3={})",
        session.section_id,
        title,
        file_bytes.len(),
        tag.hash
    );

    Ok(tag.hash)
}

/// 学生下载作业文件。
///
/// `expected_hash_hex` 来自 gossip 通知；与票据内哈希做一致性预检，防篡改。
pub async fn fetch_assignment_file(
    store: &MemStore,
    endpoint: &Endpoint,
    ticket_str: &str,
    expected_hash_hex: &str,
) -> anyhow::Result<Vec<u8>> {
    let ticket: BlobTicket = ticket_str
        .parse()
        .map_err(|e| anyhow::anyhow!("BlobTicket 解析失败: {e}"))?;

    // 完整性预检：票据哈希必须与通知里的哈希一致（防「票据被换成别的文件」）
    let want: Hash = expected_hash_hex
        .parse()
        .map_err(|e| anyhow::anyhow!("无效的哈希字符串 {expected_hash_hex}: {e}"))?;
    anyhow::ensure!(
        ticket.hash() == want,
        "票据哈希 {} 与通知哈希 {} 不一致，可能被篡改",
        ticket.hash(),
        want
    );

    // 触发下载（BLAKE3 流式校验），provider 取自票据
    download_blob(store, endpoint, ticket.hash(), ticket.addr().id).await?;
    read_blob(store, ticket.hash()).await
}

/// 仅按哈希 + provider 拉取（票据不可用时的兜底路径 / 多源下载）。
pub async fn fetch_by_hash(
    store: &MemStore,
    endpoint: &Endpoint,
    hash_hex: &str,
    provider: EndpointId,
) -> anyhow::Result<Vec<u8>> {
    let hash: Hash = hash_hex
        .parse()
        .map_err(|e| anyhow::anyhow!("无效的哈希字符串: {e}"))?;
    download_blob(store, endpoint, hash, provider).await?;
    read_blob(store, hash).await
}

// ---------------------------------------------------------------------------
// 下载 / 读回
// ---------------------------------------------------------------------------

/// 从指定 provider 下载一个 blob 到本地 store。
///
/// `Downloader::new` 会起一个下载 actor；本函数结束即 drop → actor 随之退出。
/// （后续批次可把 `Downloader` 提升为 `P2pNode` 的长生命周期成员以复用连接池。）
async fn download_blob(
    store: &MemStore,
    endpoint: &Endpoint,
    hash: Hash,
    provider: EndpointId,
) -> anyhow::Result<()> {
    let downloader = Downloader::new(store, endpoint);
    // `DownloadProgress` 实现了 `IntoFuture`（`complete()` 是 pub(crate)，外部不可调用），
    // 因此直接 `.await` 即可拿到 `Result<()>`。
    downloader
        .download(hash, vec![provider])
        .await
        .map_err(|e| anyhow::anyhow!("blob 下载失败 ({hash}): {e}"))?;
    Ok(())
}

/// 从本地 store 读回 blob 字节。
async fn read_blob(store: &MemStore, hash: Hash) -> anyhow::Result<Vec<u8>> {
    let bytes = store
        .get_bytes(hash)
        .await
        .map_err(|e| anyhow::anyhow!("blob 读回失败 ({hash}): {e}"))?;
    Ok(bytes.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_add_slice_gives_stable_hash() {
        // 纯本地：不涉及网络
        let store = MemStore::new();
        let h1 = store.add_slice(b"hello rhermes").await.unwrap().hash;
        let h2 = store.add_slice(b"hello rhermes").await.unwrap().hash;
        assert_eq!(h1, h2, "内容寻址：相同内容必须得到相同哈希");

        let h3 = store.add_slice(b"hello rhermes!").await.unwrap().hash;
        assert_ne!(h1, h3);
    }

    #[tokio::test]
    async fn test_read_back_local_blob() {
        let store = MemStore::new();
        let hash = store.add_slice(b"payload-123").await.unwrap().hash;
        let back = store.get_bytes(hash).await.unwrap();
        assert_eq!(&back[..], b"payload-123");
    }

    #[tokio::test]
    async fn test_hash_str_roundtrip() {
        // gossip 通知里传的是 hash 的字符串形式，必须能原样解析回来
        let store = MemStore::new();
        let h = store.add_slice(b"assignment pdf bytes").await.unwrap().hash;
        let s = h.to_string();
        let back: Hash = s.parse().expect("Hash 字符串必须可解析");
        assert_eq!(h, back);
    }
}
