use std::collections::{HashMap, HashSet};

/// One table row, keyed by column name.
pub type Record = HashMap<String, String>;

/// A table of string values with ordered column names.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Table {
    pub columns: Vec<String>,
    pub rows: Vec<Record>,
}

impl Table {
    pub fn has_column(&self, column: &str) -> bool {
        self.columns.iter().any(|c| c == column)
    }

    /// Build a table from positional rows; handy in tests and parsers.
    pub fn from_rows<S: AsRef<str>>(columns: &[S], rows: &[Vec<String>]) -> Self {
        let columns: Vec<String> = columns.iter().map(|c| c.as_ref().to_owned()).collect();
        let rows = rows
            .iter()
            .map(|values| {
                columns
                    .iter()
                    .cloned()
                    .zip(
                        values
                            .iter()
                            .cloned()
                            .chain(std::iter::repeat(String::new())),
                    )
                    .collect()
            })
            .collect();
        Self { columns, rows }
    }
}

/// Look up a value, treating absent columns as empty (like `dict.get(col, "")`).
pub fn value<'a>(record: &'a Record, column: &str) -> &'a str {
    record.get(column).map_or("", String::as_str)
}

/// Strip surrounding whitespace exactly like Python's `str.strip()`.
pub fn normalise_value(value: &str) -> &str {
    value.trim_matches(is_python_space)
}

/// Python's `str.isspace()` also treats the ASCII separators U+001C–U+001F as space.
pub fn is_python_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Name columns the way pandas does: empty headers become `Unnamed: <index>`
/// and duplicates are numbered.
pub fn column_names(raw: Vec<String>) -> Vec<String> {
    let named = raw
        .into_iter()
        .enumerate()
        .map(|(i, name)| {
            if name.is_empty() {
                format!("Unnamed: {i}")
            } else {
                name
            }
        })
        .collect();
    dedup_names(named)
}

/// Rename duplicate column names the way pandas does (`X`, `X.1`, `X.2`, …).
fn dedup_names(names: Vec<String>) -> Vec<String> {
    let original: HashSet<String> = names.iter().cloned().collect();
    let mut used = HashSet::new();
    names
        .into_iter()
        .map(|name| {
            let mut candidate = name.clone();
            let mut suffix = 1;
            // A suffixed name must not collide with a real column further on.
            while used.contains(&candidate) || (candidate != name && original.contains(&candidate))
            {
                candidate = format!("{name}.{suffix}");
                suffix += 1;
            }
            used.insert(candidate.clone());
            candidate
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_names_get_numeric_suffixes() {
        let names = ["A", "B", "A", "A"].map(String::from).to_vec();

        assert_eq!(dedup_names(names), ["A", "B", "A.1", "A.2"]);
    }

    #[test]
    fn suffix_skips_names_taken_by_real_columns() {
        let names = ["A", "A", "A.1"].map(String::from).to_vec();

        assert_eq!(dedup_names(names), ["A", "A.2", "A.1"]);
    }

    #[test]
    fn from_rows_pads_short_rows() {
        let table = Table::from_rows(&["A", "B"], &[vec!["1".into()]]);

        assert_eq!(value(&table.rows[0], "A"), "1");
        assert_eq!(value(&table.rows[0], "B"), "");
    }
}
