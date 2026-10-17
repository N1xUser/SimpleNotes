use rusqlite::{params, Connection, OptionalExtension};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Clone, Debug)]
pub struct Note {
    pub id: i64,
    pub text: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub archived: bool,
    pub todo: bool,
}

#[derive(Clone, Debug)]
pub struct TodoItem {
    pub id: i64,
    pub text: String,
    pub status: u8,
}

pub struct TodoState {
    pub items: Vec<TodoItem>,
    pub draft: String,
    pub editing: Option<i64>,
}

pub fn bullet_items(text: &str) -> Option<Vec<String>> {
    if !text.starts_with('-') {
        return None;
    }
    let mut items = Vec::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let title = line.trim_start().strip_prefix('-').unwrap_or(line).trim();
        if !title.is_empty() {
            items.push(title.to_owned());
        }
    }
    (!items.is_empty()).then_some(items)
}

pub struct Store {
    conn: Connection,
    pub directory: PathBuf,
}

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

impl Store {
    pub fn open(directory: &Path) -> Result<Self> {
        fs::create_dir_all(directory)?;
        let conn = Connection::open(directory.join("notes.db"))?;
        conn.busy_timeout(std::time::Duration::from_secs(3))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
            PRAGMA foreign_keys=ON; PRAGMA cache_size=-512; PRAGMA wal_autocheckpoint=128;
            CREATE TABLE IF NOT EXISTS notes (
                id INTEGER PRIMARY KEY, text TEXT NOT NULL DEFAULT '',
                x INTEGER NOT NULL DEFAULT 60, y INTEGER NOT NULL DEFAULT 60,
                width INTEGER NOT NULL DEFAULT 280, height INTEGER NOT NULL DEFAULT 260,
                archived INTEGER NOT NULL DEFAULT 0, updated INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS todo_meta (
                note_id INTEGER PRIMARY KEY REFERENCES notes(id),
                draft TEXT NOT NULL DEFAULT '', editing_id INTEGER);
            CREATE TABLE IF NOT EXISTS todo_items (
                id INTEGER PRIMARY KEY, note_id INTEGER NOT NULL REFERENCES notes(id),
                text TEXT NOT NULL, status INTEGER NOT NULL DEFAULT 0 CHECK(status BETWEEN 0 AND 2));
            CREATE INDEX IF NOT EXISTS todo_items_note ON todo_items(note_id,id);
            CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )?;
        let check: String = conn.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
        if check != "ok" {
            return Err(format!("The database needs recovery: {check}. The original files have been preserved.").into());
        }
        Ok(Self {
            conn,
            directory: directory.to_owned(),
        })
    }

