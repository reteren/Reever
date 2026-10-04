#!/usr/bin/env python3
"""Build and report the CMUdict → Russian phonetic lexicon probe.

No third-party Python packages are required. Run from any directory:
  python testdata/lexicon/build_lexicon.py build
  CARGO_BUILD_JOBS=4 cargo run --release --example lexicon_probe
  python testdata/lexicon/build_lexicon.py report

`build --fetch` refreshes pinned source snapshots. The default build reads the
snapshots stored beside this script, extracts only word tokens from the local
dictation log, and never copies complete log lines into generated files.
"""

from __future__ import annotations

import argparse
import gzip
import hashlib
import json
import math
import os
import re
import sys
import urllib.request
from collections import Counter, defaultdict
from pathlib import Path


HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
SOURCES = HERE / "sources"
CMUDICT_SHA = "74790861f652b15e4ac49015a90074ad62a27690"
FREQUENCY_SHA = "815665f95ddab30520dff6df260ed3b6244b9638"
MAX_FREQUENT_WORDS = 25_000
VARIANT_LIMIT = 40
TOKEN_RE = re.compile(r"[A-Za-zА-Яа-яЁё]+(?:[-'][A-Za-zА-Яа-яЁё]+)*")
CYRILLIC_RE = re.compile(r"[А-Яа-яЁё]")
LATIN_RE = re.compile(r"[A-Za-z]")
TEXT_KEYS = {"expected_text", "expected_command", "text"}

# The source list is copied from src/dictation.rs on every build, so the probe's
# guard cannot silently drift from the existing product rule.
MORPH_SUFFIXES = [
    "ешься", "ишься", "ятся", "ится", "атся", "аться", "иться",
    "нул", "нула", "нуло", "нули", "нулся", "нулась", "нулись",
    "ала", "ало", "али", "ил", "ила", "ило", "или", "ить", "ишь",
    "овать", "ует", "уют", "овый", "овая", "овое", "овые",
    "иями", "ами", "ями", "ах", "ях", "ам", "ям", "ов", "ев",
    "ом", "ем", "ой", "ей", "ою", "ею", "а", "я", "ы", "и", "у", "ю", "е",
]

KNOWN_COLLISIONS = {
    "mixed_opacity": {"опросите"},
    "mixed_brush": {"брошь"},
    "mixed_render": {"ориентир"},
    # A technical loanword already conventional in Russian spelling.
    "mixed_material": {"материал"},
}

PHONE_OPTIONS: dict[str, tuple[str, ...]] = {
    "AA": ("а", "о"), "AE": ("э", "а"), "AH": ("а", "о", "э", "ы"),
    "AO": ("о", "а"), "AW": ("ау", "ао"), "AY": ("ай", "эй"),
    "EH": ("э", "е"), "ER": ("ер", "эр", "ар"), "EY": ("эй", "ей", "э"),
    "IH": ("и", "ы", "е"), "IY": ("и", "ы"), "OW": ("оу", "о"),
    "OY": ("ой", "ойэ"), "UH": ("у", "о"), "UW": ("у", "ю"),
    "AX": ("а", "о", "э", "ы"),
    "B": ("б",), "CH": ("ч", "тш"), "D": ("д",), "DH": ("з", "д", "с"),
    "F": ("ф",), "G": ("г",), "HH": ("х", "г"), "JH": ("дж", "ж"),
    "K": ("к",), "L": ("л",), "M": ("м",), "N": ("н",), "NG": ("нг", "н"),
    "P": ("п",), "R": ("р",), "S": ("с", "з"), "SH": ("ш",),
    "T": ("т",), "TH": ("с", "з", "т"), "V": ("в",), "W": ("в", "у"),
    "Y": ("й", "и"), "Z": ("з", "с"), "ZH": ("ж", "з"),
}


def write_json(path: Path, value: object) -> None:
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def fetch_sources() -> None:
    SOURCES.mkdir(parents=True, exist_ok=True)
    files = {
        "cmudict.dict": f"https://raw.githubusercontent.com/cmusphinx/cmudict/{CMUDICT_SHA}/cmudict.dict",
        "LICENSE-cmudict.txt": f"https://raw.githubusercontent.com/cmusphinx/cmudict/{CMUDICT_SHA}/LICENSE",
        "word_frequencies.txt": f"https://raw.githubusercontent.com/brekker23/English-word-frequencies/{FREQUENCY_SHA}/word_frequencies.txt",
        "LICENSE-frequencies.txt": f"https://raw.githubusercontent.com/brekker23/English-word-frequencies/{FREQUENCY_SHA}/LICENSE",
    }
    for filename, url in files.items():
        request = urllib.request.Request(url, headers={"User-Agent": "Reever-lexicon-research/1.0"})
        with urllib.request.urlopen(request, timeout=60) as response:
            (SOURCES / filename).write_bytes(response.read())
        print(f"fetched {filename}")


