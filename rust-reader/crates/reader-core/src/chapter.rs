//! Explicit source/chapter identity for imported books.
//!
//! A book is matched to a manga chapter by the ids recorded here, never by its
//! title. One book carries at most one link, and one chapter of one manga on
//! one source can point at only one book, so two downloaded copies of the same
//! chapter are refused instead of silently overwriting each other.

use std::cmp::Ordering;
use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::sort::natural_cmp;
use crate::util::now_millis;
use crate::{Library, db};

/// Chapter identity supplied by the source that produced the download.
///
/// `chapter_number` keeps decimal extras (`2.5`), and `volume_number` is the
/// only volume signal used for ordering. Both are optional: a chapter without a
/// number is ordered by `chapter_title` inside its volume.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChapterMetadata {
    pub source_id: String,
    pub manga_id: String,
    pub manga_title: String,
    pub chapter_id: String,
    pub chapter_title: String,
    #[serde(default)]
    pub chapter_number: Option<f64>,
    #[serde(default)]
    pub volume_number: Option<f64>,
}

/// One book's link to a source chapter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChapterLink {
    pub book_id: String,
    pub metadata: ChapterMetadata,
}

impl Library {
    /// Links one visible book to one source chapter.
    ///
    /// `source_id`, `manga_id` and `chapter_id` must be non-empty after
    /// trimming. Linking the same book again replaces its previous link, which
    /// is how a re-downloaded or re-identified chapter is corrected. If the
    /// same chapter is already linked to a *different* book, the call fails and
    /// names that book; nothing is overwritten or merged.
    ///
    /// Non-finite chapter or volume numbers are stored as "no number" so they
    /// cannot break ordering.
    pub fn link_chapter(&self, book_id: &str, metadata: &ChapterMetadata) -> Result<()> {
        let metadata = sanitized_metadata(metadata)?;
        let mut connection = self.lock_db()?;
        db::require_book(&connection, &self.root, book_id, false)?;

        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("start chapter link")?;
        let existing: Option<String> = transaction
            .query_row(
                "SELECT book_id FROM chapter_links
                 WHERE source_id = ?1 AND manga_id = ?2 AND chapter_id = ?3",
                params![
                    &metadata.source_id,
                    &metadata.manga_id,
                    &metadata.chapter_id
                ],
                |row| row.get(0),
            )
            .optional()
            .context("look up chapter link")?;
        if let Some(existing) = existing {
            if existing != book_id {
                bail!(
                    "chapter {}/{}/{} is already linked to book {existing}; refusing to overwrite it",
                    metadata.source_id,
                    metadata.manga_id,
                    metadata.chapter_id
                );
            }
        }

        transaction
            .execute(
                "INSERT INTO chapter_links(
                     book_id, source_id, manga_id, manga_title, chapter_id, chapter_title,
                     chapter_number, volume_number, linked_at
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(book_id) DO UPDATE SET
                     source_id = excluded.source_id,
                     manga_id = excluded.manga_id,
                     manga_title = excluded.manga_title,
                     chapter_id = excluded.chapter_id,
                     chapter_title = excluded.chapter_title,
                     chapter_number = excluded.chapter_number,
                     volume_number = excluded.volume_number,
                     linked_at = excluded.linked_at",
                params![
                    book_id,
                    &metadata.source_id,
                    &metadata.manga_id,
                    &metadata.manga_title,
                    &metadata.chapter_id,
                    &metadata.chapter_title,
                    metadata.chapter_number,
                    metadata.volume_number,
                    now_millis(),
                ],
            )
            .context("store chapter link")?;
        transaction.commit().context("commit chapter link")?;
        Ok(())
    }

