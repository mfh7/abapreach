//! The text of a captured ALV grid: tab-separated lines, one header line and one line per row.
//!
//! The runner reads the grid, writes each cell as the report user would see it (an amount with the
//! decimals of its currency, a quantity with those of its unit), and sends every column of the
//! field catalog. This module decides which columns print and cleans the values, so the choice and
//! the layout are tested without a SAP system.

use crate::cli::AlvColumns;
use crate::runner_protocol::{Alv, AlvColumn};

/// The lines of the grid: a header of the column texts, then the rows. By default only the columns
/// the field catalog shows (`TECH` and `NO_OUT` are hidden), in catalog order; `All` prints every
/// column. A grid whose columns are all hidden prints nothing.
pub fn format_alv(alv: &Alv, columns: AlvColumns) -> Vec<String> {
    let shown: Vec<usize> = alv
        .columns
        .iter()
        .enumerate()
        .filter(|(_, column)| columns == AlvColumns::All || column.visible)
        .map(|(index, _)| index)
        .collect();

    if shown.is_empty() {
        return Vec::new();
    }

    let mut lines = vec![join(shown.iter().map(|index| header(&alv.columns[*index])))];

    for row in &alv.rows {
        lines.push(join(shown.iter().map(|index| clean(&row[*index]))));
    }

    lines
}

/// The column text, or its name when the catalog has no text.
fn header(column: &AlvColumn) -> String {
    let text = clean(&column.text);

    if text.trim().is_empty() {
        clean(&column.name)
    } else {
        text
    }
}

fn join(cells: impl Iterator<Item = String>) -> String {
    cells.collect::<Vec<_>>().join("\t")
}

/// A value on one line: a tab, a line break, or any other control character becomes a blank, so
/// a value never moves a column or starts a row.
fn clean(value: &str) -> String {
    value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}
