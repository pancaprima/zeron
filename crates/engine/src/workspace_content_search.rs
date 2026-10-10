//! Bounded workspace content search (literal and fzf-style fuzzy line matching).

use std::cmp::Ordering;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::time::Instant;

use ignore::WalkBuilder;
use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32String};
use zeron_proto::{
    SearchWorkspaceContentResponse, WorkspaceContentHighlightRange, WorkspaceContentMatchMode,
    WorkspaceContentSearchCompletion, WorkspaceContentSearchIncompleteReason,
    WorkspaceContentSearchMatch,
};

use crate::workspace_files::{
    MAX_PREVIEW_FILE_BYTES, MAX_SEARCH_RESULTS, WorkspaceFilesError, WorkspaceRelativePath,
    contains_git_component, is_internal_temp_wire_path, metadata_for_workspace_file, path_to_wire,
    validate_workspace_search_query,
};

pub const CONTENT_SEARCH_MAX_BYTES_PER_FILE: u64 = MAX_PREVIEW_FILE_BYTES;
pub const CONTENT_SEARCH_MAX_TOTAL_BYTES_SCANNED: u64 = 64 * 1024 * 1024;
pub const CONTENT_SEARCH_MAX_LINE_BYTES: usize = 64 * 1024;
pub const CONTENT_SEARCH_PREVIEW_MAX_CHARS: usize = 160;
pub const CONTENT_SEARCH_MAX_MATCHES_PER_LINE: usize = 8;
pub const CONTENT_SEARCH_MAX_MATCH_TEXT_CHARS: usize = 256;
pub const CONTENT_SEARCH_MAX_HIGHLIGHT_RANGES: usize = 64;
pub const CONTENT_SEARCH_SCAN_DEADLINE: std::time::Duration =
    std::time::Duration::from_millis(4_500);
const READ_CHUNK_BYTES: usize = 64 * 1024;

struct ScanBudget {
    started: Instant,
    bytes_scanned: u64,
    max_bytes: u64,
    deadline: std::time::Duration,
}

impl ScanBudget {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            bytes_scanned: 0,
            max_bytes: CONTENT_SEARCH_MAX_TOTAL_BYTES_SCANNED,
            deadline: CONTENT_SEARCH_SCAN_DEADLINE,
        }
    }

    fn record(&mut self, bytes: u64) -> bool {
        self.bytes_scanned += bytes;
        self.bytes_scanned <= self.max_bytes && self.started.elapsed() <= self.deadline
    }

    fn exhausted(&self) -> bool {
        self.bytes_scanned >= self.max_bytes || self.started.elapsed() > self.deadline
    }
}

