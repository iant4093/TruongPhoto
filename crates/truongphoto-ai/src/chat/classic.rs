use crate::chat::schema::merge_router_plan;
use crate::chat::{ChatRouterBackend, ChatTurnInput, RerankCandidate, RerankResult};
use crate::AiError;
use chrono::{DateTime, Datelike, Duration as ChronoDuration, NaiveDate, TimeZone, Utc};
use std::time::Duration;
use truongphoto_core::{AssetKind, ChatTier, RouterPlan};

/// Tier 0 deterministic offline natural-language query router.
///
/// Parses dates, media kinds, favorites, RBAC-visible person names, locations, cameras, tags,
/// OCR keywords/phrases, and semantic keywords without requiring any LLM weights.
#[derive(Debug, Default, Clone, Copy)]
pub struct ClassicChatRouter;

impl ClassicChatRouter {
    pub fn new() -> Self {
        Self
    }

    pub fn parse_turn(&self, input: &ChatTurnInput) -> RouterPlan {
        let mut text = input.user_message.trim().to_string();
        let mut turn = RouterPlan::default();

        if text.is_empty() {
            let mut merged = merge_router_plan(&input.prior_filters, &turn);
            merged.assistant_reply =
                "Tell me what photos or videos you are looking for.".to_string();
            merged.follow_up_suggestions = build_suggestions(&merged);
            return merged;
        }

        // 1. Clear / reset triggers.
        let (cleared, after_clear) = extract_clear_trigger(&text);
        turn.clear_prior_filters = cleared;
        text = after_clear;

        // 2. Explicit OCR phrases (`text saying ...`, `receipt`, `invoice`, `document`).
        let (ocr_terms, after_ocr) = extract_ocr(&text);
        turn.ocr_terms = ocr_terms;
        text = after_ocr;

        // 3. Relative and absolute dates (must run BEFORE location extraction so `in 2024`
        // or `in March 2025` is consumed as a date rather than a location).
        let (date_range, after_dates) = extract_date_range(&text, input.current_date_utc);
        if let Some((from_ms, to_ms)) = date_range {
            turn.date_from_ms = Some(from_ms);
            turn.date_to_ms = Some(to_ms);
        }
        text = after_dates;

        // 4. Known RBAC-visible person names.
        let (people, after_people) = extract_known_people(&text, &input.known_person_names);
        turn.person_names = people;
        text = after_people;

        // 5. Cameras (`shot on ...`, `taken with ...`, `camera ...`).
        let (camera, after_camera) = extract_camera(&text);
        turn.camera_model = camera;
        text = after_camera;

        // 6. Tags (`tagged ...`, `tag ...`, `#hashtag`).
        let (tags, after_tags) = extract_tags(&text);
        turn.tags = tags;
        text = after_tags;

        // 7. Media kinds (`only videos`, `live photos`, `motion photos`, `raw`, `photos`).
        let (kind, after_kind) = extract_kind(&text);
        turn.kind = kind;
        text = after_kind;

        // 8. Favorites (`favorites`, `favourites`, `starred`).
        let (fav, after_fav) = extract_favorite(&text);
        turn.favorite = fav;
        text = after_fav;

        // 9. Locations (`in <Place>`, `from <Place>`, `at <Place>`).
        let (location, after_loc) = extract_location(&text);
        turn.location_query = location;
        text = after_loc;

        // 10. Remaining meaningful content terms become `semantic_query`.
        turn.semantic_query = extract_semantic_query(&text);

        let mut merged = merge_router_plan(&input.prior_filters, &turn);
        merged.assistant_reply = build_assistant_reply(&merged);
        merged.follow_up_suggestions = build_suggestions(&merged);
        merged
    }
}

impl ChatRouterBackend for ClassicChatRouter {
    fn route_turn(&self, _tier: ChatTier, input: &ChatTurnInput) -> Result<RouterPlan, AiError> {
        Ok(self.parse_turn(input))
    }

