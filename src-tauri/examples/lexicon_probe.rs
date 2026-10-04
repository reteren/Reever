//! Исследовательский пробник фонетического словаря для кириллической диктовки.
//!
//! Из корня репозитория:
//!   $env:CARGO_BUILD_JOBS=4
//!   cargo run --release --example lexicon_probe
//!
//! Использует тот же публичный Windows ISpellChecker ru-RU, что и приложение.
//! Файл не подключён к рабочему конвейеру и не меняет исходную диктовку.

use std::{
    collections::{HashMap, HashSet},
    fs,
    path::PathBuf,
    time::Instant,
};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use strsim::normalized_levenshtein;

const MIN_SIMILARITY: f64 = 0.80;
const MIN_MARGIN: f64 = 0.10;
const FREQUENCY_BONUS: f64 = 0.015;

#[derive(Deserialize)]
struct LexiconFile {
    words: Vec<LexiconEntry>,
}

#[derive(Deserialize, Clone)]
struct LexiconEntry {
    word: String,
    display: String,
    weight: f64,
    rank: Option<usize>,
    domain: bool,
    variants: Vec<String>,
}

#[derive(Deserialize)]
struct MorphologyPolicy {
    suffixes: Vec<String>,
}

#[derive(Deserialize)]
struct PositiveCase {
    id: String,
    #[serde(rename = "set")]
    set_name: String,
    kind: String,
    text: String,
    #[serde(default)]
    expected_groups: Vec<Vec<String>>,
    #[serde(default)]
    keep_expected: Vec<String>,
    #[serde(default)]
    policy: Option<String>,
    #[serde(default)]
    known_collision_tokens: Vec<String>,
}

#[derive(Deserialize)]
struct NegativeInputs {
    tokens: Vec<NegativeToken>,
    pairs: Vec<NegativePair>,
    stats: NegativeStats,
}

#[derive(Deserialize)]
struct NegativeToken {
    token: String,
    occurrences: usize,
    sources: HashMap<String, usize>,
}

#[derive(Deserialize)]
struct NegativePair {
    left: String,
    right: String,
    occurrences: usize,
    source: String,
}

#[derive(Deserialize, Serialize)]
struct NegativeStats {
    log_marker_lines: usize,
    log_token_occurrences: usize,
    manifest_text_values: usize,
    manifest_token_occurrences: usize,
    total_token_occurrences: usize,
    unique_tokens: usize,
    cyrillic_token_occurrences: usize,
    unique_cyrillic_tokens: usize,
    unique_cyrillic_pairs: usize,
}

#[derive(Clone, Debug, Serialize)]
struct Candidate {
    word: String,
    display: String,
    domain: bool,
    /// Composite key score: 65% consonant skeleton, 35% full Cyrillic surface.
    similarity: f64,
    surface_similarity: f64,
    score: f64,
    margin: f64,
    rank: Option<usize>,
}

#[derive(Serialize)]
struct WordProbe {
    token: String,
    russian_word: Option<bool>,
    slang_guard: bool,
    punctuation_guard: bool,
    morphology_candidate: Option<Candidate>,
    best: Option<Candidate>,
    runner_up: Option<Candidate>,
    replacement: Option<Candidate>,
}

#[derive(Clone, Serialize)]
struct Proposal {
    input: String,
    word: String,
    display: String,
    source: String,
    similarity: f64,
    margin: f64,
    score: f64,
    token_count: usize,
}

#[derive(Serialize)]
struct CaseResult {
    id: String,
    set: String,
    kind: String,
    text: String,
    expected_groups: Vec<Vec<String>>,
    keep_expected: Vec<String>,
    policy: Option<String>,
    word_probes: Vec<WordProbe>,
    proposals: Vec<Proposal>,
    morphology_candidates: Vec<Candidate>,
    protected_collisions: Vec<String>,
}

#[derive(Serialize)]
struct FalseWord {
    token: String,
    english: String,
    occurrences: usize,
    sources: HashMap<String, usize>,
}

#[derive(Serialize)]
struct FalsePair {
    english: String,
    occurrences: usize,
    source: String,
}

#[derive(Serialize)]
struct NegativeResult {
    stats: NegativeStats,
    cyrillic_tokens_checked: usize,
    single_substitution_occurrences: usize,
    pair_substitution_occurrences: usize,
    false_words: Vec<FalseWord>,
    false_pair_proposals: Vec<FalsePair>,
}

