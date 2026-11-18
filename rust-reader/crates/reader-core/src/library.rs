use anyhow::{Context, Result, bail};
use rusqlite::{OptionalExtension, params};

use crate::book::{Book, sanitize_fraction};
use crate::{DEFAULT_CATEGORY, Library, db};

impl Library {
    /// Lists visible books, newest read first, then newest update, then title.
    ///
    /// A non-empty `query` matches title, author or category with a
    /// case-insensitive SQLite `LIKE` pattern. `%`, `_` and `\` are escaped.
    pub fn list_books(&self, query: &str) -> Result<Vec<Book>> {
        let connection = self.lock_db()?;
        let trimmed = query.trim();
        let pattern = like_pattern(trimmed);
        let mut statement = connection
            .prepare(&format!(
                "SELECT {columns}
                 FROM books
                 WHERE removed = 0
                   AND (?1 = ''
                        OR title LIKE ?2 ESCAPE '\\'
                        OR author LIKE ?2 ESCAPE '\\'
                        OR category LIKE ?2 ESCAPE '\\')
                 ORDER BY last_read_at IS NULL ASC,
                          last_read_at DESC,
                          updated_at DESC,
                          title COLLATE NOCASE ASC",
                columns = db::BOOK_COLUMNS
            ))
            .context("prepare library list")?;
        let books = statement
            .query_map(params![trimmed, pattern], db::stored_from_row)
            .context("query library")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("read library rows")?
            .into_iter()
            .map(|stored| stored.book)
            .collect();
        Ok(books)
    }

    /// Returns one visible book.
    pub fn get_book(&self, book_id: &str) -> Result<Book> {
        let connection = self.lock_db()?;
        let book = connection
            .query_row(
                &format!(
                    "SELECT {columns} FROM books WHERE id = ?1 AND removed = 0",
                    columns = db::BOOK_COLUMNS
                ),
                params![book_id],
                db::stored_from_row,
            )
            .optional()
            .context("read book")?
            .ok_or_else(|| anyhow::anyhow!("book not found: {book_id}"))?;
        Ok(book.book)
    }

    /// Saves reading progress at the top of a page.
    ///
    /// This keeps the original contract and stores `page_fraction = 0.0`.
    /// Callers that track continuous scrolling use [`Library::save_position`].
    pub fn save_progress(&self, book_id: &str, page: usize, private: bool) -> Result<()> {
        self.save_position(book_id, page, 0.0, private)
    }

    /// Saves reading progress, including the position inside the page.
    ///
    /// `page` is zero-based and may move backwards. `page_fraction` is the top
    /// edge of the viewport relative to the full image height: `0.0` is the
    /// top of the page, `1.0` is the bottom. Finite values outside that range
    /// are clamped, because continuous scrolling can overshoot by a rounding
    /// error; `NaN` and infinite values are rejected.
    ///
    /// When `private` is true the call validates the book, page and fraction
    /// but writes nothing: incognito reading must not change the saved page,
    /// fraction or `last_read_at`.
    pub fn save_position(
        &self,
        book_id: &str,
        page: usize,
        page_fraction: f32,
        private: bool,
    ) -> Result<()> {
        if !page_fraction.is_finite() {
            bail!("page_fraction must be a finite number");
        }
        let page_fraction = sanitize_fraction(page_fraction);
        let connection = self.lock_db()?;
        let stored = db::require_book(&connection, book_id, false)?;
        if page >= stored.book.page_count {
            bail!(
                "page {page} is out of range for a {}-page book",
                stored.book.page_count
            );
        }
        if private {
            return Ok(());
        }
        connection
            .execute(
                "UPDATE books
                 SET current_page = ?1, page_fraction = ?2, last_read_at = ?3,
                     updated_at = ?3, progress_at = ?3
                 WHERE id = ?4 AND removed = 0",
                params![
                    page as i64,
                    page_fraction as f64,
                    crate::util::now_millis(),
                    book_id
                ],
            )
            .context("save reading progress")?;
        Ok(())
    }

    /// Moves a book to a category. An empty name stores the default category.
    pub fn set_category(&self, book_id: &str, category: &str) -> Result<()> {
        let category = category.trim();
        let category = if category.is_empty() {
            DEFAULT_CATEGORY
        } else {
            category
        };
        let connection = self.lock_db()?;
        let changed = connection
            .execute(
                "UPDATE books SET category = ?1, updated_at = ?2
                 WHERE id = ?3 AND removed = 0",
                params![category, crate::util::now_millis(), book_id],
            )
            .context("set book category")?;
        if changed == 0 {
            bail!("book not found: {book_id}");
        }
        Ok(())
    }

    /// Hides a book from the library without deleting its archive copy.
    ///
    /// Importing the same content again restores the row, keeping its saved
    /// progress and category.
    pub fn remove_from_library(&self, book_id: &str) -> Result<()> {
        let connection = self.lock_db()?;
        connection
            .execute(
                "UPDATE books SET removed = 1, updated_at = ?1
                 WHERE id = ?2 AND removed = 0",
                params![crate::util::now_millis(), book_id],
            )
            .context("remove book from library")?;
        Ok(())
    }

    /// Stores a string setting.
    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        let connection = self.lock_db()?;
        connection
            .execute(
                "INSERT INTO settings(key, value) VALUES(?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .context("store setting")?;
        Ok(())
    }

    /// Reads a string setting. Missing keys return `None`.
    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        let connection = self.lock_db()?;
        connection
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()
            .context("read setting")
    }
}

fn like_pattern(query: &str) -> String {
    let mut pattern = String::with_capacity(query.len() + 2);
    pattern.push('%');
    for character in query.chars() {
        match character {
            '\\' | '%' | '_' => {
                pattern.push('\\');
                pattern.push(character);
            }
            other => pattern.push(other),
        }
    }
    pattern.push('%');
    pattern
}

#[cfg(test)]
mod tests {
    use super::like_pattern;

    #[test]
    fn search_pattern_escapes_sql_wildcards() {
        assert_eq!(like_pattern("a%b_c\\d"), "%a\\%b\\_c\\\\d%");
    }
}
