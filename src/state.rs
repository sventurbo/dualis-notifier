//! The grade cache (`grades.csv`) and change detection.
//!
//! Modules that vanish from Dualis stay in the cache and only get a
//! "missing since" timestamp. If they reappear unchanged, nothing is reported.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::sync::LazyLock;

use anyhow::{Context, Result, bail};
use regex::Regex;

use crate::table::{Record, Table, column_names, normalise_value, value};

pub const LAST_SEEN_COLUMN: &str = "_notifier_last_seen";
pub const MISSING_SINCE_COLUMN: &str = "_notifier_missing_since";
const STATE_COLUMNS: [&str; 2] = [LAST_SEEN_COLUMN, MISSING_SINCE_COLUMN];
/// Prefer the Dualis module number and only fall back to a unique module name.
const IDENTIFIER_COLUMNS: [&str; 2] = ["Nr.", "Name"];
const NAME_COLUMN: &str = "Name";
const SUMMARY_ROW_NAME: &str = "Semester-GPA";

/// Cell texts pandas turned into NaN, which the Python version cached as "".
const NA_VALUES: [&str; 18] = [
    "#N/A", "#N/A N/A", "#NA", "-1.#IND", "-1.#QNAN", "-NaN", "-nan", "1.#IND", "1.#QNAN", "<NA>",
    "N/A", "NA", "NULL", "NaN", "None", "n/a", "nan", "null",
];

/// Numbers pandas' parser strips the thousands separator `,` from.
static THOUSANDS_NUMBER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[\-\+]?([0-9]+,|[0-9])*(\.[0-9]*)?([0-9]?(E|e)\-?[0-9]+)?$").expect("valid regex")
});
static INTEGER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[\-\+]?[0-9]+$").expect("valid regex"));
static FLOAT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[\-\+]?([0-9]+\.?[0-9]*|\.[0-9]+)([eE][\-\+]?[0-9]+)?$").expect("valid regex")
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    New,
    Changed,
}

impl EventKind {
    pub fn label(self) -> &'static str {
        match self {
            EventKind::New => "New",
            EventKind::Changed => "Changed",
        }
    }
}

/// One column that differs between the cache and the latest Dualis response.
/// `old` is `None` for a brand-new module, which has nothing to compare to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldChange {
    pub column: String,
    pub old: Option<String>,
    pub new: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub kind: EventKind,
    pub name: String,
    pub fields: Vec<FieldChange>,
}

/// The form in which two values are compared.
///
/// The Python version cached values after pandas' type inference: `,` was read
/// as a thousands separator (`1,7` became `17`), numbers were re-formatted
/// (`5.0` became `5`) and NA markers became empty. Comparing through the same
/// rules keeps those caches valid, while new caches store the original text.
pub fn comparison_key(raw: &str) -> String {
    let value = normalise_value(raw);
    if NA_VALUES.contains(&value) {
        return String::new();
    }
    let value = if value.contains(',') && THOUSANDS_NUMBER.is_match(value) {
        value.replace(',', "")
    } else {
        value.to_owned()
    };
    if INTEGER.is_match(&value)
        && let Ok(number) = value.parse::<i64>()
    {
        return number.to_string();
    }
    if FLOAT.is_match(&value)
        && let Ok(number) = value.parse::<f64>()
    {
        return format_g(number);
    }
    value
}

/// Python's `format(number, "g")`: six significant digits, no trailing zeros.
fn format_g(number: f64) -> String {
    if number == 0.0 {
        return if number.is_sign_negative() { "-0" } else { "0" }.to_owned();
    }
    let scientific = format!("{number:.5e}");
    let (mantissa, exponent) = scientific.split_once('e').expect("exponent format");
    let exponent: i32 = exponent.parse().expect("integer exponent");
    if (-4..6).contains(&exponent) {
        let decimals = (5 - exponent) as usize;
        trim_fraction(&format!("{number:.decimals$}")).to_owned()
    } else {
        let sign = if exponent < 0 { '-' } else { '+' };
        format!("{}e{sign}{:02}", trim_fraction(mantissa), exponent.abs())
    }
}