#[derive(Serialize)]
struct Timing {
    matching_queries: usize,
    matching_avg_us: f64,
    matching_p95_us: f64,
    matching_max_us: f64,
    spellcheck_queries: usize,
    spellcheck_avg_us: f64,
}

#[derive(Serialize)]
struct ProbeResult {
    ru_dictionary_available: bool,
    lexicon_words: usize,
    thresholds: Thresholds,
    timing: Timing,
    positive_cases: Vec<CaseResult>,
    negative: NegativeResult,
}

#[derive(Serialize)]
struct Thresholds {
    min_similarity: f64,
    min_margin: f64,
    frequency_bonus: f64,
}

struct VariantIndex {
    entries: Vec<LexiconEntry>,
    by_skeleton: HashMap<String, Vec<(usize, usize)>>,
    by_skeleton_bigram: HashMap<String, Vec<(usize, usize)>>,
    by_skeleton_trigram: HashMap<String, Vec<(usize, usize)>>,
}

impl VariantIndex {
    fn new(entries: Vec<LexiconEntry>) -> Self {
        let mut by_skeleton: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
        let mut by_skeleton_bigram: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
        let mut by_skeleton_trigram: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
        for (entry_index, entry) in entries.iter().enumerate() {
            for (variant_index, variant) in entry.variants.iter().enumerate() {
                let hit = (entry_index, variant_index);
                let skeleton = phonetic_skeleton(variant);
                if skeleton.is_empty() {
                    continue;
                }
                by_skeleton.entry(skeleton.clone()).or_default().push(hit);
                let chars: Vec<char> = skeleton.chars().collect();
                for pair in chars.windows(2) {
                    let gram: String = pair.iter().collect();
                    by_skeleton_bigram.entry(gram).or_default().push(hit);
                }
                for triplet in chars.windows(3) {
                    let gram: String = triplet.iter().collect();
                    by_skeleton_trigram.entry(gram).or_default().push(hit);
                }
            }
        }
        Self {
            entries,
            by_skeleton,
            by_skeleton_bigram,
            by_skeleton_trigram,
        }
    }

    fn best(&self, raw: &str) -> (Option<Candidate>, Option<Candidate>) {
        let query = normalize(raw);
        let query_len = query.chars().count();
        if query_len < 3 || query_len > 24 {
            return (None, None);
        }
        let delta = ((query_len as f64 * (1.0 - MIN_SIMILARITY)).ceil() as usize).max(1);
        let min_len = query_len.saturating_sub(delta);
        let max_len = query_len + delta;
        let query_skeleton = phonetic_skeleton(&query);
        if query_skeleton.is_empty() {
            return (None, None);
        }
        let skeleton_len = query_skeleton.chars().count();
        let mut candidates: HashSet<(usize, usize)> = HashSet::new();
        if skeleton_len <= 2 {
            if let Some(hits) = self.by_skeleton.get(&query_skeleton) {
                candidates.extend(hits.iter().copied());
            }
        } else if skeleton_len <= 6 {
            if let Some(hits) = self.by_skeleton.get(&query_skeleton) {
                candidates.extend(hits.iter().copied());
            }
            let chars: Vec<char> = query_skeleton.chars().collect();
            for pair in chars.windows(2) {
                let gram: String = pair.iter().collect();
                if let Some(hits) = self.by_skeleton_bigram.get(&gram) {
                    candidates.extend(hits.iter().copied());
                }
            }
        } else {
            if let Some(hits) = self.by_skeleton.get(&query_skeleton) {
                candidates.extend(hits.iter().copied());
            }
            let chars: Vec<char> = query_skeleton.chars().collect();
            for triplet in chars.windows(3) {
                let gram: String = triplet.iter().collect();
                if let Some(hits) = self.by_skeleton_trigram.get(&gram) {
                    candidates.extend(hits.iter().copied());
                }
            }
        }

        let mut per_word: HashMap<usize, (f64, f64)> = HashMap::new();
        for (entry_index, variant_index) in candidates {
            let variant = &self.entries[entry_index].variants[variant_index];
            let variant_len = variant.chars().count();
            if variant_len < min_len || variant_len > max_len {
                continue;
            }
            let surface_similarity = normalized_levenshtein(&query, variant);
            let variant_skeleton = phonetic_skeleton(variant);
            let consonant_similarity = if query_skeleton.is_empty() || variant_skeleton.is_empty() {
                surface_similarity
            } else {
                normalized_levenshtein(&query_skeleton, &variant_skeleton)
            };
            let similarity = 0.65 * consonant_similarity + 0.35 * surface_similarity;
            let slot = per_word.entry(entry_index).or_insert((0.0, 0.0));
            if similarity > slot.0 {
                *slot = (similarity, surface_similarity);
            }
        }

        let mut ranked: Vec<Candidate> = per_word
            .into_iter()
            .map(|(entry_index, (similarity, surface_similarity))| {
                let entry = &self.entries[entry_index];
                Candidate {
                    word: entry.word.clone(),
                    display: entry.display.clone(),
                    domain: entry.domain,
                    similarity,
                    surface_similarity,
                    score: similarity + FREQUENCY_BONUS * entry.weight.sqrt(),
                    margin: 0.0,
                    rank: entry.rank,
                }
            })
            .collect();
        ranked.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| b.similarity.total_cmp(&a.similarity))
                .then_with(|| a.word.cmp(&b.word))
        });
        if ranked.is_empty() {
            return (None, None);
        }
        let runner_score = ranked
            .get(1)
            .map(|candidate| candidate.score)
            .unwrap_or(0.0);
        ranked[0].margin = ranked[0].score - runner_score;
        if ranked.len() > 1 {
            ranked[1].margin = ranked[0].score - ranked[1].score;
        }
        let best = ranked.first().cloned();
        let runner_up = ranked.get(1).cloned();
        (best, runner_up)
    }
}

