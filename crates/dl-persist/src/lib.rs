//! SQLite 持久化（WAL 模式）。
//! rusqlite 的 Connection 是 !Sync，按 DESIGN §3 的运行时纪律走专属写线程：
//! 异步侧只投递操作（std mpsc），写线程串行执行，经 tokio oneshot 回复，
//! 避免在 async 上下文里做任何阻塞调用。

use std::path::Path;
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

use rusqlite::Connection;
use tokio::sync::oneshot;

use dl_error::{DownloadError, ErrorKind, Res};
use dl_model::{now_ms, Task, TaskKind, TaskStatus};

/// rusqlite 错误 → 错误分类（孤儿规则：From impl 只能在 dl-error 内实现，这里用辅助函数）。
fn sql_err(e: rusqlite::Error) -> DownloadError {
    let kind = if matches!(e, rusqlite::Error::QueryReturnedNoRows) {
        ErrorKind::NotFound
    } else {
        ErrorKind::Internal
    };
    DownloadError::new(kind, e.to_string())
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS tasks (
  id TEXT PRIMARY KEY,
  url TEXT NOT NULL,
  final_url TEXT,
  filename TEXT,
  dir TEXT,
  total_bytes INTEGER,
  etag TEXT,
  last_modified TEXT,
  mime TEXT,
  kind TEXT NOT NULL,
  status TEXT NOT NULL,
  priority INTEGER NOT NULL DEFAULT 0,
  headers_ref TEXT,
  headers_json TEXT,
  downloaded_bytes INTEGER NOT NULL DEFAULT 0,
  variants_json TEXT,
  checksum TEXT,
  error TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(status);
CREATE INDEX IF NOT EXISTS idx_tasks_created ON tasks(created_at);
CREATE INDEX IF NOT EXISTS idx_tasks_filename ON tasks(filename);
CREATE TABLE IF NOT EXISTS task_changes (seq INTEGER PRIMARY KEY AUTOINCREMENT, task_id TEXT NOT NULL, deleted INTEGER NOT NULL);
CREATE TRIGGER IF NOT EXISTS tasks_insert AFTER INSERT ON tasks BEGIN INSERT INTO task_changes(task_id,deleted) VALUES(NEW.id,0); END;
CREATE TRIGGER IF NOT EXISTS tasks_update AFTER UPDATE ON tasks BEGIN INSERT INTO task_changes(task_id,deleted) VALUES(NEW.id,0); END;
CREATE TRIGGER IF NOT EXISTS tasks_delete AFTER DELETE ON tasks BEGIN INSERT INTO task_changes(task_id,deleted) VALUES(OLD.id,1); END;
CREATE TABLE IF NOT EXISTS settings (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
"#;

/// 旧库缺列时补列（M1.5：headers_json 透传头，keychain 迁移见 DESIGN §9）。
fn migrate(conn: &Connection) -> Result<(), DownloadError> {
    let has = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('tasks') WHERE name = 'headers_json'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map_err(sql_err)?;
    if has == 0 {
        conn.execute("ALTER TABLE tasks ADD COLUMN headers_json TEXT", [])
            .map_err(sql_err)?;
    }
    let has2 = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('tasks') WHERE name = 'downloaded_bytes'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map_err(sql_err)?;
    if has2 == 0 {
        conn.execute(
            "ALTER TABLE tasks ADD COLUMN downloaded_bytes INTEGER NOT NULL DEFAULT 0",
            [],
        )
        .map_err(sql_err)?;
    }
    let has3 = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('tasks') WHERE name = 'variants_json'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map_err(sql_err)?;
    if has3 == 0 {
        conn.execute("ALTER TABLE tasks ADD COLUMN variants_json TEXT", [])
            .map_err(sql_err)?;
    }
    let has4 = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('tasks') WHERE name = 'checksum'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map_err(sql_err)?;
    if has4 == 0 {
        conn.execute("ALTER TABLE tasks ADD COLUMN checksum TEXT", [])
            .map_err(sql_err)?;
    }
    let has5 = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('tasks') WHERE name = 'mirrors_json'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map_err(sql_err)?;
    if has5 == 0 {
        conn.execute("ALTER TABLE tasks ADD COLUMN mirrors_json TEXT", [])
            .map_err(sql_err)?;
    }
    let has6 = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('tasks') WHERE name = 'postprocess_json'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map_err(sql_err)?;
    if has6 == 0 {
        conn.execute("ALTER TABLE tasks ADD COLUMN postprocess_json TEXT", [])
            .map_err(sql_err)?;
    }
    if conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('tasks') WHERE name='file_size'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map_err(sql_err)?
        == 0
    {
        conn.execute("ALTER TABLE tasks ADD COLUMN file_size INTEGER", [])
            .map_err(sql_err)?;
    }
    Ok(())
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct TaskSnapshot {
    pub cursor: u64,
    pub full: bool,
    pub tasks: Vec<Task>,
    pub deleted: Vec<String>,
}

const COLS: &str = "id,url,final_url,filename,dir,total_bytes,etag,last_modified,mime,kind,status,priority,headers_ref,headers_json,downloaded_bytes,variants_json,checksum,mirrors_json,postprocess_json,error,created_at,updated_at,file_size";

enum Op {
    Snapshot {
        cursor: Option<u64>,
        tx: oneshot::Sender<Res<TaskSnapshot>>,
    },
    Output {
        id: String,
        dir: String,
        filename: String,
        file_size: Option<u64>,
        tx: oneshot::Sender<Res<()>>,
    },
    SetSetting {
        key: String,
        value: String,
        tx: oneshot::Sender<Res<()>>,
    },
    GetSetting {
        key: String,
        tx: oneshot::Sender<Res<Option<String>>>,
    },
    Rename {
        id: String,
        filename: String,
        tx: oneshot::Sender<Res<()>>,
    },
    UpdateDownloaded {
        id: String,
        bytes: u64,
        tx: oneshot::Sender<Res<()>>,
    },
    UpdateVariants {
        id: String,
        variants_json: String,
        tx: oneshot::Sender<Res<()>>,
    },
    FindActiveByUrl {
        url: String,
        tx: oneshot::Sender<Res<Option<Task>>>,
    },
    SetPriority {
        id: String,
        priority: i32,
        tx: oneshot::Sender<Res<()>>,
    },
    Insert(Box<Task>, oneshot::Sender<Res<()>>),
    UpdateStatus {
        id: String,
        status: TaskStatus,
        error: Option<ErrorKind>,
        tx: oneshot::Sender<Res<()>>,
    },
    UpdateProbe {
        id: String,
        total_bytes: Option<u64>,
        etag: Option<String>,
        last_modified: Option<String>,
        mime: Option<String>,
        final_url: Option<String>,
        tx: oneshot::Sender<Res<()>>,
    },
    Get {
        id: String,
        tx: oneshot::Sender<Res<Option<Task>>>,
    },
    List {
        status: Option<TaskStatus>,
        tx: oneshot::Sender<Res<Vec<Task>>>,
    },
    ListUpdatedSince {
        since_ms: i64,
        tx: oneshot::Sender<Res<Vec<Task>>>,
    },
    Delete {
        id: String,
        tx: oneshot::Sender<Res<bool>>,
    },
}

/// 跨线程克隆句柄；写线程由 open() 持有，drop 时随通道关闭退出。
#[derive(Clone)]
pub struct Db {
    tx: Sender<Op>,
}

impl Db {
    pub async fn snapshot(&self, cursor: Option<u64>) -> Res<TaskSnapshot> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Op::Snapshot { cursor, tx })
            .map_err(|_| writer_gone())?;
        rx.await.map_err(|_| writer_gone())?
    }
    pub async fn set_output(
        &self,
        id: &str,
        dir: &str,
        filename: &str,
        file_size: Option<u64>,
    ) -> Res<()> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Op::Output {
                id: id.into(),
                dir: dir.into(),
                filename: filename.into(),
                file_size,
                tx,
            })
            .map_err(|_| writer_gone())?;
        rx.await.map_err(|_| writer_gone())?
    }

    pub fn open(path: &Path) -> Res<Db> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                DownloadError::new(ErrorKind::FileAccessDenied, format!("create data dir: {e}"))
            })?;
        }
        let conn = Connection::open(path).map_err(sql_err)?;
        conn.busy_timeout(Duration::from_secs(5)).map_err(sql_err)?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(sql_err)?;
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(sql_err)?;
        conn.execute_batch(SCHEMA).map_err(sql_err)?;
        migrate(&conn)?;

        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("db-writer".into())
            .spawn(move || writer_loop(conn, rx))
            .map_err(|e| {
                DownloadError::new(ErrorKind::Internal, format!("spawn db writer: {e}"))
            })?;
        Ok(Db { tx })
    }

    pub async fn set_setting(&self, key: &str, value: &str) -> Res<()> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Op::SetSetting {
                key: key.to_string(),
                value: value.to_string(),
                tx,
            })
            .map_err(|_| writer_gone())?;
        match rx.await {
            Ok(res) => res,
            Err(_) => Err(writer_gone()),
        }
    }

    pub async fn get_setting(&self, key: &str) -> Res<Option<String>> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Op::GetSetting {
                key: key.to_string(),
                tx,
            })
            .map_err(|_| writer_gone())?;
        match rx.await {
            Ok(res) => res,
            Err(_) => Err(writer_gone()),
        }
    }

    /// 已下载字节持久化（暂停/失败后 UI 恢复显示用；定期节流写入）。
    /// 同 URL 未完结任务（done/canceled/failed 之外）。
    pub async fn set_priority(&self, id: &str, priority: i32) -> Res<()> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Op::SetPriority {
                id: id.to_string(),
                priority,
                tx,
            })
            .map_err(|_| writer_gone())?;
        match rx.await {
            Ok(res) => res,
            Err(_) => Err(writer_gone()),
        }
    }

    pub async fn find_active_by_url(&self, url: &str) -> Res<Option<Task>> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Op::FindActiveByUrl {
                url: url.to_string(),
                tx,
            })
            .map_err(|_| writer_gone())?;
        match rx.await {
            Ok(res) => res,
            Err(_) => Err(writer_gone()),
        }
    }

    pub async fn update_variants(&self, id: &str, variants_json: &str) -> Res<()> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Op::UpdateVariants {
                id: id.to_string(),
                variants_json: variants_json.to_string(),
                tx,
            })
            .map_err(|_| writer_gone())?;
        match rx.await {
            Ok(res) => res,
            Err(_) => Err(writer_gone()),
        }
    }

    pub async fn update_downloaded(&self, id: &str, bytes: u64) -> Res<()> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Op::UpdateDownloaded {
                id: id.to_string(),
                bytes,
                tx,
            })
            .map_err(|_| writer_gone())?;
        match rx.await {
            Ok(res) => res,
            Err(_) => Err(writer_gone()),
        }
    }

    pub async fn rename_task(&self, id: &str, filename: &str) -> Res<()> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Op::Rename {
                id: id.to_string(),
                filename: filename.to_string(),
                tx,
            })
            .map_err(|_| writer_gone())?;
        match rx.await {
            Ok(res) => res,
            Err(_) => Err(writer_gone()),
        }
    }

    pub async fn insert_task(&self, task: Task) -> Res<()> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Op::Insert(Box::new(task), tx))
            .map_err(|_| writer_gone())?;
        match rx.await {
            Ok(res) => res,
            Err(_) => Err(writer_gone()),
        }
    }

    /// 状态迁移落库；目标行不存在时报 NotFound。
    pub async fn update_status(
        &self,
        id: &str,
        status: TaskStatus,
        error: Option<ErrorKind>,
    ) -> Res<()> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Op::UpdateStatus {
                id: id.to_string(),
                status,
                error,
                tx,
            })
            .map_err(|_| writer_gone())?;
        match rx.await {
            Ok(res) => res,
            Err(_) => Err(writer_gone()),
        }
    }

    /// 探测完成后回写元数据（DESIGN §5.5 tasks 表字段）。
    pub async fn update_probe(
        &self,
        id: &str,
        total_bytes: Option<u64>,
        etag: Option<String>,
        last_modified: Option<String>,
        mime: Option<String>,
        final_url: Option<String>,
    ) -> Res<()> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Op::UpdateProbe {
                id: id.to_string(),
                total_bytes,
                etag,
                last_modified,
                mime,
                final_url,
                tx,
            })
            .map_err(|_| writer_gone())?;
        match rx.await {
            Ok(res) => res,
            Err(_) => Err(writer_gone()),
        }
    }

    pub async fn get_task(&self, id: &str) -> Res<Option<Task>> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Op::Get {
                id: id.to_string(),
                tx,
            })
            .map_err(|_| writer_gone())?;
        match rx.await {
            Ok(res) => res,
            Err(_) => Err(writer_gone()),
        }
    }

    /// 增量拉取：updated_at > since 的行（GUI 轮询扩展性，万级任务不全量拉）。
    pub async fn list_updated_since(&self, since_ms: i64) -> Res<Vec<Task>> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Op::ListUpdatedSince { since_ms, tx })
            .map_err(|_| writer_gone())?;
        match rx.await {
            Ok(res) => res,
            Err(_) => Err(writer_gone()),
        }
    }

    pub async fn list_tasks(&self, status: Option<TaskStatus>) -> Res<Vec<Task>> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Op::List { status, tx })
            .map_err(|_| writer_gone())?;
        match rx.await {
            Ok(res) => res,
            Err(_) => Err(writer_gone()),
        }
    }

    pub async fn delete_task(&self, id: &str) -> Res<bool> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Op::Delete {
                id: id.to_string(),
                tx,
            })
            .map_err(|_| writer_gone())?;
        match rx.await {
            Ok(res) => res,
            Err(_) => Err(writer_gone()),
        }
    }
}