    fn rerank_thumbnails(
        &self,
        _tier: ChatTier,
        _user_message: &str,
        _candidates: &[RerankCandidate],
    ) -> Result<Option<RerankResult>, AiError> {
        Ok(None)
    }

    fn evict_idle(&self, _max_idle: Duration) -> usize {
        0
    }

    fn is_loaded(&self) -> bool {
        false
    }
}

fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_' || ch == '-'
}

fn find_word_phrase_ci(haystack: &str, needle: &str) -> Option<(usize, usize)> {
    let lower_hay = haystack.to_ascii_lowercase();
    let lower_needle = needle.to_ascii_lowercase();
    if lower_needle.is_empty() {
        return None;
    }
    let mut search_from = 0usize;
    while let Some(rel_idx) = lower_hay[search_from..].find(&lower_needle) {
        let start = search_from + rel_idx;
        let end = start + lower_needle.len();
        let left_ok = haystack[..start]
            .chars()
            .next_back()
            .is_none_or(|c| !is_word_char(c));
        let right_ok = haystack[end..]
            .chars()
            .next()
            .is_none_or(|c| !is_word_char(c));
        if left_ok && right_ok {
            return Some((start, end));
        }
        // Advance safely past the start character boundary.
        let step = lower_hay[start..]
            .chars()
            .next()
            .map(|c| c.len_utf8())
            .unwrap_or(1);
        search_from = start + step;
    }
    None
}

fn remove_span(text: &str, start: usize, end: usize) -> String {
    let mut out = String::with_capacity(text.len());
    out.push_str(text[..start].trim_end());
    let rest = text[end..].trim_start();
    if !out.is_empty() && !rest.is_empty() {
        out.push(' ');
    }
    out.push_str(rest);
    out
}

fn strip_phrase_ci(text: &str, phrase: &str) -> (bool, String) {
    if let Some((s, e)) = find_word_phrase_ci(text, phrase) {
        (true, remove_span(text, s, e))
    } else {
        (false, text.to_string())
    }
}

fn extract_clear_trigger(text: &str) -> (bool, String) {
    let mut current = text.to_string();
    let mut cleared = false;

    for phrase in [
        "start over",
        "reset filters",
        "clear filters",
        "new search",
        "clear all",
        "reset search",
        "reset everything",
        "instead",
    ] {
        let (found, next) = strip_phrase_ci(&current, phrase);
        if found {
            cleared = true;
            current = next;
        }
    }

    let trimmed = current.trim().to_ascii_lowercase();
    if trimmed == "reset" || trimmed == "clear" {
        cleared = true;
        current.clear();
    }

    (cleared, current)
}

fn extract_ocr(text: &str) -> (Option<String>, String) {
    let mut current = text.to_string();

    for trigger in ["text saying", "containing text", "with text", "saying"] {
        if let Some((start, end)) = find_word_phrase_ci(&current, trigger) {
            let after = current[end..].trim_start();
            if let Some(first_ch) = after.chars().next() {
                if first_ch == '"' || first_ch == '\'' {
                    let quote = first_ch;
                    let inner_start = end + (current[end..].len() - after.len()) + quote.len_utf8();
                    if let Some(rel_close) = current[inner_start..].find(quote) {
                        let phrase = current[inner_start..inner_start + rel_close]
                            .trim()
                            .to_string();
                        let span_end = inner_start + rel_close + quote.len_utf8();
                        let updated = remove_span(&current, start, span_end);
                        if !phrase.is_empty() {
                            return (Some(phrase), updated);
                        }
                    }
                }
            }

            // Unquoted text phrase up to clause boundary.
            let after_offset = current.len() - after.len();
            let (clause, consumed_len) = take_clause_tokens(after, 6, OCR_STOP_BOUNDARIES);
            if !clause.is_empty() {
                let updated = remove_span(&current, start, after_offset + consumed_len);
                return (Some(clause), updated);
            }
        }
    }

    for (kw, normalized) in [
        ("receipts", "receipt"),
        ("receipt", "receipt"),
        ("invoices", "invoice"),
        ("invoice", "invoice"),
        ("documents", "document"),
        ("document", "document"),
    ] {
        let (found, next) = strip_phrase_ci(&current, kw);
        if found {
            current = next;
            return (Some(normalized.to_string()), current);
        }
    }

    (None, current)
}

