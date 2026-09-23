//! Extracts the Dualis results table.
//!
//! The Python version used `pandas.read_html` (lxml flavour) and cached what it
//! produced, so this module reproduces its rules: the first visible table with
//! text, `<thead>` or leading all-`<th>` rows as header, colspan/rowspan
//! expansion, pandas' whitespace clean-up and dropping of "Unnamed" columns.

use std::sync::LazyLock;

use anyhow::{Result, bail};
use regex::Regex;
use scraper::{ElementRef, Html, Node, Selector};

use crate::table::{Record, Table, column_names, normalise_value};

/// pandas' `_RE_WHITESPACE`: line breaks, or runs of two or more whitespace
/// characters, collapse to one space. A single NBSP inside a value survives.
static EXTRA_WHITESPACE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\r\n]+|[\s\x1C-\x1F]{2,}").expect("valid regex"));
static TABLE: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("table").expect("valid selector"));

/// Upper bound for colspan/rowspan so a broken page cannot exhaust memory.
const MAX_SPAN: usize = 1000;

type Rows<'a> = Vec<ElementRef<'a>>;

pub fn extract_table(html: &str) -> Result<Table> {
    let document = Html::parse_document(html);
    let Some(table) = document
        .select(&TABLE)
        .find(|table| !is_hidden(*table) && has_text(*table))
    else {
        bail!(
            "Keine Tabelle in der Dualis-Antwort gefunden – \
             ist der Login fehlgeschlagen oder hat sich die Seite geändert?"
        );
    };

    let (mut header_rows, mut body_rows, footer_rows) = collect_rows(table);
    if header_rows.is_empty() {
        // Tables without <thead>: leading rows made only of <th> are the header.
        let leading = body_rows
            .iter()
            .take_while(|row| cells(**row).all(|cell| cell.value().name() == "th"))
            .count();
        header_rows = body_rows.drain(..leading).collect();
    }
    body_rows.extend(footer_rows);

    let headers = expand_spans(&header_rows);
    let body = expand_spans(&body_rows);
    let header = match headers.as_slice() {
        [] => bail!("Die Dualis-Tabelle hat keine Kopfzeile."),
        [single] => single.clone(),
        // pandas would build a MultiIndex here; the last row carrying text is
        // the one with the actual column names.
        [.., _] => headers
            .iter()
            .rev()
            .find(|row| row.iter().any(|text| !text.is_empty()))
            .cloned()
            .unwrap_or_default(),
    };

    let width = headers.iter().chain(&body).map(Vec::len).max().unwrap_or(0);
    let names = column_names(pad(header, width));
    let kept: Vec<(usize, String)> = names
        .into_iter()
        .enumerate()
        .filter(|(_, name)| !name.to_lowercase().contains("unnamed"))
        .collect();

    let rows = body
        .into_iter()
        .map(|texts| pad(texts, width))
        .map(|texts| -> Record {
            kept.iter()
                .map(|(i, name)| (name.clone(), texts[*i].clone()))
                .collect()
        })
        // pandas kept empty spacer rows, which then broke the module lookup.
        .filter(|record| record.values().any(|text| !text.is_empty()))
        .collect();

    Ok(Table {
        columns: kept.into_iter().map(|(_, name)| name).collect(),
        rows,
    })
}

/// Rows of `<thead>`, of `<tbody>` (plus rows directly in `<table>`) and of `<tfoot>`.
fn collect_rows(table: ElementRef<'_>) -> (Rows<'_>, Rows<'_>, Rows<'_>) {
    let (mut header, mut body, mut footer) = (Vec::new(), Vec::new(), Vec::new());
    for section in visible_children(table) {
        match section.value().name() {
            "thead" => header.extend(visible_children(section).filter(is_row)),
            "tbody" => body.extend(visible_children(section).filter(is_row)),
            "tfoot" => footer.extend(visible_children(section).filter(is_row)),
            "tr" => body.push(section),
            _ => {}
        }
    }
    (header, body, footer)
}