fn writer_gone() -> DownloadError {
    DownloadError::new(ErrorKind::Internal, "db writer thread terminated")
}

fn writer_loop(conn: Connection, rx: Receiver<Op>) {
    while let Ok(op) = rx.recv() {
        match op {
            Op::Snapshot { cursor, tx } => {
                let _ = tx.send(snapshot(&conn, cursor));
            }
            Op::Output {
                id,
                dir,
                filename,
                file_size,
                tx,
            } => {
                let result=conn.execute("UPDATE tasks SET dir=?1,filename=?2,file_size=COALESCE(?3,file_size),updated_at=?4 WHERE id=?5",rusqlite::params![dir,filename,file_size,now_ms(),id]).map(|_|()).map_err(sql_err);
                let _ = tx.send(result);
            }
            Op::SetSetting { key, value, tx } => {
                let res = conn
                    .execute(
                        "INSERT INTO settings(key, value) VALUES (?1, ?2)
                         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                        rusqlite::params![&key, &value],
                    )
                    .map(|_| ())
                    .map_err(sql_err);
                let _ = tx.send(res);
            }
            Op::SetPriority { id, priority, tx } => {
                let res = conn
                    .execute(
                        "UPDATE tasks SET priority = ?1, updated_at = ?2 WHERE id = ?3",
                        rusqlite::params![priority, now_ms(), &id],
                    )
                    .map(|_| ())
                    .map_err(sql_err);
                let _ = tx.send(res);
            }
            Op::FindActiveByUrl { url, tx } => {
                let sql = format!(
                    "SELECT {COLS} FROM tasks WHERE url = ?1 AND status NOT IN ('done', 'canceled', 'failed') LIMIT 1"
                );
                let res = match conn.query_row(&sql, [&url], row_to_task) {
                    Ok(t) => Ok(Some(t)),
                    Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                    Err(e) => Err(sql_err(e)),
                };
                let _ = tx.send(res);
            }
            Op::UpdateVariants {
                id,
                variants_json,
                tx,
            } => {
                let res = conn
                    .execute(
                        "UPDATE tasks SET variants_json = ?1, updated_at = ?2 WHERE id = ?3",
                        rusqlite::params![&variants_json, now_ms(), &id],
                    )
                    .map(|_| ())
                    .map_err(sql_err);
                let _ = tx.send(res);
            }
            Op::UpdateDownloaded { id, bytes, tx } => {
                let res = conn
                    .execute(
                        "UPDATE tasks SET downloaded_bytes = ?1, updated_at = ?2 WHERE id = ?3",
                        rusqlite::params![bytes as i64, now_ms(), &id],
                    )
                    .map(|_| ())
                    .map_err(sql_err);
                let _ = tx.send(res);
            }
            Op::Rename { id, filename, tx } => {
                let res = match conn.execute(
                    "UPDATE tasks SET filename = ?1, updated_at = ?2 WHERE id = ?3",
                    rusqlite::params![&filename, now_ms(), &id],
                ) {
                    Ok(0) => Err(DownloadError::new(
                        ErrorKind::NotFound,
                        format!("task {id} not found"),
                    )),
                    Ok(_) => Ok(()),
                    Err(e) => Err(sql_err(e)),
                };
                let _ = tx.send(res);
            }
            Op::GetSetting { key, tx } => {
                let res =
                    match conn.query_row("SELECT value FROM settings WHERE key = ?1", [&key], |r| {
                        r.get::<_, String>(0)
                    }) {
                        Ok(v) => Ok(Some(v)),
                        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                        Err(e) => Err(sql_err(e)),
                    };
                let _ = tx.send(res);
            }
            Op::Insert(task, tx) => {
                let _ = tx.send(insert(&conn, &task));
            }
            Op::UpdateStatus {
                id,
                status,
                error,
                tx,
            } => match update_status(&conn, &id, status, error.as_ref()) {
                Ok(0) => {
                    let _ = tx.send(Err(DownloadError::new(
                        ErrorKind::NotFound,
                        format!("task {id} not found"),
                    )));
                }
                Ok(_) => {
                    let _ = tx.send(Ok(()));
                }
                Err(e) => {
                    let _ = tx.send(Err(sql_err(e)));
                }
            },
            Op::UpdateProbe {
                id,
                total_bytes,
                etag,
                last_modified,
                mime,
                final_url,
                tx,
            } => {
                let res = match conn.execute(
                    "UPDATE tasks SET total_bytes = ?1, etag = ?2, last_modified = ?3, mime = ?4, final_url = ?5, updated_at = ?6 WHERE id = ?7",
                    rusqlite::params![
                        total_bytes.map(|v| v as i64),
                        &etag,
                        &last_modified,
                        &mime,
                        &final_url,
                        now_ms(),
                        &id,
                    ],
                ) {
                    Ok(0) => Err(DownloadError::new(
                        ErrorKind::NotFound,
                        format!("task {id} not found"),
                    )),
                    Ok(_) => Ok(()),
                    Err(e) => Err(sql_err(e)),
                };
                let _ = tx.send(res);
            }
            Op::Get { id, tx } => {
                let sql = format!("SELECT {COLS} FROM tasks WHERE id = ?1");
                let res = match conn.query_row(&sql, [&id], row_to_task) {
                    Ok(t) => Ok(Some(t)),
                    Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                    Err(e) => Err(sql_err(e)),
                };
                let _ = tx.send(res);
            }
            Op::List { status, tx } => {
                let res = list(&conn, status);
                let _ = tx.send(res);
            }
            Op::ListUpdatedSince { since_ms, tx } => {
                let sql = format!(
                    "SELECT {COLS} FROM tasks WHERE updated_at > ?1 ORDER BY updated_at ASC, id"
                );
                let res = (|| -> Res<Vec<Task>> {
                    let mut stmt = conn.prepare(&sql).map_err(sql_err)?;
                    let rows = stmt.query_map([since_ms], row_to_task).map_err(sql_err)?;
                    let mut tasks = Vec::new();
                    for row in rows {
                        tasks.push(row.map_err(sql_err)?);
                    }
                    Ok(tasks)
                })();
                let _ = tx.send(res);
            }
            Op::Delete { id, tx } => {
                let res = conn
                    .execute("DELETE FROM tasks WHERE id = ?1", [&id])
                    .map(|n| n > 0)
                    .map_err(sql_err);
                let _ = tx.send(res);
            }
        }
    }
}