fn days_in_month(year: i32, month: u32) -> u32 {
    let (ny, nm) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    NaiveDate::from_ymd_opt(ny, nm, 1)
        .and_then(|d| d.pred_opt())
        .map(|d| d.day())
        .unwrap_or(28)
}

fn date_range_ms(start: NaiveDate, end: NaiveDate) -> (i64, i64) {
    let start_dt = Utc.from_utc_datetime(&start.and_hms_opt(0, 0, 0).expect("valid 00:00:00"));
    let end_dt = Utc.from_utc_datetime(
        &end.and_hms_milli_opt(23, 59, 59, 999)
            .expect("valid 23:59:59.999"),
    );
    (start_dt.timestamp_millis(), end_dt.timestamp_millis())
}

fn parse_month_name(token: &str) -> Option<u32> {
    match token.to_ascii_lowercase().as_str() {
        "january" | "jan" => Some(1),
        "february" | "feb" => Some(2),
        "march" | "mar" => Some(3),
        "april" | "apr" => Some(4),
        "may" => Some(5),
        "june" | "jun" => Some(6),
        "july" | "jul" => Some(7),
        "august" | "aug" => Some(8),
        "september" | "sept" | "sep" => Some(9),
        "october" | "oct" => Some(10),
        "november" | "nov" => Some(11),
        "december" | "dec" => Some(12),
        _ => None,
    }
}

fn strip_optional_leading_prep(text: &str, match_start: usize) -> usize {
    let prefix = text[..match_start].trim_end();
    for prep in ["during", "from", "in", "on", "of"] {
        if let Some(stripped) = prefix.strip_suffix_ci_word(prep) {
            return stripped.len();
        }
    }
    match_start
}

trait StripSuffixWord {
    fn strip_suffix_ci_word(&self, word: &str) -> Option<&str>;
}

impl StripSuffixWord for str {
    fn strip_suffix_ci_word(&self, word: &str) -> Option<&str> {
        if self.len() < word.len() {
            return None;
        }
        let split = self.len() - word.len();
        if !self.is_char_boundary(split) {
            return None;
        }
        if self[split..].eq_ignore_ascii_case(word)
            && self[..split]
                .chars()
                .next_back()
                .is_none_or(|c| !is_word_char(c))
        {
            Some(&self[..split])
        } else {
            None
        }
    }
}

type DateRangeBuilder = fn(NaiveDate, i32, u32) -> (NaiveDate, NaiveDate);