struct ProbeEngine {
    index: VariantIndex,
    ru_available: bool,
    slang: HashSet<String>,
    suffixes: Vec<String>,
    spelling_cache: HashMap<String, Option<bool>>,
    match_cache: HashMap<String, (Option<Candidate>, Option<Candidate>)>,
    word_match_times_us: Vec<f64>,
    spellcheck_time_us: f64,
    spellcheck_queries: usize,
}

impl ProbeEngine {
    fn new(
        index: VariantIndex,
        ru_available: bool,
        slang: Vec<String>,
        mut suffixes: Vec<String>,
    ) -> Self {
        suffixes.sort_by_key(|suffix| std::cmp::Reverse(suffix.len()));
        Self {
            index,
            ru_available,
            slang: slang.into_iter().map(|word| normalize(&word)).collect(),
            suffixes,
            spelling_cache: HashMap::new(),
            match_cache: HashMap::new(),
            word_match_times_us: Vec::new(),
            spellcheck_time_us: 0.0,
            spellcheck_queries: 0,
        }
    }

    fn best(&mut self, token: &str) -> (Option<Candidate>, Option<Candidate>) {
        let key = normalize(token);
        if let Some(cached) = self.match_cache.get(&key) {
            return cached.clone();
        }
        let matched = self.index.best(&key);
        self.match_cache.insert(key, matched.clone());
        matched
    }

    fn is_russian_word(&mut self, token: &str) -> Option<bool> {
        if !self.ru_available || !has_cyrillic(token) {
            return None;
        }
        let key = token.trim().to_lowercase().replace('ё', "е");
        if let Some(cached) = self.spelling_cache.get(&key) {
            return *cached;
        }
        let started = Instant::now();
        let result = reever_lib::spellcheck::misspelled_ratio("ru-RU", &key)
            .map(|(ratio, count)| count == 1 && ratio == 0.0);
        self.spellcheck_time_us += started.elapsed().as_secs_f64() * 1_000_000.0;
        self.spellcheck_queries += 1;
        self.spelling_cache.insert(key, result);
        result
    }

    fn morphology_candidate(&mut self, token: &str) -> Option<Candidate> {
        let lower = normalize(token);
        let suffixes = self.suffixes.clone();
        for suffix in suffixes {
            let Some(root) = lower.strip_suffix(&suffix) else {
                continue;
            };
            if root.chars().count() < 3 {
                continue;
            }
            let (best, _) = self.best(root);
            if let Some(candidate) =
                best.filter(|candidate| candidate.domain && candidate.similarity >= 0.75)
            {
                return Some(candidate);
            }
        }
        None
    }