pub fn search_workspace_content_blocking(
    root: &Path,
    query: &str,
    mode: WorkspaceContentMatchMode,
    include_ignored: bool,
    limit: usize,
    cancel: &AtomicBool,
) -> Result<SearchWorkspaceContentResponse, WorkspaceFilesError> {
    validate_workspace_search_query(query)?;
    let limit = limit.min(MAX_SEARCH_RESULTS);
    if limit == 0 {
        return Ok(empty_response());
    }

    let root =
        std::fs::canonicalize(root).map_err(|error| WorkspaceFilesError::Io(error.to_string()))?;

    let mut builder = WalkBuilder::new(&root);
    builder.follow_links(false).hidden(false);
    if include_ignored {
        builder.standard_filters(false);
    }
    builder.filter_entry(|entry| {
        let name = entry.file_name();
        !name.to_string_lossy().eq_ignore_ascii_case(".git")
    });

    let mut budget = ScanBudget::new();
    let mut matches = Vec::new();
    let mut files_scanned = 0u32;
    let mut skipped_binary = 0u32;
    let mut skipped_too_large = 0u32;
    let mut skipped_unsupported = 0u32;
    let mut skipped_errors = 0u32;
    let mut scan_incomplete = false;
    let mut incomplete_reason = None;
    let mut hit_result_cap = false;

    let mut fuzzy = match mode {
        WorkspaceContentMatchMode::Literal => None,
        WorkspaceContentMatchMode::Fuzzy => Some(FuzzyLineMatcher::new(query)),
    };
    let query_lower = query.to_lowercase();

    for result in builder.build() {
        if cancel.load(AtomicOrdering::Relaxed) {
            return Err(WorkspaceFilesError::Io(
                "workspace content search cancelled".into(),
            ));
        }
        if budget.exhausted() {
            scan_incomplete = true;
            incomplete_reason = Some(WorkspaceContentSearchIncompleteReason::ScanBudgetExceeded);
            break;
        }
        let entry = match result {
            Ok(entry) => entry,
            Err(_) => {
                skipped_errors += 1;
                continue;
            }
        };
        if entry.depth() == 0 {
            continue;
        }
        let relative = match entry.path().strip_prefix(&root) {
            Ok(relative) if !contains_git_component(relative) => relative,
            _ => continue,
        };
        let file_type = entry.file_type();
        if !file_type.is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let path = match path_to_wire(relative) {
            Ok(path) => path,
            Err(_) => {
                skipped_errors += 1;
                continue;
            }
        };
        if is_internal_temp_wire_path(&path) {
            continue;
        }

        let metadata = match metadata_for_workspace_file(&root, &path) {
            Ok(metadata) => metadata,
            Err(WorkspaceFilesError::Unsupported(_)) => {
                skipped_unsupported += 1;
                continue;
            }
            Err(_) => {
                skipped_errors += 1;
                continue;
            }
        };
        if metadata.len() > CONTENT_SEARCH_MAX_BYTES_PER_FILE {
            skipped_too_large += 1;
            continue;
        }

        files_scanned += 1;
        let relative_path = match WorkspaceRelativePath::file(&path) {
            Ok(relative) => relative,
            Err(_) => {
                skipped_errors += 1;
                continue;
            }
        };
        let scan = scan_file_lines(
            &root,
            &relative_path,
            &path,
            &query_lower,
            &mut fuzzy,
            limit,
            &mut matches,
            &mut hit_result_cap,
            &mut budget,
            cancel,
        );
        match scan {
            FileScanOutcome::Ok => {}
            FileScanOutcome::BinaryTail => skipped_binary += 1,
            FileScanOutcome::Binary => skipped_binary += 1,
            FileScanOutcome::Unsupported => skipped_unsupported += 1,
            FileScanOutcome::Error => skipped_errors += 1,
            FileScanOutcome::BudgetExceeded => {
                scan_incomplete = true;
                incomplete_reason =
                    Some(WorkspaceContentSearchIncompleteReason::ScanBudgetExceeded);
                break;
            }
            FileScanOutcome::Cancelled => {
                return Err(WorkspaceFilesError::Io(
                    "workspace content search cancelled".into(),
                ));
            }
        }

        if scan_incomplete {
            break;
        }
    }

    sort_matches(&mut matches, mode);
    if matches.len() > limit {
        matches.truncate(limit);
        hit_result_cap = true;
    }

    let completion = if scan_incomplete {
        WorkspaceContentSearchCompletion::ScanIncomplete
    } else if hit_result_cap {
        WorkspaceContentSearchCompletion::ResultLimitReached
    } else {
        WorkspaceContentSearchCompletion::Complete
    };

    Ok(SearchWorkspaceContentResponse {
        matches,
        files_scanned,
        skipped_binary,
        skipped_too_large,
        skipped_unsupported,
        skipped_errors,
        completion,
        incomplete_reason,
    })
}

fn empty_response() -> SearchWorkspaceContentResponse {
    SearchWorkspaceContentResponse {
        matches: Vec::new(),
        files_scanned: 0,
        skipped_binary: 0,
        skipped_too_large: 0,
        skipped_unsupported: 0,
        skipped_errors: 0,
        completion: WorkspaceContentSearchCompletion::Complete,
        incomplete_reason: None,
    }
}

enum FileScanOutcome {
    Ok,
    /// Text matches were collected before a NUL byte appeared later in the file.
    BinaryTail,
    Binary,
    Unsupported,
    Error,
    BudgetExceeded,
    Cancelled,
}

struct LineBuffer {
    carry: Vec<u8>,
    line_number: u32,
    skipping_oversized: bool,
    saw_invalid_utf8: bool,
}