fn extract_date_range(text: &str, now_utc: DateTime<Utc>) -> (Option<(i64, i64)>, String) {
    let today = now_utc.date_naive();
    let year = today.year();
    let month = today.month();

    // Relative phrases.
    let relative_phrases: &[(&str, DateRangeBuilder)] = &[
        ("last winter", |_, y, m| {
            let winter_end_year = if m >= 3 { y } else { y - 1 };
            let start = NaiveDate::from_ymd_opt(winter_end_year - 1, 12, 1).unwrap();
            let feb_days = days_in_month(winter_end_year, 2);
            let end = NaiveDate::from_ymd_opt(winter_end_year, 2, feb_days).unwrap();
            (start, end)
        }),
        ("last spring", |_, y, m| {
            let sy = if m >= 6 { y } else { y - 1 };
            (
                NaiveDate::from_ymd_opt(sy, 3, 1).unwrap(),
                NaiveDate::from_ymd_opt(sy, 5, 31).unwrap(),
            )
        }),
        ("last summer", |_, y, m| {
            let sy = if m >= 9 { y } else { y - 1 };
            (
                NaiveDate::from_ymd_opt(sy, 6, 1).unwrap(),
                NaiveDate::from_ymd_opt(sy, 8, 31).unwrap(),
            )
        }),
        ("last autumn", |_, y, m| {
            let sy = if m == 12 { y } else { y - 1 };
            (
                NaiveDate::from_ymd_opt(sy, 9, 1).unwrap(),
                NaiveDate::from_ymd_opt(sy, 11, 30).unwrap(),
            )
        }),
        ("last fall", |_, y, m| {
            let sy = if m == 12 { y } else { y - 1 };
            (
                NaiveDate::from_ymd_opt(sy, 9, 1).unwrap(),
                NaiveDate::from_ymd_opt(sy, 11, 30).unwrap(),
            )
        }),
        ("last month", |_, y, m| {
            let (py, pm) = if m == 1 { (y - 1, 12) } else { (y, m - 1) };
            (
                NaiveDate::from_ymd_opt(py, pm, 1).unwrap(),
                NaiveDate::from_ymd_opt(py, pm, days_in_month(py, pm)).unwrap(),
            )
        }),
        ("this month", |_, y, m| {
            (
                NaiveDate::from_ymd_opt(y, m, 1).unwrap(),
                NaiveDate::from_ymd_opt(y, m, days_in_month(y, m)).unwrap(),
            )
        }),
        ("last week", |t, _, _| (t - ChronoDuration::days(7), t)),
        ("this week", |t, _, _| {
            let days_from_mon = t.weekday().num_days_from_monday() as i64;
            (t - ChronoDuration::days(days_from_mon), t)
        }),
        ("last year", |_, y, _| {
            (
                NaiveDate::from_ymd_opt(y - 1, 1, 1).unwrap(),
                NaiveDate::from_ymd_opt(y - 1, 12, 31).unwrap(),
            )
        }),
        ("this year", |_, y, _| {
            (
                NaiveDate::from_ymd_opt(y, 1, 1).unwrap(),
                NaiveDate::from_ymd_opt(y, 12, 31).unwrap(),
            )
        }),
        ("yesterday", |t, _, _| {
            let yday = t - ChronoDuration::days(1);
            (yday, yday)
        }),
        ("today", |t, _, _| (t, t)),
    ];

    for (phrase, calc) in relative_phrases {
        if let Some((start, end)) = find_word_phrase_ci(text, phrase) {
            let span_start = strip_optional_leading_prep(text, start);
            let (d_start, d_end) = calc(today, year, month);
            return (
                Some(date_range_ms(d_start, d_end)),
                remove_span(text, span_start, end),
            );
        }
    }

    // "<Month> <YYYY>" e.g. "March 2025".
    let tokens = tokenize_with_spans(text);
    for pair in tokens.windows(2) {
        let (w1, s1, _e1) = &pair[0];
        let (w2, _s2, e2) = &pair[1];
        if let (Some(m_num), Some(y_num)) = (parse_month_name(w1), parse_year_token(w2)) {
            let span_start = strip_optional_leading_prep(text, *s1);
            let start = NaiveDate::from_ymd_opt(y_num, m_num, 1).unwrap();
            let end = NaiveDate::from_ymd_opt(y_num, m_num, days_in_month(y_num, m_num)).unwrap();
            return (
                Some(date_range_ms(start, end)),
                remove_span(text, span_start, *e2),
            );
        }
    }

    // "in YYYY" / "from YYYY" / "during YYYY" or standalone 4-digit year.
    for (word, s, e) in &tokens {
        if let Some(y_num) = parse_year_token(word) {
            let span_start = strip_optional_leading_prep(text, *s);
            let start = NaiveDate::from_ymd_opt(y_num, 1, 1).unwrap();
            let end = NaiveDate::from_ymd_opt(y_num, 12, 31).unwrap();
            return (
                Some(date_range_ms(start, end)),
                remove_span(text, span_start, *e),
            );
        }
    }

    (None, text.to_string())
}

fn parse_year_token(tok: &str) -> Option<i32> {
    if tok.len() != 4 || !tok.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let y: i32 = tok.parse().ok()?;
    if (1970..=2099).contains(&y) {
        Some(y)
    } else {
        None
    }
}

