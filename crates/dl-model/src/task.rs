use crate::types::TaskStatus;

/// 状态机迁移表（DESIGN §5.1）。
/// 不变量：Done 是唯一终点且不可迁出；Verifying → Done 是唯一完成路径（Verifying 为强制阶段）。
/// Canceled 可从除 Done/Canceled 外的任意状态进入；Failed/Canceled 均可重新入队。
/// Paused 可失败/可重新入队：续传恢复先重新探测，签名 URL 过期等探测失败
/// 必须能落 Failed（否则任务永远卡在暂停、错误被吞），瞬态网络错误则经
/// Paused → Queued 走自动重试。
pub fn can_transition(from: TaskStatus, to: TaskStatus) -> bool {
    use TaskStatus::*;

    if to == Canceled {
        return !matches!(from, Done | Canceled);
    }

    matches!(
        (from, to),
        (Queued, Probing)
            | (Probing, Downloading)
            | (Probing, WaitingRetry)
            | (Probing, Failed)
            | (Probing, Paused)
            | (Downloading, Paused)
            | (Downloading, WaitingRetry)
            | (Downloading, Merging)
            | (Downloading, Verifying)
            | (Downloading, Failed)
            | (Paused, Downloading)
            | (Paused, Probing)
            | (Paused, Queued)
            | (Paused, Failed)
            | (WaitingRetry, Downloading)
            | (WaitingRetry, Probing)
            | (WaitingRetry, Paused)
            | (WaitingRetry, Failed)
            | (Merging, Verifying)
            | (Merging, Failed)
            | (Verifying, Decrypting)
            | (Verifying, Done)
            | (Verifying, WaitingRetry)
            | (Verifying, Failed)
            | (Decrypting, Verifying)
            | (Decrypting, Failed)
            | (Failed, Queued)
            | (Canceled, Queued)
    )
}

#[cfg(test)]
mod tests {
    use super::can_transition;
    use crate::types::TaskStatus::*;

    #[test]
    fn happy_path_http() {
        assert!(can_transition(Queued, Probing));
        assert!(can_transition(Probing, Downloading));
        assert!(can_transition(Downloading, Verifying));
        assert!(can_transition(Verifying, Done));
    }

    #[test]
    fn happy_path_hls() {
        assert!(can_transition(Downloading, Merging));
        assert!(can_transition(Merging, Verifying));
        assert!(can_transition(Verifying, Done));
    }

    #[test]
    fn verifying_is_mandatory() {
        assert!(!can_transition(Downloading, Done));
        assert!(!can_transition(Merging, Done));
    }

    #[test]
    fn done_is_frozen() {
        for to in [
            Queued,
            Probing,
            Downloading,
            Paused,
            WaitingRetry,
            Merging,
            Verifying,
            Failed,
            Canceled,
        ] {
            assert!(!can_transition(Done, to), "Done -> {to:?} must be denied");
        }
    }

    #[test]
    fn cancel_from_everywhere_except_done() {
        for from in [
            Queued,
            Probing,
            Downloading,
            Paused,
            WaitingRetry,
            Merging,
            Verifying,
            Failed,
        ] {
            assert!(
                can_transition(from, Canceled),
                "{from:?} -> Canceled must be allowed"
            );
        }
        assert!(!can_transition(Done, Canceled));
        assert!(!can_transition(Canceled, Canceled));
    }

    #[test]
    fn pause_resume_and_retry_paths() {
        assert!(can_transition(Downloading, Paused));
        assert!(can_transition(Paused, Downloading));
        assert!(can_transition(Downloading, WaitingRetry));
        assert!(can_transition(WaitingRetry, Downloading));
        assert!(can_transition(WaitingRetry, Paused));
        assert!(can_transition(WaitingRetry, Probing));
        assert!(can_transition(Probing, Failed));
        assert!(can_transition(Verifying, Failed));
        assert!(can_transition(Failed, Queued));
        assert!(can_transition(Canceled, Queued));
        // 暂停恢复路径：恢复探测可见（Paused→Probing）、探测失败可落 Failed、
        // 瞬态错误可重新入队走自动重试——缺了这三条，续传失败的任务会
        // 永远卡在「已暂停」且错误被吞（真实事故：paused -> failed 被拒）
        assert!(can_transition(Paused, Probing));
        assert!(can_transition(Paused, Failed));
        assert!(can_transition(Paused, Queued));
        // 非法路径抽查
        assert!(!can_transition(Queued, Paused));
        assert!(!can_transition(Paused, Done));
        assert!(!can_transition(Queued, Downloading));
    }
}
