use crate::chat::schema::merge_router_plan;
use crate::chat::sidecar::{
    has_word, is_meta_library_question, plan_has_any_criteria, season_date_range_ms,
    strip_folder_count, strip_semantic_filler, try_apply_filter_removal,
};
use crate::chat::{ChatRouterBackend, ChatTurnInput, RerankCandidate, RerankResult};
use crate::AiError;
use chrono::{DateTime, Datelike, Duration as ChronoDuration, NaiveDate, TimeZone, Utc};
use std::time::Duration;
use truongphoto_core::{AssetKind, ChatTier, RouterPlan};

/// Tier 0 deterministic offline natural-language query router.
///
/// Parses dates, media kinds, favorites, RBAC-visible person names, negations/exclusions,
/// face counts, EXIF parameters (focal length, aperture, ISO, shutter), orientation,
/// superlative sorts, locations/folders, cameras, lenses, tags, OCR phrases, and semantic
/// keywords without requiring any LLM weights.
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

        // 0. Deterministic single-filter removal ("remove the location filter", clickable chip removal).
        let lower_raw = text.to_ascii_lowercase();
        if try_apply_filter_removal(&lower_raw, &input.prior_filters, &mut turn) {
            let mut merged = merge_router_plan(&input.prior_filters, &turn);
            merged.assistant_reply = build_assistant_reply(&merged);
            merged.follow_up_suggestions = build_suggestions(&merged);
            return merged;
        }

        // 0b. Informational meta-questions ("who are in my pics", "what cameras do I use", "hi", etc.)
        // Keep filtered count queries ("how many photos of Sebastian", "how many videos") as searches!
        let has_specific_count_filter = (lower_raw.contains("how many")
            || lower_raw.contains("total "))
            && (lower_raw.contains(" of ")
                || lower_raw.contains(" in ")
                || lower_raw.contains(" from ")
                || lower_raw.contains(" with ")
                || lower_raw.contains("how many video")
                || lower_raw.contains("how many favorite")
                || lower_raw.contains("how many favourite")
                || input
                    .known_person_names
                    .iter()
                    .any(|p| find_word_phrase_ci(&lower_raw, p).is_some()));

        if is_meta_library_question(&lower_raw) && !has_specific_count_filter {
            return RouterPlan {
                intent: Some("answer".to_string()),
                clear_prior_filters: true,
                ..Default::default()
            };
        }

        // 1. Clear / reset triggers & implicit new-topic detection.
        let (mut cleared, after_clear) = extract_clear_trigger(&text);
        text = after_clear;

        // Implicit topic switch: "show me ...", "find ...", "search ..." starts a fresh search
        // unless the user used a refinement cue ("only", "also", "narrow", "just", "from those", etc.).
        if !cleared && plan_has_any_criteria(&input.prior_filters) {
            let starts_fresh = [
                "show me ",
                "find ",
                "search ",
                "get me ",
                "give me ",
                "pull up ",
                "what did i shoot",
                "what have i shot",
            ]
            .iter()
            .any(|p| lower_raw.starts_with(p));
            let is_refinement = [
                "only ",
                "just ",
                "also ",
                "narrow",
                "from those",
                "in those",
                "of those",
                "from these",
                "of these",
                "among ",
                "within ",
                "without ",
                "except ",
                "but ",
            ]
            .iter()
            .any(|p| lower_raw.contains(p));
            if starts_fresh && !is_refinement {
                cleared = true;
            }
        }
        turn.clear_prior_filters = cleared;

        // 1b. Entity swap ("what about AJ?", "how about Sebastian?") replaces prior person_names
        // while keeping other active filters.
        let is_entity_swap = lower_raw.starts_with("what about ")
            || lower_raw.starts_with("how about ")
            || lower_raw.starts_with("now show ")
            || lower_raw.starts_with("switch to ");

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

        // 4. Negations & exclusions (must run BEFORE positive people/location/kind extractors).
        let (excl_people, after_ep) = extract_excluded_people(&text, &input.known_person_names);
        turn.exclude_person_names = excl_people;
        text = after_ep;

        let (excl_loc, after_el) = extract_excluded_location(&text, &input.library.known_folders);
        turn.exclude_location_query = excl_loc;
        text = after_el;

        for phrase in [
            "no videos",
            "no video",
            "not videos",
            "not video",
            "without videos",
            "without video",
            "excluding videos",
        ] {
            let (found, next) = strip_phrase_ci(&text, phrase);
            if found {
                turn.exclude_kind = Some(AssetKind::Video);
                text = next;
                break;
            }
        }

        for neg_phrase in [
            "without any people",
            "without people",
            "no people",
            "without faces",
            "no faces",
            "unpeopled",
            "nobody",
        ] {
            let (found, next) = strip_phrase_ci(&text, neg_phrase);
            if found {
                turn.has_people = Some(false);
                text = next;
            }
        }

        // 5. Known RBAC-visible person names (positive).
        let (people, after_people) = extract_known_people(&text, &input.known_person_names);
        turn.person_names = people;
        text = after_people;

        // 5b. Face-count cues ("solo", "couple", "group shot").
        let (min_f, max_f, after_fc) = extract_face_counts(&text, !turn.person_names.is_empty());
        turn.min_faces = min_f;
        turn.max_faces = max_f;
        text = after_fc;

        // 6. EXIF filters (focal length, aperture, ISO, shutter speed) — run BEFORE camera/location
        // so `"shot at f/1.4"` or `"at 1/1000s"` or `"at ISO 3200"` is never misparsed as location!
        text = extract_exif_filters(&text, &mut turn);

        // 7. Orientation & superlative sort cues.
        let (orient, after_orient) = extract_orientation(&text);
        turn.orientation = orient;
        text = after_orient;

        let (sort_by, after_sort) = extract_sort_by(&text);
        turn.sort_by = sort_by;
        text = after_sort;

        // 8. Cameras & lenses (both explicit triggers AND direct matches against known library gear).
        let (lens, after_lens) = extract_known_lens(&text, &input.known_lenses);
        turn.lens_model = lens;
        text = after_lens;

        let (camera, after_camera) = extract_camera(&text, &input.known_cameras);
        turn.camera_model = camera;
        text = after_camera;

        // 9. Tags (`tagged ...`, `tag ...`, `#hashtag`).
        let (tags, after_tags) = extract_tags(&text);
        turn.tags = tags;
        text = after_tags;

        // 10. Media kinds (`only videos`, `live photos`, `motion photos`, `raw`, `photos`).
        let (kind, after_kind) = extract_kind(&text);
        turn.kind = kind;
        text = after_kind;

        // 11. Favorites (`favorites`, `favourites`, `starred`).
        let (fav, after_fav) = extract_favorite(&text);
        turn.favorite = fav;
        text = after_fav;

        // 12. Known library folders & locations (`in <Place>`, `from <Place>`, `at <Place>`).
        let (location, after_loc) = extract_location(&text, &input.library.known_folders);
        turn.location_query = location;
        text = after_loc;

        // 13. Broad people presence ("show me people" / "my friends") when no specific person named.
        if turn.has_people.is_none() && turn.person_names.is_empty() {
            for kw in ["my friends", "friends", "people", "everyone", "folks"] {
                let (found, next) = strip_phrase_ci(&text, kw);
                if found {
                    turn.has_people = Some(true);
                    text = next;
                    break;
                }
            }
        }

        // 14. Remaining meaningful content terms become `semantic_query` (cleaned with shared filler stripper).
        let raw_semantic = extract_semantic_query(&text);
        turn.semantic_query = raw_semantic.and_then(|s| {
            let cleaned = strip_semantic_filler(&s)?;
            let low = cleaned.to_ascii_lowercase();
            if turn.orientation.is_some()
                && matches!(
                    low.as_str(),
                    "wallpaper"
                        | "wallpapers"
                        | "phone wallpaper"
                        | "desktop wallpaper"
                        | "background"
                        | "lock screen"
                )
            {
                None
            } else {
                Some(cleaned)
            }
        });

        // Apply entity swap if requested ("what about AJ?").
        let base_prior = if is_entity_swap && !turn.person_names.is_empty() {
            let mut p = input.prior_filters.clone();
            p.person_names.clear();
            p
        } else {
            input.prior_filters.clone()
        };

        // 15. Vague/exploratory browsing rescue when no explicit filter or semantic term survived.
        let mut merged = merge_router_plan(&base_prior, &turn);
        if !plan_has_any_criteria(&merged) && !turn.clear_prior_filters {
            let wants_recent = [
                "recent",
                "latest",
                "newest",
                "what did i shoot",
                "what have i shot",
                "everything",
                "all my",
                "my photos",
                "my pics",
                "browse",
            ]
            .iter()
            .any(|kw| lower_raw.contains(kw));
            let wants_best = [
                "cool",
                "best",
                "favorite",
                "favourite",
                "highlight",
                "standout",
                "surprise",
                "interesting",
                "awesome",
                "amazing",
                "great",
                "nice",
                "good",
                "random",
                "throwback",
                "memories",
            ]
            .iter()
            .any(|kw| has_word(&lower_raw, kw) || lower_raw.contains(kw));

            if wants_recent {
                merged.sort_by = Some("capture_desc".to_string());
                merged.clear_prior_filters = true;
            } else if wants_best {
                merged.sort_by = Some("best".to_string());
                merged.clear_prior_filters = true;
            }
        }

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

