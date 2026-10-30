//! Reading the tables of an LDBC SNB `.tar.zst` archive without unpacking it.

use crate::schema::{self, PERSON_LISTS, PersonLists};
use std::io::{BufRead, BufReader};
use std::path::Path;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Calls `f` for every table file in the archive with its path below the top folder, its table
/// name and a reader positioned at its header line. Checksum files and update streams are skipped.
pub fn each_table(
    archive: &Path,
    mut f: impl FnMut(&str, &str, &mut dyn BufRead) -> Result<()>,
) -> Result<()> {
    let mut ar = tar::Archive::new(zstd::Decoder::new(std::fs::File::open(archive)?)?);
    for entry in ar.entries()? {
        let entry = entry?;
        let path = entry.path()?.to_string_lossy().replace('\\', "/");
        let file_name = path.rsplit('/').next().unwrap_or_default();
        if file_name.starts_with('.') {
            continue;
        }
        let Some(table) = schema::table_of(file_name) else {
            continue;
        };
        let rel = path.split_once('/').map_or(path.as_str(), |(_, r)| r);
        f(rel, table, &mut BufReader::with_capacity(1 << 20, entry))?;
    }
    Ok(())
}

/// The header's column names and an iterator over the rows' cells.
pub fn rows(
    r: &mut dyn BufRead,
) -> Result<(Vec<String>, impl Iterator<Item = Result<Vec<String>>>)> {
    let mut lines = r.lines();
    let header = lines.next().ok_or("empty table file")??;
    let header: Vec<String> = header.split('|').map(str::to_owned).collect();
    let n = header.len();
    let rows = lines.map(move |l| {
        let l = l?;
        let cells: Vec<String> = l.split('|').map(str::to_owned).collect();
        if cells.len() != n {
            return Err(format!("{} cells, header has {n}: {l}", cells.len()).into());
        }
        Ok(cells)
    });
    Ok((header, rows))
}

pub fn col(header: &[String], name: &str) -> Result<usize> {
    header
        .iter()
        .position(|h| h == name)
        .ok_or_else(|| format!("no column {name} in {header:?}").into())
}

/// The tables that become list properties of persons, read in a pass of their own because the
/// archive's file order is not fixed.
pub fn person_lists(archive: &Path) -> Result<PersonLists> {
    let mut lists = PersonLists::new();
    each_table(archive, |_, table, r| {
        let Some(&(t, _)) = PERSON_LISTS.iter().find(|(t, _)| *t == table) else {
            return Ok(());
        };
        let (_, rows) = rows(r)?;
        let map = lists.entry(t).or_default();
        for row in rows {
            let row = row?;
            map.entry(row[0].parse()?).or_default().push(row[1].clone());
        }
        Ok(())
    })?;
    Ok(lists)
}