impl LineBuffer {
    fn new() -> Self {
        Self {
            carry: Vec::new(),
            line_number: 0,
            skipping_oversized: false,
            saw_invalid_utf8: false,
        }
    }

    fn on_newline(&mut self) {
        self.skipping_oversized = false;
    }

    fn begin_line(&mut self) {
        self.line_number += 1;
    }
}

fn scan_file_lines(
    root: &Path,
    relative: &WorkspaceRelativePath,
    wire_path: &str,
    query_lower: &str,
    fuzzy: &mut Option<FuzzyLineMatcher>,
    limit: usize,
    matches: &mut Vec<WorkspaceContentSearchMatch>,
    hit_result_cap: &mut bool,
    budget: &mut ScanBudget,
    cancel: &AtomicBool,
) -> FileScanOutcome {
    let path = root.join(relative.as_path());
    let initial_metadata = match metadata_for_workspace_file(root, wire_path) {
        Ok(metadata) => metadata,
        Err(WorkspaceFilesError::Unsupported(_)) => return FileScanOutcome::Unsupported,
        Err(_) => return FileScanOutcome::Error,
    };
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(_) => return FileScanOutcome::Error,
    };
    let mut reader = BufReader::new(file);
    let mut chunk = [0u8; READ_CHUNK_BYTES];
    let mut lines = LineBuffer::new();
    let mut bytes_read_from_file = 0u64;
    let file_byte_cap = initial_metadata
        .len()
        .min(CONTENT_SEARCH_MAX_BYTES_PER_FILE);
    let mut saw_binary = false;

    loop {
        if cancel.load(AtomicOrdering::Relaxed) {
            return FileScanOutcome::Cancelled;
        }
        if bytes_read_from_file >= file_byte_cap {
            break;
        }
        let read = match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => return FileScanOutcome::Error,
        };
        let read = read.min((file_byte_cap - bytes_read_from_file) as usize);
        if !budget.record(read as u64) {
            return FileScanOutcome::BudgetExceeded;
        }
        bytes_read_from_file += read as u64;
        let mut process_len = read;
        if let Some(nul_pos) = chunk[..read].iter().position(|&b| b == b'\0') {
            saw_binary = true;
            process_len = nul_pos;
        }

        if lines.skipping_oversized {
            if let Some(split) = chunk[..process_len].iter().position(|&b| b == b'\n') {
                lines.carry.clear();
                lines
                    .carry
                    .extend_from_slice(&chunk[split + 1..process_len]);
                lines.on_newline();
            }
            if saw_binary {
                break;
            }
            continue;
        }

        lines.carry.extend_from_slice(&chunk[..process_len]);
        while let Some(split) = lines.carry.iter().position(|&b| b == b'\n') {
            let mut line_bytes = lines.carry.drain(..=split).collect::<Vec<_>>();
            let _had_newline = line_bytes.pop() == Some(b'\n');
            lines.on_newline();
            lines.begin_line();
            if line_bytes.len() > CONTENT_SEARCH_MAX_LINE_BYTES {
                continue;
            }
            let mode = mode_from_fuzzy(fuzzy);
            match process_line(
                wire_path,
                lines.line_number,
                &line_bytes,
                query_lower,
                fuzzy.as_mut(),
                matches,
                limit,
                hit_result_cap,
                mode,
            ) {
                LineOutcome::Ok => {}
                LineOutcome::InvalidUtf8 => lines.saw_invalid_utf8 = true,
            }
        }

        if lines.carry.len() > CONTENT_SEARCH_MAX_LINE_BYTES {
            lines.begin_line();
            lines.skipping_oversized = true;
            lines.carry.clear();
        }

        if saw_binary {
            break;
        }
    }

    if !lines.skipping_oversized && !lines.carry.is_empty() {
        lines.begin_line();
        if lines.carry.len() <= CONTENT_SEARCH_MAX_LINE_BYTES {
            let mode = mode_from_fuzzy(fuzzy);
            match process_line(
                wire_path,
                lines.line_number,
                &lines.carry,
                query_lower,
                fuzzy.as_mut(),
                matches,
                limit,
                hit_result_cap,
                mode,
            ) {
                LineOutcome::Ok => {}
                LineOutcome::InvalidUtf8 => lines.saw_invalid_utf8 = true,
            }
        }
    }

    if saw_binary {
        return if matches.iter().any(|entry| entry.path == wire_path) {
            FileScanOutcome::BinaryTail
        } else {
            FileScanOutcome::Binary
        };
    }
    if lines.saw_invalid_utf8 {
        return FileScanOutcome::Unsupported;
    }
    FileScanOutcome::Ok
}