fn tokenize_with_spans(text: &str) -> Vec<(&str, usize, usize)> {
    let mut out = Vec::new();
    let mut idx = 0usize;
    let bytes = text.as_bytes();
    while idx < bytes.len() {
        while idx < bytes.len() {
            let ch = text[idx..].chars().next().unwrap();
            if ch.is_alphanumeric() {
                break;
            }
            idx += ch.len_utf8();
        }
        if idx >= bytes.len() {
            break;
        }
        let start = idx;
        while idx < bytes.len() {
            let ch = text[idx..].chars().next().unwrap();
            if !ch.is_alphanumeric() && ch != '-' && ch != '_' {
                break;
            }
            idx += ch.len_utf8();
        }
        out.push((&text[start..idx], start, idx));
    }
    out
}

fn extract_known_people(text: &str, known_names: &[String]) -> (Vec<String>, String) {
    let mut sorted: Vec<&str> = known_names
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    sorted.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));

    let mut current = text.to_string();
    let mut matched = Vec::new();

    for name in sorted {
        if let Some((start, end)) = find_word_phrase_ci(&current, name) {
            if !matched
                .iter()
                .any(|m: &String| m.eq_ignore_ascii_case(name))
            {
                matched.push(name.to_string());
            }
            let prefix = current[..start].trim_end();
            let mut span_start = start;
            for prep in ["featuring", "with", "of", "by"] {
                if let Some(stripped) = prefix.strip_suffix_ci_word(prep) {
                    span_start = stripped.len();
                    break;
                }
            }
            current = remove_span(&current, span_start, end);
        }
    }

    (matched, current)
}

const CLAUSE_STOP_BOUNDARIES: &[&str] = &[
    "in",
    "from",
    "at",
    "with",
    "shot",
    "taken",
    "camera",
    "tagged",
    "tag",
    "last",
    "this",
    "only",
    "favorite",
    "favorites",
    "favourite",
    "favourites",
    "starred",
    "video",
    "videos",
    "photo",
    "photos",
    "pictures",
    "live",
    "motion",
    "raw",
    "today",
    "yesterday",
    "receipt",
    "receipts",
    "invoice",
    "invoices",
    "document",
    "documents",
];

const OCR_STOP_BOUNDARIES: &[&str] = &[
    "in",
    "from",
    "at",
    "with",
    "shot",
    "taken",
    "tagged",
    "last",
    "this",
    "only",
    "today",
    "yesterday",
];

fn take_clause_tokens(after: &str, max_tokens: usize, stop_words: &[&str]) -> (String, usize) {
    let mut parts = Vec::new();
    let mut consumed = 0usize;
    let mut pos = 0usize;

    while pos < after.len() && parts.len() < max_tokens {
        // Skip spaces.
        while pos < after.len() && after[pos..].starts_with(char::is_whitespace) {
            let ch = after[pos..].chars().next().unwrap();
            pos += ch.len_utf8();
        }
        if pos >= after.len() {
            break;
        }
        // Stop on clause punctuation.
        let first = after[pos..].chars().next().unwrap();
        if matches!(first, ',' | '.' | ';' | '!' | '?' | ')') {
            break;
        }

        let tok_start = pos;
        while pos < after.len() {
            let ch = after[pos..].chars().next().unwrap();
            if ch.is_whitespace() || matches!(ch, ',' | '.' | ';' | '!' | '?' | ')') {
                break;
            }
            pos += ch.len_utf8();
        }
        let raw_tok = &after[tok_start..pos];
        let clean_tok = raw_tok
            .trim_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '_' && c != '+');
        if clean_tok.is_empty() {
            break;
        }
        let lower = clean_tok.to_ascii_lowercase();
        if stop_words.contains(&lower.as_str()) {
            break;
        }
        parts.push(clean_tok.to_string());
        consumed = pos;
    }

    (parts.join(" "), consumed)
}

