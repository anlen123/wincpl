use image::{codecs::png::PngEncoder, ImageBuffer, ImageEncoder, Rgba};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs::{self, OpenOptions},
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

const LIST_TEXT_CHARS: usize = 2_048;
const SUMMARY_CHARS: usize = 120;
const THUMBNAIL_EDGE: u32 = 160;
const MAX_ITEMS: usize = 5_000;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Entry {
    pub id: i64,
    pub kind: String,
    pub text: String,
    pub summary: String,
    pub image_path: Option<String>,
    pub thumbnail_path: Option<String>,
    pub created_at: i64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub ocr_status: String,
    pub ocr_error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Snippet {
    pub id: i64,
    pub title: String,
    pub content: String,
    pub created_at: i64,
    pub updated_at: i64,
}

fn snippet_from_row(row: &Row<'_>) -> rusqlite::Result<Snippet> {
    Ok(Snippet {
        id: row.get(0)?,
        title: row.get(1)?,
        content: row.get(2)?,
        created_at: row.get(3)?,
        updated_at: row.get(4)?,
    })
}

pub struct Store {
    conn: Connection,
    max_items: usize,
    images_dir: PathBuf,
    thumbnails_dir: PathBuf,
}

impl Store {
    pub fn open(dir: &Path, max_items: usize) -> Result<Self, String> {
        validate_max_items(max_items)?;
        fs::create_dir_all(dir)
            .map_err(|error| format!("无法创建数据目录 {}：{error}", dir.display()))?;
        let images_dir = dir.join("images");
        let thumbnails_dir = dir.join("thumbnails");
        fs::create_dir_all(&images_dir)
            .map_err(|error| format!("无法创建图片目录 {}：{error}", images_dir.display()))?;
        fs::create_dir_all(&thumbnails_dir)
            .map_err(|error| format!("无法创建缩略图目录 {}：{error}", thumbnails_dir.display()))?;

        let conn = Connection::open(dir.join("history.sqlite3"))
            .map_err(|error| format!("无法打开历史数据库：{error}"))?;
        conn.busy_timeout(std::time::Duration::from_secs(3))
            .map_err(|error| format!("无法配置数据库等待时间：{error}"))?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|error| format!("无法启用数据库 WAL：{error}"))?;
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(|error| format!("无法配置数据库同步模式：{error}"))?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(|error| format!("无法启用数据库外键：{error}"))?;
        conn.pragma_update(None, "temp_store", "MEMORY")
            .map_err(|error| format!("无法配置数据库临时存储：{error}"))?;
        conn.pragma_update(None, "cache_size", -1_024_i64)
            .map_err(|error| format!("无法配置数据库缓存：{error}"))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS entries (
                id             INTEGER PRIMARY KEY AUTOINCREMENT,
                kind           TEXT NOT NULL CHECK (kind IN ('text', 'image')),
                text           TEXT NOT NULL,
                summary        TEXT NOT NULL,
                image_path     TEXT,
                thumbnail_path TEXT,
                created_at     INTEGER NOT NULL,
                width          INTEGER,
                height         INTEGER,
                ocr_status     TEXT NOT NULL CHECK (ocr_status IN ('none', 'pending', 'ready', 'empty', 'error')),
                content_hash   TEXT NOT NULL UNIQUE
            );
            CREATE INDEX IF NOT EXISTS entries_newest ON entries(created_at DESC, id DESC);
            CREATE INDEX IF NOT EXISTS entries_pending ON entries(ocr_status, created_at, id);
            CREATE TABLE IF NOT EXISTS snippets (
                id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL UNIQUE,
                content TEXT NOT NULL, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
            );",
        )
        .map_err(|error| format!("无法初始化历史数据库：{error}"))?;
        let has_error_column = conn
            .prepare("PRAGMA table_info(entries)")
            .and_then(|mut statement| {
                let columns = statement.query_map([], |row| row.get::<_, String>(1))?;
                columns.collect::<Result<Vec<_>, _>>()
            })
            .map_err(|e| e.to_string())?
            .iter()
            .any(|name| name == "ocr_error");
        if !has_error_column {
            conn.execute_batch("BEGIN; ALTER TABLE entries ADD COLUMN ocr_error TEXT;
                UPDATE entries SET ocr_status = 'pending' WHERE kind = 'image' AND ocr_status = 'empty'; COMMIT;")
                .map_err(|e| format!("无法升级 OCR 历史：{e}"))?;
        }

        let mut store = Self {
            conn,
            max_items,
            images_dir,
            thumbnails_dir,
        };
        store.cleanup_orphans()?;
        store.prune()?;
        Ok(store)
    }

    pub fn list_snippets(&self, query: &str, limit: usize) -> Result<Vec<Snippet>, String> {
        let mut statement = self.conn.prepare(r"SELECT id,title,'',created_at,updated_at FROM snippets WHERE title LIKE ?1 ESCAPE '\' COLLATE NOCASE ORDER BY updated_at DESC,id DESC LIMIT ?2").map_err(|e| e.to_string())?;
        let rows = statement
            .query_map(
                params![literal_like_pattern(query), limit.clamp(1, 100) as i64],
                snippet_from_row,
            )
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }

    pub fn get_snippet(&self, id: i64) -> Result<Snippet, String> {
        self.conn
            .query_row(
                "SELECT id,title,content,created_at,updated_at FROM snippets WHERE id=?1",
                [id],
                snippet_from_row,
            )
            .optional()
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "片段不存在或已被删除".into())
    }

    pub fn upsert_snippet(
        &mut self,
        id: Option<i64>,
        title: &str,
        content: &str,
    ) -> Result<i64, String> {
        let title = title.trim();
        if title.is_empty() || title.chars().count() > 200 {
            return Err("片段标题需要 1 到 200 个字符".into());
        }
        if content.is_empty() || content.chars().count() > 100_000 || content.contains('\0') {
            return Err("片段内容需要 1 到 100000 个字符，不能含空字符".into());
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_secs() as i64;
        let newest: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(MAX(updated_at),0) FROM snippets",
                [],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        let timestamp = now.max(newest.saturating_add(1));
        let result = if let Some(id) = id {
            self.conn.execute(
                "UPDATE snippets SET title=?1,content=?2,updated_at=?3 WHERE id=?4",
                params![title, content, timestamp, id],
            )
        } else {
            self.conn.execute(
                "INSERT INTO snippets(title,content,created_at,updated_at) VALUES(?1,?2,?3,?3)",
                params![title, content, timestamp],
            )
        }
        .map_err(|e| format!("无法保存片段（标题不能重复）：{e}"))?;
        if result == 0 {
            return Err("片段不存在或已被删除".into());
        }
        Ok(id.unwrap_or_else(|| self.conn.last_insert_rowid()))
    }

    pub fn delete_snippet(&mut self, id: i64) -> Result<(), String> {
        self.conn
            .execute("DELETE FROM snippets WHERE id=?1", [id])
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    pub fn list(&self, query: &str, limit: usize) -> Result<Vec<Entry>, String> {
        let limit = limit.clamp(1, 100) as i64;
        let (sql, pattern) = if query.is_empty() {
            (
                "SELECT id, kind, text, summary, image_path, thumbnail_path, created_at, width, height, ocr_status, ocr_error
                 FROM entries ORDER BY created_at DESC, id DESC LIMIT ?1",
                None,
            )
        } else {
            (
                "SELECT id, kind, text, summary, image_path, thumbnail_path, created_at, width, height, ocr_status, ocr_error
                 FROM entries
                 WHERE text LIKE ?1 ESCAPE '\\' COLLATE NOCASE
                 ORDER BY created_at DESC, id DESC LIMIT ?2",
                Some(literal_like_pattern(query)),
            )
        };
        let mut statement = self
            .conn
            .prepare(sql)
            .map_err(|error| format!("无法准备历史查询：{error}"))?;
        let mut rows = match pattern.as_ref() {
            Some(pattern) => statement.query(params![pattern, limit]),
            None => statement.query(params![limit]),
        }
        .map_err(|error| format!("无法查询历史记录：{error}"))?;

        let mut entries = Vec::with_capacity(limit as usize);
        while let Some(row) = rows
            .next()
            .map_err(|error| format!("无法读取历史记录：{error}"))?
        {
            let mut entry =
                entry_from_row(row).map_err(|error| format!("历史记录内容损坏：{error}"))?;
            entry.text = truncate_chars(&entry.text, LIST_TEXT_CHARS);
            entries.push(entry);
        }
        Ok(entries)
    }

    pub fn get(&self, id: i64) -> Result<Entry, String> {
        self.conn
            .query_row(
                "SELECT id, kind, text, summary, image_path, thumbnail_path, created_at, width, height, ocr_status, ocr_error
                 FROM entries WHERE id = ?1",
                params![id],
                entry_from_row,
            )
            .optional()
            .map_err(|error| format!("无法读取历史记录：{error}"))?
            .ok_or_else(|| "记录不存在或已被删除".into())
    }

    pub fn pending_image(&self) -> Result<Option<Entry>, String> {
        self.conn
            .query_row(
                "SELECT id, kind, text, summary, image_path, thumbnail_path, created_at, width, height, ocr_status, ocr_error
                 FROM entries WHERE kind = 'image' AND ocr_status = 'pending'
                 ORDER BY created_at ASC, id ASC LIMIT 1",
                [],
                entry_from_row,
            )
            .optional()
            .map_err(|error| format!("无法读取待识别图片：{error}"))
    }

    pub fn insert_text(&mut self, text: &str) -> Result<i64, String> {
        if text.is_empty() {
            return Err("不能保存空文字".into());
        }
        let hash = content_hash(b"text\0", &[], text.as_bytes());
        let existing = self
            .conn
            .query_row(
                "SELECT id FROM entries WHERE content_hash = ?1",
                params![hash],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(|error| format!("无法检查重复文字：{error}"))?;
        let created_at = self.next_timestamp()?;
        if let Some(id) = existing {
            self.conn
                .execute(
                    "UPDATE entries SET created_at = ?1 WHERE id = ?2",
                    params![created_at, id],
                )
                .map_err(|error| format!("无法更新重复文字：{error}"))?;
            self.prune()?;
            return Ok(id);
        }

        let summary = summarize(text, SUMMARY_CHARS);
        self.conn
            .execute(
                "INSERT INTO entries
                 (kind, text, summary, image_path, thumbnail_path, created_at, width, height, ocr_status, content_hash)
                 VALUES ('text', ?1, ?2, NULL, NULL, ?3, NULL, NULL, 'none', ?4)",
                params![text, summary, created_at, hash],
            )
            .map_err(|error| format!("无法保存文字：{error}"))?;
        let id = self.conn.last_insert_rowid();
        self.prune()?;
        Ok(id)
    }

    pub fn insert_image(&mut self, rgba: &[u8], width: u32, height: u32) -> Result<i64, String> {
        validate_rgba(rgba, width, height)?;
        let mut dimensions = [0_u8; 8];
        dimensions[..4].copy_from_slice(&width.to_le_bytes());
        dimensions[4..].copy_from_slice(&height.to_le_bytes());
        let hash = content_hash(b"image\0", &dimensions, rgba);
        let existing = self
            .conn
            .query_row(
                "SELECT id FROM entries WHERE content_hash = ?1",
                params![hash],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(|error| format!("无法检查重复图片：{error}"))?;
        let (image_path, thumbnail_path, created_files) =
            self.persist_image(rgba, width, height, &hash)?;
        let image_path_text = image_path.to_string_lossy().into_owned();
        let thumbnail_path_text = thumbnail_path.to_string_lossy().into_owned();
        let created_at = self.next_timestamp()?;

        let database_result = if let Some(id) = existing {
            self.conn
                .execute(
                    "UPDATE entries
                     SET created_at = ?1, image_path = ?2, thumbnail_path = ?3, width = ?4, height = ?5
                     WHERE id = ?6",
                    params![
                        created_at,
                        image_path_text,
                        thumbnail_path_text,
                        width,
                        height,
                        id
                    ],
                )
                .map(|_| id)
                .map_err(|error| format!("无法更新重复图片：{error}"))
        } else {
            let summary = image_summary("", width, height);
            self.conn
                .execute(
                    "INSERT INTO entries
                     (kind, text, summary, image_path, thumbnail_path, created_at, width, height, ocr_status, content_hash)
                     VALUES ('image', '', ?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7)",
                    params![
                        summary,
                        image_path_text,
                        thumbnail_path_text,
                        created_at,
                        width,
                        height,
                        hash
                    ],
                )
                .map(|_| self.conn.last_insert_rowid())
                .map_err(|error| format!("无法保存图片记录：{error}"))
        };

        let id = match database_result {
            Ok(id) => id,
            Err(error) => {
                for path in created_files {
                    let _ = fs::remove_file(path);
                }
                return Err(error);
            }
        };
        self.prune()?;
        Ok(id)
    }

    pub fn touch(&mut self, id: i64) -> Result<(), String> {
        let timestamp = self.next_timestamp()?;
        let changed = self
            .conn
            .execute(
                "UPDATE entries SET created_at = ?1 WHERE id = ?2",
                params![timestamp, id],
            )
            .map_err(|e| e.to_string())?;
        if changed == 0 {
            return Err("记录不存在或已被删除".into());
        }
        Ok(())
    }

    pub fn retry_ocr(&mut self, id: i64) -> Result<(), String> {
        self.update_ocr(id, "", "pending", None)
    }

    pub fn update_ocr(
        &mut self,
        id: i64,
        text: &str,
        status: &str,
        error: Option<&str>,
    ) -> Result<(), String> {
        if !matches!(status, "pending" | "ready" | "empty" | "error") {
            return Err("OCR 状态无效".into());
        }
        let dimensions = self
            .conn
            .query_row(
                "SELECT width, height FROM entries WHERE id = ?1 AND kind = 'image'",
                params![id],
                |row| Ok((row.get::<_, u32>(0)?, row.get::<_, u32>(1)?)),
            )
            .optional()
            .map_err(|error| format!("无法读取图片尺寸：{error}"))?
            .ok_or_else(|| "图片记录不存在或已被删除".to_string())?;
        let summary = image_summary(text, dimensions.0, dimensions.1);
        let changed = self
            .conn
            .execute(
                "UPDATE entries SET text = ?1, summary = ?2, ocr_status = ?3, ocr_error = ?5 WHERE id = ?4 AND kind = 'image'",
                params![text, summary, status, id, if status == "error" { error } else { None }],
            )
            .map_err(|error| format!("无法更新 OCR 结果：{error}"))?;
        if changed == 0 {
            return Err("图片记录不存在或已被删除".into());
        }
        Ok(())
    }

    pub fn delete(&mut self, id: i64) -> Result<(), String> {
        let paths = self
            .conn
            .query_row(
                "SELECT image_path, thumbnail_path FROM entries WHERE id = ?1",
                params![id],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| format!("无法读取待删除记录：{error}"))?;
        let Some(paths) = paths else {
            return Ok(());
        };
        self.conn
            .execute("DELETE FROM entries WHERE id = ?1", params![id])
            .map_err(|error| format!("无法删除历史记录：{error}"))?;
        self.remove_entry_files(paths.0.as_deref(), paths.1.as_deref())
    }

    pub fn clear(&mut self) -> Result<(), String> {
        self.conn
            .execute("DELETE FROM entries", [])
            .map_err(|error| format!("无法清空历史记录：{error}"))?;
        cleanup_directory(&self.images_dir, &HashSet::new())?;
        cleanup_directory(&self.thumbnails_dir, &HashSet::new())
    }

    pub fn set_max_items(&mut self, max_items: usize) -> Result<(), String> {
        validate_max_items(max_items)?;
        self.max_items = max_items;
        self.prune()
    }

    fn next_timestamp(&self) -> Result<i64, String> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(i64::MAX as u128) as i64;
        let newest = self
            .conn
            .query_row(
                "SELECT COALESCE(MAX(created_at), 0) FROM entries",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| format!("无法读取历史时间：{error}"))?;
        Ok(now.max(newest.saturating_add(1)))
    }

    fn persist_image(
        &self,
        rgba: &[u8],
        width: u32,
        height: u32,
        hash: &str,
    ) -> Result<(PathBuf, PathBuf, Vec<PathBuf>), String> {
        let image_path = self.images_dir.join(format!("{hash}.png"));
        let thumbnail_path = self.thumbnails_dir.join(format!("{hash}.png"));
        let mut created = Vec::with_capacity(2);

        if write_png_if_missing(&image_path, rgba, width, height)? {
            created.push(image_path.clone());
        }

        let thumbnail_result = if width <= THUMBNAIL_EDGE && height <= THUMBNAIL_EDGE {
            write_png_if_missing(&thumbnail_path, rgba, width, height)
        } else if thumbnail_path.exists() {
            Ok(false)
        } else {
            let source = ImageBuffer::<Rgba<u8>, &[u8]>::from_raw(width, height, rgba)
                .ok_or_else(|| "图片像素长度与尺寸不一致".to_string())?;
            let thumbnail = image::imageops::thumbnail(&source, THUMBNAIL_EDGE, THUMBNAIL_EDGE);
            write_png_if_missing(
                &thumbnail_path,
                thumbnail.as_raw(),
                thumbnail.width(),
                thumbnail.height(),
            )
        };
        match thumbnail_result {
            Ok(true) => created.push(thumbnail_path.clone()),
            Ok(false) => {}
            Err(error) => {
                for path in &created {
                    let _ = fs::remove_file(path);
                }
                return Err(error);
            }
        }
        Ok((image_path, thumbnail_path, created))
    }

    fn prune(&mut self) -> Result<(), String> {
        let stale = {
            let mut statement = self
                .conn
                .prepare(
                    "SELECT id, image_path, thumbnail_path FROM entries
                     ORDER BY created_at DESC, id DESC LIMIT -1 OFFSET ?1",
                )
                .map_err(|error| format!("无法准备历史清理：{error}"))?;
            let rows = statement
                .query_map(params![self.max_items as i64], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                })
                .map_err(|error| format!("无法查询过期历史：{error}"))?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|error| format!("无法读取过期历史：{error}"))?
        };
        if stale.is_empty() {
            return Ok(());
        }

        let transaction = self
            .conn
            .transaction()
            .map_err(|error| format!("无法开始历史清理：{error}"))?;
        for (id, _, _) in &stale {
            transaction
                .execute("DELETE FROM entries WHERE id = ?1", params![id])
                .map_err(|error| format!("无法清理过期历史：{error}"))?;
        }
        transaction
            .commit()
            .map_err(|error| format!("无法提交历史清理：{error}"))?;
        for (_, image_path, thumbnail_path) in stale {
            self.remove_entry_files(image_path.as_deref(), thumbnail_path.as_deref())?;
        }
        Ok(())
    }

    fn remove_entry_files(
        &self,
        image_path: Option<&str>,
        thumbnail_path: Option<&str>,
    ) -> Result<(), String> {
        if let Some(path) = image_path {
            remove_managed_file(Path::new(path), &self.images_dir)?;
        }
        if let Some(path) = thumbnail_path {
            remove_managed_file(Path::new(path), &self.thumbnails_dir)?;
        }
        Ok(())
    }

    fn cleanup_orphans(&mut self) -> Result<(), String> {
        let broken_ids = {
            let mut statement = self
                .conn
                .prepare("SELECT id, image_path FROM entries WHERE kind = 'image'")
                .map_err(|error| format!("无法检查图片记录：{error}"))?;
            let rows = statement
                .query_map([], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?))
                })
                .map_err(|error| format!("无法查询图片记录：{error}"))?;
            let records = rows
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| format!("无法读取图片记录：{error}"))?;
            records
                .into_iter()
                .filter_map(|(id, path)| {
                    path.filter(|path| Path::new(path).is_file())
                        .map(|_| ())
                        .is_none()
                        .then_some(id)
                })
                .collect::<Vec<_>>()
        };
        if !broken_ids.is_empty() {
            let transaction = self
                .conn
                .transaction()
                .map_err(|error| format!("无法开始损坏记录清理：{error}"))?;
            for id in broken_ids {
                transaction
                    .execute("DELETE FROM entries WHERE id = ?1", params![id])
                    .map_err(|error| format!("无法清理缺失图片记录：{error}"))?;
            }
            transaction
                .commit()
                .map_err(|error| format!("无法提交损坏记录清理：{error}"))?;
        }

        let (images, thumbnails) = {
            let mut statement = self
                .conn
                .prepare("SELECT image_path, thumbnail_path FROM entries WHERE kind = 'image'")
                .map_err(|error| format!("无法准备图片文件清理：{error}"))?;
            let rows = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                    ))
                })
                .map_err(|error| format!("无法查询图片文件：{error}"))?;
            let mut images = HashSet::new();
            let mut thumbnails = HashSet::new();
            for row in rows {
                let (image, thumbnail) =
                    row.map_err(|error| format!("无法读取图片文件引用：{error}"))?;
                if let Some(path) = image {
                    images.insert(PathBuf::from(path));
                }
                if let Some(path) = thumbnail {
                    thumbnails.insert(PathBuf::from(path));
                }
            }
            (images, thumbnails)
        };
        cleanup_directory(&self.images_dir, &images)?;
        cleanup_directory(&self.thumbnails_dir, &thumbnails)
    }
}