def parse_cmudict(path: Path) -> dict[str, list[list[str]]]:
    entries: dict[str, list[list[str]]] = defaultdict(list)
    for line in path.read_text(encoding="ascii", errors="replace").splitlines():
        if not line or line.startswith(";;;"):
            continue
        fields = line.split()
        if len(fields) < 2:
            continue
        head = re.sub(r"\(\d+\)$", "", fields[0]).lower()
        if re.fullmatch(r"[a-z]+", head):
            entries[head].append(fields[1:])
    return entries


def parse_frequencies(path: Path) -> dict[str, float]:
    values: dict[str, float] = {}
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        fields = line.split()
        if len(fields) < 2 or not re.fullmatch(r"[a-z]+", fields[0].lower()):
            continue
        try:
            value = float(fields[1])
        except ValueError:
            continue
        values[fields[0].lower()] = value
    return values


def compact_variant(text: str) -> str:
    return re.sub(r"\s+", "", text.lower().replace("ё", "е"))


def pronunciation_variants(phones: list[str]) -> list[str]:
    normalized = [re.sub(r"\d+$", "", phone).upper() for phone in phones]
    choices = [PHONE_OPTIONS.get(phone, ("",)) for phone in normalized]
    base = "".join(option[0] for option in choices)
    output: list[str] = [base]

    # One-phone alternatives cover the main English↔Russian ambiguities without
    # exploding into a Cartesian product (the runtime file remains small).
    for index, options in enumerate(choices):
        for option in options[1:]:
            output.append("".join(choices[i][0] if i != index else option for i in range(len(choices))))
            if len(output) >= VARIANT_LIMIT:
                break
        if len(output) >= VARIANT_LIMIT:
            break

    # A final N-AH0-S is heard in Russian as -нес/-нис/-ныс as well as -нас.
    if len(normalized) >= 3 and normalized[-3:] == ["N", "AH", "S"]:
        prefix = "".join(choices[i][0] for i in range(len(choices) - 3))
        output.extend(prefix + suffix for suffix in ("нес", "нис", "ныс", "нас"))

    unique: list[str] = []
    seen: set[str] = set()
    for item in output:
        item = compact_variant(item)
        if item and item not in seen:
            seen.add(item)
            unique.append(item)
    return unique[:VARIANT_LIMIT]


def terms_from_file(path: Path) -> list[str]:
    terms = [
        line.split("#", 1)[0].strip().lower()
        for line in path.read_text(encoding="utf-8").splitlines()
        if line.split("#", 1)[0].strip()
    ]
    return list(dict.fromkeys(terms))


def parse_mix_report(path: Path) -> list[dict[str, object]]:
    cases: list[dict[str, object]] = []
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line.startswith("|") or line.startswith("|---"):
            continue
        cells = [cell.strip() for cell in line.strip().strip("|").split("|")]
        if len(cells) < 4 or cells[0].startswith("ID / class"):
            continue
        identity = cells[0]
        target_match = re.search(r"`([^`]+)`", identity)
        if not target_match:
            continue
        case_id = identity.split("/", 1)[0].strip()
        kind_match = re.search(r"\b(mixed|russian_slang)\b", identity)
        kind = kind_match.group(1) if kind_match else "unknown"
        target = target_match.group(1).lower()
        collisions = sorted(KNOWN_COLLISIONS.get(case_id, set()))
        cases.append({
            "id": case_id,
            "set": "mix-report",
            "kind": kind,
            "text": cells[3],
            "expected_groups": [[target]] if kind == "mixed" else [],
            "keep_expected": [target] if kind == "russian_slang" else [],
            "known_collision_tokens": collisions,
        })
    return cases


def parse_mix2_report(report_path: Path, manifest_path: Path) -> list[dict[str, object]]:
    if not report_path.exists() or not manifest_path.exists():
        return []
    manifest_rows = json.loads(manifest_path.read_text(encoding="utf-8"))
    by_id = {str(row["id"]): row for row in manifest_rows}
    cases: list[dict[str, object]] = []
    for line in report_path.read_text(encoding="utf-8").splitlines():
        if not line.startswith("|") or line.startswith("|---"):
            continue
        cells = [cell.strip() for cell in line.strip().strip("|").split("|")]
        if len(cells) < 4:
            continue
        identity = cells[0]
        match = re.search(r"`([^`]+)`", identity)
        if not match:
            continue
        row = by_id.get(match.group(1))
        if row is None:
            continue
        kind = str(row.get("kind", ""))
        terms = [
            part.lower()
            for term in row.get("expected_terms", [])
            for part in re.split(r"\s+", str(term).strip())
            if part
        ]
        cases.append({
            "id": str(row["id"]),
            "set": "mix2-report",
            "kind": "mixed" if kind == "mixed" else "russian_slang",
            "text": cells[3],
            "expected_groups": [terms] if kind == "mixed" and terms else [],
            "keep_expected": [],
            "known_collision_tokens": [],
        })
    return cases