fn shift_month(year: i32, month: u32, delta: i32) -> (i32, u32) {
    let total = year * 12 + (month as i32 - 1) + delta;
    let ny = total.div_euclid(12);
    let nm = (total.rem_euclid(12) + 1) as u32;
    (ny, nm)
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
    for prep in ["during", "from", "in", "on", "of", "since", "past", "over"] {
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
    let lower = text.to_ascii_lowercase();

    // 1. "last/past N days/weeks/months" (e.g., "last 30 days", "past 2 weeks", "last 3 months").
    let tokens = tokenize_with_spans(text);
    for win in tokens.windows(3) {
        let (w0, s0, _) = win[0];
        let (w1, _, _) = win[1];
        let (w2, _, e2) = win[2];
        if (w0.eq_ignore_ascii_case("last") || w0.eq_ignore_ascii_case("past"))
            && w1.chars().all(|c| c.is_ascii_digit())
        {
            if let Ok(n) = w1.parse::<i64>() {
                if (1..=3650).contains(&n) {
                    let start_opt = match w2.to_ascii_lowercase().as_str() {
                        "day" | "days" => Some(today - ChronoDuration::days(n)),
                        "week" | "weeks" => Some(today - ChronoDuration::days(n * 7)),
                        "month" | "months" => Some(today - ChronoDuration::days(n * 30)),
                        _ => None,
                    };
                    if let Some(d_start) = start_opt {
                        let span_start = strip_optional_leading_prep(text, s0);
                        return (
                            Some(date_range_ms(d_start, today)),
                            remove_span(text, span_start, e2),
                        );
                    }
                }
            }
        }
    }

    // 2. "between <Month1> and <Month2> [YYYY]" or "between <YYYY1> and <YYYY2>".
    for (idx, &(w, s, _)) in tokens.iter().enumerate() {
        if !w.eq_ignore_ascii_case("between") || idx + 3 >= tokens.len() {
            continue;
        }
        let (a_tok, _, _) = tokens[idx + 1];
        let (and_tok, _, _) = tokens[idx + 2];
        let (b_tok, _, b_end) = tokens[idx + 3];
        if !and_tok.eq_ignore_ascii_case("and") {
            continue;
        }
        if let (Some(y1), Some(y2)) = (parse_year_token(a_tok), parse_year_token(b_tok)) {
            let (lo, hi) = if y1 <= y2 { (y1, y2) } else { (y2, y1) };
            let start = NaiveDate::from_ymd_opt(lo, 1, 1).unwrap();
            let end = NaiveDate::from_ymd_opt(hi, 12, 31).unwrap();
            return (Some(date_range_ms(start, end)), remove_span(text, s, b_end));
        }
        if let (Some(m1), Some(m2)) = (parse_month_name(a_tok), parse_month_name(b_tok)) {
            let (target_y, end_span) = if idx + 4 < tokens.len() {
                if let Some(y_num) = parse_year_token(tokens[idx + 4].0) {
                    (y_num, tokens[idx + 4].2)
                } else {
                    (if m2 <= month { year } else { year - 1 }, b_end)
                }
            } else {
                (if m2 <= month { year } else { year - 1 }, b_end)
            };
            let (lo, hi) = if m1 <= m2 { (m1, m2) } else { (m2, m1) };
            let start = NaiveDate::from_ymd_opt(target_y, lo, 1).unwrap();
            let end = NaiveDate::from_ymd_opt(target_y, hi, days_in_month(target_y, hi)).unwrap();
            return (
                Some(date_range_ms(start, end)),
                remove_span(text, s, end_span),
            );
        }
    }

    // 3. "before <Month/YYYY>" / "after <Month/YYYY>".
    for (idx, &(w, s, _)) in tokens.iter().enumerate() {
        let is_before = w.eq_ignore_ascii_case("before");
        let is_after = w.eq_ignore_ascii_case("after") || w.eq_ignore_ascii_case("since");
        if (!is_before && !is_after) || idx + 1 >= tokens.len() {
            continue;
        }
        let (next_w, _, next_e) = tokens[idx + 1];
        if let Some(y_num) = parse_year_token(next_w) {
            let (start, end) = if is_before {
                (
                    NaiveDate::from_ymd_opt(1970, 1, 1).unwrap(),
                    NaiveDate::from_ymd_opt(y_num - 1, 12, 31).unwrap(),
                )
            } else {
                (NaiveDate::from_ymd_opt(y_num, 1, 1).unwrap(), today)
            };
            return (
                Some(date_range_ms(start, end)),
                remove_span(text, s, next_e),
            );
        }
        if let Some(m_num) = parse_month_name(next_w) {
            let (target_y, span_end) = if idx + 2 < tokens.len() {
                if let Some(y_num) = parse_year_token(tokens[idx + 2].0) {
                    (y_num, tokens[idx + 2].2)
                } else {
                    (if m_num <= month { year } else { year - 1 }, next_e)
                }
            } else {
                (if m_num <= month { year } else { year - 1 }, next_e)
            };
            let (start, end) = if is_before {
                let first = NaiveDate::from_ymd_opt(target_y, m_num, 1).unwrap();
                (
                    NaiveDate::from_ymd_opt(1970, 1, 1).unwrap(),
                    first.pred_opt().unwrap_or(first),
                )
            } else {
                (NaiveDate::from_ymd_opt(target_y, m_num, 1).unwrap(), today)
            };
            return (
                Some(date_range_ms(start, end)),
                remove_span(text, s, span_end),
            );
        }
    }

    // 4. Relative phrases.
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
        ("two months ago", |_, y, m| {
            let (py, pm) = shift_month(y, m, -2);
            (
                NaiveDate::from_ymd_opt(py, pm, 1).unwrap(),
                NaiveDate::from_ymd_opt(py, pm, days_in_month(py, pm)).unwrap(),
            )
        }),
        ("2 months ago", |_, y, m| {
            let (py, pm) = shift_month(y, m, -2);
            (
                NaiveDate::from_ymd_opt(py, pm, 1).unwrap(),
                NaiveDate::from_ymd_opt(py, pm, days_in_month(py, pm)).unwrap(),
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
        ("last weekend", |t, _, _| {
            let days_from_mon = t.weekday().num_days_from_monday() as i64;
            if days_from_mon == 6 {
                (t - ChronoDuration::days(1), t)
            } else {
                let sat = t - ChronoDuration::days(days_from_mon + 2);
                (sat, sat + ChronoDuration::days(1))
            }
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

    // 5. Christmas / Xmas (+ optional explicit year).
    for holiday in ["christmas", "xmas"] {
        if let Some((start, end)) = find_word_phrase_ci(text, holiday) {
            let cy = crate::chat::sidecar::extract_explicit_year(&lower).unwrap_or(
                if today.month() == 12 && today.day() >= 24 {
                    year
                } else {
                    year - 1
                },
            );
            let d_start = NaiveDate::from_ymd_opt(cy, 12, 24).unwrap();
            let d_end = NaiveDate::from_ymd_opt(cy, 12, 26).unwrap();
            let span_start = strip_optional_leading_prep(text, start);
            let mut cleaned = remove_span(text, span_start, end);
            let cy_str = cy.to_string();
            let (_, next) = strip_phrase_ci(&cleaned, &cy_str);
            cleaned = next;
            return (Some(date_range_ms(d_start, d_end)), cleaned);
        }
    }

    // 6. Standalone seasons (+ optional explicit year like "summer 2025").
    if let Some((from_ms, to_ms)) = season_date_range_ms(&lower, now_utc) {
        let mut cleaned = text.to_string();
        for s_word in ["summer", "spring", "autumn", "fall", "winter"] {
            if let Some((start, end)) = find_word_phrase_ci(&cleaned, s_word) {
                let span_start = strip_optional_leading_prep(&cleaned, start);
                cleaned = remove_span(&cleaned, span_start, end);
                break;
            }
        }
        if let Some(ey) = crate::chat::sidecar::extract_explicit_year(&lower) {
            let ey_str = ey.to_string();
            let (_, next) = strip_phrase_ci(&cleaned, &ey_str);
            cleaned = next;
        }
        return (Some((from_ms, to_ms)), cleaned);
    }

    // 7. "<Month> <YYYY>" e.g. "March 2025".
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

    // 8. "in YYYY" / "from YYYY" / "during YYYY" or standalone 4-digit year.
    // Skip 4-digit numbers preceded by "iso" or "1/" (e.g. "ISO 2000", "1/2000").
    for (word, s, e) in &tokens {
        if let Some(y_num) = parse_year_token(word) {
            let before = text[..*s].trim_end();
            if before.ends_with('/')
                || before.strip_suffix_ci_word("iso").is_some()
                || before.ends_with("iso")
            {
                continue;
            }
            let span_start = strip_optional_leading_prep(text, *s);
            let start = NaiveDate::from_ymd_opt(y_num, 1, 1).unwrap();
            let end = NaiveDate::from_ymd_opt(y_num, 12, 31).unwrap();
            return (
                Some(date_range_ms(start, end)),
                remove_span(text, span_start, *e),
            );
        }
    }

    // 9. Standalone month name ("in June", "October") -> most recent occurrence of that month.
    for (word, s, e) in &tokens {
        if let Some(m_num) = parse_month_name(word) {
            let prefix_before = text[..*s].trim_end();
            let has_prep = ["in", "during", "from", "of", "on"]
                .iter()
                .any(|p| prefix_before.strip_suffix_ci_word(p).is_some());
            if word.eq_ignore_ascii_case("may") && !has_prep {
                continue;
            }
            let target_year = if m_num <= month { year } else { year - 1 };
            let span_start = strip_optional_leading_prep(text, *s);
            let start = NaiveDate::from_ymd_opt(target_year, m_num, 1).unwrap();
            let end =
                NaiveDate::from_ymd_opt(target_year, m_num, days_in_month(target_year, m_num))
                    .unwrap();
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

fn extract_excluded_people(text: &str, known_names: &[String]) -> (Vec<String>, String) {
    let mut sorted: Vec<&str> = known_names
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    sorted.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));

    let mut current = text.to_string();
    let mut excluded = Vec::new();

    for name in &sorted {
        let first = name.split_whitespace().next().unwrap_or(name);
        let candidates: Vec<&str> = if first.len() >= 2 && !first.eq_ignore_ascii_case(name) {
            vec![*name, first]
        } else {
            vec![*name]
        };
        for cand in candidates {
            for neg in ["without", "except", "excluding", "no", "not"] {
                let phrase = format!("{neg} {cand}");
                if let Some((s, e)) = find_word_phrase_ci(&current, &phrase) {
                    if !excluded
                        .iter()
                        .any(|m: &String| m.eq_ignore_ascii_case(name))
                    {
                        excluded.push((*name).to_string());
                    }
                    current = remove_span(&current, s, e);
                    break;
                }
            }
        }
    }

    (excluded, current)
}

fn extract_excluded_location(text: &str, known_folders: &[String]) -> (Option<String>, String) {
    for trigger in [
        "not from",
        "not at",
        "not in",
        "except at",
        "except in",
        "except from",
        "excluding",
        "except",
    ] {
        if let Some((start, end)) = find_word_phrase_ci(text, trigger) {
            let after = text[end..].trim_start();
            let after_offset = text.len() - after.len();
            let (raw_place, consumed) = take_clause_tokens(after, 3, CLAUSE_STOP_BOUNDARIES);
            let place = clean_location_candidate(&raw_place);
            if !place.is_empty() && is_plausible_location(&place, known_folders) {
                return (
                    Some(place),
                    remove_span(text, start, after_offset + consumed),
                );
            }
        }
    }
    (None, text.to_string())
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

    for name in &sorted {
        if let Some((start, end)) = find_word_phrase_ci(&current, name) {
            if !matched
                .iter()
                .any(|m: &String| m.eq_ignore_ascii_case(name))
            {
                matched.push((*name).to_string());
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

    // Fallback: unique first-name match (e.g. "Nate" -> "Nate Higgers").
    for name in &sorted {
        let Some(first) = name.split_whitespace().next() else {
            continue;
        };
        if first.len() < 2 || first.eq_ignore_ascii_case(name) {
            continue;
        }
        let is_unique = sorted
            .iter()
            .filter(|other| {
                other
                    .split_whitespace()
                    .next()
                    .is_some_and(|w| w.eq_ignore_ascii_case(first))
            })
            .count()
            == 1;
        if !is_unique {
            continue;
        }
        if let Some((start, end)) = find_word_phrase_ci(&current, first) {
            if !matched
                .iter()
                .any(|m: &String| m.eq_ignore_ascii_case(name))
            {
                matched.push((*name).to_string());
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

fn extract_face_counts(text: &str, has_named_person: bool) -> (Option<u32>, Option<u32>, String) {
    let mut current = text.to_string();
    for phrase in [
        "by themselves",
        "by himself",
        "by herself",
        "solo shot",
        "solo photo",
        "solo",
    ] {
        let (found, next) = strip_phrase_ci(&current, phrase);
        if found {
            current = next;
            let min_f = if has_named_person { None } else { Some(1) };
            return (min_f, Some(1), current);
        }
    }
    if has_named_person {
        let (found, next) = strip_phrase_ci(&current, "alone");
        if found {
            return (None, Some(1), next);
        }
    }
    for phrase in ["two people", "2 people", "couple"] {
        let (found, next) = strip_phrase_ci(&current, phrase);
        if found {
            return (Some(2), Some(2), next);
        }
    }
    for phrase in [
        "group shots",
        "group shot",
        "group photos",
        "group photo",
        "group pics",
        "group pic",
        "crowd",
    ] {
        let (found, next) = strip_phrase_ci(&current, phrase);
        if found {
            return (Some(3), None, next);
        }
    }
    (None, None, current)
}

fn extract_exif_filters(text: &str, turn: &mut RouterPlan) -> String {
    let mut current = text.to_string();
    let lower = current.to_ascii_lowercase();

    let has_upper_word = |prefix: &str| {
        let p = prefix.trim_end().to_ascii_lowercase();
        p.ends_with("under")
            || p.ends_with("below")
            || p.ends_with("less than")
            || p.ends_with("wider than")
            || p.ends_with("shorter than")
            || p.ends_with("faster than")
            || p.ends_with("<=")
            || p.ends_with('<')
    };
    let has_lower_word = |prefix: &str| {
        let p = prefix.trim_end().to_ascii_lowercase();
        p.ends_with("over")
            || p.ends_with("above")
            || p.ends_with("more than")
            || p.ends_with("at least")
            || p.ends_with("longer than")
            || p.ends_with("narrower than")
            || p.ends_with("slower than")
            || p.ends_with("higher than")
            || p.ends_with(">=")
            || p.ends_with('>')
    };

    // 1. Focal length numeric (e.g., "12mm", "under 24mm", "over 100mm", "24-70mm", "50mm+").
    let focal_scan = current.clone();
    let mut focal_matches: Vec<(usize, usize, f64, bool, bool)> = Vec::new();
    for (idx, _) in focal_scan.to_ascii_lowercase().match_indices("mm") {
        let before = &focal_scan[..idx];
        let num_start = before
            .rfind(|c: char| !c.is_ascii_digit() && c != '.')
            .map(|i| i + 1)
            .unwrap_or(0);
        let num_str = &before[num_start..];
        if num_str.is_empty() || !num_str.chars().any(|c| c.is_ascii_digit()) {
            continue;
        }
        let after_end = idx + 2;
        if focal_scan[after_end..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic())
        {
            continue;
        }
        if let Ok(val) = num_str.parse::<f64>() {
            if (4.0..=2000.0).contains(&val) {
                let plus_end = if focal_scan[after_end..].starts_with('+') {
                    after_end + 1
                } else {
                    after_end
                };
                let prefix = &focal_scan[..num_start];
                let is_upper = has_upper_word(prefix);
                let is_lower = has_lower_word(prefix) || plus_end > after_end;
                focal_matches.push((num_start, plus_end, val, is_lower, is_upper));
            }
        }
    }
    if let Some(&(s, e, val, is_lower, is_upper)) = focal_matches.first() {
        let after_lower = current[e..].trim_start().to_ascii_lowercase();
        let suffix_lower = after_lower.starts_with("or more")
            || after_lower.starts_with("or longer")
            || after_lower.starts_with("or higher");
        let suffix_upper = after_lower.starts_with("or less")
            || after_lower.starts_with("or wider")
            || after_lower.starts_with("or shorter");
        if is_lower || suffix_lower {
            turn.focal_length_min_mm = Some(val);
        } else if is_upper || suffix_upper {
            turn.focal_length_max_mm = Some(val);
        } else {
            turn.focal_length_min_mm = Some(val);
            turn.focal_length_max_mm = Some(val);
        }
        let span_start = strip_optional_leading_prep(&current, s);
        current = remove_span(&current, span_start, e);
    } else {
        for (phrase, min_mm, max_mm) in [
            ("ultrawide", None, Some(20.0)),
            ("ultra-wide", None, Some(20.0)),
            ("ultra wide", None, Some(20.0)),
            ("fisheye", None, Some(16.0)),
            ("wide angle", None, Some(28.0)),
            ("wide-angle", None, Some(28.0)),
            ("telephoto", Some(85.0), None),
        ] {
            let (found, next) = strip_phrase_ci(&current, phrase);
            if found {
                turn.focal_length_min_mm = min_mm;
                turn.focal_length_max_mm = max_mm;
                current = next;
                break;
            }
        }
    }

    // 2. Aperture numeric ("f/1.4", "f1.8", "wider than f/2", "f/8 or narrower") or qualitative ("wide open", "bokeh").
    let ap_scan = current.clone();
    let ap_lower = ap_scan.to_ascii_lowercase();
    let mut ap_found = false;
    for (idx, _) in ap_lower.match_indices('f') {
        let left_ok = idx == 0
            || ap_scan[..idx]
                .chars()
                .next_back()
                .is_none_or(|c| !c.is_alphanumeric());
        if !left_ok {
            continue;
        }
        let rest = &ap_scan[idx + 1..];
        let (num_offset, num_slice) = if let Some(after_slash) = rest.strip_prefix('/') {
            (idx + 2, after_slash)
        } else if rest.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            (idx + 1, rest)
        } else {
            continue;
        };
        let num_len = num_slice
            .find(|c: char| !c.is_ascii_digit() && c != '.')
            .unwrap_or(num_slice.len());
        if num_len == 0 {
            continue;
        }
        if let Ok(val) = num_slice[..num_len].parse::<f64>() {
            if (0.7..=64.0).contains(&val) {
                let end = num_offset + num_len;
                let prefix = &ap_scan[..idx];
                let after = ap_scan[end..].trim_start().to_ascii_lowercase();
                if has_upper_word(prefix)
                    || after.starts_with("or wider")
                    || after.starts_with("or lower")
                {
                    turn.aperture_max_f = Some(val);
                } else if has_lower_word(prefix)
                    || after.starts_with("or narrower")
                    || after.starts_with("or higher")
                {
                    turn.aperture_min_f = Some(val);
                } else {
                    turn.aperture_min_f = Some(val);
                    turn.aperture_max_f = Some(val);
                }
                let span_start = strip_optional_leading_prep(&current, idx);
                current = remove_span(&current, span_start, end);
                ap_found = true;
                break;
            }
        }
    }
    if !ap_found {
        for phrase in [
            "shallow depth of field",
            "blurry background",
            "wide open",
            "wide-open",
            "bokeh",
        ] {
            let (found, next) = strip_phrase_ci(&current, phrase);
            if found {
                turn.aperture_max_f = Some(2.0);
                current = next;
                break;
            }
        }
    }

    // 3. ISO numeric ("ISO 6400", "ISO 3200+", "over ISO 1600") or qualitative ("high ISO", "grainy", "low ISO").
    if let Some((start, end)) = find_word_phrase_ci(&current, "iso") {
        let after = current[end..].trim_start();
        let after_offset = current.len() - after.len();
        let digits_len = after
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(after.len());
        if digits_len > 0 {
            if let Ok(val) = after[..digits_len].parse::<u32>() {
                if (25..=1_000_000).contains(&val) {
                    let plus = after[digits_len..].starts_with('+');
                    let num_end = after_offset + digits_len + usize::from(plus);
                    let prefix = &current[..start];
                    let tail = current[num_end..].trim_start().to_ascii_lowercase();
                    if plus
                        || has_lower_word(prefix)
                        || tail.starts_with("or higher")
                        || tail.starts_with("or more")
                    {
                        turn.iso_min = Some(val);
                    } else if has_upper_word(prefix)
                        || tail.starts_with("or lower")
                        || tail.starts_with("or less")
                    {
                        turn.iso_max = Some(val);
                    } else {
                        turn.iso_min = Some(val);
                        turn.iso_max = Some(val);
                    }
                    let span_start = strip_optional_leading_prep(&current, start);
                    current = remove_span(&current, span_start, num_end);
                }
            }
        }
    }
    if turn.iso_min.is_none() && turn.iso_max.is_none() {
        for phrase in ["high iso", "grainy", "noisy"] {
            let (found, next) = strip_phrase_ci(&current, phrase);
            if found {
                turn.iso_min = Some(3200);
                current = next;
                break;
            }
        }
    }
    if turn.iso_min.is_none() && turn.iso_max.is_none() {
        for phrase in ["low iso", "base iso"] {
            let (found, next) = strip_phrase_ci(&current, phrase);
            if found {
                turn.iso_max = Some(200);
                current = next;
                break;
            }
        }
    }

    // 4. Shutter speed ("1/1000s", "1/500", "2s", "long exposure", "silky", "slow shutter").
    let sh_scan = current.clone();
    let mut sh_found = false;
    if let Some(idx) = sh_scan.find("1/") {
        let left_ok = idx == 0
            || sh_scan[..idx]
                .chars()
                .next_back()
                .is_none_or(|c| !c.is_alphanumeric());
        if left_ok {
            let after_slash = &sh_scan[idx + 2..];
            let d_len = after_slash
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(after_slash.len());
            if d_len > 0 {
                if let Ok(denom) = after_slash[..d_len].parse::<f64>() {
                    if (2.0..=64000.0).contains(&denom) {
                        let mut end = idx + 2 + d_len;
                        if sh_scan[end..].starts_with('s')
                            && !sh_scan[end + 1..]
                                .chars()
                                .next()
                                .is_some_and(|c| c.is_ascii_alphabetic())
                        {
                            end += 1;
                        }
                        let sec = 1.0 / denom;
                        let prefix = &sh_scan[..idx];
                        let tail = sh_scan[end..].trim_start().to_ascii_lowercase();
                        if has_upper_word(prefix)
                            || tail.starts_with("or faster")
                            || lower.contains("fast shutter")
                        {
                            turn.shutter_speed_max_sec = Some(sec);
                        } else if has_lower_word(prefix)
                            || tail.starts_with("or slower")
                            || tail.starts_with("or longer")
                        {
                            turn.shutter_speed_min_sec = Some(sec);
                        } else {
                            turn.shutter_speed_min_sec = Some(sec);
                            turn.shutter_speed_max_sec = Some(sec);
                        }
                        let span_start = strip_optional_leading_prep(&current, idx);
                        current = remove_span(&current, span_start, end);
                        sh_found = true;
                    }
                }
            }
        }
    }
    if !sh_found {
        for phrase in ["long exposures", "long exposure", "slow shutter", "silky"] {
            let (found, next) = strip_phrase_ci(&current, phrase);
            if found {
                turn.shutter_speed_min_sec = Some(0.25);
                current = next;
                break;
            }
        }
    }

    // Clean stray comparison qualifiers left behind when EXIF filters were consumed.
    if turn.focal_length_min_mm.is_some()
        || turn.focal_length_max_mm.is_some()
        || turn.aperture_min_f.is_some()
        || turn.aperture_max_f.is_some()
        || turn.iso_min.is_some()
        || turn.iso_max.is_some()
        || turn.shutter_speed_min_sec.is_some()
        || turn.shutter_speed_max_sec.is_some()
    {
        for stray in [
            "or faster",
            "or slower",
            "or longer",
            "or wider",
            "or narrower",
            "or higher",
            "or lower",
            "or more",
            "or less",
            "wider than",
            "narrower than",
            "faster than",
            "slower than",
            "longer than",
            "shorter than",
            "higher than",
            "lower than",
            "more than",
            "less than",
            "at least",
            "under",
            "over",
            "above",
            "below",
            "at",
        ] {
            let (found, next) = strip_phrase_ci(&current, stray);
            if found {
                current = next;
            }
        }
    }

    current
}

fn extract_orientation(text: &str) -> (Option<String>, String) {
    let patterns: &[(&str, &str)] = &[
        ("panoramas", "panorama"),
        ("panorama", "panorama"),
        ("panoramic", "panorama"),
        ("pano", "panorama"),
        ("phone wallpapers", "portrait"),
        ("phone wallpaper", "portrait"),
        ("phone background", "portrait"),
        ("lock screen", "portrait"),
        ("portrait orientation", "portrait"),
        ("portrait mode", "portrait"),
        ("only portrait", "portrait"),
        ("portrait only", "portrait"),
        ("vertical", "portrait"),
        ("desktop wallpapers", "landscape"),
        ("desktop wallpaper", "landscape"),
        ("desktop background", "landscape"),
        ("only landscape", "landscape"),
        ("landscape only", "landscape"),
        ("landscape orientation", "landscape"),
        ("horizontal", "landscape"),
        ("square", "square"),
        ("1:1", "square"),
    ];
    for &(phrase, orient) in patterns {
        let (found, next) = strip_phrase_ci(text, phrase);
        if found {
            return (Some(orient.to_string()), next);
        }
    }
    (None, text.to_string())
}

fn extract_sort_by(text: &str) -> (Option<String>, String) {
    let patterns: &[(&str, &str)] = &[
        ("widest angle", "focal_asc"),
        ("widest lens", "focal_asc"),
        ("widest shot", "focal_asc"),
        ("widest", "focal_asc"),
        ("most zoomed in", "focal_desc"),
        ("most zoomed", "focal_desc"),
        ("longest lens", "focal_desc"),
        ("highest iso", "iso_desc"),
        ("noisiest", "iso_desc"),
        ("lowest iso", "iso_asc"),
        ("cleanest iso", "iso_asc"),
        ("fastest shutter", "shutter_asc"),
        ("longest exposure", "shutter_desc"),
        ("slowest shutter", "shutter_desc"),
        ("longest videos", "duration_desc"),
        ("longest video", "duration_desc"),
        ("oldest", "capture_asc"),
        ("earliest", "capture_asc"),
        ("newest", "capture_desc"),
        ("latest", "capture_desc"),
        ("most recent", "capture_desc"),
        ("recent", "capture_desc"),
        ("recently", "capture_desc"),
        ("widest aperture", "aperture_asc"),
    ];
    for &(phrase, sort_key) in patterns {
        let (found, next) = strip_phrase_ci(text, phrase);
        if found {
            return (Some(sort_key.to_string()), next);
        }
    }
    (None, text.to_string())
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
        while pos < after.len() && after[pos..].starts_with(char::is_whitespace) {
            let ch = after[pos..].chars().next().unwrap();
            pos += ch.len_utf8();
        }
        if pos >= after.len() {
            break;
        }
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

fn extract_known_lens(text: &str, known_lenses: &[String]) -> (Option<String>, String) {
    let mut sorted: Vec<&str> = known_lenses
        .iter()
        .map(|s| strip_folder_count(s))
        .filter(|s| !s.is_empty())
        .collect();
    sorted.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));

    for lens in &sorted {
        if let Some((s, e)) = find_word_phrase_ci(text, lens) {
            let span_start = strip_optional_leading_prep(text, s);
            return (Some((*lens).to_string()), remove_span(text, span_start, e));
        }
    }
    (None, text.to_string())
}

fn extract_camera(text: &str, known_cameras: &[String]) -> (Option<String>, String) {
    // 1. Direct match against known cameras in the user's library (longest-first).
    let mut sorted: Vec<&str> = known_cameras
        .iter()
        .map(|s| strip_folder_count(s))
        .filter(|s| !s.is_empty())
        .collect();
    sorted.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));

    for cam in &sorted {
        if let Some((start, end)) = find_word_phrase_ci(text, cam) {
            let prefix = text[..start].trim_end();
            let mut span_start = start;
            for trig in ["shot on", "taken on", "taken with", "with", "on", "camera"] {
                if let Some(stripped) = prefix.strip_suffix_ci_word(trig) {
                    span_start = stripped.len();
                    break;
                }
            }
            return (Some((*cam).to_string()), remove_span(text, span_start, end));
        }
    }

    // 2. Explicit camera phrasing ("shot on Fujifilm X100VI").
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
    let explicit_patterns: &[(&str, AssetKind)] = &[
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
        ("photos only", AssetKind::Photo),
        ("still photos", AssetKind::Photo),
    ];

    for &(phrase, kind) in explicit_patterns {
        let (found, next) = strip_phrase_ci(text, phrase);
        if found {
            return (Some(kind), next);
        }
    }

    let mut cleaned = text.to_string();
    for generic in [
        "photos", "photo", "pictures", "picture", "pics", "images", "image",
    ] {
        let (found, next) = strip_phrase_ci(&cleaned, generic);
        if found {
            cleaned = next;
        }
    }
    (None, cleaned)
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

fn clean_location_candidate(raw: &str) -> String {
    let mut parts: Vec<&str> = raw.split_whitespace().collect();
    while let Some(first) = parts.first() {
        if first.eq_ignore_ascii_case("the")
            || first.eq_ignore_ascii_case("my")
            || first.eq_ignore_ascii_case("our")
        {
            parts.remove(0);
        } else {
            break;
        }
    }
    while let Some(last) = parts.last() {
        if matches!(
            last.to_ascii_lowercase().as_str(),
            "trip"
                | "trips"
                | "vacation"
                | "visit"
                | "folder"
                | "album"
                | "event"
                | "pics"
                | "photos"
        ) {
            parts.pop();
        } else {
            break;
        }
    }
    parts.join(" ")
}

fn is_plausible_location(place: &str, known_folders: &[String]) -> bool {
    let low = place.trim().to_ascii_lowercase();
    if low.is_empty() {
        return false;
    }

    // Always allow if it matches a known folder name.
    if known_folders.iter().any(|f| {
        let clean = strip_folder_count(f).to_ascii_lowercase();
        clean == low || clean.contains(&low)
    }) {
        return true;
    }

    const NON_LOC_WORDS: &[&str] = &[
        "the",
        "my",
        "all",
        "a",
        "an",
        "this",
        "that",
        "these",
        "those",
        "library",
        "folder",
        "album",
        "search",
        "sun",
        "sunlight",
        "shade",
        "shadow",
        "shadows",
        "dark",
        "darkness",
        "night",
        "sunset",
        "sunrise",
        "dawn",
        "dusk",
        "golden",
        "hour",
        "rain",
        "snow",
        "fog",
        "mist",
        "clouds",
        "sky",
        "water",
        "air",
        "flight",
        "motion",
        "action",
        "focus",
        "silhouette",
        "black",
        "white",
        "color",
        "colour",
        "monochrome",
        "sepia",
        "cage",
        "tree",
        "trees",
        "grass",
        "bed",
        "car",
        "window",
        "mirror",
        "hat",
        "dress",
        "suit",
        "red",
        "blue",
        "green",
        "yellow",
        "outdoor",
        "outdoors",
        "indoor",
        "indoors",
        "outside",
        "inside",
        "nature",
        "wild",
        "distance",
        "background",
        "foreground",
        "above",
        "below",
        "front",
        "back",
    ];

    // Reject if EVERY word (ignoring articles) is a visual/scene/style word.
    let words: Vec<String> = low
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_string())
        .filter(|w| !w.is_empty())
        .collect();
    if words.is_empty() {
        return false;
    }
    if words.iter().all(|w| NON_LOC_WORDS.contains(&w.as_str())) {
        return false;
    }
    // Reject obvious visual idioms like "black and white", "a cage", "the sun".
    if matches!(
        low.as_str(),
        "black and white" | "black & white" | "golden hour" | "blue hour" | "a cage" | "a tree"
    ) {
        return false;
    }
    true
}

fn extract_location(text: &str, known_folders: &[String]) -> (Option<String>, String) {
    // 1. Direct match against known library folder names (longest-first, count-stripped).
    let mut clean_folders: Vec<&str> = known_folders
        .iter()
        .map(|f| strip_folder_count(f))
        .filter(|f| !f.is_empty())
        .collect();
    clean_folders.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));

    for folder in &clean_folders {
        if let Some((start, end)) = find_word_phrase_ci(text, folder) {
            let span_start = strip_optional_leading_prep(text, start);
            return (
                Some((*folder).to_string()),
                remove_span(text, span_start, end),
            );
        }
    }

    // 1b. Distinctive folder keyword match (e.g. "zoo" -> "Portland Zoo 6-21", "graduation"/"grad" -> "CS Grad Pics 2026").
    let lower_text = text.to_ascii_lowercase();
    for folder in &clean_folders {
        let f_low = folder.to_ascii_lowercase();
        for kw in [
            ("graduation", "grad"),
            ("grad", "grad"),
            ("zoo", "zoo"),
            ("waterfall", "falls"),
        ] {
            if has_word(&lower_text, kw.0) && f_low.contains(kw.1) {
                if let Some((s, e)) = find_word_phrase_ci(text, kw.0) {
                    let span_start = strip_optional_leading_prep(text, s);
                    return (
                        Some((*folder).to_string()),
                        remove_span(text, span_start, e),
                    );
                }
            }
        }
    }

    // 2. Preposition-triggered locations ("in Kyoto", "from Tokyo", "at Multnomah Falls"),
    // guarded against visual/scene phrases like "cat in the sun" or "monkey in a cage".
    for trigger in ["in", "from", "at"] {
        if let Some((start, end)) = find_word_phrase_ci(text, trigger) {
            let after = text[end..].trim_start();
            let after_offset = text.len() - after.len();
            let (raw_place, consumed) = take_clause_tokens(after, 3, CLAUSE_STOP_BOUNDARIES);
            let place = clean_location_candidate(&raw_place);
            if is_plausible_location(&place, known_folders) {
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
        "shots",
        "containing",
        "during",
        "how",
        "many",
        "do",
        "i",
        "what",
        "which",
        "where",
        "when",
        "pull",
        "up",
        "look",
    ];

    let words: Vec<String> = text
        .split_whitespace()
        .filter_map(|raw| {
            let cleaned = raw.trim_matches(|c: char| {
                !c.is_alphanumeric() && c != '-' && c != '_' && c != '+' && c != '&' && c != '/'
            });
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

fn build_assistant_reply(plan: &RouterPlan) -> String {
    if !plan_has_any_criteria(plan) {
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
    if !plan.exclude_person_names.is_empty() {
        parts.push(format!("without {}", plan.exclude_person_names.join(", ")));
    }
    if plan.has_people == Some(false) {
        parts.push("without people".to_string());
    } else if plan.has_people == Some(true) && plan.person_names.is_empty() {
        parts.push("with people".to_string());
    }
    if let (Some(lo), Some(hi)) = (plan.min_faces, plan.max_faces) {
        if lo == hi {
            parts.push(format!("with {lo} face{}", if lo == 1 { "" } else { "s" }));
        } else {
            parts.push(format!("with {lo}-{hi} faces"));
        }
    } else if plan.max_faces == Some(1) {
        parts.push("solo".to_string());
    } else if let Some(lo) = plan.min_faces {
        parts.push(format!("with {lo}+ people"));
    }
    if let Some(loc) = &plan.location_query {
        parts.push(format!("in {loc}"));
    }
    if let Some(ex_loc) = &plan.exclude_location_query {
        parts.push(format!("not in {ex_loc}"));
    }
    if let Some(cam) = &plan.camera_model {
        parts.push(format!("shot on {cam}"));
    }
    if let Some(lens) = &plan.lens_model {
        parts.push(format!("with {lens}"));
    }
    if let Some(orient) = &plan.orientation {
        parts.push(format!("({orient})"));
    }
    if !plan.tags.is_empty() {
        parts.push(format!("tagged {}", plan.tags.join(", ")));
    }
    if plan.date_from_ms.is_some() || plan.date_to_ms.is_some() {
        parts.push("within the selected date range".to_string());
    }
    if let Some(sort) = &plan.sort_by {
        let label = match sort.as_str() {
            "best" => "sorted by top picks",
            "capture_desc" => "newest first",
            "capture_asc" => "oldest first",
            "iso_desc" => "highest ISO first",
            "iso_asc" => "lowest ISO first",
            "focal_asc" => "widest focal length first",
            "focal_desc" => "most zoomed first",
            "aperture_asc" => "widest aperture first",
            "shutter_asc" => "fastest shutter first",
            "shutter_desc" => "longest exposure first",
            "duration_desc" => "longest video first",
            _ => "",
        };
        if !label.is_empty() {
            parts.push(label.to_string());
        }
    }

    format!("Searching for {}.", parts.join(" "))
}

fn build_suggestions(plan: &RouterPlan) -> Vec<String> {
    if !plan_has_any_criteria(plan) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::LibraryChatContext;

    fn test_input(msg: &str, prior: RouterPlan) -> ChatTurnInput {
        ChatTurnInput {
            user_message: msg.to_string(),
            prior_filters: prior,
            recent_turns: Vec::new(),
            known_person_names: vec!["Sebastian".to_string(), "AJ".to_string()],
            known_cameras: vec!["ILCE-7M4".to_string(), "iPhone 15 Pro".to_string()],
            known_lenses: vec!["FE 24-70mm F2.8 GM II".to_string()],
            library: LibraryChatContext {
                known_folders: vec![
                    "Portland Zoo 6-21 (146)".to_string(),
                    "CS Grad Pics 2026 (236)".to_string(),
                ],
                ..Default::default()
            },
            current_date_utc: Utc.with_ymd_and_hms(2026, 11, 22, 12, 0, 0).unwrap(),
        }
    }

    #[test]
    fn classic_handles_exif_orientation_negations_and_folders() {
        let router = ClassicChatRouter::new();

        // Preposition false-positive guard: "cat in the sun" must NOT set location_query="sun"!
        let cat = router.parse_turn(&test_input("cat in the sun", RouterPlan::default()));
        assert_eq!(cat.location_query, None);
        assert_eq!(cat.semantic_query.as_deref(), Some("cat sun"));

        // EXIF: focal + aperture + ISO + shutter
        let exif = router.parse_turn(&test_input(
            "ultrawide wide open grainy shots",
            RouterPlan::default(),
        ));
        assert_eq!(exif.focal_length_max_mm, Some(20.0));
        assert_eq!(exif.aperture_max_f, Some(2.0));
        assert_eq!(exif.iso_min, Some(3200));

        // Shutter + sort
        let sh = router.parse_turn(&test_input(
            "photos at 1/1000s or faster",
            RouterPlan::default(),
        ));
        assert!(sh.shutter_speed_max_sec.is_some());
        assert_eq!(sh.location_query, None);

        // Negations & face counts
        let neg = router.parse_turn(&test_input(
            "solo photos of Sebastian without AJ no videos",
            RouterPlan::default(),
        ));
        assert_eq!(neg.person_names, vec!["Sebastian".to_string()]);
        assert_eq!(neg.exclude_person_names, vec!["AJ".to_string()]);
        assert_eq!(neg.max_faces, Some(1));
        assert_eq!(neg.exclude_kind, Some(AssetKind::Video));

        // Known folder + gear matching
        let folder = router.parse_turn(&test_input(
            "graduation photos ILCE-7M4",
            RouterPlan::default(),
        ));
        assert_eq!(folder.location_query.as_deref(), Some("CS Grad Pics 2026"));
        assert_eq!(folder.camera_model.as_deref(), Some("ILCE-7M4"));

        // Filtered count stays search; broad meta goes to answer.
        let count = router.parse_turn(&test_input(
            "how many photos of Sebastian",
            RouterPlan::default(),
        ));
        assert_ne!(count.intent.as_deref(), Some("answer"));
        assert_eq!(count.person_names, vec!["Sebastian".to_string()]);

        let meta = router.parse_turn(&test_input("what cameras do I use?", RouterPlan::default()));
        assert_eq!(meta.intent.as_deref(), Some("answer"));
    }
}
