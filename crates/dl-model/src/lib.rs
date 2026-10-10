//! 任务领域模型：Task/状态/类型、状态机迁移表。纯数据与规则，无 IO。

pub mod task;
pub mod types;

pub use task::can_transition;
pub use types::{new_task_id, now_ms, CreateContentTask, CreateTask, PostProcess, PostProcessAlgo, Task, TaskKind, TaskStatus, HEADER_ALLOWLIST};