    fn probe_word(&mut self, token: &str) -> WordProbe {
        let key = normalize(token);
        let russian_word = self.is_russian_word(token);
        let cold_word_lookup = !self.match_cache.contains_key(&key);
        let word_started = Instant::now();
        let (best, runner_up) = self.best(&key);
        if cold_word_lookup {
            self.word_match_times_us
                .push(word_started.elapsed().as_secs_f64() * 1_000_000.0);
        }
        let slang_guard = self.slang.contains(&key);
        let punctuation_guard = token.contains('-') || token.contains('\'');
        let morphology_candidate = self.morphology_candidate(&key);
        let replacement = if self.ru_available
            && russian_word == Some(false)
            && !slang_guard
            && !punctuation_guard
            && morphology_candidate.is_none()
        {
            best.clone().filter(is_confident)
        } else {
            None
        };
        WordProbe {
            token: key,
            russian_word,
            slang_guard,
            punctuation_guard,
            morphology_candidate,
            best,
            runner_up,
            replacement,
        }
    }

    fn collision_split_proposals(&mut self, token: &str) -> Vec<Proposal> {
        let chars: Vec<char> = token.chars().collect();
        if chars.len() < 8 || chars.len() > 24 {
            return Vec::new();
        }
        if self.is_russian_word(token) != Some(false) {
            return Vec::new();
        }
        let mut proposals = Vec::new();
        for split_at in 3..=chars.len() - 3 {
            let left: String = chars[..split_at].iter().collect();
            let right: String = chars[split_at..].iter().collect();
            let left_word = self.is_russian_word(&left) == Some(true);
            let right_word = self.is_russian_word(&right) == Some(true);
            if left_word == right_word {
                continue;
            }
            let (candidate_side, output_prefix) = if left_word {
                (right.as_str(), left.clone())
            } else {
                (left.as_str(), String::new())
            };
            if self.slang.contains(&normalize(candidate_side)) {
                continue;
            }
            let (best, _) = self.best(candidate_side);
            let Some(candidate) = best.filter(is_confident) else {
                continue;
            };
            // Splitting is a probe only when one side is an intact Russian word;
            // otherwise there is no reliable boundary evidence.
            let output = if output_prefix.is_empty() {
                format!("{} {}", candidate.display, right)
            } else {
                format!("{} {}", output_prefix, candidate.display)
            };
            proposals.push(Proposal {
                input: format!("{left}+{right}"),
                word: candidate.word,
                display: output,
                source: "split".to_string(),
                similarity: candidate.similarity,
                margin: candidate.margin,
                score: candidate.score,
                token_count: 1,
            });
        }
        proposals
    }

    fn pair_proposal(&mut self, left: &str, right: &str) -> Option<Proposal> {
        if !has_cyrillic(left) || !has_cyrillic(right) {
            return None;
        }
        let left = normalize(left);
        let right = normalize(right);
        if left.chars().count() < 2 || right.chars().count() < 2 {
            return None;
        }
        if self.is_russian_word(&left) != Some(false)
            || self.is_russian_word(&right) != Some(false)
            || self.slang.contains(&left)
            || self.slang.contains(&right)
            || self.morphology_candidate(&left).is_some()
            || self.morphology_candidate(&right).is_some()
        {
            return None;
        }
        let fused = format!("{left}{right}");
        if fused.chars().count() > 24 {
            return None;
        }
        let (best, _) = self.best(&fused);
        let candidate = best.filter(is_confident)?;
        Some(Proposal {
            input: format!("{left} + {right}"),
            word: candidate.word,
            display: candidate.display,
            source: "pair".to_string(),
            similarity: candidate.similarity,
            margin: candidate.margin,
            score: candidate.score,
            token_count: 2,
        })
    }

