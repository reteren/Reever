"""Экспорт lexicon.json (build_lexicon.py) в компактный ресурс приложения.

    python testdata/lexicon/export_tsv.py

Пишет src-tauri/resources/anglicisms.tsv — по строке на английское слово:
`display<TAB>weight<TAB>domain(0/1)<TAB>вариант,вариант,...`. Формат читает
src-tauri/src/anglicism.rs. Источники и лицензии — testdata/lexicon/provenance.json.
"""

import json
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
SOURCE = HERE / "lexicon.json"
TARGET = ROOT / "src-tauri" / "resources" / "anglicisms.tsv"


def main() -> None:
    data = json.loads(SOURCE.read_text(encoding="utf-8"))
    lines = [
        "# Reever: английские слова и их русские фонетические записи.",
        "# Сгенерировано testdata/lexicon/export_tsv.py из CMUdict (BSD-2-Clause)",
        "# и English-word-frequencies (Apache-2.0); см. THIRD-PARTY-NOTICES.md.",
        "# display\tweight\tdomain\tvariants",
    ]
    for entry in data["words"]:
        variants = [v for v in entry["variants"] if v and "," not in v and "\t" not in v]
        if not variants:
            continue
        lines.append(
            "\t".join(
                [
                    entry["display"],
                    f"{float(entry['weight']):.8g}",
                    "1" if entry["domain"] else "0",
                    ",".join(variants),
                ]
            )
        )
    TARGET.write_text("\n".join(lines) + "\n", encoding="utf-8", newline="\n")
    print(f"{TARGET}: {len(lines) - 4} слов, {TARGET.stat().st_size / 1e6:.2f} МБ")


if __name__ == "__main__":
    main()