fn is_row(element: &ElementRef<'_>) -> bool {
    element.value().name() == "tr"
}

fn cells(row: ElementRef<'_>) -> impl Iterator<Item = ElementRef<'_>> {
    visible_children(row).filter(|cell| matches!(cell.value().name(), "td" | "th"))
}

/// Child elements, minus those pandas drops: `<style>` and `display:none`.
fn visible_children(element: ElementRef<'_>) -> impl Iterator<Item = ElementRef<'_>> {
    element
        .children()
        .filter_map(ElementRef::wrap)
        .filter(|child| child.value().name() != "style" && !is_hidden(*child))
}

fn is_hidden(element: ElementRef<'_>) -> bool {
    element
        .value()
        .attr("style")
        .is_some_and(|style| style.replace(' ', "").contains("display:none"))
}

/// pandas only considers tables containing text that matches `.+`.
fn has_text(table: ElementRef<'_>) -> bool {
    table.text().any(|text| text.chars().any(|c| c != '\n'))
}

/// Port of pandas' `_expand_colspan_rowspan`.
fn expand_spans(rows: &[ElementRef<'_>]) -> Vec<Vec<String>> {
    let mut all_texts = Vec::new();
    // (column index, text, rows still to fill)
    let mut remainder: Vec<(usize, String, usize)> = Vec::new();

    for row in rows {
        let mut texts = Vec::new();
        let mut next_remainder = Vec::new();
        let mut pending = remainder.into_iter().peekable();
        let mut index = 0;

        for cell in cells(*row) {
            while let Some((prev_index, text, rowspan)) =
                pending.next_if(|(prev_index, _, _)| *prev_index <= index)
            {
                texts.push(text.clone());
                if rowspan > 1 {
                    next_remainder.push((prev_index, text, rowspan - 1));
                }
                index += 1;
            }

            let text = cell_text(cell);
            let rowspan = span(cell, "rowspan");
            for _ in 0..span(cell, "colspan") {
                texts.push(text.clone());
                if rowspan > 1 {
                    next_remainder.push((index, text.clone(), rowspan - 1));
                }
                index += 1;
            }
        }

        for (prev_index, text, rowspan) in pending {
            texts.push(text.clone());
            if rowspan > 1 {
                next_remainder.push((prev_index, text, rowspan - 1));
            }
        }

        all_texts.push(texts);
        remainder = next_remainder;
    }

    // Rows that only exist because a rowspan reaches past the last <tr>.
    while !remainder.is_empty() {
        let mut texts = Vec::new();
        let mut next_remainder = Vec::new();
        for (prev_index, text, rowspan) in remainder {
            texts.push(text.clone());
            if rowspan > 1 {
                next_remainder.push((prev_index, text, rowspan - 1));
            }
        }
        all_texts.push(texts);
        remainder = next_remainder;
    }

    all_texts
}

fn span(cell: ElementRef<'_>, attribute: &str) -> usize {
    cell.value()
        .attr(attribute)
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(1)
        .min(MAX_SPAN)
}

fn cell_text(cell: ElementRef<'_>) -> String {
    let mut text = String::new();
    collect_text(cell, &mut text);
    remove_whitespace(&text)
}

fn collect_text(element: ElementRef<'_>, out: &mut String) {
    for child in element.children() {
        if let Node::Text(text) = child.value() {
            out.push_str(text);
        } else if let Some(child) = ElementRef::wrap(child)
            && child.value().name() != "style"
            && !is_hidden(child)
        {
            collect_text(child, out);
        }
    }
}

/// Port of pandas' `_remove_whitespace`.
fn remove_whitespace(text: &str) -> String {
    normalise_value(&EXTRA_WHITESPACE.replace_all(text, " ")).to_owned()
}

fn pad(mut texts: Vec<String>, width: usize) -> Vec<String> {
    texts.resize(width, String::new());
    texts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::value;

    const FIXTURE: &str = include_str!("../tests/fixtures/courseresults.html");

    fn column(table: &Table, name: &str) -> Vec<String> {
        table
            .rows
            .iter()
            .map(|row| value(row, name).to_owned())
            .collect()
    }

    #[test]
    fn parses_dualis_results_page() {
        let table = extract_table(FIXTURE).unwrap();

        assert_eq!(
            table.columns,
            ["Nr.", "Name", "Endnote", "Credits", "Status"]
        );
        assert_eq!(
            column(&table, "Nr."),
            ["T3INF1001", "T3INF1002", "T3INF1003", "Semester-GPA"]
        );
        // Same strings pandas produced: a lone NBSP survives, runs collapse.
        assert_eq!(
            column(&table, "Name"),
            [
                "Mathematik\u{a0}I (WiSe 2023/24)",
                "Theoretische Informatik I",
                "Einführung in die Öffentlichkeitsarbeit – Grundlagen",
                "Semester-GPA",
            ]
        );
        assert_eq!(
            column(&table, "Endnote"),
            ["1,7", "noch nicht gesetzt", "2,0", "1,8"]
        );
        assert_eq!(column(&table, "Status"), ["bestanden", "", "bestanden", ""]);
    }

    #[test]
    fn whitespace_and_hidden_content_match_pandas() {
        // Expected values were produced by pandas 2.2.2 `read_html`.
        let html = "<table><tr><th>A</th><th>B</th><th>&nbsp;</th></tr>\
            <tr><td>x\ty</td><td>\n a\nb&nbsp; c&nbsp;</td><td>z</td></tr>\
            <tr><td>1<span style=\"display: none\">HIDDEN</span>2</td>\
            <td>q<style>.x{}</style>w</td><td>n</td></tr></table>";

        let table = extract_table(html).unwrap();

        assert_eq!(table.columns, ["A", "B"]);
        assert_eq!(column(&table, "A"), ["x\ty", "12"]);
        assert_eq!(column(&table, "B"), ["a b c", "qw"]);

        // A line break is replaced on its own, so the following space stays.
        let table = extract_table(
            "<table><tr><th>A</th><th>B</th></tr><tr><td>x\n y</td><td>p \n q</td></tr></table>",
        )
        .unwrap();
        assert_eq!(column(&table, "A"), ["x  y"]);
        assert_eq!(column(&table, "B"), ["p q"]);
    }

    #[test]
    fn expands_rowspan_and_colspan() {
        let html = "<table><thead><tr><th>A</th><th>B</th><th>C</th></tr></thead><tbody>\
            <tr><td rowspan=\"2\">r</td><td colspan=\"2\">c</td></tr>\
            <tr><td>b</td><td>x</td></tr></tbody></table>";

        let table = extract_table(html).unwrap();

        assert_eq!(column(&table, "A"), ["r", "r"]);
        assert_eq!(column(&table, "B"), ["c", "b"]);
        assert_eq!(column(&table, "C"), ["c", "x"]);
    }

    #[test]
    fn skips_hidden_and_empty_tables_and_blank_rows() {
        let html = "<table style=\"display:none\"><tr><th>Hidden</th></tr></table>\
            <table>\n</table>\
            <table><tr><th>Nr.</th><th>Name</th></tr>\
            <tr><td>1</td><td>A</td></tr><tr><td> </td><td></td></tr></table>";

        let table = extract_table(html).unwrap();

        assert_eq!(table.columns, ["Nr.", "Name"]);
        assert_eq!(table.rows.len(), 1);
    }

    #[test]
    fn page_without_table_is_an_error() {
        let error = extract_table("<html><body>Sitzung abgelaufen</body></html>").unwrap_err();

        assert!(error.to_string().contains("Keine Tabelle"));
    }
}