    /// Lists chapter links for visible books in reading order.
    ///
    /// Links are ordered by source, then manga title (natural order), then
    /// volume and chapter number. A chapter without a volume belongs to the
    /// work's first explicit volume group; a work with no volume numbers at all
    /// is one unnumbered group. Numbered chapters come before unnumbered ones
    /// inside a group, and unnumbered chapters fall back to `chapter_title` in
    /// natural order. Decimal chapter numbers such as `2.5` keep their exact
    /// value. Hidden books are not listed; removing a book from the library
    /// never deletes its link.
    pub fn list_chapter_links(&self) -> Result<Vec<ChapterLink>> {
        let connection = self.lock_db()?;
        query_links(&connection)
    }
}

/// Loads every link of a visible book. Callers must already hold the
/// connection so this never nests the library mutex.
pub(crate) fn query_links(connection: &rusqlite::Connection) -> Result<Vec<ChapterLink>> {
    let mut statement = connection
        .prepare(
            "SELECT l.book_id, l.source_id, l.manga_id, l.manga_title, l.chapter_id,
                    l.chapter_title, l.chapter_number, l.volume_number
             FROM chapter_links l
             JOIN books b ON b.id = l.book_id
             WHERE b.removed = 0",
        )
        .context("prepare chapter link list")?;
    let links = statement
        .query_map([], |row| {
            Ok(ChapterLink {
                book_id: row.get(0)?,
                metadata: ChapterMetadata {
                    source_id: row.get(1)?,
                    manga_id: row.get(2)?,
                    manga_title: row.get(3)?,
                    chapter_id: row.get(4)?,
                    chapter_title: row.get(5)?,
                    chapter_number: sanitize_number(row.get(6)?),
                    volume_number: sanitize_number(row.get(7)?),
                },
            })
        })
        .context("query chapter links")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("read chapter links")?;
    Ok(ordered_links(links))
}

fn sanitize_number(value: Option<f64>) -> Option<f64> {
    value.filter(|number| number.is_finite())
}

fn sanitized_metadata(metadata: &ChapterMetadata) -> Result<ChapterMetadata> {
    let source_id = metadata.source_id.trim();
    let manga_id = metadata.manga_id.trim();
    let chapter_id = metadata.chapter_id.trim();
    if source_id.is_empty() || manga_id.is_empty() || chapter_id.is_empty() {
        bail!("chapter link needs non-empty source_id, manga_id and chapter_id");
    }
    Ok(ChapterMetadata {
        source_id: source_id.to_string(),
        manga_id: manga_id.to_string(),
        manga_title: metadata.manga_title.trim().to_string(),
        chapter_id: chapter_id.to_string(),
        chapter_title: metadata.chapter_title.trim().to_string(),
        chapter_number: sanitize_number(metadata.chapter_number),
        volume_number: sanitize_number(metadata.volume_number),
    })
}

/// Sorts links with keys computed once per work.
///
/// A chapter without a volume joins the work's first explicit volume group
/// (its smallest volume number); if the whole work has no volume numbers all
/// of its links share one unnumbered group. This keeps every work on a single
/// comparable key space, so the comparator stays transitive instead of mixing
/// `Some` and `None` mid-sequence.
fn ordered_links(links: Vec<ChapterLink>) -> Vec<ChapterLink> {
    let mut first_volume: HashMap<(String, String), f64> = HashMap::new();
    for link in &links {
        let Some(volume) = link.metadata.volume_number else {
            continue;
        };
        let key = (
            link.metadata.source_id.clone(),
            link.metadata.manga_id.clone(),
        );
        first_volume
            .entry(key)
            .and_modify(|current| {
                if volume < *current {
                    *current = volume;
                }
            })
            .or_insert(volume);
    }

    let mut keyed = links
        .into_iter()
        .map(|link| {
            let volume = link
                .metadata
                .volume_number
                .or_else(|| {
                    first_volume
                        .get(&(
                            link.metadata.source_id.clone(),
                            link.metadata.manga_id.clone(),
                        ))
                        .copied()
                })
                .unwrap_or(0.0);
            let chapter_missing = link.metadata.chapter_number.is_none();
            let chapter = link.metadata.chapter_number.unwrap_or(0.0);
            let key = ChapterSortKey {
                source_id: link.metadata.source_id.clone(),
                manga_title: link.metadata.manga_title.clone(),
                manga_id: link.metadata.manga_id.clone(),
                volume,
                chapter_missing,
                chapter,
                chapter_title: link.metadata.chapter_title.clone(),
                chapter_id: link.metadata.chapter_id.clone(),
            };
            (key, link)
        })
        .collect::<Vec<_>>();
    keyed.sort_by(|left, right| compare_keys(&left.0, &right.0));
    keyed.into_iter().map(|(_, link)| link).collect()
}