fn entry_from_row(row: &Row<'_>) -> rusqlite::Result<Entry> {
    Ok(Entry {
        id: row.get(0)?,
        kind: row.get(1)?,
        text: row.get(2)?,
        summary: row.get(3)?,
        image_path: row.get(4)?,
        thumbnail_path: row.get(5)?,
        created_at: row.get(6)?,
        width: row.get(7)?,
        height: row.get(8)?,
        ocr_status: row.get(9)?,
        ocr_error: row.get(10)?,
    })
}

fn validate_max_items(max_items: usize) -> Result<(), String> {
    if !(1..=MAX_ITEMS).contains(&max_items) {
        return Err(format!("历史记录上限必须在 1 到 {MAX_ITEMS} 之间"));
    }
    Ok(())
}

fn validate_rgba(rgba: &[u8], width: u32, height: u32) -> Result<(), String> {
    if width == 0 || height == 0 {
        return Err("图片尺寸不能为零".into());
    }
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| "图片尺寸过大".to_string())?;
    if rgba.len() != expected {
        return Err(format!(
            "图片像素长度不正确：应为 {expected} 字节，实际为 {} 字节",
            rgba.len()
        ));
    }
    Ok(())
}

fn content_hash(prefix: &[u8], metadata: &[u8], content: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(prefix);
    digest.update(metadata);
    digest.update(content);
    let bytes = digest.finalize();
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn literal_like_pattern(query: &str) -> String {
    let mut pattern = String::with_capacity(query.len() + 2);
    pattern.push('%');
    for character in query.chars() {
        if matches!(character, '%' | '_' | '\\') {
            pattern.push('\\');
        }
        pattern.push(character);
    }
    pattern.push('%');
    pattern
}

fn summarize(text: &str, limit: usize) -> String {
    let mut summary = String::new();
    let mut whitespace = false;
    let mut count = 0;
    let mut truncated = false;
    for character in text.trim().chars() {
        if character.is_whitespace() {
            whitespace = !summary.is_empty();
            continue;
        }
        if whitespace {
            if count == limit {
                truncated = true;
                break;
            }
            summary.push(' ');
            count += 1;
            whitespace = false;
        }
        if count == limit {
            truncated = true;
            break;
        }
        summary.push(character);
        count += 1;
    }
    if truncated {
        summary.push('…');
    }
    summary
}

fn image_summary(text: &str, width: u32, height: u32) -> String {
    let summary = summarize(text, SUMMARY_CHARS);
    if summary.is_empty() {
        format!("图片 · {width}×{height}")
    } else {
        summary
    }
}

fn truncate_chars(text: &str, limit: usize) -> String {
    let mut chars = text.chars();
    let mut output: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() {
        output.push('…');
    }
    output
}

fn write_png_if_missing(path: &Path, rgba: &[u8], width: u32, height: u32) -> Result<bool, String> {
    atomic_write_if_missing(path, |file| {
        PngEncoder::new(file)
            .write_image(rgba, width, height, image::ExtendedColorType::Rgba8)
            .map_err(|error| format!("无法编码 PNG 图片：{error}"))
    })
}

fn atomic_write_if_missing(
    path: &Path,
    write: impl FnOnce(&mut fs::File) -> Result<(), String>,
) -> Result<bool, String> {
    if path.exists() {
        return Ok(false);
    }
    let parent = path
        .parent()
        .ok_or_else(|| "图片路径缺少父目录".to_string())?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "图片文件名无效".to_string())?;
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        sequence
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| format!("无法创建临时图片 {}：{error}", temporary.display()))?;
    if let Err(error) = write(&mut file) {
        drop(file);
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    if let Err(error) = file.sync_all() {
        drop(file);
        let _ = fs::remove_file(&temporary);
        return Err(format!("无法写入临时图片 {}：{error}", temporary.display()));
    }
    drop(file);
    match fs::rename(&temporary, path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == ErrorKind::AlreadyExists && path.exists() => {
            let _ = fs::remove_file(&temporary);
            Ok(false)
        }
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            Err(format!("无法原子写入图片 {}：{error}", path.display()))
        }
    }
}

