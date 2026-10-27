use super::{model::*, mutation, receipt};
use anyhow::{Context, Result, anyhow, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};
use turso::{Connection, params};

pub(super) const APPLICATION_ID: i64 = 0x41474e44;
const SCHEMA: &str = "
CREATE TABLE store_meta(id INTEGER PRIMARY KEY CHECK(id=1), revision INTEGER NOT NULL CHECK(revision>=0)) STRICT;
INSERT INTO store_meta VALUES(1,0);
CREATE TABLE sources(id TEXT PRIMARY KEY, writable INTEGER NOT NULL CHECK(writable IN (0,1)), deletable INTEGER NOT NULL CHECK(deletable IN (0,1)), completable INTEGER NOT NULL CHECK(completable IN (0,1))) STRICT;
INSERT INTO sources VALUES('local',1,1,1);
CREATE TABLE items(id TEXT PRIMARY KEY, source_id TEXT NOT NULL REFERENCES sources(id), source_uid TEXT, kind TEXT NOT NULL CHECK(kind IN ('event','task')), revision INTEGER NOT NULL CHECK(revision>0), payload BLOB NOT NULL CHECK(length(payload)<=1048576), UNIQUE(source_id,source_uid)) STRICT;
CREATE INDEX items_source ON items(source_id);
CREATE TABLE operation_receipts(request_id TEXT PRIMARY KEY, fingerprint TEXT NOT NULL, payload BLOB NOT NULL CHECK(length(payload)<=1048576)) STRICT;
CREATE TABLE migrations(name TEXT PRIMARY KEY, fingerprint TEXT NOT NULL, payload BLOB NOT NULL) STRICT;
";
pub(super) struct Database {
    pub(super) conn: Connection,
    _database: turso::Database,
    _database_file: File,
    _ownership: File,
    root: PathBuf,
    read_only: bool,
    pub(super) blocked: bool,
}
#[derive(Serialize)]
#[serde(tag = "operation")]
pub(super) enum Mutation {
    Create {
        request_id: String,
        item: NewItem,
    },
    Patch {
        request_id: String,
        id: String,
        expected_revision: u64,
        patch: ItemPatch,
    },
    Delete {
        request_id: String,
        id: String,
        expected_revision: u64,
    },
}
impl Mutation {
    fn request_id(&self) -> &str {
        match self {
            Self::Create { request_id, .. }
            | Self::Patch { request_id, .. }
            | Self::Delete { request_id, .. } => request_id,
        }
    }
}
pub(super) fn fingerprint(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub(super) fn private_file(path: &Path, exclusive: bool) -> Result<File> {
    private_file_options(path, !exclusive, exclusive)
}
fn existing_private_file(path: &Path) -> Result<File> {
    private_file_options(path, false, false)
}
fn private_file_options(path: &Path, create: bool, exclusive: bool) -> Result<File> {
    if let Ok(meta) = fs::symlink_metadata(path) {
        ensure!(
            meta.is_file() && !meta.file_type().is_symlink(),
            "unsafe agenda file {}",
            path.display()
        );
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .create_new(exclusive)
        .mode(0o600)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)?;
    let meta = file.metadata()?;
    ensure!(
        meta.uid() == rustix::process::geteuid().as_raw() && meta.nlink() == 1,
        "unsafe file ownership/hardlinks"
    );
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(file)
}
impl Database {
    pub(super) async fn open(
        root: &Path,
        legacy: Option<&Path>,
        existing_only: bool,
    ) -> Result<Self> {
        if !existing_only {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(root)?;
        }
        let meta = fs::symlink_metadata(root)?;
        ensure!(
            meta.is_dir()
                && !meta.file_type().is_symlink()
                && meta.uid() == rustix::process::geteuid().as_raw(),
            "unsafe agenda directory"
        );
        fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
        let ownership = private_file(&root.join("agenda.db.lock"), false)?;
        ownership.try_lock().context("agenda ownership lock busy")?;
        let path = root.join("agenda.db");
        let (database_file, newly_created) = match fs::symlink_metadata(&path) {
            Ok(_) => (existing_private_file(&path)?, false),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && !existing_only => {
                (private_file(&path, true)?, true)
            }
            Err(error) => return Err(error.into()),
        };
        ensure!(
            newly_created
                || database_file.metadata()?.len() > 0
                || root.join("agenda.db-wal").try_exists()?,
            "preexisting empty agenda database; preserve for recovery"
        );
        for suffix in ["agenda.db-wal", "agenda.db-shm"] {
            if root.join(suffix).try_exists()? {
                drop(existing_private_file(&root.join(suffix))?);
            }
        }
        let database = turso::Builder::new_local(
            path.to_str()
                .ok_or_else(|| anyhow!("non-UTF8 database path"))?,
        )
        .with_io(turso::IoBackend::Syscall)
        .read_only(existing_only)
        .build()
        .await?;
        let opened = fs::symlink_metadata(&path)?;
        let held = database_file.metadata()?;
        ensure!(
            opened.is_file() && opened.ino() == held.ino() && opened.dev() == held.dev(),
            "agenda database changed during open"
        );
        let conn = database.connect()?;
        conn.execute_batch(
            "PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON; PRAGMA cache_size=-8192;",
        )
        .await?;
        let mut db = Self {
            conn,
            _database: database,
            _database_file: database_file,
            _ownership: ownership,
            root: root.to_owned(),
            read_only: existing_only,
            blocked: false,
        };
        let version = db.integer("PRAGMA user_version").await?;
        let application = db.integer("PRAGMA application_id").await?;
        if newly_created
            && version == 0
            && application == 0
            && db
                .integer("SELECT count(*) FROM sqlite_master WHERE type='table'")
                .await?
                == 0
        {
            db.conn
                .transactional_batch(
                    format!(
                        "{SCHEMA} PRAGMA application_id={APPLICATION_ID}; PRAGMA user_version=1;"
                    )
                    .split(';')
                    .filter(|s| !s.trim().is_empty()),
                    turso::transaction::TransactionBehavior::Immediate,
                )
                .await?;
        } else {
            ensure!(
                version == 1 && application == APPLICATION_ID,
                "unsupported agenda schema; preserve for recovery"
            );
        }
        db.integrity().await?;
        ensure!(
            db.capabilities("local").await? == Capabilities::local(),
            "invalid reserved local source capabilities"
        );
        if let Some(path) = legacy {
            db.migrate_legacy(root, path).await?;
        }
        // Validate persisted payloads and their indexed identities before advertising capabilities.
        db.snapshot().await?;
        db.validate_receipts().await?;
        db.ensure_payload_budget().await?;
        db.harden_sidecars()?;
        Ok(db)
    }
    pub(super) async fn integer(&self, sql: &str) -> Result<i64> {
        Ok(self
            .conn
            .query(sql, ())
            .await?
            .next()
            .await?
            .ok_or_else(|| anyhow!("missing scalar"))?
            .get(0)?)
    }
    async fn integrity(&self) -> Result<()> {
        let mut rows = self.conn.query("PRAGMA integrity_check", ()).await?;
        let value: String = rows
            .next()
            .await?
            .ok_or_else(|| anyhow!("missing integrity result"))?
            .get(0)?;
        ensure!(value == "ok", "agenda integrity failure: {value}");
        let mut rows = self.conn.query("PRAGMA foreign_key_check", ()).await?;
        ensure!(rows.next().await?.is_none(), "agenda foreign key failure");
        Ok(())
    }
    fn harden_sidecars(&self) -> Result<()> {
        // Turso creates WAL with the process umask, not the main DB's mode.
        // The containing directory is already 0700; never change process-global umask.
        for name in ["agenda.db-wal", "agenda.db-shm"] {
            let path = self.root.join(name);
            match fs::symlink_metadata(&path) {
                Ok(_) => {
                    drop(existing_private_file(&path)?);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
    pub(super) async fn checkpoint(&self) -> Result<()> {
        // Existing-only maintenance readers never write a WAL or checkpoint.
        if self.read_only {
            return Ok(());
        }
        ensure!(
            !self.blocked,
            "agenda fenced after transaction recovery failure"
        );
        let mut rows = self
            .conn
            .query("PRAGMA wal_checkpoint(TRUNCATE)", ())
            .await?;
        while let Some(row) = rows.next().await? {
            let busy: i64 = row.get(0)?;
            ensure!(busy == 0, "agenda checkpoint busy");
        }
        self.harden_sidecars()?;
        Ok(())
    }
    pub(super) async fn finish<T>(&mut self, result: Result<T>) -> Result<T> {
        let result = match result {
            Ok(value) => match self.conn.execute("COMMIT", ()).await {
                Ok(_) => Ok(value),
                Err(error) => {
                    // Commit may have become durable despite its error. No subsequent
                    // writes until reopening reconciles receipts and the catalog.
                    self.blocked = true;
                    Err(error.into())
                }
            },
            Err(error) => Err(error),
        };
        if result.is_err() && !self.conn.is_autocommit().unwrap_or(false) {
            let rollback = self
                .conn
                .execute("ROLLBACK", ())
                .await
                .map(|_| ())
                .map_err(Into::into);
            self.record_rollback_result(rollback);
        }
        if let Err(error) = self.harden_sidecars() {
            self.blocked = true;
            if result.is_ok() {
                return Err(error)
                    .context("committed but sidecar protection failed; resolve by receipt");
            }
        }
        result
    }
    pub(super) fn record_rollback_result(&mut self, result: Result<()>) {
        if result.is_err() {
            self.blocked = true;
        }
    }
    pub(super) async fn ensure_payload_budget(&self) -> Result<()> {
        let items = self
            .integer("SELECT coalesce(sum(length(payload)),0) FROM items")
            .await?;
        let receipts = self
            .integer("SELECT coalesce(sum(length(payload)),0) FROM operation_receipts")
            .await?;
        let migrations = self
            .integer("SELECT coalesce(sum(length(payload)),0) FROM migrations")
            .await?;
        ensure!(
            items + receipts + migrations <= 100 * 1024 * 1024,
            "agenda payload capacity reached; export and reconcile explicitly"
        );
        Ok(())
    }
    pub(super) async fn invalidate_catalog(&mut self) -> Result<u64> {
        ensure!(!self.read_only && !self.blocked, "agenda writes fenced");
        self.conn.execute("BEGIN IMMEDIATE", ()).await?;
        let result = async {
            self.conn
                .execute("UPDATE store_meta SET revision=revision+1 WHERE id=1", ())
                .await?;
            self.revision().await
        }
        .await;
        self.finish(result).await
    }
    pub(super) async fn revision(&self) -> Result<u64> {
        Ok(self
            .integer("SELECT revision FROM store_meta WHERE id=1")
            .await? as u64)
    }
    async fn decode_row(&self, row: &turso::Row) -> Result<Record> {
        let bytes: Vec<u8> = row.get(5)?;
        let record: Record = serde_json::from_slice(&bytes)?;
        record.validate()?;
        ensure!(
            record.id == row.get::<String>(0)?
                && record.source_id == row.get::<String>(1)?
                && record.source_uid == row.get::<Option<String>>(2)?
                && record.revision == row.get::<i64>(4)? as u64,
            "record index/payload mismatch"
        );
        let kind: String = row.get(3)?;
        ensure!(
            kind == match record.kind {
                ItemKind::Event => "event",
                ItemKind::Task => "task",
            },
            "kind mismatch"
        );
        let mut caps = self.capabilities(&record.source_id).await?;
        if record.kind == ItemKind::Event {
            caps.complete = false;
        }
        ensure!(
            record.capabilities == caps,
            "record/source capability mismatch"
        );
        Ok(record)
    }
    async fn capabilities(&self, source: &str) -> Result<Capabilities> {
        let row = self
            .conn
            .query(
                "SELECT writable,deletable,completable FROM sources WHERE id=?",
                [source],
            )
            .await?
            .next()
            .await?
            .ok_or_else(|| anyhow!("unknown source"))?;
        Ok(Capabilities {
            write: row.get::<i64>(0)? != 0,
            delete: row.get::<i64>(1)? != 0,
            complete: row.get::<i64>(2)? != 0,
        })
    }
    pub(super) async fn snapshot(&self) -> Result<Snapshot> {
        ensure!(
            self.integer("SELECT count(*) FROM items").await? <= 10000,
            "agenda item limit exceeded"
        );
        let mut rows = self.conn.query("SELECT id,source_id,source_uid,kind,revision,payload FROM items ORDER BY id LIMIT 10001", ()).await?;
        let mut items = Vec::new();
        while let Some(row) = rows.next().await? {
            items.push(self.decode_row(&row).await?);
        }
        Ok(Snapshot {
            items,
            catalog_revision: self.revision().await?,
        })
    }
    pub(super) async fn get(&self, id: &str) -> Result<Option<Record>> {
        let row = self
            .conn
            .query(
                "SELECT id,source_id,source_uid,kind,revision,payload FROM items WHERE id=?",
                [id],
            )
            .await?
            .next()
            .await?;
        match row {
            Some(row) => Ok(Some(self.decode_row(&row).await?)),
            None => Ok(None),
        }
    }
    async fn validate_receipts(&self) -> Result<()> {
        ensure!(
            self.integer("SELECT count(*) FROM operation_receipts")
                .await?
                <= 100000,
            "receipt capacity exceeded"
        );
        let revision = self.revision().await?;
        let mut rows = self
            .conn
            .query(
                "SELECT request_id,fingerprint,payload FROM operation_receipts",
                (),
            )
            .await?;
        while let Some(row) = rows.next().await? {
            let request: String = row.get(0)?;
            let hash: String = row.get(1)?;
            receipt::decode(&request, &hash, &row.get::<Vec<u8>>(2)?, revision)?;
        }
        Ok(())
    }
    pub(super) async fn receipt(&self, request: &str) -> Result<Option<(String, MutationResult)>> {
        identifier(request)?;
        let row = self
            .conn
            .query(
                "SELECT fingerprint,payload FROM operation_receipts WHERE request_id=?",
                [request],
            )
            .await?
            .next()
            .await?;
        match row {
            Some(row) => {
                let hash: String = row.get(0)?;
                let result = receipt::decode(
                    request,
                    &hash,
                    &row.get::<Vec<u8>>(1)?,
                    self.revision().await?,
                )?;
                Ok(Some((hash, result)))
            }
            None => Ok(None),
        }
    }
    pub(super) async fn insert_record(&self, record: &Record) -> Result<()> {
        record.validate()?;
        let mut caps = self.capabilities(&record.source_id).await?;
        if record.kind == ItemKind::Event {
            caps.complete = false;
        }
        ensure!(
            record.capabilities == caps,
            "record/source capability mismatch"
        );
        let bytes = serde_json::to_vec(record)?;
        self.conn.execute("INSERT INTO items(id,source_id,source_uid,kind,revision,payload) VALUES(?,?,?,?,?,?)", params![record.id.clone(), record.source_id.clone(), record.source_uid.clone(), match record.kind { ItemKind::Event => "event", ItemKind::Task => "task" }, record.revision as i64, bytes]).await?;
        Ok(())
    }
    pub(super) async fn mutate(&mut self, mutation: Mutation) -> Result<MutationResult> {
        ensure!(
            !self.read_only,
            "existing-only maintenance store is read-only"
        );
        ensure!(!self.blocked, "agenda writes fenced");
        let request = mutation.request_id().to_owned();
        identifier(&request)?;
        // Reject oversized envelopes before allocating a serialized fingerprint.
        match &mutation {
            Mutation::Create { item, .. } => item.validate_bounds()?,
            Mutation::Patch { patch, .. } => patch.validate_bounds()?,
            Mutation::Delete { .. } => {}
        }
        let hash = fingerprint(&serde_json::to_vec(&mutation)?);
        if let Some((previous, result)) = self.receipt(&request).await? {
            ensure!(hash == previous, "request_id fingerprint mismatch");
            return Ok(result);
        }
        // Never evict receipts: an old uncertain mutation must not become replayable.
        ensure!(
            self.integer("SELECT count(*) FROM operation_receipts")
                .await?
                < 100000,
            "receipt capacity reached; export and reconcile explicitly"
        );
        self.conn.execute("BEGIN IMMEDIATE", ()).await?;
        let result = self.apply(mutation, &request, &hash).await;
        self.finish(result).await
    }
    async fn create_item(&self, request: &str, item: NewItem, now: i64) -> Result<Record> {
        ensure!(
            self.integer("SELECT count(*) FROM items").await? < 10000,
            "agenda item capacity reached"
        );
        ensure!(
            self.capabilities("local").await?.write,
            "local source is read-only"
        );
        let id = format!("local:{}", fingerprint(request.as_bytes()));
        let record = mutation::create_record(id, item, now);
        self.insert_record(&record).await?;
        Ok(record)
    }
    async fn patch_item(
        &self,
        id: &str,
        expected_revision: u64,
        patch: ItemPatch,
        now: i64,
    ) -> Result<Record> {
        let record = self
            .get(id)
            .await?
            .ok_or_else(|| anyhow!("item not found"))?;
        let record = mutation::patch_record(record, expected_revision, patch, now)?;
        let changes = self
            .conn
            .execute(
                "UPDATE items SET revision=?,payload=? WHERE id=? AND revision=?",
                params![
                    record.revision as i64,
                    serde_json::to_vec(&record)?,
                    id,
                    expected_revision as i64
                ],
            )
            .await?;
        ensure!(changes == 1, "revision conflict");
        Ok(record)
    }
    async fn delete_item(&self, id: &str, expected_revision: u64) -> Result<()> {
        let record = self
            .get(id)
            .await?
            .ok_or_else(|| anyhow!("item not found"))?;
        ensure!(record.revision == expected_revision, "revision conflict");
        ensure!(record.capabilities.delete, "source cannot delete");
        ensure!(
            self.conn
                .execute(
                    "DELETE FROM items WHERE id=? AND revision=?",
                    params![id, expected_revision as i64]
                )
                .await?
                == 1,
            "revision conflict"
        );
        Ok(())
    }
    async fn apply(&self, mutation: Mutation, request: &str, hash: &str) -> Result<MutationResult> {
        let now = chrono::Utc::now().timestamp_millis();
        let (item, deleted_id) = match mutation {
            Mutation::Create { item, .. } => {
                (Some(self.create_item(request, item, now).await?), None)
            }
            Mutation::Patch {
                id,
                expected_revision,
                patch,
                ..
            } => (
                Some(self.patch_item(&id, expected_revision, patch, now).await?),
                None,
            ),
            Mutation::Delete {
                id,
                expected_revision,
                ..
            } => {
                self.delete_item(&id, expected_revision).await?;
                (None, Some(id))
            }
        };
        self.conn
            .execute("UPDATE store_meta SET revision=revision+1 WHERE id=1", ())
            .await?;
        let result = MutationResult {
            item,
            catalog_revision: self.revision().await?,
            request_id: request.into(),
            deleted_id,
        };
        self.conn
            .execute(
                "INSERT INTO operation_receipts VALUES(?,?,?)",
                params![request, hash, serde_json::to_vec(&result)?],
            )
            .await?;
        self.ensure_payload_budget().await?;
        Ok(result)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Export {
    application_id: i64,
    schema: u32,
    snapshot: Snapshot,
    sources: Vec<(String, i64, i64, i64)>,
    receipts: Vec<(String, String, Vec<u8>)>,
    migrations: Vec<(String, String, Vec<u8>)>,
}
impl Database {
    async fn triples(&self, sql: &str) -> Result<Vec<(String, String, Vec<u8>)>> {
        let mut rows = self.conn.query(sql, ()).await?;
        let mut values = Vec::new();
        while let Some(row) = rows.next().await? {
            values.push((row.get(0)?, row.get(1)?, row.get(2)?));
        }
        Ok(values)
    }
    pub(super) async fn export(&self, path: &Path) -> Result<()> {
        ensure!(!self.blocked, "agenda fenced");
        let mut rows = self
            .conn
            .query(
                "SELECT id,writable,deletable,completable FROM sources ORDER BY id",
                (),
            )
            .await?;
        let mut sources = Vec::new();
        while let Some(row) = rows.next().await? {
            sources.push((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?));
        }
        let export = Export { application_id: APPLICATION_ID, schema: 1, snapshot: self.snapshot().await?, sources, receipts: self.triples("SELECT request_id,fingerprint,payload FROM operation_receipts ORDER BY request_id").await?, migrations: self.triples("SELECT name,fingerprint,payload FROM migrations ORDER BY name").await? };
        let mut file = private_file(path, true)?;
        serde_json::to_writer(&mut file, &export)?;
        file.flush()?;
        file.sync_all()?;
        File::open(path.parent().unwrap_or(Path::new(".")))?.sync_all()?;
        Ok(())
    }
    pub(super) async fn restore(&mut self, path: &Path) -> Result<()> {
        ensure!(
            !self.read_only,
            "existing-only maintenance store is read-only"
        );
        ensure!(!self.blocked, "agenda fenced");
        ensure!(
            fs::metadata(path)?.len() <= 512 * 1024 * 1024,
            "export too large"
        );
        let export: Export = serde_json::from_reader(File::open(path)?)?;
        ensure!(
            export.application_id == APPLICATION_ID && export.schema == 1,
            "unsupported export schema"
        );
        ensure!(
            export.snapshot.items.len() <= 10000
                && export.receipts.len() <= 100000
                && export.sources.len() <= 10001
                && export.migrations.len() <= 1,
            "export capacity exceeded"
        );
        ensure!(
            export.snapshot.catalog_revision <= i64::MAX as u64,
            "invalid catalog revision"
        );
        ensure!(
            self.integer("SELECT count(*) FROM items").await? == 0
                && self
                    .integer("SELECT count(*) FROM operation_receipts")
                    .await?
                    == 0
                && self.integer("SELECT count(*) FROM migrations").await? == 0
                && self.revision().await? == 0,
            "restore requires empty store"
        );
        self.conn.execute("BEGIN IMMEDIATE", ()).await?;
        let result = async {
            self.conn.execute("DELETE FROM sources", ()).await?;
            for (id, w, d, c) in export.sources {
                identifier(&id)?;
                self.conn
                    .execute("INSERT INTO sources VALUES(?,?,?,?)", params![id, w, d, c])
                    .await?;
            }
            for item in export.snapshot.items {
                let mut caps = self.capabilities(&item.source_id).await?;
                if item.kind == ItemKind::Event {
                    caps.complete = false;
                }
                ensure!(item.capabilities == caps, "restored capability mismatch");
                self.insert_record(&item).await?;
            }
            for (id, hash, body) in export.receipts {
                receipt::decode(&id, &hash, &body, export.snapshot.catalog_revision)?;
                self.conn
                    .execute(
                        "INSERT INTO operation_receipts VALUES(?,?,?)",
                        params![id, hash, body],
                    )
                    .await?;
            }
            for (id, hash, body) in export.migrations {
                ensure!(
                    id == "legacy-todos-v1"
                        && body.len() <= 32 * 1024 * 1024
                        && (hash == fingerprint(&body) || (hash == "missing" && body.is_empty())),
                    "invalid migration provenance"
                );
                self.conn
                    .execute(
                        "INSERT INTO migrations VALUES(?,?,?)",
                        params![id, hash, body],
                    )
                    .await?;
            }
            self.conn
                .execute(
                    "UPDATE store_meta SET revision=? WHERE id=1",
                    [export.snapshot.catalog_revision as i64],
                )
                .await?;
            ensure!(
                self.capabilities("local").await? == Capabilities::local(),
                "invalid reserved local source capabilities"
            );
            self.integrity().await?;
            self.ensure_payload_budget().await?;
            Ok(())
        }
        .await;
        self.finish(result).await
    }
}