fn snapshot(conn: &Connection, since: Option<u64>) -> Res<TaskSnapshot> {
    let transaction = conn.unchecked_transaction().map_err(sql_err)?;
    let cursor: u64 = transaction
        .query_row("SELECT COALESCE(MAX(seq),0) FROM task_changes", [], |r| {
            r.get(0)
        })
        .map_err(sql_err)?;
    let oldest: u64 = transaction
        .query_row("SELECT COALESCE(MIN(seq),0) FROM task_changes", [], |r| {
            r.get(0)
        })
        .map_err(sql_err)?;
    let full = since.is_none_or(|s| s > cursor || (oldest > 0 && s < oldest.saturating_sub(1)));
    let tasks = if full {
        list(&transaction, None)?
    } else {
        let sql=format!("SELECT {COLS} FROM tasks WHERE id IN (SELECT task_id FROM task_changes WHERE seq>?1 AND seq<=?2)");
        let mut stmt = transaction.prepare(&sql).map_err(sql_err)?;
        let rows = stmt
            .query_map(rusqlite::params![since.unwrap_or(0), cursor], row_to_task)
            .map_err(sql_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(sql_err)?
    };
    let deleted = if full {
        Vec::new()
    } else {
        let mut stmt=transaction.prepare("SELECT DISTINCT task_id FROM task_changes WHERE deleted=1 AND seq>?1 AND seq<=?2 AND task_id NOT IN (SELECT id FROM tasks)").map_err(sql_err)?;
        let rows = stmt
            .query_map(rusqlite::params![since.unwrap_or(0), cursor], |r| {
                r.get::<_, String>(0)
            })
            .map_err(sql_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(sql_err)?
    };
    transaction.commit().map_err(sql_err)?;
    if cursor > 100_000 {
        conn.execute("DELETE FROM task_changes WHERE seq<?1", [cursor - 100_000])
            .map_err(sql_err)?;
    }
    Ok(TaskSnapshot {
        cursor,
        full,
        tasks,
        deleted,
    })
}

fn insert(conn: &Connection, t: &Task) -> Res<()> {
    let error_json = t.error.as_ref().and_then(|e| serde_json::to_string(e).ok());
    conn.execute(
        "INSERT INTO tasks (id,url,final_url,filename,dir,total_bytes,etag,last_modified,mime,kind,status,priority,headers_ref,headers_json,downloaded_bytes,variants_json,checksum,mirrors_json,postprocess_json,error,created_at,updated_at,file_size)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23)",
        rusqlite::params![
            &t.id,
            &t.url,
            &t.final_url,
            &t.filename,
            &t.dir,
            t.total_bytes.map(|v| v as i64),
            &t.etag,
            &t.last_modified,
            &t.mime,
            t.kind.as_str(),
            t.status.as_str(),
            t.priority,
            &t.headers_ref,
            &t.headers_json,
            t.downloaded_bytes as i64,
            &t.variants_json,
            &t.checksum,
            &t.mirrors_json,
            &t.postprocess_json,
            error_json,
            t.created_at,
            t.updated_at,
            t.file_size,
        ],
    )
    .map_err(sql_err)?;
    Ok(())
}

fn update_status(
    conn: &Connection,
    id: &str,
    status: TaskStatus,
    error: Option<&ErrorKind>,
) -> rusqlite::Result<usize> {
    let error_json = error.and_then(|e| serde_json::to_string(e).ok());
    conn.execute(
        "UPDATE tasks SET status = ?1, error = ?2, updated_at = ?3 WHERE id = ?4",
        rusqlite::params![status.as_str(), error_json, now_ms(), id],
    )
}

fn list(conn: &Connection, status: Option<TaskStatus>) -> Res<Vec<Task>> {
    let mut tasks = Vec::new();
    match status {
        Some(s) => {
            let sql = format!(
                "SELECT {COLS} FROM tasks WHERE status = ?1 ORDER BY priority DESC, created_at ASC, id"
            );
            let mut stmt = conn.prepare(&sql).map_err(sql_err)?;
            let rows = stmt.query_map([s.as_str()], row_to_task).map_err(sql_err)?;
            for row in rows {
                tasks.push(row.map_err(sql_err)?);
            }
        }
        None => {
            let sql =
                format!("SELECT {COLS} FROM tasks ORDER BY priority DESC, created_at ASC, id");
            let mut stmt = conn.prepare(&sql).map_err(sql_err)?;
            let rows = stmt.query_map([], row_to_task).map_err(sql_err)?;
            for row in rows {
                tasks.push(row.map_err(sql_err)?);
            }
        }
    }
    Ok(tasks)
}

fn row_to_task(row: &rusqlite::Row) -> rusqlite::Result<Task> {
    let kind_str: String = row.get("kind")?;
    let status_str: String = row.get("status")?;
    let error_json: Option<String> = row.get("error")?;
    Ok(Task {
        id: row.get("id")?,
        url: row.get("url")?,
        final_url: row.get("final_url")?,
        filename: row.get("filename")?,
        dir: row.get("dir")?,
        total_bytes: row.get::<_, Option<i64>>("total_bytes")?.map(|v| v as u64),
        etag: row.get("etag")?,
        last_modified: row.get("last_modified")?,
        mime: row.get("mime")?,
        kind: TaskKind::parse(&kind_str).unwrap_or(TaskKind::Http),
        status: TaskStatus::parse(&status_str).unwrap_or(dl_model::TaskStatus::Queued),
        priority: row.get("priority")?,
        headers_ref: row.get("headers_ref")?,
        headers_json: row.get("headers_json")?,
        downloaded_bytes: row.get::<_, Option<i64>>("downloaded_bytes")?.unwrap_or(0) as u64,
        variants_json: row.get("variants_json")?,
        checksum: row.get("checksum")?,
        mirrors_json: row.get("mirrors_json")?,
        postprocess_json: row.get("postprocess_json")?,
        error: error_json.and_then(|s| serde_json::from_str(&s).ok()),
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
        file_size: row.get("file_size")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use dl_error::ErrorKind;

    fn sample_task(id: &str) -> Task {
        Task {
            id: id.to_string(),
            url: "https://example.com/video.mp4".into(),
            final_url: None,
            filename: Some("video.mp4".into()),
            dir: Some("D:\\Downloads".into()),
            file_size: None,
            total_bytes: Some(12345),
            etag: Some("\"abc\"".into()),
            last_modified: None,
            mime: Some("video/mp4".into()),
            kind: TaskKind::Http,
            status: TaskStatus::Queued,
            priority: 0,
            headers_ref: None,
            headers_json: None,
            downloaded_bytes: 0,
            variants_json: None,
            checksum: None,
            mirrors_json: None,
            postprocess_json: None,
            error: Some(ErrorKind::Http {
                status: 429,
                retry_after_secs: Some(3),
            }),
            created_at: 1000,
            updated_at: 1000,
        }
    }

    #[tokio::test]
    async fn task_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.db")).unwrap();

        let task = sample_task("t1");
        db.insert_task(task.clone()).await.unwrap();

        let got = db.get_task("t1").await.unwrap().unwrap();
        assert_eq!(got, task);

        db.update_status("t1", TaskStatus::Downloading, None)
            .await
            .unwrap();
        let got = db.get_task("t1").await.unwrap().unwrap();
        assert_eq!(got.status, TaskStatus::Downloading);
        assert_eq!(got.error, None);

        db.update_status("t1", TaskStatus::Failed, Some(ErrorKind::DiskFull))
            .await
            .unwrap();
        let got = db.get_task("t1").await.unwrap().unwrap();
        assert_eq!(got.error, Some(ErrorKind::DiskFull));

        assert_eq!(db.list_tasks(None).await.unwrap().len(), 1);
        assert!(db
            .list_tasks(Some(TaskStatus::Done))
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            db.list_tasks(Some(TaskStatus::Failed)).await.unwrap().len(),
            1
        );

        assert!(db.delete_task("t1").await.unwrap());
        assert!(!db.delete_task("t1").await.unwrap());
        assert!(db.get_task("t1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn update_status_missing_row_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.db")).unwrap();
        let err = db
            .update_status("nope", TaskStatus::Downloading, None)
            .await
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::NotFound);
    }
}