fn extract_camera(text: &str) -> (Option<String>, String) {
    for trigger in ["shot on", "taken on", "taken with", "camera"] {
        if let Some((start, end)) = find_word_phrase_ci(text, trigger) {
            let after = text[end..].trim_start();
            let after_offset = text.len() - after.len();
            let (cam, consumed) = take_clause_tokens(after, 4, CLAUSE_STOP_BOUNDARIES);
            if !cam.is_empty() {
                return (Some(cam), remove_span(text, start, after_offset + consumed));
            }
        }
    }
    (None, text.to_string())
}

fn extract_tags(text: &str) -> (Vec<String>, String) {
    let mut current = text.to_string();
    let mut tags = Vec::new();

    for trigger in ["tagged", "tag"] {
        while let Some((start, end)) = find_word_phrase_ci(&current, trigger) {
            let after = current[end..].trim_start();
            let after_offset = current.len() - after.len();
            let (tag_str, consumed) = take_clause_tokens(after, 2, CLAUSE_STOP_BOUNDARIES);
            if tag_str.is_empty() {
                break;
            }
            tags.push(tag_str);
            current = remove_span(&current, start, after_offset + consumed);
        }
    }

    // Also extract `#hashtag` tokens.
    let mut cleaned_tokens = Vec::new();
    for tok in current.split_whitespace() {
        if let Some(rest) = tok.strip_prefix('#') {
            let tag = rest.trim_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '_');
            if !tag.is_empty() {
                tags.push(tag.to_string());
                continue;
            }
        }
        cleaned_tokens.push(tok);
    }

    (tags, cleaned_tokens.join(" "))
}

fn extract_kind(text: &str) -> (Option<AssetKind>, String) {
    let patterns: &[(&str, AssetKind)] = &[
        ("live photos", AssetKind::LivePhoto),
        ("live photo", AssetKind::LivePhoto),
        ("motion photos", AssetKind::MotionPhoto),
        ("motion photo", AssetKind::MotionPhoto),
        ("raw + jpg", AssetKind::RawJpgStack),
        ("raw+jpg", AssetKind::RawJpgStack),
        ("raw photos", AssetKind::RawJpgStack),
        ("raw files", AssetKind::RawJpgStack),
        ("only raw", AssetKind::RawJpgStack),
        ("raw", AssetKind::RawJpgStack),
        ("only videos", AssetKind::Video),
        ("videos", AssetKind::Video),
        ("video", AssetKind::Video),
        ("movies", AssetKind::Video),
        ("clips", AssetKind::Video),
        ("only photos", AssetKind::Photo),
        ("photos", AssetKind::Photo),
        ("photo", AssetKind::Photo),
        ("pictures", AssetKind::Photo),
        ("picture", AssetKind::Photo),
        ("pics", AssetKind::Photo),
        ("images", AssetKind::Photo),
        ("image", AssetKind::Photo),
    ];

    for &(phrase, kind) in patterns {
        let (found, next) = strip_phrase_ci(text, phrase);
        if found {
            return (Some(kind), next);
        }
    }
    (None, text.to_string())
}

fn extract_favorite(text: &str) -> (Option<bool>, String) {
    for phrase in [
        "only favorites",
        "only favourites",
        "favorites only",
        "favourites only",
        "favorites",
        "favorite",
        "favourites",
        "favourite",
        "starred",
    ] {
        let (found, next) = strip_phrase_ci(text, phrase);
        if found {
            return (Some(true), next);
        }
    }
    (None, text.to_string())
}

fn extract_location(text: &str) -> (Option<String>, String) {
    const NON_LOC_WORDS: &[&str] = &[
        "the", "my", "all", "a", "an", "this", "that", "these", "those", "library", "folder",
        "album", "search",
    ];

    for trigger in ["in", "from", "at"] {
        if let Some((start, end)) = find_word_phrase_ci(text, trigger) {
            let after = text[end..].trim_start();
            let after_offset = text.len() - after.len();
            let (place, consumed) = take_clause_tokens(after, 3, CLAUSE_STOP_BOUNDARIES);
            if !place.is_empty()
                && !place
                    .split_whitespace()
                    .all(|w| NON_LOC_WORDS.contains(&w.to_ascii_lowercase().as_str()))
            {
                return (
                    Some(place),
                    remove_span(text, start, after_offset + consumed),
                );
            }
        }
    }

    (None, text.to_string())
}