fn mode_from_fuzzy(fuzzy: &Option<FuzzyLineMatcher>) -> WorkspaceContentMatchMode {
    if fuzzy.is_some() {
        WorkspaceContentMatchMode::Fuzzy
    } else {
        WorkspaceContentMatchMode::Literal
    }
}

enum LineOutcome {
    Ok,
    InvalidUtf8,
}

fn process_line(
    wire_path: &str,
    line_number: u32,
    line_bytes: &[u8],
    query_lower: &str,
    fuzzy: Option<&mut FuzzyLineMatcher>,
    matches: &mut Vec<WorkspaceContentSearchMatch>,
    limit: usize,
    hit_result_cap: &mut bool,
    mode: WorkspaceContentMatchMode,
) -> LineOutcome {
    let line = match std::str::from_utf8(strip_crlf(line_bytes)) {
        Ok(line) => line,
        Err(_) => return LineOutcome::InvalidUtf8,
    };
    if let Some(fuzzy) = fuzzy {
        if let Some((score, start, end, highlight_bytes)) = fuzzy.match_line(line) {
            push_match_from_parts(
                matches,
                wire_path,
                line_number,
                line,
                start,
                end,
                highlight_bytes,
                Some(score as i64),
                limit,
                hit_result_cap,
                mode,
            );
        }
    } else if let Some(ranges) = literal_ranges(line, query_lower) {
        for (start, end) in ranges.into_iter().take(CONTENT_SEARCH_MAX_MATCHES_PER_LINE) {
            let highlight_bytes = literal_highlight_bytes(line, &[(start, end)]);
            push_match_from_parts(
                matches,
                wire_path,
                line_number,
                line,
                start,
                end,
                highlight_bytes,
                None,
                limit,
                hit_result_cap,
                mode,
            );
        }
    }
    LineOutcome::Ok
}

fn strip_crlf(bytes: &[u8]) -> &[u8] {
    if bytes.ends_with(b"\r") {
        &bytes[..bytes.len() - 1]
    } else {
        bytes
    }
}

struct LiteralFoldUnit {
    ch: char,
    start_byte: usize,
    end_byte: usize,
}

/// One fold unit per source character: the first `char` from `to_lowercase()`.
/// Trailing codepoints from case mapping (e.g. U+0307 after LATIN CAPITAL I WITH DOT
/// ABOVE) are not separate units — they stay part of the source character's byte span.
fn literal_fold_units(line: &str) -> Vec<LiteralFoldUnit> {
    let mut units = Vec::new();
    for (start, ch) in line.char_indices() {
        let end_byte = start + ch.len_utf8();
        let lower: Vec<char> = ch.to_lowercase().collect();
        if let Some(&fc) = lower.first() {
            units.push(LiteralFoldUnit {
                ch: fc,
                start_byte: start,
                end_byte,
            });
        }
    }
    units
}

/// Case-insensitive literal matches with byte ranges in the original `line`.
fn literal_ranges(line: &str, query_lower: &str) -> Option<Vec<(usize, usize)>> {
    let query: Vec<char> = query_lower.chars().collect();
    if query.is_empty() {
        return None;
    }
    let folded = literal_fold_units(line);
    if folded.len() < query.len() {
        return None;
    }
    let mut ranges = Vec::new();
    for start in 0..=folded.len() - query.len() {
        if folded[start..start + query.len()]
            .iter()
            .zip(query.iter())
            .all(|(unit, expected)| unit.ch == *expected)
        {
            ranges.push((
                folded[start].start_byte,
                folded[start + query.len() - 1].end_byte,
            ));
        }
    }
    if ranges.is_empty() {
        None
    } else {
        Some(ranges)
    }
}