fn remove_managed_file(path: &Path, managed_dir: &Path) -> Result<(), String> {
    if path.parent() != Some(managed_dir) {
        return Ok(());
    }
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("无法删除图片 {}：{error}", path.display())),
    }
}

fn cleanup_directory(dir: &Path, referenced: &HashSet<PathBuf>) -> Result<(), String> {
    let entries = fs::read_dir(dir)
        .map_err(|error| format!("无法扫描图片目录 {}：{error}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("无法读取图片目录项：{error}"))?;
        let path = entry.path();
        if entry
            .file_type()
            .map_err(|error| format!("无法读取图片文件类型 {}：{error}", path.display()))?
            .is_file()
            && !referenced.contains(&path)
        {
            fs::remove_file(&path)
                .map_err(|error| format!("无法清理孤立图片 {}：{error}", path.display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paste_and_copy_promote_one_record_searches_beyond_display_limit() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path(), 100).unwrap();
        let old = store.insert_text("隐藏的旧记录").unwrap();
        for index in 0..30 {
            store.insert_text(&format!("记录{index}")).unwrap();
        }
        assert!(!store.list("", 20).unwrap().iter().any(|e| e.id == old));
        assert_eq!(store.list("隐藏", 20).unwrap()[0].id, old);
        store.touch(old).unwrap();
        assert_eq!(store.list("", 10).unwrap()[0].id, old);
        assert_eq!(store.insert_text("隐藏的旧记录").unwrap(), old);
        assert_eq!(store.list("", 100).unwrap().len(), 31);
        let image = store.insert_image(&[0, 0, 0, 255], 1, 1).unwrap();
        store.touch(old).unwrap();
        assert_eq!(store.insert_image(&[0, 0, 0, 255], 1, 1).unwrap(), image);
        assert_eq!(store.list("", 10).unwrap()[0].id, image);
        assert_eq!(store.list("", 100).unwrap().len(), 32);
        store.delete(old).unwrap();
        assert!(store.touch(old).is_err());
        assert!(store.list("隐藏", 20).unwrap().is_empty());
    }

    #[test]
    fn old_empty_ocr_is_retried_once_and_errors_survive_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path(), 100).unwrap();
        let id = store.insert_image(&[255, 255, 255, 255], 1, 1).unwrap();
        store.update_ocr(id, "", "empty", None).unwrap();
        store
            .conn
            .execute_batch("ALTER TABLE entries DROP COLUMN ocr_error")
            .unwrap();
        drop(store);
        let mut store = Store::open(dir.path(), 100).unwrap();
        assert_eq!(store.pending_image().unwrap().unwrap().id, id);
        let timestamp = store.get(id).unwrap().created_at;
        store
            .update_ocr(id, "", "error", Some("语言包缺失"))
            .unwrap();
        drop(store);
        let mut store = Store::open(dir.path(), 100).unwrap();
        assert_eq!(
            store.get(id).unwrap().ocr_error.as_deref(),
            Some("语言包缺失")
        );
        store.retry_ocr(id).unwrap();
        assert_eq!(store.get(id).unwrap().created_at, timestamp);
        assert_eq!(store.get(id).unwrap().ocr_error, None);
        store.update_ocr(id, "", "empty", None).unwrap();
        drop(store);
        let store = Store::open(dir.path(), 100).unwrap();
        assert!(store.pending_image().unwrap().is_none());
    }

    #[test]
    fn literal_search_is_case_insensitive_and_dedup_moves_item_to_front() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let mut store = Store::open(directory.path(), 10).expect("open store");
        let first = store
            .insert_text("中文进度 100%_完成 Alpha")
            .expect("insert first");
        let second = store
            .insert_text("中文进度 100xx完成 alpha")
            .expect("insert second");

        let literal = store.list("100%_", 20).expect("literal search");
        assert_eq!(
            literal.iter().map(|entry| entry.id).collect::<Vec<_>>(),
            [first]
        );
        assert_eq!(store.list("ALPHA", 20).expect("case search").len(), 2);

        let duplicate = store
            .insert_text("中文进度 100%_完成 Alpha")
            .expect("deduplicate");
        assert_eq!(duplicate, first);
        let entries = store.list("", 20).expect("list history");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].id, first);
        assert_eq!(entries[1].id, second);
    }

    #[test]
    fn pruning_removes_database_rows_and_managed_image_files() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let mut store = Store::open(directory.path(), 2).expect("open store");
        let image_id = store
            .insert_image(&[255, 0, 0, 255, 0, 255, 0, 255], 2, 1)
            .expect("insert image");
        let image = store.get(image_id).expect("get image");
        let image_path = PathBuf::from(image.image_path.expect("image path"));
        let thumbnail_path = PathBuf::from(image.thumbnail_path.expect("thumbnail path"));
        assert!(image_path.is_file());
        assert!(thumbnail_path.is_file());

        store.insert_text("较新记录一").expect("insert text");
        store.insert_text("较新记录二").expect("insert text");
        assert!(store.get(image_id).is_err());
        assert!(!image_path.exists());
        assert!(!thumbnail_path.exists());
        assert_eq!(store.list("", 20).expect("list pruned").len(), 2);

        store.set_max_items(1).expect("shrink retention");
        assert_eq!(store.list("", 20).expect("list shrunk").len(), 1);
    }
}