fn extract_semantic_query(text: &str) -> Option<String> {
    const STOP_WORDS: &[&str] = &[
        "show",
        "me",
        "find",
        "search",
        "for",
        "get",
        "give",
        "all",
        "my",
        "the",
        "a",
        "an",
        "of",
        "only",
        "just",
        "with",
        "and",
        "or",
        "please",
        "some",
        "any",
        "from",
        "in",
        "at",
        "on",
        "that",
        "those",
        "these",
        "have",
        "has",
        "had",
        "are",
        "were",
        "was",
        "is",
        "instead",
        "about",
        "taken",
        "shot",
        "containing",
        "during",
    ];

    let words: Vec<String> = text
        .split_whitespace()
        .filter_map(|raw| {
            let cleaned = raw
                .trim_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '_' && c != '+');
            if cleaned.is_empty() {
                return None;
            }
            let lower = cleaned.to_ascii_lowercase();
            if STOP_WORDS.contains(&lower.as_str()) {
                return None;
            }
            Some(cleaned.to_string())
        })
        .collect();

    if words.is_empty() {
        None
    } else {
        Some(words.join(" "))
    }
}

fn has_any_filter(plan: &RouterPlan) -> bool {
    plan.semantic_query.is_some()
        || plan.ocr_terms.is_some()
        || !plan.person_names.is_empty()
        || plan.location_query.is_some()
        || plan.camera_model.is_some()
        || !plan.tags.is_empty()
        || plan.kind.is_some()
        || plan.favorite.is_some()
        || plan.date_from_ms.is_some()
        || plan.date_to_ms.is_some()
}

fn build_assistant_reply(plan: &RouterPlan) -> String {
    if !has_any_filter(plan) {
        if plan.clear_prior_filters {
            return "Cleared active search filters. What would you like to find?".to_string();
        }
        return "Tell me what photos or videos you are looking for.".to_string();
    }

    let mut parts = Vec::new();
    let noun = match plan.kind {
        Some(AssetKind::Video) => "videos",
        Some(AssetKind::LivePhoto) => "Live Photos",
        Some(AssetKind::MotionPhoto) => "Motion Photos",
        Some(AssetKind::RawJpgStack) => "RAW files",
        Some(AssetKind::Photo) => "photos",
        None => "items",
    };

    if plan.favorite == Some(true) {
        parts.push(format!("favorite {noun}"));
    } else {
        parts.push(noun.to_string());
    }

    if let Some(q) = &plan.semantic_query {
        parts.push(format!("matching \"{q}\""));
    }
    if let Some(ocr) = &plan.ocr_terms {
        parts.push(format!("with text \"{ocr}\""));
    }
    if !plan.person_names.is_empty() {
        parts.push(format!("featuring {}", plan.person_names.join(", ")));
    }
    if let Some(loc) = &plan.location_query {
        parts.push(format!("in {loc}"));
    }
    if let Some(cam) = &plan.camera_model {
        parts.push(format!("shot on {cam}"));
    }
    if !plan.tags.is_empty() {
        parts.push(format!("tagged {}", plan.tags.join(", ")));
    }
    if plan.date_from_ms.is_some() || plan.date_to_ms.is_some() {
        parts.push("within the selected date range".to_string());
    }

    format!("Searching for {}.", parts.join(" "))
}

fn build_suggestions(plan: &RouterPlan) -> Vec<String> {
    if !has_any_filter(plan) {
        return vec![
            "Favorites".to_string(),
            "Videos from last month".to_string(),
            "Photos from last summer".to_string(),
        ];
    }

    let mut suggestions = Vec::new();
    if plan.kind != Some(AssetKind::Video) {
        suggestions.push("Only videos".to_string());
    }
    if plan.favorite != Some(true) {
        suggestions.push("Favorites only".to_string());
    }
    suggestions.push("Start over".to_string());
    suggestions
}