fn trim_fraction(number: &str) -> &str {
    if number.contains('.') {
        number.trim_end_matches('0').trim_end_matches('.')
    } else {
        number
    }
}

/// Remove summary rows and normalise every grade value before comparison.
pub fn prepare_grades(table: &Table) -> Table {
    let rows = table
        .rows
        .iter()
        .filter(|row| normalise_value(value(row, NAME_COLUMN)) != SUMMARY_ROW_NAME)
        .map(|row| {
            table
                .columns
                .iter()
                .map(|column| {
                    (
                        column.clone(),
                        normalise_value(value(row, column)).to_owned(),
                    )
                })
                .collect()
        })
        .collect();
    Table {
        columns: table.columns.clone(),
        rows,
    }
}

pub fn identifier_column(grades: &Table) -> Result<&'static str> {
    for column in IDENTIFIER_COLUMNS {
        if !grades.has_column(column) {
            continue;
        }
        let mut seen = HashSet::new();
        let usable = grades.rows.iter().all(|row| {
            let key = comparison_key(value(row, column));
            !key.is_empty() && seen.insert(key)
        });
        if usable {
            return Ok(column);
        }
    }
    bail!(
        "Keine eindeutige Modulkennung gefunden. Erwartet wird eine eindeutige Spalte \
         'Nr.' oder 'Name'."
    )
}

/// The state written on the very first run, without any notifications.
pub fn baseline(current: &Table, observed_at: &str) -> Result<Table> {
    let (state, _) = reconcile(&Table::default(), current, observed_at)?;
    Ok(state)
}

/// Merge the latest Dualis response into the cached state.
///
/// Returns the new state and one event per new module or real value change.
pub fn reconcile(
    previous: &Table,
    current: &Table,
    observed_at: &str,
) -> Result<(Table, Vec<Event>)> {
    let current = prepare_grades(current);
    let key_column = identifier_column(&current)?;

    // A cache without the identifier column cannot be matched; treat it as empty.
    let (previous_columns, previous_rows): (&[String], Vec<&Record>) =
        if previous.has_column(key_column) {
            let rows = previous
                .rows
                .iter()
                .filter(|row| !comparison_key(value(row, key_column)).is_empty())
                .collect();
            (&previous.columns, rows)
        } else {
            (&[], Vec::new())
        };

    let mut old_positions = HashMap::new();
    for (position, row) in previous_rows.iter().enumerate() {
        if old_positions
            .insert(comparison_key(value(row, key_column)), position)
            .is_some()
        {
            bail!("Der Noten-Cache enthält doppelte Modulkennungen.");
        }
    }
    let mut unmatched: Vec<Option<&Record>> = previous_rows.into_iter().map(Some).collect();

    let mut value_columns: Vec<String> = Vec::new();
    for column in current.columns.iter().chain(previous_columns) {
        if !STATE_COLUMNS.contains(&column.as_str()) && !value_columns.contains(column) {
            value_columns.push(column.clone());
        }
    }

    let mut events = Vec::new();
    let mut state_rows = Vec::new();

    for current_record in &current.rows {
        let module_id = value(current_record, key_column);
        let old_record = old_positions
            .get(&comparison_key(module_id))
            .and_then(|&position| unmatched[position].take());

        let mut record: Record = value_columns
            .iter()
            .map(|column| (column.clone(), value(current_record, column).to_owned()))
            .collect();
        record.insert(LAST_SEEN_COLUMN.to_owned(), observed_at.to_owned());
        record.insert(MISSING_SINCE_COLUMN.to_owned(), String::new());

        let reportable_columns = value_columns
            .iter()
            .filter(|column| column.as_str() != key_column && column.as_str() != NAME_COLUMN);

        let event_kind = match old_record {
            None => {
                let fields = reportable_columns
                    .filter_map(|column| {
                        let new = value(current_record, column);
                        (!new.is_empty()).then(|| FieldChange {
                            column: column.clone(),
                            old: None,
                            new: new.to_owned(),
                        })
                    })
                    .collect();
                Some((EventKind::New, fields))
            }
            Some(old_record) => {
                let fields: Vec<FieldChange> = reportable_columns
                    .filter_map(|column| {
                        let old = value(old_record, column);
                        let new = value(current_record, column);
                        (comparison_key(old) != comparison_key(new)).then(|| FieldChange {
                            column: column.clone(),
                            old: Some(old.to_owned()),
                            new: new.to_owned(),
                        })
                    })
                    .collect();
                (!fields.is_empty()).then_some((EventKind::Changed, fields))
            }
        };
        if let Some((kind, fields)) = event_kind {
            let name = match value(&record, NAME_COLUMN) {
                "" => module_id,
                name => name,
            };
            events.push(Event {
                kind,
                name: name.to_owned(),
                fields,
            });
        }

        state_rows.push(record);
    }

    for old_record in unmatched.into_iter().flatten() {
        let mut record: Record = value_columns
            .iter()
            .map(|column| {
                let old_value = normalise_value(value(old_record, column));
                (column.clone(), old_value.to_owned())
            })
            .collect();
        let last_seen = normalise_value(value(old_record, LAST_SEEN_COLUMN));
        let missing_since = match normalise_value(value(old_record, MISSING_SINCE_COLUMN)) {
            "" => observed_at,
            since => since,
        };
        record.insert(LAST_SEEN_COLUMN.to_owned(), last_seen.to_owned());
        record.insert(MISSING_SINCE_COLUMN.to_owned(), missing_since.to_owned());
        state_rows.push(record);
    }

    let mut columns = value_columns;
    columns.extend(STATE_COLUMNS.map(String::from));
    Ok((
        Table {
            columns,
            rows: state_rows,
        },
        events,
    ))
}

