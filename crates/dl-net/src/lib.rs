//! HTTP 能力域：客户端构建（identity 编码 + 代理）与 URL 探测决策表。

pub mod client;
pub mod probe;

pub use client::{
    build_client, classify_status, identity_headers, map_reqwest_err, retry_after_secs, USER_AGENT,
};
pub use probe::{probe as probe_url, ProbeResult};