fn literal_highlight_bytes(line: &str, ranges: &[(usize, usize)]) -> Vec<(usize, usize)> {
    ranges
        .iter()
        .filter_map(|(start, end)| {
            if line.is_char_boundary(*start) && line.is_char_boundary(*end) {
                Some((*start, *end))
            } else {
                None
            }
        })
        .take(CONTENT_SEARCH_MAX_HIGHLIGHT_RANGES)
        .collect()
}

struct FuzzyLineMatcher {
    pattern: Pattern,
    matcher: Matcher,
    haystack: Utf32String,
    index_scratch: Vec<u32>,
}

impl FuzzyLineMatcher {
    fn new(query: &str) -> Self {
        Self {
            pattern: Pattern::new(
                query,
                CaseMatching::Ignore,
                Normalization::Smart,
                AtomKind::Fuzzy,
            ),
            matcher: Matcher::new(Config::DEFAULT),
            haystack: Utf32String::default(),
            index_scratch: Vec::new(),
        }
    }

    fn match_line(&mut self, line: &str) -> Option<(u32, usize, usize, Vec<(usize, usize)>)> {
        self.haystack = Utf32String::from(line);
        let haystack = self.haystack.slice(..);
        self.index_scratch.clear();
        let score = self
            .pattern
            .indices(haystack, &mut self.matcher, &mut self.index_scratch)?;
        self.index_scratch.sort_unstable();
        self.index_scratch.dedup();
        if self.index_scratch.is_empty() {
            return None;
        }
        let char_positions = self
            .index_scratch
            .iter()
            .map(|&idx| idx as usize)
            .collect::<Vec<_>>();
        let start = line.char_indices().nth(char_positions[0]).map(|(i, _)| i)?;
        let last = char_positions[char_positions.len() - 1];
        let end = line
            .char_indices()
            .nth(last)
            .map(|(i, c)| i + c.len_utf8())?;
        let highlight_bytes = char_positions
            .iter()
            .filter_map(|&char_idx| {
                line.char_indices()
                    .nth(char_idx)
                    .map(|(byte, ch)| (byte, byte + ch.len_utf8()))
            })
            .take(CONTENT_SEARCH_MAX_HIGHLIGHT_RANGES)
            .collect();
        Some((score, start, end, highlight_bytes))
    }
}

fn push_match_from_parts(
    matches: &mut Vec<WorkspaceContentSearchMatch>,
    path: &str,
    line_number: u32,
    line: &str,
    start_byte: usize,
    end_byte: usize,
    highlight_bytes: Vec<(usize, usize)>,
    score: Option<i64>,
    limit: usize,
    hit_result_cap: &mut bool,
    mode: WorkspaceContentMatchMode,
) {
    let line_match_start = line[..start_byte].chars().count() as u32;
    let line_match_end = line[..end_byte].chars().count() as u32;
    let match_text = line
        .chars()
        .skip(line_match_start as usize)
        .take((line_match_end - line_match_start) as usize)
        .take(CONTENT_SEARCH_MAX_MATCH_TEXT_CHARS)
        .collect::<String>();
    let (preview, preview_highlights) = build_preview(line, &highlight_bytes);
    matches.push(WorkspaceContentSearchMatch {
        path: path.to_string(),
        line: line_number,
        preview,
        preview_highlights,
        line_match_start,
        line_match_end,
        match_text,
        score,
    });
    if matches.len() > limit {
        *hit_result_cap = true;
        trim_to_best_matches(matches, mode, limit);
    }
}

