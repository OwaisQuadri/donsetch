//! Column-consensus table detection.
//!
//! Real documents (reports, specs, invoices, w-9-like forms, API
//! tables) set rows as aligned runs without ruling lines. We detect
//! them geometrically: candidate rows have ≥2 "cells" split by wide
//! whitespace; a column template is the consensus of cell-gap midpoint
//! positions across rows; when ≥3 rows fit the template we emit a
//! markdown table. Otherwise the run stays prose — never garbage.
//!
//! Degradation rule: if template consensus is weak or words would
//! straddle column cuts too often, return None and let the caller
//! render the lines as a paragraph.

use super::layout::{Line, Word};
use crate::extract::blocks::Block;

/// (start, len, block) over a slice of lines.
pub struct Found {
    pub start: usize,
    pub len: usize,
    pub block: Block,
}

/// Detect tables among `lines[start..end]`. Returned blocks replace
/// those line ranges.
pub fn detect(lines: &[Line]) -> Vec<Found> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if is_row_candidate(&lines[i]) {
            // Grow the run.
            let mut j = i + 1;
            while j < lines.len() && is_row_candidate(&lines[j]) && same_band(&lines[i], &lines[j])
            {
                j += 1;
            }
            let run_len = j - i;
            if run_len >= 3 {
                if let Some(block) = build_table(&lines[i..j]) {
                    out.push(Found { start: i, len: run_len, block });
                }
            }
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

/// A row candidate: multiple words with at least one wide interior gap.
fn is_row_candidate(l: &Line) -> bool {
    if l.words.len() < 2 || l.mono {
        return false;
    }
    let thresh = 0.8 * l.size; // wide whitespace needs ~2+ spaces
    l.words.windows(2).any(|w| w[1].x0 - w[0].x1 > thresh)
}

fn same_band(a: &Line, b: &Line) -> bool {
    (a.size - b.size).abs() <= 0.5 && a.page == b.page
}

/// Build the table block for a candidate run, or None to degrade.
fn build_table(run: &[Line]) -> Option<Block> {
    // Gather all gap midpoints across the run.
    let mut mids: Vec<f32> = Vec::new();
    for l in run {
        for w in l.words.windows(2) {
            let g = w[1].x0 - w[0].x1;
            if g > 0.8 * l.size {
                mids.push((w[0].x1 + w[1].x0) * 0.5);
            }
        }
    }
    if mids.is_empty() {
        return None;
    }
    mids.sort_by(|a, b| a.total_cmp(b));

    // Cluster midpoints: two rows agree on a cut when within tolerance.
    let tol = 1.2 * run.iter().map(|l| l.size).sum::<f32>() / run.len() as f32 * 0.9;
    let mut clusters: Vec<(f32, usize)> = Vec::new(); // (center x, votes)
    for m in mids {
        match clusters.last_mut() {
            Some((c, n)) if (m - *c).abs() <= tol => {
                *c = (*c * *n as f32 + m) / (*n as f32 + 1.0);
                *n += 1;
            }
            _ => clusters.push((m, 1)),
        }
    }
    // Consensus: a cut must appear in at least half the rows.
    let need = (run.len() as f32 * 0.5).ceil() as usize;
    let cuts: Vec<f32> = clusters
        .into_iter()
        .filter(|(_, n)| *n >= need)
        .map(|(c, _)| c)
        .collect();
    if cuts.is_empty() {
        return None;
    }
    let ncols = cuts.len() + 1;

    // Assign words to cells.
    let mut rows: Vec<Vec<String>> = Vec::with_capacity(run.len());
    let mut straddle = 0usize;
    for l in run {
        let mut cells = vec![String::new(); ncols];
        for w in &l.words {
            let col = word_col(w, &cuts);
            // A word whose x-range crosses a cut edge is suspicious for
            // a true table (midpoint alone would hide the overlap).
            if cuts.iter().any(|&c| w.x0 < c - 0.5 && w.x1 > c + 0.5) {
                straddle += 1;
            }
            if !cells[col].is_empty() {
                cells[col].push(' ');
            }
            cells[col].push_str(&w.text);
        }
        // Drop fully-empty rows (spacing artifacts at caption distances).
        if cells.iter().any(|c| !c.trim().is_empty()) {
            rows.push(cells);
        }
    }
    if rows.len() < 3 {
        return None;
    }
    // Too many straddles → this isn't a clean table.
    let total_words: usize = run.iter().map(|l| l.words.len()).sum();
    if straddle * 3 > total_words.max(1) {
        return None;
    }

    let headers = rows.remove(0);
    Some(Block::Table { headers, rows, truncated: false, path: Vec::new() })
}

fn word_col(w: &Word, cuts: &[f32]) -> usize {
    let mid = (w.x0 + w.x1) * 0.5;
    cuts.iter().position(|&c| mid < c).unwrap_or(cuts.len())
}
