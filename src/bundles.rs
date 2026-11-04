//! SQLite-backed storage for user-defined service bundles.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, params};

const BUNDLE_DIR: &str = ".services-manager";
const BUNDLE_DB: &str = "bundles.sqlite3";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bundle {
    pub id: i64,
    pub name: String,
    pub service_ids: Vec<String>,
}

/// Stores bundle definitions in a workspace-local SQLite database.
///
/// Enabled state is intentionally not persisted: bundles control running processes, which do not
/// survive an app restart, so a fresh session always starts with no active bundle.
pub struct BundleStore {
    conn: Connection,
    path: PathBuf,
}

impl BundleStore {
    pub fn open(workspace_root: &Path) -> Result<Self> {
        let dir = workspace_root.join(BUNDLE_DIR);
        fs::create_dir_all(&dir)
            .with_context(|| format!("cannot create bundle directory {}", dir.display()))?;
        let path = dir.join(BUNDLE_DB);
        let conn = Connection::open(&path)
            .with_context(|| format!("cannot open bundle database {}", path.display()))?;
        Self::initialize(conn, path)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().context("cannot open in-memory bundle database")?;
        Self::initialize(conn, PathBuf::from(":memory:"))
    }

    fn initialize(conn: Connection, path: PathBuf) -> Result<Self> {
        conn.pragma_update(None, "foreign_keys", "ON")
            .context("cannot enable SQLite foreign keys")?;
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS bundles (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                name       TEXT NOT NULL COLLATE NOCASE UNIQUE,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );

            CREATE TABLE IF NOT EXISTS bundle_services (
                bundle_id  INTEGER NOT NULL REFERENCES bundles(id) ON DELETE CASCADE,
                service_id TEXT NOT NULL,
                position   INTEGER NOT NULL,
                PRIMARY KEY (bundle_id, service_id)
            );
            ",
        )
        .context("cannot initialize bundle database schema")?;
        Ok(Self { conn, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn list(&self) -> Result<Vec<Bundle>> {
        let mut bundle_stmt = self
            .conn
            .prepare("SELECT id, name FROM bundles ORDER BY name COLLATE NOCASE, id")?;
        let base: Vec<(i64, String)> = bundle_stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        drop(bundle_stmt);

        let mut member_stmt = self.conn.prepare(
            "SELECT service_id FROM bundle_services WHERE bundle_id = ?1 ORDER BY position",
        )?;
        let mut bundles = Vec::with_capacity(base.len());
        for (id, name) in base {
            let service_ids = member_stmt
                .query_map(params![id], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            bundles.push(Bundle {
                id,
                name,
                service_ids,
            });
        }
        Ok(bundles)
    }

    #[cfg(test)]
    pub fn get(&self, id: i64) -> Result<Option<Bundle>> {
        Ok(self.list()?.into_iter().find(|bundle| bundle.id == id))
    }

    pub fn create(&mut self, name: &str, service_ids: &[String]) -> Result<i64> {
        let name = validate_name(name)?;
        let service_ids = dedupe_service_ids(service_ids)?;
        if self.name_taken(&name, None)? {
            bail!("a bundle named \"{name}\" already exists");
        }

        let tx = self.conn.transaction()?;
        tx.execute("INSERT INTO bundles(name) VALUES (?1)", params![name])?;
        let id = tx.last_insert_rowid();
        insert_members(&tx, id, &service_ids)?;
        tx.commit()?;
        Ok(id)
    }

    pub fn update(&mut self, id: i64, name: &str, service_ids: &[String]) -> Result<()> {
        let name = validate_name(name)?;
        let service_ids = dedupe_service_ids(service_ids)?;
        if self.name_taken(&name, Some(id))? {
            bail!("a bundle named \"{name}\" already exists");
        }

        let tx = self.conn.transaction()?;
        let changed = tx.execute(
            "UPDATE bundles SET name = ?1, updated_at = CURRENT_TIMESTAMP WHERE id = ?2",
            params![name, id],
        )?;
        if changed == 0 {
            bail!("bundle no longer exists");
        }
        tx.execute(
            "DELETE FROM bundle_services WHERE bundle_id = ?1",
            params![id],
        )?;
        insert_members(&tx, id, &service_ids)?;
        tx.commit()?;
        Ok(())
    }

    pub fn delete(&mut self, id: i64) -> Result<()> {
        let tx = self.conn.transaction()?;
        let changed = tx.execute("DELETE FROM bundles WHERE id = ?1", params![id])?;
        if changed == 0 {
            bail!("bundle no longer exists");
        }
        tx.commit()?;
        Ok(())
    }

    fn name_taken(&self, name: &str, exclude_id: Option<i64>) -> Result<bool> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM bundles
             WHERE name = ?1 COLLATE NOCASE
               AND (?2 IS NULL OR id <> ?2)",
            params![name, exclude_id],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }
}

fn validate_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        bail!("bundle name cannot be empty");
    }
    if name.chars().count() > 60 {
        bail!("bundle name must be 60 characters or fewer");
    }
    if name.chars().any(char::is_control) {
        bail!("bundle name cannot contain control characters");
    }
    Ok(name.to_string())
}

fn dedupe_service_ids(service_ids: &[String]) -> Result<Vec<String>> {
    let mut seen = HashSet::new();
    let ids: Vec<String> = service_ids
        .iter()
        .filter(|id| !id.is_empty())
        .filter(|id| seen.insert((*id).clone()))
        .cloned()
        .collect();
    if ids.is_empty() {
        bail!("select at least one service");
    }
    Ok(ids)
}

fn insert_members(conn: &Connection, bundle_id: i64, service_ids: &[String]) -> Result<()> {
    let mut stmt = conn.prepare(
        "INSERT INTO bundle_services(bundle_id, service_id, position) VALUES (?1, ?2, ?3)",
    )?;
    for (position, service_id) in service_ids.iter().enumerate() {
        stmt.execute(params![bundle_id, service_id, position as i64])?;
    }
    Ok(())
}