    fn proposals_for_case(&mut self, text: &str, probes: &[WordProbe]) -> Vec<Proposal> {
        let words = tokenize(text);
        let mut candidates = Vec::new();
        for probe in probes {
            if let Some(candidate) = &probe.replacement {
                candidates.push(Proposal {
                    input: probe.token.clone(),
                    word: candidate.word.clone(),
                    display: candidate.display.clone(),
                    source: "single".to_string(),
                    similarity: candidate.similarity,
                    margin: candidate.margin,
                    score: candidate.score,
                    token_count: 1,
                });
            }
            candidates.extend(self.collision_split_proposals(&probe.token));
        }
        for pair in words.windows(2) {
            if let Some(proposal) = self.pair_proposal(&pair[0], &pair[1]) {
                candidates.push(proposal);
            }
        }
        candidates.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.source.cmp(&b.source))
        });
        let mut occupied = HashSet::new();
        let mut selected = Vec::new();
        for candidate in candidates {
            // Greedy non-overlap selection prevents a pair and either of its
            // component single-token proposals from being applied together.
            let start = words.iter().position(|word| {
                normalize(word)
                    == normalize(&candidate.input.split(['+', ' ']).next().unwrap_or(""))
            });
            if candidate.source == "split" {
                if let Some(index) = words.iter().position(|word| {
                    normalize(word) == normalize(&candidate.input.replace('+', ""))
                }) {
                    if occupied.contains(&index) {
                        continue;
                    }
                    occupied.insert(index);
                }
                selected.push(candidate);
                continue;
            }
            if let Some(start) = start {
                let end = (start + candidate.token_count).min(words.len());
                if (start..end).any(|index| occupied.contains(&index)) {
                    continue;
                }
                for index in start..end {
                    occupied.insert(index);
                }
            }
            selected.push(candidate);
        }
        selected.sort_by(|a, b| a.input.cmp(&b.input));
        selected
    }
}

fn is_confident(candidate: &Candidate) -> bool {
    candidate.similarity >= MIN_SIMILARITY && candidate.margin >= MIN_MARGIN
}

fn normalize(text: &str) -> String {
    text.trim()
        .to_lowercase()
        .replace('ё', "е")
        .chars()
        .filter(|ch| ch.is_alphanumeric())
        .collect()
}

fn phonetic_skeleton(text: &str) -> String {
    text.chars()
        .filter(|ch| {
            !matches!(
                ch,
                'а' | 'о' | 'у' | 'ы' | 'э' | 'е' | 'и' | 'я' | 'ю' | 'ё' | 'ь' | 'ъ'
            )
        })
        .collect()
}

fn has_cyrillic(text: &str) -> bool {
    text.chars()
        .any(|ch| ('\u{0400}'..='\u{04ff}').contains(&ch))
}

fn is_cyrillic_only(text: &str) -> bool {
    has_cyrillic(text) && !text.chars().any(|ch| ch.is_ascii_alphabetic())
}

fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_alphabetic() || ch == '\'' || ch == '-' {
            current.push(ch);
        } else if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn get_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn percentile95(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let index = ((sorted.len() as f64 * 0.95).ceil() as usize).saturating_sub(1);
    sorted[index]
}