def user_cases() -> list[dict[str, object]]:
    raw = [
        ("user_opacity", "апасити", [["opacity"]], ""),
        ("user_hardness", "харднес", [["hardness"]], ""),
        ("user_roughness", "рафнес", [["roughness"]], ""),
        ("user_escape", "эскейп", [["escape"]], ""),
        ("user_png", "пнг", [["png"]], ""),
        ("user_scale_inflected", "скейлится", [["scale"]], "preserve_morphology"),
        ("user_high_png", "хайф пнг", [["high", "png"]], ""),
        ("user_import_inflected", "импортнул", [["import"]], "preserve_morphology"),
        ("user_export_inflected", "экспортнул", [["export"]], "preserve_morphology"),
        ("user_size", "сайс", [["size"]], ""),
        ("user_tool_eraser", "тул и рас", [["tool", "eraser"], ["eraser", "tool"]], "ambiguous_order"),
    ]
    return [{
        "id": case_id,
        "set": "user-examples",
        "kind": "user-example",
        "text": text,
        "expected_groups": groups,
        "keep_expected": [],
        "policy": policy,
        "known_collision_tokens": [],
    } for case_id, text, groups, policy in raw]


def collect_manifest_texts(value: object) -> list[str]:
    found: list[str] = []
    if isinstance(value, dict):
        for key, child in value.items():
            if key in TEXT_KEYS and isinstance(child, str) and child.strip():
                found.append(child)
            elif isinstance(child, (dict, list)):
                found.extend(collect_manifest_texts(child))
    elif isinstance(value, list):
        for child in value:
            if isinstance(child, (dict, list)):
                found.extend(collect_manifest_texts(child))
    return found


def add_negative_text(text: str, source: str, tokens: Counter, pair_counts: Counter) -> int:
    matches = list(TOKEN_RE.finditer(text))
    for match in matches:
        token = match.group(0).lower().replace("ё", "е")
        tokens[(token, source)] += 1
    for left, right in zip(matches, matches[1:]):
        gap = text[left.end():right.start()]
        if re.fullmatch(r"[\s\-]*", gap):
            a = left.group(0).lower().replace("ё", "е")
            b = right.group(0).lower().replace("ё", "е")
            if CYRILLIC_RE.search(a) and CYRILLIC_RE.search(b):
                pair_counts[(a, b, source)] += 1
    return len(matches)


def build_negative_inputs() -> dict[str, object]:
    manifest_path = ROOT / "testdata" / "dictation-manifest.json"
    log_path = Path(os.environ.get("APPDATA", "")) / "com.troid.reever" / "reever.log"
    if not manifest_path.exists():
        raise FileNotFoundError(f"Required negative corpus is missing: {manifest_path}")
    if not log_path.exists():
        raise FileNotFoundError(f"Required user log is missing: {log_path}")

    token_counts: Counter = Counter()
    pair_counts: Counter = Counter()
    log_lines = [line for line in log_path.read_text(encoding="utf-8", errors="replace").splitlines() if "✎" in line]
    log_tokens = 0
    for line in log_lines:
        tail = line.split("✎", 1)[1]
        log_tokens += add_negative_text(tail, "reever.log", token_counts, pair_counts)

    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    manifest_texts = collect_manifest_texts(manifest)
    manifest_tokens = 0
    for text in manifest_texts:
        manifest_tokens += add_negative_text(text, "dictation-manifest", token_counts, pair_counts)

    grouped: dict[str, dict[str, object]] = {}
    for (token, source), count in token_counts.items():
        row = grouped.setdefault(token, {"token": token, "occurrences": 0, "sources": {}})
        row["occurrences"] = int(row["occurrences"]) + count
        sources = row["sources"]
        assert isinstance(sources, dict)
        sources[source] = count
    pairs = [
        {"left": left, "right": right, "occurrences": count, "source": source}
        for (left, right, source), count in sorted(pair_counts.items())
    ]
    cyrillic_occurrences = sum(
        int(row["occurrences"]) for row in grouped.values()
        if isinstance(row, dict) and CYRILLIC_RE.search(str(row["token"]))
    )
    output = {
        "tokens": sorted(grouped.values(), key=lambda row: str(row["token"])),
        "pairs": pairs,
        "stats": {
            "log_path": "%APPDATA%\\com.troid.reever\\reever.log",
            "log_marker_lines": len(log_lines),
            "log_token_occurrences": log_tokens,
            "manifest_text_values": len(manifest_texts),
            "manifest_token_occurrences": manifest_tokens,
            "total_token_occurrences": log_tokens + manifest_tokens,
            "unique_tokens": len(grouped),
            "cyrillic_token_occurrences": cyrillic_occurrences,
            "unique_cyrillic_tokens": sum(1 for token in grouped if CYRILLIC_RE.search(token)),
            "unique_cyrillic_pairs": sum(1 for row in pairs if CYRILLIC_RE.search(row["left"]) and CYRILLIC_RE.search(row["right"])),
        },
    }
    write_json(HERE / "negative-cases.json", output)
    return output


