//! 层级令牌桶（DESIGN §5.2）：全局桶 → 任务子桶，两级都拿到额度才放行；
//! 按实际流逝时间补充令牌（不依赖固定 tick）；限速时睡眠等待，不忙等。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const GLOBAL_BURST: u64 = 1024 * 1024;
const TASK_BURST: u64 = 256 * 1024;
const MAX_SLEEP: Duration = Duration::from_millis(200);

#[derive(Debug)]
struct Bucket {
    rate_bps: u64,
    burst: u64,
    tokens: f64,
    last: Instant,
}

impl Bucket {
    fn new(rate_bps: u64, burst: u64) -> Self {
        Bucket {
            rate_bps,
            burst: burst.max(1),
            tokens: burst as f64,
            last: Instant::now(),
        }
    }

    fn take(&mut self, want: u64) -> u64 {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + elapsed * self.rate_bps as f64).min(self.burst as f64);
        let granted = (self.tokens as u64).min(want);
        self.tokens -= granted as f64;
        granted
    }

    fn give_back(&mut self, n: u64) {
        self.tokens = (self.tokens + n as f64).min(self.burst as f64);
    }

    fn deficit_secs(&self, want: u64) -> f64 {
        want as f64 / self.rate_bps as f64
    }
}

/// 两级限速器。None = 不限速。
#[derive(Clone, Debug, Default)]
pub struct Limiter {
    global: Arc<Mutex<Option<Bucket>>>,
    task: Option<Arc<Mutex<Bucket>>>,
}

impl Limiter {
    pub fn new(global_bps: Option<u64>, task_bps: Option<u64>) -> Self {
        let limiter = Self::default();
        limiter.set_global_rate(global_bps);
        limiter.with_task(task_bps)
    }

    pub fn set_global_rate(&self, rate: Option<u64>) {
        let rate = rate.filter(|r| *r > 0);
        let mut bucket = self.global.lock().unwrap();
        if bucket.as_ref().map(|b| b.rate_bps) != rate {
            *bucket = rate.map(|r| Bucket::new(r, GLOBAL_BURST.max(r / 2)));
        }
    }

    pub fn with_task(&self, rate: Option<u64>) -> Self {
        Self {
            global: self.global.clone(),
            task: rate
                .filter(|r| *r > 0)
                .map(|r| Arc::new(Mutex::new(Bucket::new(r, TASK_BURST.max(r / 2))))),
        }
    }

    pub fn unlimited() -> Self {
        Limiter::default()
    }

    /// 单次获取额度的推荐上限：桶 burst 恒 ≥ 此值，保证全额可满足。
    pub const ACQUIRE_CAP: u64 = 256 * 1024;

    /// 等待直到拿到 n 字节额度。部分额度会退回并按缺口睡眠。
    /// want 会被截断到 ACQUIRE_CAP——burst 之下的额度才可能被全额满足，
    /// 否则 `want > burst` 时永远凑不满（死等）。
    pub async fn acquire(&self, want: u64) {
        let mut remaining = want;
        while remaining > 0 {
            let amount = remaining.min(Self::ACQUIRE_CAP);
            self.acquire_chunk(amount).await;
            remaining -= amount;
        }
    }

    async fn acquire_chunk(&self, want: u64) {
        while let Some(delay) = self.try_acquire(want) {
            tokio::time::sleep(delay).await;
        }
    }

    fn try_acquire(&self, want: u64) -> Option<Duration> {
        // Take both budgets under their locks; refund either successful take
        // when the other budget cannot yet satisfy the request.
        let mut global = self.global.lock().unwrap();
        let mut task = self.task.as_ref().map(|b| b.lock().unwrap());
        let g_short = match global.as_mut() {
            Some(b) => {
                let got = b.take(want);
                if got == want {
                    None
                } else {
                    b.give_back(got);
                    Some(b.deficit_secs(want))
                }
            }
            None => None,
        };
        let t_short = match task.as_mut() {
            Some(b) => {
                let got = b.take(want);
                if got == want {
                    None
                } else {
                    b.give_back(got);
                    Some(b.deficit_secs(want))
                }
            }
            None => None,
        };
        if g_short.is_some() && t_short.is_none() {
            if let Some(b) = task.as_mut() {
                b.give_back(want);
            }
        }
        if t_short.is_some() && g_short.is_none() {
            if let Some(b) = global.as_mut() {
                b.give_back(want);
            }
        }
        let delay = match (g_short, t_short) {
            (None, None) => return None,
            (g, t) => {
                let secs = [g, t].into_iter().flatten().fold(0.0f64, f64::max);
                let secs = if secs.is_finite() && secs > 0.0 {
                    secs
                } else {
                    0.05
                };
                MAX_SLEEP.min(Duration::from_secs_f64(secs))
            }
        };
        Some(delay)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn limits_to_configured_rate() {
        let lim = Limiter::new(None, Some(512 * 1024)); // 512KB/s
        let total = 2 * 1024 * 1024u64;
        let start = Instant::now();
        // 16KB 块
        let mut sent = 0u64;
        while sent < total {
            lim.acquire(16 * 1024).await;
            sent += 16 * 1024;
        }
        let elapsed = start.elapsed();
        // burst 256KB 立即放行，剩余 1.75MB @512KB/s ≈ 3.4s
        assert!(elapsed >= Duration::from_secs(3), "elapsed {elapsed:?}");
        assert!(elapsed <= Duration::from_secs(8), "elapsed {elapsed:?}");
    }
}