/// Load both legacy caches and the current stateful cache format.
pub fn load_cache(path: &Path) -> Result<Table> {
    let mut reader = csv::ReaderBuilder::new()
        .flexible(true)
        .from_path(path)
        .with_context(|| format!("Noten-Cache {} kann nicht gelesen werden", path.display()))?;
    let headers = column_names(reader.headers()?.iter().map(str::to_owned).collect());
    // Old versions wrote the pandas index, which reads back as "Unnamed: 0".
    let kept: Vec<(usize, String)> = headers
        .into_iter()
        .enumerate()
        .filter(|(_, name)| !name.starts_with("Unnamed"))
        .collect();

    let mut rows = Vec::new();
    for record in reader.records() {
        let record =
            record.with_context(|| format!("Noten-Cache {} ist beschädigt", path.display()))?;
        rows.push(
            kept.iter()
                .map(|(i, name)| (name.clone(), record.get(*i).unwrap_or("").to_owned()))
                .collect(),
        );
    }

    Ok(Table {
        columns: kept.into_iter().map(|(_, name)| name).collect(),
        rows,
    })
}

/// Write the cache atomically, so an interrupted run never leaves half a file.
pub fn save_cache(path: &Path, table: &Table) -> Result<()> {
    let temporary = path.with_extension("csv.tmp");
    let write = || -> Result<()> {
        let mut writer = csv::WriterBuilder::new()
            .terminator(csv::Terminator::Any(b'\n'))
            .from_path(&temporary)?;
        writer.write_record(&table.columns)?;
        for row in &table.rows {
            writer.write_record(table.columns.iter().map(|column| value(row, column)))?;
        }
        writer.flush()?;
        Ok(())
    };
    write()
        .and_then(|()| Ok(fs::rename(&temporary, path)?))
        .with_context(|| {
            format!(
                "Noten-Cache {} kann nicht geschrieben werden",
                path.display()
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::extract_table;

    const COLUMNS: [&str; 5] = ["Nr.", "Name", "Endnote", "Credits", "Status"];

    fn grades(rows: &[[&str; 5]]) -> Table {
        let rows: Vec<Vec<String>> = rows
            .iter()
            .map(|row| row.iter().map(|v| v.to_string()).collect())
            .collect();
        Table::from_rows(&COLUMNS, &rows)
    }

    fn initial() -> Table {
        grades(&[["M-101", "Mathematik", "1,7", "5", "bestanden"]])
    }

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    fn new_field(column: &str, new: &str) -> FieldChange {
        FieldChange {
            column: column.to_owned(),
            old: None,
            new: new.to_owned(),
        }
    }

    fn changed_field(column: &str, old: &str, new: &str) -> FieldChange {
        FieldChange {
            column: column.to_owned(),
            old: Some(old.to_owned()),
            new: new.to_owned(),
        }
    }

    fn new(name: &str, fields: Vec<FieldChange>) -> Event {
        Event {
            kind: EventKind::New,
            name: name.to_owned(),
            fields,
        }
    }

    fn changed(name: &str, fields: Vec<FieldChange>) -> Event {
        Event {
            kind: EventKind::Changed,
            name: name.to_owned(),
            fields,
        }
    }

    #[test]
    fn new_module_creates_new_event() {
        let (_, events) =
            reconcile(&Table::default(), &initial(), "2026-09-14T06:00:00+00:00").unwrap();

        assert_eq!(
            events,
            [new(
                "Mathematik",
                vec![
                    new_field("Endnote", "1,7"),
                    new_field("Credits", "5"),
                    new_field("Status", "bestanden"),
                ]
            )]
        );
    }

    #[test]
    fn changed_grade_creates_changed_event() {
        let (state, _) =
            reconcile(&Table::default(), &initial(), "2026-09-14T06:00:00+00:00").unwrap();
        let updated = grades(&[["M-101", "Mathematik", "1,3", "5", "bestanden"]]);

        let (_, events) = reconcile(&state, &updated, "2026-09-14T06:15:00+00:00").unwrap();

        assert_eq!(
            events,
            [changed(
                "Mathematik",
                vec![changed_field("Endnote", "1,7", "1,3")]
            )]
        );
    }

    #[test]
    fn missing_module_is_marked_without_notification() {
        let (state, _) =
            reconcile(&Table::default(), &initial(), "2026-09-14T06:00:00+00:00").unwrap();

        let (missing_state, events) =
            reconcile(&state, &grades(&[]), "2026-09-14T06:15:00+00:00").unwrap();

        assert_eq!(events, []);
        assert_eq!(
            value(&missing_state.rows[0], MISSING_SINCE_COLUMN),
            "2026-09-14T06:15:00+00:00"
        );
        assert_eq!(
            value(&missing_state.rows[0], LAST_SEEN_COLUMN),
            "2026-09-14T06:00:00+00:00"
        );
    }

    #[test]
    fn unchanged_module_reappearing_after_absence_is_silent() {
        let (state, _) =
            reconcile(&Table::default(), &initial(), "2026-09-14T06:00:00+00:00").unwrap();
        let (missing_state, _) =
            reconcile(&state, &grades(&[]), "2026-09-14T06:15:00+00:00").unwrap();

        let (restored_state, events) =
            reconcile(&missing_state, &initial(), "2026-09-14T06:30:00+00:00").unwrap();

        assert_eq!(events, []);
        assert_eq!(value(&restored_state.rows[0], MISSING_SINCE_COLUMN), "");
    }

    #[test]
    fn missing_since_keeps_first_timestamp() {
        let (state, _) = reconcile(&Table::default(), &initial(), "06:00").unwrap();
        let (state, _) = reconcile(&state, &grades(&[]), "06:15").unwrap();

        let (state, _) = reconcile(&state, &grades(&[]), "06:30").unwrap();

        assert_eq!(value(&state.rows[0], MISSING_SINCE_COLUMN), "06:15");
    }

    #[test]
    fn summary_row_is_ignored() {
        let current = grades(&[
            ["M-101", "Mathematik", "1,7", "5", "bestanden"],
            ["Semester-GPA", "Semester-GPA", "1,7", "5", ""],
        ]);

        let (state, events) = reconcile(&Table::default(), &current, "t").unwrap();

        assert_eq!(
            events,
            [new(
                "Mathematik",
                vec![
                    new_field("Endnote", "1,7"),
                    new_field("Credits", "5"),
                    new_field("Status", "bestanden"),
                ]
            )]
        );
        assert_eq!(state.rows.len(), 1);
    }

    #[test]
    fn falls_back_to_unique_name_without_module_number() {
        let current = Table::from_rows(&["Name", "Endnote"], &[vec!["Mathe".into(), "2,0".into()]]);

        assert_eq!(identifier_column(&current).unwrap(), "Name");
    }

    #[test]
    fn duplicate_identifiers_are_rejected() {
        let current = grades(&[
            ["M-101", "Mathe", "1,0", "5", ""],
            ["M-101", "Mathe", "2,0", "5", ""],
        ]);

        assert!(reconcile(&Table::default(), &current, "t").is_err());
    }

    #[test]
    fn comparison_key_matches_pandas_coercion() {
        // Expected values were produced by pandas 2.2.2 `read_html` plus the
        // Python version's `normalise_value`.
        let cases = [
            ("1,7", "17"),
            ("5,0", "50"),
            ("5", "5"),
            ("5.0", "5"),
            ("05", "5"),
            ("1,000", "1000"),
            ("12,500", "12500"),
            ("1.5", "1.5"),
            ("2.25", "2.25"),
            ("1.", "1"),
            ("1e3", "1000"),
            ("0.1", "0.1"),
            ("+4", "4"),
            ("-3", "-3"),
            ("n/a", ""),
            ("NA", ""),
            ("  bestanden ", "bestanden"),
            ("noch nicht gesetzt", "noch nicht gesetzt"),
            ("T3INF1001", "T3INF1001"),
        ];
        for (raw, expected) in cases {
            assert_eq!(comparison_key(raw), expected, "value {raw:?}");
        }
    }

    #[test]
    fn format_g_matches_python() {
        let cases = [
            (1_234_567.0, "1.23457e+06"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (123_456.0, "123456"),
            (2.5, "2.5"),
            (-0.0, "-0"),
        ];
        for (number, expected) in cases {
            assert_eq!(format_g(number), expected);
        }
    }

    #[test]
    fn caches_written_by_python_version_stay_valid() {
        let html = include_str!("../tests/fixtures/courseresults.html");
        let changed_html = include_str!("../tests/fixtures/courseresults_changed.html");
        let current = extract_table(html).unwrap();
        let changed_table = extract_table(changed_html).unwrap();

        for cache in ["legacy_grades_indexed.csv", "legacy_grades_stateful.csv"] {
            let previous = load_cache(&fixture(cache)).unwrap();
            assert!(!previous.has_column("Unnamed: 0"), "{cache}");

            let (_, events) = reconcile(&previous, &current, "t").unwrap();
            assert_eq!(events, [], "{cache}");

            let (_, events) = reconcile(&previous, &changed_table, "t").unwrap();
            assert_eq!(
                events,
                [changed(
                    "Theoretische Informatik I",
                    vec![changed_field("Endnote", "noch nicht gesetzt", "2,3")]
                )],
                "{cache}"
            );
        }
    }

    #[test]
    fn legacy_summary_row_becomes_a_silent_missing_module() {
        let previous = load_cache(&fixture("legacy_grades_indexed.csv")).unwrap();
        let current = extract_table(include_str!("../tests/fixtures/courseresults.html")).unwrap();

        let (state, _) = reconcile(&previous, &current, "t").unwrap();

        let summary = state.rows.last().unwrap();
        assert_eq!(value(summary, "Nr."), "Semester-GPA");
        assert_eq!(value(summary, MISSING_SINCE_COLUMN), "t");
    }

    #[test]
    fn cache_round_trip_keeps_original_values() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("grades.csv");
        let current = grades(&[["M-101", "Mathe, \"Teil 1\"", "1,7", "5", ""]]);
        let state = baseline(&current, "t").unwrap();

        save_cache(&path, &state).unwrap();

        assert_eq!(load_cache(&path).unwrap(), state);
        assert!(!path.with_extension("csv.tmp").exists());
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(
            text,
            "Nr.,Name,Endnote,Credits,Status,_notifier_last_seen,_notifier_missing_since\n\
             M-101,\"Mathe, \"\"Teil 1\"\"\",\"1,7\",5,,t,\n"
        );
    }
}