def extract_slang_guard() -> list[str]:
    source = (ROOT / "src-tauri" / "src" / "dictation.rs").read_text(encoding="utf-8")
    match = re.search(r"const RUSSIAN_SLANG_GUARD:\s*&\[&str\]\s*=\s*&\[(.*?)\];", source, re.S)
    if not match:
        raise RuntimeError("Could not find RUSSIAN_SLANG_GUARD in src/dictation.rs")
    return re.findall(r'"([^"\\]+)"', match.group(1))


def build() -> None:
    if not (SOURCES / "cmudict.dict").exists() or not (SOURCES / "word_frequencies.txt").exists():
        raise FileNotFoundError("Source snapshots are absent; run with --fetch first")

    cmu = parse_cmudict(SOURCES / "cmudict.dict")
    frequencies = parse_frequencies(SOURCES / "word_frequencies.txt")
    ranked = sorted(frequencies.items(), key=lambda item: (-item[1], item[0]))
    rank_by_word = {word: rank for rank, (word, _) in enumerate(ranked, start=1)}
    chosen = {word for word, _ in ranked if word in cmu}
    chosen = set(sorted(chosen, key=lambda word: rank_by_word.get(word, 10**9))[:MAX_FREQUENT_WORDS])
    domain = terms_from_file(HERE / "domain_terms.txt")
    overrides = json.loads((HERE / "acronym_overrides.json").read_text(encoding="utf-8"))
    chosen.update(word for word in domain if word in cmu)
    chosen.update(overrides)

    max_weight = max(frequencies.values(), default=1.0)
    entries: list[dict[str, object]] = []
    missing_domain: list[str] = []
    total_variants = 0
    for word in sorted(chosen, key=lambda item: (rank_by_word.get(item, 10**9), item)):
        pronunciations = cmu.get(word, [])
        variants: list[str] = []
        for pronunciation in pronunciations:
            for variant in pronunciation_variants(pronunciation):
                if variant not in variants:
                    variants.append(variant)
        override = overrides.get(word)
        display = word
        if override:
            display = override.get("display", word)
            for variant in override.get("variants", []):
                compact = compact_variant(str(variant))
                if compact and compact not in variants:
                    variants.append(compact)
        if not variants:
            if word in domain:
                missing_domain.append(word)
            continue
        weight = frequencies.get(word, 0.05 * max_weight) / max_weight
        # Hand-added domain words get enough prior to survive obscure-word ties;
        # the phonetic similarity and ambiguity margin still dominate the score.
        if word in domain and word not in frequencies:
            weight = 0.08
        entry = {
            "word": word,
            "display": display,
            "weight": round(min(1.0, max(0.0, weight)), 8),
            "rank": rank_by_word.get(word),
            "domain": word in domain,
            "variants": variants,
        }
        entries.append(entry)
        total_variants += len(variants)

    write_json(HERE / "lexicon.json", {
        "format": 1,
        "policy": {"min_similarity": 0.80, "min_margin": 0.05, "frequency_bonus": "0.015 * sqrt(relative_frequency)"},
        "words": entries,
    })
    lexicon_bytes = (HERE / "lexicon.json").read_bytes()
    (HERE / "lexicon.json.gz").write_bytes(gzip.compress(lexicon_bytes, compresslevel=9, mtime=0))
    slang = extract_slang_guard()
    write_json(HERE / "slang_guard.json", slang)
    write_json(HERE / "morphology_policy.json", {
        "policy": "Keep inflected English roots written with Russian morphology in Cyrillic.",
        "suffixes": MORPH_SUFFIXES,
        "examples": ["скейлится", "импортнул", "экспортнул"],
    })

    positive = (
        parse_mix_report(ROOT / "testdata" / "mix-report.md")
        + parse_mix2_report(ROOT / "testdata" / "mix2-report.md", ROOT / "testdata" / "mix2-manifest.json")
        + user_cases()
    )
    write_json(HERE / "positive-cases.json", positive)
    negative = build_negative_inputs()

    source_files = [SOURCES / "cmudict.dict", SOURCES / "LICENSE-cmudict.txt", SOURCES / "word_frequencies.txt", SOURCES / "LICENSE-frequencies.txt"]
    provenance = {
        "generated_utc_date": "2026-10-04",
        "frequent_word_limit": MAX_FREQUENT_WORDS,
        "lexicon_words": len(entries),
        "phonetic_variant_count": total_variants,
        "domain_term_count": len(domain),
        "missing_domain_terms_without_pronunciation": missing_domain,
        "slang_guard_count": len(slang),
        "negative_stats": negative["stats"],
        "sources": [
            {"name": "CMUdict", "commit": CMUDICT_SHA, "license": "BSD-2-Clause style", "sha256": sha256(SOURCES / "cmudict.dict"), "bytes": (SOURCES / "cmudict.dict").stat().st_size},
            {"name": "English-word-frequencies", "commit": FREQUENCY_SHA, "license": "Apache-2.0", "sha256": sha256(SOURCES / "word_frequencies.txt"), "bytes": (SOURCES / "word_frequencies.txt").stat().st_size},
        ],
        "source_snapshot_bytes": sum(path.stat().st_size for path in source_files),
        "generated_lexicon_bytes": (HERE / "lexicon.json").stat().st_size,
        "generated_lexicon_gzip_bytes": (HERE / "lexicon.json.gz").stat().st_size,
    }
    write_json(HERE / "provenance.json", provenance)
    print(json.dumps({
        "words": len(entries), "variants": total_variants,
        "lexicon_bytes": provenance["generated_lexicon_bytes"],
        "gzip_bytes": provenance["generated_lexicon_gzip_bytes"],
        "negative_tokens": negative["stats"]["total_token_occurrences"],
        "negative_unique_cyrillic": negative["stats"]["unique_cyrillic_tokens"],
        "positive_cases": len(positive), "slang_guard": len(slang),
    }, ensure_ascii=False, indent=2))