fn build_preview(
    line: &str,
    highlight_bytes: &[(usize, usize)],
) -> (String, Vec<WorkspaceContentHighlightRange>) {
    if line.is_empty() {
        return (String::new(), Vec::new());
    }
    if line.chars().count() <= CONTENT_SEARCH_PREVIEW_MAX_CHARS {
        let highlights = highlight_bytes
            .iter()
            .filter_map(|(start, end)| {
                if line.is_char_boundary(*start) && line.is_char_boundary(*end) {
                    Some(WorkspaceContentHighlightRange {
                        start: *start as u32,
                        end: *end as u32,
                    })
                } else {
                    None
                }
            })
            .collect();
        return (line.to_string(), highlights);
    }
    let min_highlight = highlight_bytes
        .iter()
        .map(|(start, _)| *start)
        .min()
        .unwrap_or(0);
    let max_highlight = highlight_bytes
        .iter()
        .map(|(_, end)| *end)
        .max()
        .unwrap_or(min_highlight);
    let line_len = line.len();
    let mut window_start = min_highlight.min(line_len);
    let mut window_end = end_byte_after_chars(line, window_start, CONTENT_SEARCH_PREVIEW_MAX_CHARS)
        .unwrap_or(line_len);
    if window_end < max_highlight {
        let deficit = max_highlight - window_end;
        window_start = start_byte_before_chars(line, window_start, deficit).unwrap_or(0);
        window_end = end_byte_after_chars(line, window_start, CONTENT_SEARCH_PREVIEW_MAX_CHARS)
            .unwrap_or(line_len);
    }
    if !line.is_char_boundary(window_start) || !line.is_char_boundary(window_end) {
        return (
            line.chars()
                .take(CONTENT_SEARCH_PREVIEW_MAX_CHARS)
                .collect(),
            Vec::new(),
        );
    }
    let preview = line[window_start..window_end].to_string();
    let offset = window_start;
    let highlights = highlight_bytes
        .iter()
        .filter_map(|(start, end)| {
            if *end <= offset || *start >= window_end {
                return None;
            }
            let rel_start = start.saturating_sub(offset);
            let rel_end = (*end).min(window_end).saturating_sub(offset);
            if preview.is_char_boundary(rel_start) && preview.is_char_boundary(rel_end) {
                Some(WorkspaceContentHighlightRange {
                    start: rel_start as u32,
                    end: rel_end as u32,
                })
            } else {
                None
            }
        })
        .collect();
    (preview, highlights)
}

fn end_byte_after_chars(line: &str, byte_start: usize, max_chars: usize) -> Option<usize> {
    if !line.is_char_boundary(byte_start) {
        return None;
    }
    let mut count = 0usize;
    for (offset, ch) in line[byte_start..].char_indices() {
        count += 1;
        if count >= max_chars {
            return Some(byte_start + offset + ch.len_utf8());
        }
    }
    Some(line.len())
}

fn start_byte_before_chars(line: &str, byte_start: usize, max_chars: usize) -> Option<usize> {
    if max_chars == 0 {
        return Some(byte_start);
    }
    let prefix = &line[..byte_start];
    let indices = prefix.char_indices().collect::<Vec<_>>();
    if indices.len() <= max_chars {
        return Some(0);
    }
    let (offset, _) = indices[indices.len() - max_chars];
    Some(offset)
}

fn trim_to_best_matches(
    matches: &mut Vec<WorkspaceContentSearchMatch>,
    mode: WorkspaceContentMatchMode,
    limit: usize,
) {
    sort_matches(matches, mode);
    matches.truncate(limit);
}

fn sort_matches(matches: &mut [WorkspaceContentSearchMatch], mode: WorkspaceContentMatchMode) {
    matches.sort_by(|left, right| compare_matches(left, right, mode));
}