fn main() -> Result<()> {
    let root = get_root();
    let data = root.join("testdata").join("lexicon");
    let lexicon: LexiconFile = serde_json::from_slice(&fs::read(data.join("lexicon.json"))?)?;
    let cases: Vec<PositiveCase> =
        serde_json::from_slice(&fs::read(data.join("positive-cases.json"))?)?;
    let negatives: NegativeInputs =
        serde_json::from_slice(&fs::read(data.join("negative-cases.json"))?)?;
    let slang: Vec<String> = serde_json::from_slice(&fs::read(data.join("slang_guard.json"))?)?;
    let morphology: MorphologyPolicy =
        serde_json::from_slice(&fs::read(data.join("morphology_policy.json"))?)?;
    let lexicon_words = lexicon.words.len();
    let mut engine = ProbeEngine::new(
        VariantIndex::new(lexicon.words),
        reever_lib::spellcheck::is_available("ru-RU"),
        slang,
        morphology.suffixes,
    );

    let mut positive_cases = Vec::with_capacity(cases.len());
    for case in cases {
        let tokens = tokenize(&case.text);
        let mut word_probes = Vec::with_capacity(tokens.len());
        let mut morphology_candidates = Vec::new();
        for token in tokens {
            if !is_cyrillic_only(&token) {
                continue;
            }
            let probe = engine.probe_word(&token);
            if let Some(candidate) = &probe.morphology_candidate {
                morphology_candidates.push(candidate.clone());
            }
            word_probes.push(probe);
        }
        let proposals = engine.proposals_for_case(&case.text, &word_probes);
        let mut protected_collisions = Vec::new();
        let mut protected_seen = HashSet::new();
        let expected_words: HashSet<String> = case
            .expected_groups
            .iter()
            .flatten()
            .map(|word| normalize(word))
            .collect();
        for probe in &word_probes {
            let target_candidate = probe
                .best
                .iter()
                .chain(probe.runner_up.iter())
                .any(|candidate| expected_words.contains(&candidate.word));
            if probe.russian_word == Some(true)
                && target_candidate
                && protected_seen.insert(probe.token.clone())
            {
                protected_collisions.push(probe.token.clone());
            }
        }
        for token in &case.known_collision_tokens {
            if engine.is_russian_word(token) == Some(true) && protected_seen.insert(token.clone()) {
                protected_collisions.push(token.clone());
            }
        }
        positive_cases.push(CaseResult {
            id: case.id,
            set: case.set_name,
            kind: case.kind,
            text: case.text,
            expected_groups: case.expected_groups,
            keep_expected: case.keep_expected,
            policy: case.policy,
            word_probes,
            proposals,
            morphology_candidates,
            protected_collisions,
        });
    }

    let mut false_words = Vec::new();
    let mut cyrillic_tokens_checked = 0;
    let mut single_substitution_occurrences = 0;
    for item in &negatives.tokens {
        if !is_cyrillic_only(&item.token) {
            continue;
        }
        cyrillic_tokens_checked += 1;
        let probe = engine.probe_word(&item.token);
        if let Some(candidate) = probe.replacement {
            single_substitution_occurrences += item.occurrences;
            false_words.push(FalseWord {
                token: item.token.clone(),
                english: candidate.word,
                occurrences: item.occurrences,
                sources: item.sources.clone(),
            });
        }
    }
    false_words.sort_by(|a, b| {
        b.occurrences
            .cmp(&a.occurrences)
            .then_with(|| a.token.cmp(&b.token))
    });

    let mut false_pair_proposals = Vec::new();
    let mut pair_substitution_occurrences = 0;
    for pair in &negatives.pairs {
        if let Some(proposal) = engine.pair_proposal(&pair.left, &pair.right) {
            pair_substitution_occurrences += pair.occurrences;
            false_pair_proposals.push(FalsePair {
                english: proposal.word,
                occurrences: pair.occurrences,
                source: pair.source.clone(),
            });
        }
    }
    false_pair_proposals.sort_by(|a, b| {
        b.occurrences
            .cmp(&a.occurrences)
            .then_with(|| a.english.cmp(&b.english))
    });

    let matching_avg_us = if engine.word_match_times_us.is_empty() {
        0.0
    } else {
        engine.word_match_times_us.iter().sum::<f64>() / engine.word_match_times_us.len() as f64
    };
    let spellcheck_avg_us = if engine.spellcheck_queries == 0 {
        0.0
    } else {
        engine.spellcheck_time_us / engine.spellcheck_queries as f64
    };
    let result = ProbeResult {
        ru_dictionary_available: engine.ru_available,
        lexicon_words,
        thresholds: Thresholds {
            min_similarity: MIN_SIMILARITY,
            min_margin: MIN_MARGIN,
            frequency_bonus: FREQUENCY_BONUS,
        },
        timing: Timing {
            matching_queries: engine.word_match_times_us.len(),
            matching_avg_us,
            matching_p95_us: percentile95(&engine.word_match_times_us),
            matching_max_us: engine
                .word_match_times_us
                .iter()
                .copied()
                .fold(0.0, f64::max),
            spellcheck_queries: engine.spellcheck_queries,
            spellcheck_avg_us,
        },
        positive_cases,
        negative: NegativeResult {
            stats: negatives.stats,
            cyrillic_tokens_checked,
            single_substitution_occurrences,
            pair_substitution_occurrences,
            false_words,
            false_pair_proposals,
        },
    };
    let output = data.join("probe-results.json");
    fs::write(&output, serde_json::to_vec_pretty(&result)?)
        .with_context(|| format!("write {}", output.display()))?;
    if !result.ru_dictionary_available {
        bail!("ISpellChecker ru-RU is not available; no replacements were enabled");
    }
    println!(
        "lexicon={} words; ru-RU={}; positives={}; negative checked={}; false singles={}, false pairs={}; match={:.1} µs/word; spellcheck={:.1} µs/unique word; report data={}",
        result.lexicon_words,
        result.ru_dictionary_available,
        result.positive_cases.len(),
        result.negative.cyrillic_tokens_checked,
        result.negative.single_substitution_occurrences,
        result.negative.pair_substitution_occurrences,
        result.timing.matching_avg_us,
        result.timing.spellcheck_avg_us,
        output.display(),
    );
    Ok(())
}