def safe_cell(text: object) -> str:
    return str(text).replace("|", "\\|").replace("\n", " ")


def all_proposals(case: dict[str, object]) -> list[dict[str, object]]:
    return list(case.get("proposals", []))


def expected_group(case: dict[str, object]) -> list[str]:
    groups = case.get("expected_groups", [])
    if not groups:
        return []
    return [str(word).lower() for word in groups[0]]


def best_probe_hint(case: dict[str, object]) -> str:
    probes = [probe for probe in case.get("word_probes", []) if probe.get("best")]
    if not probes:
        return "нет кандидатов"
    probe = max(probes, key=lambda item: item["best"]["score"])
    best = probe["best"]
    runner = probe.get("runner_up")
    suffix = f" vs {runner['word']}" if runner else ""
    return f"{probe['token']}→{best['word']} {best['similarity']:.2f}/Δ{best['margin']:.2f}{suffix}"


def display_best_for_target(case: dict[str, object]) -> str:
    wanted = {str(word).lower() for group in case.get("expected_groups", []) for word in group}
    if not wanted:
        return "—"
    protected = case.get("protected_collisions", [])
    if protected:
        target = next(iter(sorted(wanted)))
        return "; ".join(f"{token}→{target} (ru-RU защищено)" for token in protected[:3])
    hits: list[str] = []
    for probe in case.get("word_probes", []):
        top = probe.get("best")
        runner = probe.get("runner_up")
        if isinstance(top, dict) and top.get("word") in wanted:
            hits.append(f"{probe['token']}→{top['word']} {top['similarity']:.2f}/{top['margin']:.2f}")
        elif isinstance(runner, dict) and runner.get("word") in wanted:
            hits.append(f"{probe['token']}→{runner['word']} {runner['similarity']:.2f}/{runner['margin']:.2f}")
    if hits:
        return "; ".join(hits[:3])
    proposals = case.get("proposals", [])
    if proposals:
        return "; ".join(f"{p['input']}→{p['word']}" for p in proposals[:3])
    return best_probe_hint(case)