    pub fn notes(&self) -> Result<Vec<Note>> {
        let mut query = self.conn.prepare(
            "SELECT id,text,x,y,width,height,archived,EXISTS(SELECT 1 FROM todo_meta WHERE note_id=notes.id) FROM notes ORDER BY archived,id DESC",
        )?;
        let rows = query.query_map([], |r| {
            Ok(Note {
                id: r.get(0)?,
                text: r.get(1)?,
                x: r.get(2)?,
                y: r.get(3)?,
                width: r.get(4)?,
                height: r.get(5)?,
                archived: r.get(6)?,
                todo: r.get(7)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn create(&self, text: &str, x: i32, y: i32) -> Result<Note> {
        self.conn.execute(
            "INSERT INTO notes(text,x,y,updated) VALUES (?1,?2,?3,?4)",
            params![text, x, y, now()],
        )?;
        let id = self.conn.last_insert_rowid();
        Ok(Note {
            id,
            text: text.into(),
            x,
            y,
            width: 280,
            height: 260,
            archived: false,
            todo: false,
        })
    }

    // One durable transaction per edit; no revision history is generated.
    pub fn save_text(&self, id: i64, text: &str) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let old: String = tx.query_row("SELECT text FROM notes WHERE id=?1", [id], |r| r.get(0))?;
        if old == text {
            return Ok(());
        }
        tx.execute(
            "UPDATE notes SET text=?2,updated=?3 WHERE id=?1",
            params![id, text, now()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn geometry(&self, id: i64, x: i32, y: i32, width: i32, height: i32) -> Result<()> {
        self.conn.execute(
            "UPDATE notes SET x=?2,y=?3,width=?4,height=?5 WHERE id=?1",
            params![id, x, y, width, height],
        )?;
        Ok(())
    }

    pub fn archive(&self, id: i64, archived: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE notes SET archived=?2 WHERE id=?1",
            params![id, archived],
        )?;
        Ok(())
    }

    pub fn convert_to_todo(&self, id: i64) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let content: String =
            tx.query_row("SELECT text FROM notes WHERE id=?1", [id], |r| r.get(0))?;
        let items = bullet_items(&content)
            .ok_or("The note must start with '-' and contain at least one task.")?;
        tx.execute("INSERT INTO todo_meta(note_id) VALUES(?1)", [id])?;
        let prior = self
            .conn
            .query_row(
                "SELECT value FROM settings WHERE key=?1",
                [format!("todo-disabled-{}", id)],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        let restore = prior.as_deref() == Some(content.as_str());
        if !restore {
            tx.execute("DELETE FROM todo_items WHERE note_id=?1", [id])?;
        }
        for title in if restore { Vec::new() } else { items } {
            tx.execute(
                "INSERT INTO todo_items(note_id,text) VALUES(?1,?2)",
                params![id, title],
            )?;
        }
        self.sync_todo_text(id)?;
        tx.commit()?;
        Ok(())
    }

    pub fn disable_todo(&self, id: i64) -> Result<String> {
        let tx = self.conn.unchecked_transaction()?;
        let mut state = self.todo_state(id)?;
        if !state.draft.trim().is_empty() {
            if let Some(item) = state.editing {
                self.conn.execute(
                    "UPDATE todo_items SET text=?3 WHERE note_id=?1 AND id=?2",
                    params![id, item, state.draft.trim()],
                )?;
            } else {
                self.conn.execute(
                    "INSERT INTO todo_items(note_id,text) VALUES(?1,?2)",
                    params![id, state.draft.trim()],
                )?;
            }
            state = self.todo_state(id)?;
        }
        let content = state
            .items
            .iter()
            .map(|item| format!("- {}", item.text))
            .collect::<Vec<_>>()
            .join("\r\n");
        // Keep the last task states for a possible re-enable; no hidden data is discarded.
        self.conn.execute("INSERT INTO settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![format!("todo-disabled-{}",id),content])?;
        self.conn
            .execute("DELETE FROM todo_meta WHERE note_id=?1", [id])?;
        self.conn.execute(
            "UPDATE notes SET text=?2,updated=?3 WHERE id=?1",
            params![id, content, now()],
        )?;
        tx.commit()?;
        Ok(content)
    }

    pub fn todo_state(&self, id: i64) -> Result<TodoState> {
        let (draft, editing) = self.conn.query_row(
            "SELECT draft,editing_id FROM todo_meta WHERE note_id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let mut query = self
            .conn
            .prepare("SELECT id,text,status FROM todo_items WHERE note_id=?1 ORDER BY id")?;
        let items = query
            .query_map([id], |r| {
                Ok(TodoItem {
                    id: r.get(0)?,
                    text: r.get(1)?,
                    status: r.get(2)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(TodoState {
            items,
            draft,
            editing,
        })
    }

    fn sync_todo_text(&self, id: i64) -> Result<()> {
        let state = self.todo_state(id)?;
        let mut text = state
            .items
            .iter()
            .map(|item| {
                format!(
                    "- [{}] {}",
                    ["Pending", "In progress", "Done"][item.status as usize],
                    item.text
                )
            })
            .collect::<Vec<_>>()
            .join("\r\n");
        if !state.draft.is_empty() {
            text.push_str(&format!("\r\n[Draft] {}", state.draft));
        }
        self.conn.execute(
            "UPDATE notes SET text=?2,updated=?3 WHERE id=?1",
            params![id, text, now()],
        )?;
        Ok(())
    }

    pub fn save_todo_draft(&self, id: i64, draft: &str) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        self.conn.execute(
            "UPDATE todo_meta SET draft=?2 WHERE note_id=?1",
            params![id, draft],
        )?;
        self.sync_todo_text(id)?;
        tx.commit()?;
        Ok(())
    }

    pub fn edit_todo_item(&self, id: i64, item: i64) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let title: String = self.conn.query_row(
            "SELECT text FROM todo_items WHERE note_id=?1 AND id=?2",
            params![id, item],
            |r| r.get(0),
        )?;
        self.conn.execute(
            "UPDATE todo_meta SET draft=?2,editing_id=?3 WHERE note_id=?1",
            params![id, title, item],
        )?;
        self.sync_todo_text(id)?;
        tx.commit()?;
        Ok(())
    }

    pub fn commit_todo_draft(&self, id: i64) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let state = self.todo_state(id)?;
        let title = state.draft.trim();
        if title.is_empty() {
            return Err("Enter a task name.".into());
        }
        if let Some(item) = state.editing {
            self.conn.execute(
                "UPDATE todo_items SET text=?3 WHERE note_id=?1 AND id=?2",
                params![id, item, title],
            )?;
        } else {
            self.conn.execute(
                "INSERT INTO todo_items(note_id,text) VALUES(?1,?2)",
                params![id, title],
            )?;
        }
        self.conn.execute(
            "UPDATE todo_meta SET draft='',editing_id=NULL WHERE note_id=?1",
            [id],
        )?;
        self.sync_todo_text(id)?;
        tx.commit()?;
        Ok(())
    }

    pub fn cycle_todo_item(&self, id: i64, item: i64) -> Result<bool> {
        let tx = self.conn.unchecked_transaction()?;
        if self.conn.execute(
            "UPDATE todo_items SET status=(status+1)%3 WHERE note_id=?1 AND id=?2",
            params![id, item],
        )? != 1
        {
            return Err("The task could not be found.".into());
        }
        self.sync_todo_text(id)?;
        let state = self.todo_state(id)?;
        let complete = !state.items.is_empty()
            && state.items.iter().all(|i| i.status == 2)
            && state.draft.trim().is_empty();
        tx.commit()?;
        Ok(complete)
    }

    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM settings WHERE key=?1", [key], |r| {
                r.get(0)
            })
            .optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute("INSERT INTO settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key,value])?;
        Ok(())
    }

    pub fn backup(&self, force: bool) -> Result<Option<PathBuf>> {
        let day = now() / 86400;
        if !force && self.setting("backup_day")?.as_deref() == Some(&day.to_string()) {
            return Ok(None);
        }
        let dir = self.directory.join("backups");
        fs::create_dir_all(&dir)?;
        // Never replace a known-good backup. SQLite backup includes committed WAL data.
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let path = dir.join(format!(
            "{}-{stamp}.db",
            if force { "manual" } else { "daily" }
        ));
        let mut destination = Connection::open(&path)?;
        rusqlite::backup::Backup::new(&self.conn, &mut destination)?.run_to_completion(
            64,
            std::time::Duration::from_millis(1),
            None,
        )?;
        self.set_setting("backup_day", &day.to_string())?;
        if !force {
            let mut paths = fs::read_dir(&dir)?
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .is_some_and(|s| s.to_string_lossy().starts_with("daily-"))
                        && p.extension().is_some_and(|s| s == "db")
                })
                .collect::<Vec<_>>();
            paths.sort();
            let extra = paths.len().saturating_sub(30);
            for path in paths.into_iter().take(extra) {
                fs::remove_file(path)?;
            }
        }
        Ok(Some(path))
    }

    pub fn export(&self) -> Result<PathBuf> {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let dir = self
            .directory
            .join("exports")
            .join(format!("notes-{stamp}"));
        fs::create_dir_all(&dir)?;
        for note in self.notes()? {
            let path = dir.join(format!(
                "note-{}{}.txt",
                note.id,
                if note.archived { "-archived" } else { "" }
            ));
            let mut file = fs::File::create(&path)?;
            file.write_all(note.text.as_bytes())?;
            file.sync_all()?;
        }
        Ok(dir)
    }

    pub fn checkpoint(&self) -> Result<()> {
        self.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    fn temp() -> PathBuf {
        std::env::temp_dir().join(format!(
            "simplenotes-test-{}-{}-{}",
            std::process::id(),
            now(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn persists_unicode_geometry_settings_and_archive() {
        let dir = temp();
        {
            let s = Store::open(&dir).unwrap();
            let n = s.create("", 10, 20).unwrap();
            s.save_text(n.id, "Mañana: programación 🦀\r\n日本語")
                .unwrap();
            s.geometry(n.id, -900, 42, 345, 567).unwrap();
            s.set_setting("scale", "125").unwrap();
            s.archive(n.id, true).unwrap();
        }
        {
            let s = Store::open(&dir).unwrap();
            let n = s.notes().unwrap().remove(0);
            assert_eq!(n.text, "Mañana: programación 🦀\r\n日本語");
            assert_eq!((n.x, n.y, n.width, n.height), (-900, 42, 345, 567));
            assert!(n.archived);
            assert_eq!(s.setting("scale").unwrap().as_deref(), Some("125"));
            s.archive(n.id, false).unwrap();
            assert!(!s.notes().unwrap()[0].archived);
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn backup_is_independent_and_export_preserves_notes() {
        let dir = temp();
        {
            let s = Store::open(&dir).unwrap();
            let n = s
                .create("Texto importante con muchos detalles.", 60, 60)
                .unwrap();
            s.save_text(n.id, "Texto importante con muchos detalles. Más texto.")
                .unwrap();
            let backup = s.backup(true).unwrap().unwrap();
            s.save_text(n.id, "").unwrap();
            let b = Connection::open(backup).unwrap();
            let saved: String = b
                .query_row("SELECT text FROM notes", [], |r| r.get(0))
                .unwrap();
            assert_eq!(saved, "Texto importante con muchos detalles. Más texto.");
            let export = s.export().unwrap();
            assert!(export.join(format!("note-{}.txt", n.id)).exists());
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failed_transaction_does_not_replace_saved_text() {
        let dir = temp();
        {
            let s = Store::open(&dir).unwrap();
            let n = s.create("conservar", 10, 20).unwrap();
            s.conn.execute_batch("CREATE TRIGGER reject_update BEFORE UPDATE OF text ON notes BEGIN SELECT RAISE(ABORT, 'simulated disk failure'); END;").unwrap();
            assert!(s.save_text(n.id, "reemplazo").is_err());
            assert_eq!(s.notes().unwrap()[0].text, "conservar");
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn todo_menu_accepts_first_character_dash_without_space() {
        assert_eq!(
            bullet_items("-dsadsa\n-dasdsadsa\n-\n-"),
            Some(vec!["dsadsa".into(), "dasdsadsa".into()])
        );
        assert!(bullet_items("-\n- ").is_none());
        assert!(bullet_items(" - Uno").is_none());
        assert!(bullet_items("Texto normal\n- Uno\n- Dos\n- Tres").is_none());
        assert_eq!(
            bullet_items("- Ayudantía (Guia)\r\n - Sistemas Operativos\r\n - EuroDiessel"),
            Some(vec![
                "Ayudantía (Guia)".into(),
                "Sistemas Operativos".into(),
                "EuroDiessel".into()
            ])
        );
    }

    #[test]
    fn todo_conversion_drafts_statuses_and_archive_survive_restart() {
        let dir = temp();
        let id;
        {
            let s = Store::open(&dir).unwrap();
            id = s.create("- Uno\n- Dos\n- Tres", 20, 30).unwrap().id;
            s.convert_to_todo(id).unwrap();
            let items = s.todo_state(id).unwrap().items;
            assert_eq!(items.len(), 3);
            for item in &items {
                assert!(!s.cycle_todo_item(id, item.id).unwrap()); // red -> yellow
                let complete = s.cycle_todo_item(id, item.id).unwrap(); // yellow -> green
                assert_eq!(complete, item.id == items[2].id);
            }
            s.archive(id, true).unwrap();
            s.save_todo_draft(id, "Una tarea aún sin agregar 🦀")
                .unwrap();
        }
        {
            let s = Store::open(&dir).unwrap();
            let n = s.notes().unwrap().remove(0);
            assert!(n.todo && n.archived);
            let state = s.todo_state(id).unwrap();
            assert!(state.items.iter().all(|i| i.status == 2));
            assert_eq!(state.draft, "Una tarea aún sin agregar 🦀");
            s.commit_todo_draft(id).unwrap();
            let state = s.todo_state(id).unwrap();
            assert_eq!(state.items.len(), 4);
            assert_eq!(state.items[3].status, 0);
            s.edit_todo_item(id, state.items[1].id).unwrap();
            s.save_todo_draft(id, "Nombre corregido").unwrap();
        }
        {
            let s = Store::open(&dir).unwrap();
            let state = s.todo_state(id).unwrap();
            assert_eq!(state.editing, Some(state.items[1].id));
            s.commit_todo_draft(id).unwrap();
            assert_eq!(s.todo_state(id).unwrap().items[1].text, "Nombre corregido");
            let exported = s.export().unwrap();
            let content =
                fs::read_to_string(exported.join(format!("note-{id}-archived.txt"))).unwrap();
            assert!(content.contains("[Done] Nombre corregido"));
            assert!(content.contains("[Pending] Una tarea aún sin agregar 🦀"));
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failed_todo_conversion_keeps_plain_note_intact() {
        let dir = temp();
        {
            let s = Store::open(&dir).unwrap();
            let id = s.create("- Uno\n- Dos\n- Tres", 0, 0).unwrap().id;
            s.conn.execute_batch("CREATE TRIGGER reject_item BEFORE INSERT ON todo_items BEGIN SELECT RAISE(ABORT,'disk failure'); END;").unwrap();
            assert!(s.convert_to_todo(id).is_err());
            let note = s.notes().unwrap().remove(0);
            assert!(!note.todo);
            assert_eq!(note.text, "- Uno\n- Dos\n- Tres");
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn todo_can_be_disabled_and_enabled_without_losing_states_or_drafts() {
        let dir = temp();
        {
            let s = Store::open(&dir).unwrap();
            let id = s.create("-Primera\n-Segunda\n-", 0, 0).unwrap().id;
            s.convert_to_todo(id).unwrap();
            let item = s.todo_state(id).unwrap().items[0].id;
            s.cycle_todo_item(id, item).unwrap();
            s.save_todo_draft(id, "Tercera sin confirmar").unwrap();
            let content = s.disable_todo(id).unwrap();
            assert_eq!(content, "- Primera\r\n- Segunda\r\n- Tercera sin confirmar");
            assert!(!s.notes().unwrap()[0].todo);
            s.convert_to_todo(id).unwrap();
            let state = s.todo_state(id).unwrap();
            assert_eq!(state.items.len(), 3);
            assert_eq!(state.items[0].status, 1);
            s.edit_todo_item(id, item).unwrap();
            s.save_todo_draft(id, "Primera corregida").unwrap();
            s.disable_todo(id).unwrap();
            s.convert_to_todo(id).unwrap();
            let state = s.todo_state(id).unwrap();
            assert_eq!(state.items.len(), 3);
            assert_eq!(state.items[0].text, "Primera corregida");
            assert_eq!(state.items[0].status, 1);
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn upgrades_original_schema_without_changing_existing_notes() {
        let dir = temp();
        fs::create_dir_all(&dir).unwrap();
        {
            let old = Connection::open(dir.join("notes.db")).unwrap();
            old.execute_batch("CREATE TABLE notes(id INTEGER PRIMARY KEY,text TEXT NOT NULL,x INTEGER NOT NULL,y INTEGER NOT NULL,width INTEGER NOT NULL,height INTEGER NOT NULL,archived INTEGER NOT NULL,updated INTEGER NOT NULL); INSERT INTO notes VALUES(7,'Nota existente',50,60,280,260,0,0); CREATE TABLE revisions(id INTEGER PRIMARY KEY,note_id INTEGER,text TEXT,created INTEGER);").unwrap();
        }
        {
            let s = Store::open(&dir).unwrap();
            let note = s.notes().unwrap().remove(0);
            assert_eq!(note.text, "Nota existente");
            assert!(!note.todo);
            s.save_text(note.id, "Editada sin historial").unwrap();
            let count: i64 = s
                .conn
                .query_row("SELECT COUNT(*) FROM revisions", [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 0);
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn crash_writer_child() {
        if let Some(dir) = std::env::var_os("SIMPLENOTES_CRASH_TEST") {
            let s = Store::open(Path::new(&dir)).unwrap();
            let n = s.create("original", 60, 60).unwrap();
            s.save_text(n.id, "Persistir incluso sin cerrar la conexión 🦀")
                .unwrap();
            if std::env::var_os("SIMPLENOTES_WAIT_FOR_KILL").is_some() {
                if std::env::var_os("SIMPLENOTES_PENDING_WRITE").is_some() {
                    s.conn
                        .execute_batch(
                            "BEGIN IMMEDIATE; UPDATE notes SET text='Escritura incompleta';",
                        )
                        .unwrap();
                }
                fs::write(Path::new(&dir).join("ready"), b"ready").unwrap();
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(30));
                }
            }
            // ExitProcess closes the process without Rust/SQLite destructors or checkpoint.
            std::process::exit(93);
        }
    }

    #[test]
    fn committed_wal_survives_abrupt_process_exit() {
        let dir = temp();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "storage::tests::crash_writer_child",
                "--nocapture",
            ])
            .env("SIMPLENOTES_CRASH_TEST", &dir)
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(93));
        assert!(dir.join("notes.db-wal").exists());
        {
            let s = Store::open(&dir).unwrap();
            assert_eq!(
                s.notes().unwrap()[0].text,
                "Persistir incluso sin cerrar la conexión 🦀"
            );
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn forced_kill_preserves_commit_and_rolls_back_incomplete_write() {
        for pending_write in [false, true] {
            let dir = temp();
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "storage::tests::crash_writer_child",
                    "--nocapture",
                ])
                .env("SIMPLENOTES_CRASH_TEST", &dir)
                .env("SIMPLENOTES_WAIT_FOR_KILL", "1")
                .stdout(std::process::Stdio::null());
            if pending_write {
                command.env("SIMPLENOTES_PENDING_WRITE", "1");
            }
            let mut child = command.spawn().unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while !dir.join("ready").exists() && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            let ready = dir.join("ready").exists();
            // Child::kill uses TerminateProcess on Windows, as a force-quit does.
            child.kill().unwrap();
            child.wait().unwrap();
            assert!(
                ready,
                "The test child did not reach the requested write state"
            );
            {
                let s = Store::open(&dir).unwrap();
                assert_eq!(
                    s.notes().unwrap()[0].text,
                    "Persistir incluso sin cerrar la conexión 🦀"
                );
            }
            fs::remove_dir_all(dir).unwrap();
        }
    }
}