fn compare_matches(
    left: &WorkspaceContentSearchMatch,
    right: &WorkspaceContentSearchMatch,
    mode: WorkspaceContentMatchMode,
) -> Ordering {
    match mode {
        WorkspaceContentMatchMode::Fuzzy => right
            .score
            .unwrap_or(0)
            .cmp(&left.score.unwrap_or(0))
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.line.cmp(&right.line)),
        WorkspaceContentMatchMode::Literal => left
            .path
            .cmp(&right.path)
            .then_with(|| left.line.cmp(&right.line)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    fn no_cancel() -> AtomicBool {
        AtomicBool::new(false)
    }

    #[test]
    fn literal_finds_case_insensitive_matches_with_line_numbers() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        std::fs::write(
            root.path().join("src/lib.rs"),
            "pub fn UserService() {}\n// userservice note\n",
        )
        .unwrap();
        let root = std::fs::canonicalize(root.path()).unwrap();
        let response = search_workspace_content_blocking(
            &root,
            "userservice",
            WorkspaceContentMatchMode::Literal,
            false,
            200,
            &no_cancel(),
        )
        .unwrap();
        assert_eq!(response.matches.len(), 2);
        assert_eq!(response.matches[0].line, 1);
        assert_eq!(response.matches[0].match_text, "UserService");
        assert_eq!(response.matches[1].line, 2);
    }

    #[test]
    fn literal_unicode_casefold_maps_to_original_bytes() {
        let line = "prefix İtem suffix";
        let ranges = literal_ranges(line, "item").expect("match");
        let (start, end) = ranges[0];
        assert_eq!(&line[start..end], "İtem");
    }

    #[test]
    fn literal_non_ascii_casefold_before_ascii_match() {
        let line = "İabc";
        let ranges = literal_ranges(line, "iabc").expect("match");
        assert_eq!(&line[ranges[0].0..ranges[0].1], "İabc");
    }

    #[test]
    fn fuzzy_matches_subsequence_not_typo_distance() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("svc.rs"), "pub struct UserService;\n").unwrap();
        let root = std::fs::canonicalize(root.path()).unwrap();
        let hit = search_workspace_content_blocking(
            &root,
            "usrsvc",
            WorkspaceContentMatchMode::Fuzzy,
            false,
            200,
            &no_cancel(),
        )
        .unwrap();
        assert_eq!(hit.matches.len(), 1);
        assert!(hit.matches[0].score.unwrap_or(0) > 0);

        let miss = search_workspace_content_blocking(
            &root,
            "svcusr",
            WorkspaceContentMatchMode::Fuzzy,
            false,
            200,
            &no_cancel(),
        )
        .unwrap();
        assert!(miss.matches.is_empty());
    }

    #[test]
    fn fuzzy_keeps_higher_scoring_line_when_over_limit() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("spread.rs"), "a l p h a\n").unwrap();
        std::fs::write(root.path().join("tight.rs"), "find alpha here\n").unwrap();
        let root = std::fs::canonicalize(root.path()).unwrap();
        let response = search_workspace_content_blocking(
            &root,
            "alpha",
            WorkspaceContentMatchMode::Fuzzy,
            false,
            1,
            &no_cancel(),
        )
        .unwrap();
        assert_eq!(response.matches.len(), 1);
        assert_eq!(response.matches[0].path, "tight.rs");
        assert_eq!(
            response.completion,
            WorkspaceContentSearchCompletion::ResultLimitReached
        );
    }

    #[test]
    fn oversized_line_then_match_on_next_line() {
        let root = tempfile::tempdir().unwrap();
        let long = "x".repeat(CONTENT_SEARCH_MAX_LINE_BYTES + 10);
        std::fs::write(root.path().join("big.txt"), format!("{long}\nneedle\n")).unwrap();
        let root = std::fs::canonicalize(root.path()).unwrap();
        let response = search_workspace_content_blocking(
            &root,
            "needle",
            WorkspaceContentMatchMode::Literal,
            false,
            200,
            &no_cancel(),
        )
        .unwrap();
        assert_eq!(response.matches.len(), 1);
        assert_eq!(response.matches[0].line, 2);
    }

    #[test]
    fn crlf_and_chunk_boundaries() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("crlf.txt"), "before\r\nneedle\r\n").unwrap();
        let root = std::fs::canonicalize(root.path()).unwrap();
        let response = search_workspace_content_blocking(
            &root,
            "needle",
            WorkspaceContentMatchMode::Literal,
            false,
            200,
            &no_cancel(),
        )
        .unwrap();
        assert_eq!(response.matches.len(), 1);
        assert_eq!(response.matches[0].line, 2);
    }

    #[test]
    fn invalid_utf8_unterminated_last_line_marks_unsupported() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("tail.txt"), b"needle\n\xff").unwrap();
        let root = std::fs::canonicalize(root.path()).unwrap();
        let response = search_workspace_content_blocking(
            &root,
            "needle",
            WorkspaceContentMatchMode::Literal,
            false,
            200,
            &no_cancel(),
        )
        .unwrap();
        assert_eq!(response.matches.len(), 1);
        assert_eq!(response.skipped_unsupported, 1);
    }

    #[test]
    fn invalid_utf8_last_line_marks_unsupported_without_dropping_prior_hits() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("mix.txt"), b"needle\nok line\n\xff\n").unwrap();
        let root = std::fs::canonicalize(root.path()).unwrap();
        let response = search_workspace_content_blocking(
            &root,
            "needle",
            WorkspaceContentMatchMode::Literal,
            false,
            200,
            &no_cancel(),
        )
        .unwrap();
        assert_eq!(response.matches.len(), 1);
        assert_eq!(response.skipped_unsupported, 1);
    }

    #[test]
    fn binary_tail_preserves_earlier_matches() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("mix.bin"), b"needle\n\x00tail\n").unwrap();
        let root = std::fs::canonicalize(root.path()).unwrap();
        let response = search_workspace_content_blocking(
            &root,
            "needle",
            WorkspaceContentMatchMode::Literal,
            false,
            200,
            &no_cancel(),
        )
        .unwrap();
        assert_eq!(response.matches.len(), 1);
        assert_eq!(response.skipped_binary, 1);
    }

    #[test]
    fn skips_binary_and_git_paths() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("text.txt"), "hello needle\n").unwrap();
        std::fs::write(root.path().join("binary.bin"), b"hello\x00needle\n").unwrap();
        std::fs::create_dir_all(root.path().join(".git")).unwrap();
        std::fs::write(root.path().join(".git/config"), "needle").unwrap();
        let root = std::fs::canonicalize(root.path()).unwrap();
        let response = search_workspace_content_blocking(
            &root,
            "needle",
            WorkspaceContentMatchMode::Literal,
            true,
            200,
            &no_cancel(),
        )
        .unwrap();
        assert_eq!(response.matches.len(), 1);
        assert_eq!(response.matches[0].path, "text.txt");
        assert_eq!(response.skipped_binary, 1);
    }

    #[test]
    fn preview_crops_forward_from_first_match_on_long_line() {
        let prefix = "z".repeat(300);
        let line = format!("{prefix}needle");
        let (preview, highlights) = build_preview(&line, &[(300, 306)]);
        assert!(preview.contains("needle"));
        assert!(preview.chars().count() <= CONTENT_SEARCH_PREVIEW_MAX_CHARS + 4);
        assert_eq!(highlights.len(), 1);
        assert!(
            preview[highlights[0].start as usize..highlights[0].end as usize].contains("needle")
        );
    }

    #[test]
    fn preview_highlights_use_utf8_byte_offsets() {
        let (preview, highlights) = build_preview("café résumé", &[(5, 9)]);
        assert_eq!(preview, "café résumé");
        assert_eq!(highlights.len(), 1);
        assert_eq!(highlights[0].start, 5);
        assert_eq!(highlights[0].end, 9);
    }

    #[test]
    fn oversized_line_spans_read_chunks_before_match() {
        let root = tempfile::tempdir().unwrap();
        let padding = "x".repeat(READ_CHUNK_BYTES + 100);
        std::fs::write(
            root.path().join("chunked.txt"),
            format!("{padding}\nneedle\n"),
        )
        .unwrap();
        let root = std::fs::canonicalize(root.path()).unwrap();
        let response = search_workspace_content_blocking(
            &root,
            "needle",
            WorkspaceContentMatchMode::Literal,
            false,
            200,
            &no_cancel(),
        )
        .unwrap();
        assert_eq!(response.matches.len(), 1);
        assert_eq!(response.matches[0].line, 2);
    }

    #[test]
    fn literal_multiple_occurrences_on_one_line() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("dup.txt"), "needle needle\n").unwrap();
        let root = std::fs::canonicalize(root.path()).unwrap();
        let response = search_workspace_content_blocking(
            &root,
            "needle",
            WorkspaceContentMatchMode::Literal,
            false,
            200,
            &no_cancel(),
        )
        .unwrap();
        assert_eq!(response.matches.len(), 2);
        assert_eq!(response.matches[0].line, 1);
        assert_eq!(response.matches[1].line, 1);
    }

    #[test]
    fn cancellation_surfaces_as_error() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.txt"), "needle\n").unwrap();
        let root = std::fs::canonicalize(root.path()).unwrap();
        let cancel = AtomicBool::new(true);
        let error = search_workspace_content_blocking(
            &root,
            "needle",
            WorkspaceContentMatchMode::Literal,
            false,
            200,
            &cancel,
        )
        .unwrap_err();
        assert!(error.to_string().contains("cancelled"));
    }
}