def report() -> None:
    result_path = HERE / "probe-results.json"
    if not result_path.exists():
        raise FileNotFoundError("Run cargo run --release --example lexicon_probe first")
    result = json.loads(result_path.read_text(encoding="utf-8"))
    provenance = json.loads((HERE / "provenance.json").read_text(encoding="utf-8"))
    negative_source = json.loads((HERE / "negative-cases.json").read_text(encoding="utf-8"))
    cases = result["positive_cases"]
    mixed = [case for case in cases if case["set"] in {"mix-report", "mix2-report"} and case["kind"] == "mixed"]
    slang_cases = [case for case in cases if case["set"] in {"mix-report", "mix2-report"} and case["kind"] == "russian_slang"]
    users = [case for case in cases if case["set"] == "user-examples"]
    mix2_russian = [case for case in slang_cases if case["set"] == "mix2-report"]

    def set_score(selected: list[dict[str, object]]) -> tuple[int, int]:
        matched = 0
        count = 0
        for case in selected:
            expected = expected_group(case)
            proposed = {str(proposal["word"]).lower() for proposal in all_proposals(case)}
            count += len(expected)
            matched += sum(1 for word in expected if word in proposed)
        return matched, count

    mix1_score = set_score([case for case in mixed if case["set"] == "mix-report"])
    mix2_score = set_score([case for case in mixed if case["set"] == "mix2-report"])
    user_score = set_score(users)

    targets = sum(len(expected_group(case)) for case in mixed + users)
    true_positive = 0
    phrase_success = 0
    positive_proposals = 0
    for case in mixed + users:
        expected = set(expected_group(case))
        proposed = [str(proposal["word"]).lower() for proposal in all_proposals(case)]
        true_positive += sum(1 for word in expected if word in proposed)
        positive_proposals += len(proposed)
        if expected and expected.issubset(set(proposed)):
            phrase_success += 1
    slang_proposals = sum(len(all_proposals(case)) for case in slang_cases)
    positive_proposals += slang_proposals

    negative = result["negative"]
    negative_substitutions = int(negative["single_substitution_occurrences"]) + int(negative["pair_substitution_occurrences"])
    false_positive = (positive_proposals - true_positive) + negative_substitutions
    precision = true_positive / (true_positive + false_positive) if true_positive + false_positive else 0.0
    raw_recall = true_positive / targets if targets else 0.0
    morph_targets = {"scale", "import", "export"}
    excluded_morph_targets = {
        word
        for case in users
        if case.get("policy") == "preserve_morphology"
        for word in expected_group(case)
    }
    collision_targets: set[str] = set()
    collision_rows: list[tuple[str, str, str]] = []
    seen_collision_rows: set[tuple[str, str]] = set()
    for case in mixed:
        expected = expected_group(case)
        for token in case.get("protected_collisions", []):
            collision_targets.update(expected)
            row_key = (token, expected[0])
            if row_key not in seen_collision_rows:
                seen_collision_rows.add(row_key)
                collision_rows.append((token, expected[0], "ru-RU словарное; оставлено"))
    excluded_targets = collision_targets | excluded_morph_targets
    excluded_morph = len(excluded_morph_targets)
    eligible_targets = max(0, targets - len(excluded_targets))
    eligible_recall = true_positive / eligible_targets if eligible_targets else 0.0
    negative_token_set = {row["token"] for row in negative_source["tokens"]}
    user_input_tokens = {
        token.lower().replace("ё", "е")
        for case in users
        for token in TOKEN_RE.findall(str(case["text"]))
    }
    overlap_tokens = sorted(user_input_tokens & negative_token_set)

    sizes = json.loads((HERE / "provenance.json").read_text(encoding="utf-8"))
    source_bytes = sizes["source_snapshot_bytes"]
    lexicon_bytes = sizes["generated_lexicon_bytes"]
    gz_bytes = sizes["generated_lexicon_gzip_bytes"]
    timing = result["timing"]
    min_similarity = result["thresholds"]["min_similarity"]
    min_margin = result["thresholds"]["min_margin"]

    output: list[str] = [
        "# Кириллический lexicon probe: английские слова без второй ASR-модели",
        "",
        "Дата: 2026-10-04. Это исследовательский прототип; код приложения не менялся.",
        "Вход для набора (а) взят из колонок GigaAM RU в `testdata/mix-report.md` и GigaAM в `testdata/mix2-report.md`; аудио и модели повторно не запускались.",
        "",
        "## Результат",
        "",
        f"- Точные английские цели: **{true_positive}/{targets}** ({raw_recall:.1%}); успешны все цели в **{phrase_success}/{len(mixed) + len(users)}** фразах.",
        f"- По поднаборам: mix-report **{mix1_score[0]}/{mix1_score[1]}**, mix2 **{mix2_score[0]}/{mix2_score[1]}**, пользовательские примеры **{user_score[0]}/{user_score[1]}**; наборы (а)+(б) вместе — **{mix1_score[0] + mix2_score[0] + user_score[0]}/{mix1_score[1] + mix2_score[1] + user_score[1]}**.",
        f"- Синтетическая оценка precision: **{precision:.1%}** ({true_positive} верных замен на {true_positive + false_positive} предложений); включает каждое предложение на отрицательном наборе и каждую замену нецелевого токена в положительных фразах.",
        f"- Отрицательный набор: **{negative_substitutions}** ложных замен; одиночных — {negative['single_substitution_occurrences']}, склеек пар — {negative['pair_substitution_occurrences']}. Уникальные ложные входные слова: {len(negative['false_words'])}.",
        f"- Дополнительные русские контрольные фразы mix2: **{len(mix2_russian)}**, ложных предложений в них — **{sum(len(all_proposals(case)) for case in mix2_russian)}**.",
        f"- Проверка ru-RU доступна: **{result['ru_dictionary_available']}**. Без словаря правило запрещает замену.",
        f"- Словарь: **{provenance['lexicon_words']:,} английских записей**, {provenance['phonetic_variant_count']:,} кириллических фонетических вариантов.",
        f"- Данные словаря: **{lexicon_bytes / 1_000_000:.2f} MB** JSON, **{gz_bytes / 1_000_000:.2f} MB** gzip; исходные снапшоты для воспроизводимости — {source_bytes / 1_000_000:.2f} MB.",
        f"- Поиск без ISpellChecker: среднее **{timing['matching_avg_us']:.1f} µs/слово**, p95 {timing['matching_p95_us']:.1f} µs; ISpellChecker: среднее {timing['spellcheck_avg_us']:.1f} µs/уникальное слово. Matcher замерен при параллельных прогонах, поэтому эти цифры помечены как под нагрузкой.",
        "",
        f"Порог решения: similarity ≥ **{min_similarity:.2f}** и отрыв от второго кандидата ≥ **{min_margin:.2f}**. В оценку входят частотный prior, ru-RU словарный фильтр, сленг-список из `src/dictation.rs`, проверка соседних токенов и разрезание длинного слитного токена.",
        "",
        "Recall ниже из-за трёх форм с русской морфологией (`скейлится`, `импортнул`, `экспортнул`): политика сохраняет их кириллицей. Ещё словарные коллизии намеренно не переписываются; отдельная оценка replaceable-only recall исключает эти случаи.",
        (f"В отрицательном логе встретились также пользовательские положительные формы: {', '.join(f'`{word}`' for word in overlap_tokens)}. По условию исследования все слова журнала считаются негативными и каждое их изменение засчитывается как FP, даже если написание совпадает с набором (б)." if overlap_tokens else "Пользовательские положительные примеры не пересекаются с токенами отрицательного лога."),
        "",
        "## Источники и лицензии",
        "",
        f"- [CMUdict](https://github.com/cmusphinx/cmudict/tree/{CMUDICT_SHA}) — произношения ARPAbet, BSD-2-Clause-style лицензия; исходный README просит указывать происхождение, а LICENSE требует сохранять notice/disclaimer. SHA-256 `cmudict.dict`: `{provenance['sources'][0]['sha256']}`.",
        f"- [English-word-frequencies](https://github.com/brekker23/English-word-frequencies/tree/{FREQUENCY_SHA}) — частоты слов по книгам public domain, Apache-2.0; автор предупреждает, что частоты плохо отражают новые слова. SHA-256 `word_frequencies.txt`: `{provenance['sources'][1]['sha256']}`.",
        f"- Ранжированы первые {provenance['frequent_word_limit']:,} частотных слов, пересечённых с CMUdict; курировано {provenance['domain_term_count']} графических, разработческих, клавиатурных и форматных терминов (отсутствующие в частотном списке добавлены). Для PNG/JPEG/GPU/USB заданы варианты произношения по буквам и русские короткие варианты (например, `пиэнджи` и `пнг`).",
        "- Частоты взяты из корпуса public-domain книг, поэтому могут быть смещены к старой письменной лексике. Технические слова получают небольшой фиксированный prior, но phonetic score доминирует.",
        "",
        "## Набор (а): GigaAM RU из mix-report и mix2-report",
        "",
        "| ID | GigaAM RU | Цель | Лучшее сопоставление | Решение |",
        "|---|---|---|---|---|",
    ]
    for case in cases:
        if case["set"] not in {"mix-report", "mix2-report"}:
            continue
        if case["kind"] == "mixed":
            expected = " + ".join(expected_group(case))
            proposals = all_proposals(case)
            proposed = {str(p["word"]).lower() for p in proposals}
            decision = "восстановлено" if set(expected.split(", ")).issubset(proposed) else ("ошибочная подмена" if proposals else "оставлено")
        else:
            expected = "сленг: " + ", ".join(case.get("keep_expected", []))
            decision = "сохранено" if not all_proposals(case) else "ЛОЖНАЯ замена"
        output.append(f"| `{safe_cell(case['id'])}` | {safe_cell(case['text'])} | `{safe_cell(expected)}` | {safe_cell(display_best_for_target(case))} | {decision} |")

    output.extend(["", "## Набор (б): примеры пользователя", "", "| Вход GigaAM | Цель | Предложение | Статус |", "|---|---|---|---|"])
    for case in users:
        target = " / ".join(" + ".join(group) for group in case.get("expected_groups", []))
        proposals = all_proposals(case)
        proposal_text = "; ".join(f"{p['input']}→{p['word']} ({p['similarity']:.2f}, Δ{p['margin']:.2f})" for p in proposals) or best_probe_hint(case)
        candidate_words = {str(p["word"]).lower() for p in proposals}
        is_hit = bool(expected_group(case)) and set(expected_group(case)).issubset(candidate_words)
        if case.get("policy") == "preserve_morphology":
            morph = case.get("morphology_candidates", [])
            proposal_text = "оставить кириллицей" + (f" (основа → {morph[0]['word']})" if morph else "")
            status = "сохранено по политике"
        elif is_hit:
            status = "восстановлено"
        elif proposals:
            status = "неполно / неверный кандидат"
        else:
            status = "пропуск"
        output.append(f"| `{safe_cell(case['text'])}` | `{safe_cell(target)}` | {safe_cell(proposal_text)} | {status} |")

    output.extend(["", "## Коллизии с русскими словами", ""])
    if collision_rows:
        output.extend(["| GigaAM RU | Английская цель | Проверка |", "|---|---|---|"])
        for token, target, check in collision_rows:
            output.append(f"| `{token}` | `{target}` | {check} |")
    else:
        output.append("Не обнаружены словарные коллизии из отмеченных примеров.")
    output.append(f"\nИз {targets} английских целевых слов {len(collision_targets)} были заблокированы ru-RU как существующие русские слова; ещё {excluded_morph} целевых формы защищены русскими окончаниями. Replaceable-only recall: **{true_positive}/{eligible_targets}** ({eligible_recall:.1%}).")

    output.extend(["", "## Набор (в): ложные срабатывания на русской диктовке", ""])
    stats = negative_source["stats"]
    output.append(f"Взято {stats['log_marker_lines']} строк с `✎` из `%APPDATA%\\com.troid.reever\\reever.log` и {stats['manifest_text_values']} текстовых полей манифеста. Это {stats['total_token_occurrences']:,} словоупотреблений, из них кириллических — {stats['cyrillic_token_occurrences']:,}; уникальных кириллических слов — {stats['unique_cyrillic_tokens']:,}. Полные строки журнала в отчёт не копировались.")
    if negative["false_words"]:
        output.append("")
        output.append("Ложные подмены (кириллический токен → предложенное английское слово; число употреблений):")
        output.append("")
        output.append(", ".join(f"`{row['token']}→{row['english']}` ×{row['occurrences']}" for row in negative["false_words"]))
    else:
        output.append("Ложные токены: **нет**.")
    if negative.get("false_pair_proposals"):
        output.append(f"\nТакже ложных замен склейкой соседних токенов: **{negative['pair_substitution_occurrences']}** ({len(negative['false_pair_proposals'])} различных пар/предложений); текстовые фрагменты журнала не приведены.")
    output.append(f"\nВсего ложных предложений на (в): **{negative_substitutions}**. Список выше содержит только отдельные слова, без исходных предложений.")

    output.extend([
        "",
        "## Метод и ограничения",
        "",
        "- ARPAbet разворачивается в русский вариантный набор: `ER → ер/эр/ар`, schwa → `а/о/э/ы`, `TH → с/з/т`, `W → в/у`; для конечного `N AH0 S` отдельно добавлены `-нес/-нис/-ныс/-нас`. Варианты добавляются по одному неоднозначному фонему, а не полным декартовым произведением.",
        "- Для одного русского токена сравниваются согласный фонетический каркас (65%) и полная кириллическая запись (35%); score получает частотную добавку `0.015 × √относительной частоты`. При близком втором кандидате замена отменяется.",
        "- Слова, которые ru-RU считает правильными, не меняются. Слова из текущего `RUSSIAN_SLANG_GUARD` тоже сохраняются. Склейка двух неизвестных соседних токенов и разрезание длинного токена тестируются отдельно; все изменения должны проходить тот же порог уверенности.",
        "- Если кириллический суффикс отделяется и основа уверенно совпадает с доменным словом, форма остаётся кириллицей. Список покрывает глагольные окончания и падежные формы (`скейлится`, `скейла`, `импортнул`, `экспортнул`); это безопаснее, чем печатать гибрид `scaleится`/`importнул`.",
        f"- Измерение выполнено на Windows с текущим системным словарём ru-RU. Matcher — {timing['matching_queries']} уникальных кириллических токенов; в среднем {timing['matching_avg_us']:.1f} µs/слово, максимум {timing['matching_max_us']:.1f} µs; прогон шёл под нагрузкой параллельных воркеров. COM-check измеряется отдельно и не включён в matcher latency.",
        f"- Набор частично синтетический: {sum(1 for case in mixed if case['set'] == 'mix-report')} фраз из первого корпуса и {sum(1 for case in mixed if case['set'] == 'mix2-report')} из расширенного SAPI-корпуса, пользовательские примеры и текущие логи/манифест. Precision на этих данных не переносится напрямую на живую диктовку; логовые негативы — выборка из одного пользователя.",
        "",
        "## Вывод",
        "",
        f"Встроить можно по размеру ({lexicon_bytes / 1_000_000:.2f} MB JSON / {gz_bytes / 1_000_000:.2f} MB gzip) и скорости поиска ({timing['matching_avg_us']:.1f} µs/слово без COM). Качество ограничивает неоднозначность, а не объём словаря: фактический recall {raw_recall:.1%}, оценённый precision {precision:.1%}, ложных подмен на текущем негативном наборе — {negative_substitutions}. Это годится для opt-in подсказок или чернового текста с отменой, но пока не обосновывает автоматическую подмену в обычной диктовке; особенно рискованны split-кандидаты и старые частотные priors.",
        "",
        "Полный текст логов не сохранён в отчёте. Генератор, входные токены, лицензии и зафиксированные источники лежат в `testdata/lexicon/`.",
    ])
    target = ROOT / "testdata" / "lexicon-report.md"
    target.write_text("\n".join(output) + "\n", encoding="utf-8")
    print(f"wrote {target}")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("command", choices=("build", "report"), nargs="?", default="build")
    parser.add_argument("--fetch", action="store_true", help="download pinned source snapshots before building")
    args = parser.parse_args()
    if args.command == "build":
        if args.fetch:
            fetch_sources()
        build()
    else:
        report()


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(f"error: {error}", file=sys.stderr)
        raise