struct ChapterSortKey {
    source_id: String,
    manga_title: String,
    manga_id: String,
    volume: f64,
    chapter_missing: bool,
    chapter: f64,
    chapter_title: String,
    chapter_id: String,
}

fn compare_keys(left: &ChapterSortKey, right: &ChapterSortKey) -> Ordering {
    natural_cmp(&left.source_id, &right.source_id)
        .then_with(|| natural_cmp(&left.manga_title, &right.manga_title))
        .then_with(|| natural_cmp(&left.manga_id, &right.manga_id))
        .then_with(|| left.volume.total_cmp(&right.volume))
        .then_with(|| left.chapter_missing.cmp(&right.chapter_missing))
        .then_with(|| left.chapter.total_cmp(&right.chapter))
        .then_with(|| natural_cmp(&left.chapter_title, &right.chapter_title))
        .then_with(|| natural_cmp(&left.chapter_id, &right.chapter_id))
}

#[cfg(test)]
mod tests {
    use super::{ChapterLink, ChapterMetadata, ordered_links};

    fn link(
        manga: &str,
        chapter: &str,
        title: &str,
        number: Option<f64>,
        volume: Option<f64>,
    ) -> ChapterLink {
        ChapterLink {
            book_id: chapter.to_string(),
            metadata: ChapterMetadata {
                source_id: "source".to_string(),
                manga_id: manga.to_string(),
                manga_title: manga.to_string(),
                chapter_id: chapter.to_string(),
                chapter_title: title.to_string(),
                chapter_number: number,
                volume_number: volume,
            },
        }
    }

    #[test]
    fn decimal_chapters_sort_between_whole_chapters() {
        let links = ordered_links(vec![
            link("manga", "chapter-10", "第10话", Some(10.0), Some(1.0)),
            link("manga", "chapter-2.5", "第2.5话", Some(2.5), Some(1.0)),
            link("manga", "chapter-2", "第2话", Some(2.0), Some(1.0)),
            link("manga", "extra", "番外", None, Some(1.0)),
        ]);
        let order = links
            .iter()
            .map(|link| link.metadata.chapter_number)
            .collect::<Vec<_>>();
        assert_eq!(
            order,
            vec![Some(2.0), Some(2.5), Some(10.0), None],
            "numbered chapters come first and the unnumbered chapter falls back to its title"
        );
    }

    #[test]
    fn unnumbered_volume_joins_the_first_explicit_volume_group() {
        let links = ordered_links(vec![
            link("manga", "v2", "第1话", Some(1.0), Some(2.0)),
            link("manga", "loose-3", "第3话", Some(3.0), None),
            link("manga", "v1-2", "第2话", Some(2.0), Some(1.0)),
            link("manga", "loose-extra", "番外", None, None),
            link("manga", "v1-1", "第1话", Some(1.0), Some(1.0)),
        ]);
        let order = links
            .iter()
            .map(|link| link.metadata.chapter_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            order,
            vec!["v1-1", "v1-2", "loose-3", "loose-extra", "v2"],
            "volume-less chapters join volume 1 and sort by number, then title"
        );
    }

    #[test]
    fn a_work_without_any_volume_numbers_is_one_group() {
        let links = ordered_links(vec![
            link("manga", "c2", "第2话", Some(2.0), None),
            link("manga", "extra", "番外", None, None),
            link("manga", "c1", "第1话", Some(1.0), None),
        ]);
        let order = links
            .iter()
            .map(|link| link.metadata.chapter_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(order, vec!["c1", "c2", "extra"]);
    }
}
