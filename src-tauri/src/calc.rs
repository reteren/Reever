//! Разбор арифметики в нажатия клавиш калькулятора Windows и значение для лога.
//! Стандартный калькулятор считает операции по порядку ввода, поэтому keys получают
//! скобки только для умножения/деления после сложения/вычитания; `calcapp` включает
//! инженерный режим для такого ввода. Квадрат и куб разворачиваются в умножение.
//! Корень не поддерживается: его клавиша не входит в разрешённые символы контракта.

/// Разобранное выражение.
#[derive(Clone, Debug, PartialEq)]
pub struct CalcExpr {
    /// Нажатия для калькулятора Windows, заканчиваются `=`: `33*78=`, `2.5+4=`.
    /// Допустимы только цифры, `.`, `+ - * /`, `%`, `=`, `(` и `)`.
    pub keys: String,
    /// Как показать выражение в логе: `33 × 78`.
    pub pretty: String,
    /// Результат, если собственный расчёт конечен; деление на ноль даёт `None`.
    pub value: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Operator {
    Add,
    Subtract,
    Multiply,
    Divide,
}

impl Operator {
    fn key(self) -> char {
        match self {
            Self::Add => '+',
            Self::Subtract => '-',
            Self::Multiply => '*',
            Self::Divide => '/',
        }
    }

    fn pretty(self) -> char {
        match self {
            Self::Add => '+',
            Self::Subtract => '−',
            Self::Multiply => '×',
            Self::Divide => '÷',
        }
    }

    fn is_high_precedence(self) -> bool {
        matches!(self, Self::Multiply | Self::Divide)
    }
}

#[derive(Clone, Debug)]
struct Operand {
    value: f64,
    /// Может быть произведением для «в квадрате/в кубе», например `3*3`.
    keys: String,
    pretty: String,
    percent: bool,
    composite: bool,
    power_expression: bool,
}

/// Разбирает нормализованную цель команды, например `33 умножить на 78`.
/// Отдельные префиксы-вводные слова допустимы, но любые посторонние слова внутри
/// выражения приводят к `None`, чтобы случайно не вводить распознанный мусор.
/// Скобки нужны лишь в выражениях, где стандартный режим вычислил бы слева направо
/// иначе, чем математика; calcapp переключает режим калькулятора при наличии `(`.
/// Фразы о корне возвращают `None`; степень целого неотрицательного числа раскрывается
/// повторным умножением, поскольку `^` и клавиша квадратного корня вне разрешённых keys.
pub fn parse_expression(text: &str) -> Option<CalcExpr> {
    let mut words = spoken_numbers(tokenize(text));
    strip_polite_wrappers(&mut words);
    if words.is_empty() || words.len() > 128 {
        return None;
    }

    let (first, mut cursor) = parse_operand(&words, 0)?;
    let (mut operands, mut operators) = if first.percent
        && words
            .get(cursor)
            .is_some_and(|word| matches!(word.as_str(), "от" | "of"))
    {
        let (base, next) = parse_operand(&words, cursor + 1)?;
        if base.percent {
            return None;
        }
        cursor = next;
        (vec![base, first], vec![Operator::Multiply])
    } else {
        (vec![first], Vec::new())
    };

    while cursor < words.len() {
        if is_polite_word(&words[cursor]) {
            cursor += 1;
            continue;
        }
        let (operator, next) = parse_operator(&words, cursor)?;
        let (operand, after_operand) = parse_operand(&words, next)?;
        operators.push(operator);
        operands.push(operand);
        if operators.len() > 32 {
            return None;
        }
        cursor = after_operand;
    }

    // Одно число или число с процентом само по себе не является выражением.
    if (operators.is_empty() && !operands.first()?.power_expression)
        || operands.len() != operators.len() + 1
    {
        return None;
    }

    let keys = render_expression(&operands, &operators, true);
    let pretty = render_expression(&operands, &operators, false);
    let value = evaluate(&operands, &operators);
    Some(CalcExpr {
        keys,
        pretty,
        value,
    })
}

fn tokenize(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.to_lowercase().chars().collect();
    let mut words = Vec::new();
    let mut current = String::new();
    for (index, character) in chars.iter().copied().enumerate() {
        let symbol = match character {
            '−' => Some('-'),
            '×' => Some('*'),
            '÷' => Some('/'),
            '+' | '-' | '*' | '/' | '%' => Some(character),
            _ => None,
        };
        if let Some(symbol) = symbol {
            flush_word(&mut words, &mut current);
            words.push(symbol.to_string());
        } else if character == '.' || character == ',' {
            let decimal_separator = current.chars().all(|c| c.is_ascii_digit())
                && !current.is_empty()
                && chars
                    .get(index + 1)
                    .is_some_and(|next| next.is_ascii_digit());
            if decimal_separator {
                current.push('.');
            } else {
                flush_word(&mut words, &mut current);
            }
        } else if character.is_alphanumeric() {
            current.push(character);
        } else {
            flush_word(&mut words, &mut current);
        }
    }
    flush_word(&mut words, &mut current);
    words
}

/// Числа словами на языках из языковых пакетов и иероглифами → цифры. Только здесь, в
/// калькуляторе: в обычной команде «otto», «elf» или «sei» бывают названием, а «un» и
/// «um» — артиклем. Ещё «12x12» → «12 * 12».
fn spoken_numbers(words: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(words.len());
    let mut index = 0;
    while index < words.len() {
        let word = words[index].as_str();
        if let Some((left, right)) = word.split_once('x') {
            if !left.is_empty()
                && !right.is_empty()
                && left.chars().all(|c| c.is_ascii_digit())
                && right.chars().all(|c| c.is_ascii_digit())
            {
                out.extend([left.to_string(), "*".to_string(), right.to_string()]);
                index += 1;
                continue;
            }
        }
        if word == "x" && out.last().is_some_and(|last| last.parse::<f64>().is_ok()) {
            out.push("*".into());
            index += 1;
            continue;
        }
        // Иероглифы normalize разбил по одному: «十 二» — одно число.
        if word.chars().all(is_cjk_numeral) {
            let mut run = String::new();
            while let Some(next) = words.get(index).filter(|next| next.chars().all(is_cjk_numeral)) {
                run.push_str(next);
                index += 1;
            }
            match cjk_number(&run) {
                Some(value) => out.push(value.to_string()),
                None => out.push(run),
            }
            continue;
        }
        if foreign_number(word).is_some() {
            let mut parts = Vec::new();
            let mut end = index;
            while end < words.len() {
                if let Some(value) = foreign_number(&words[end]) {
                    parts.push(value);
                    end += 1;
                } else if matches!(words[end].as_str(), "und" | "y" | "e" | "et")
                    && words.get(end + 1).and_then(|next| foreign_number(next)).is_some()
                    && !parts.is_empty()
                {
                    end += 1;
                } else {
                    break;
                }
            }
            out.push(combine_number(&parts).to_string());
            index = end;
            continue;
        }
        out.push(words[index].clone());
        index += 1;
    }
    out
}

fn is_cjk_numeral(c: char) -> bool {
    "零〇一二两三四五六七八九十百千万".contains(c)
}

/// «十二» → 12, «一百零五» → 105, «两千» → 2000.
fn cjk_number(text: &str) -> Option<u64> {
    let (mut total, mut section, mut number) = (0u64, 0u64, None::<u64>);
    for c in text.chars() {
        let digit = match c {
            '零' | '〇' => Some(0),
            '一' => Some(1),
            '二' | '两' => Some(2),
            '三' => Some(3),
            '四' => Some(4),
            '五' => Some(5),
            '六' => Some(6),
            '七' => Some(7),
            '八' => Some(8),
            '九' => Some(9),
            _ => None,
        };
        if let Some(digit) = digit {
            number = Some(digit);
            continue;
        }
        let unit = match c {
            '十' => 10,
            '百' => 100,
            '千' => 1000,
            '万' => {
                total = (total + section + number.take().unwrap_or(0)) * 10_000;
                section = 0;
                continue;
            }
            _ => return None,
        };
        section += number.take().unwrap_or(1) * unit;
    }
    Some(total + section + number.unwrap_or(0))
}

/// Значение числительного немецкого, французского, испанского, итальянского и
/// португальского; разряды (100, 1000) — со знаком минус, чтобы отличить от слагаемых.
fn foreign_number(word: &str) -> Option<i64> {
    // Немецкое «zweiundzwanzig» — одно слово.
    if let Some((unit, ten)) = word.split_once("und") {
        if let (Some(unit), Some(ten)) = (foreign_number(unit), foreign_number(ten)) {
            if (1..10).contains(&unit) && ten >= 20 && ten % 10 == 0 {
                return Some(unit + ten);
            }
        }
    }
    let value = match word {
        "eins" | "un" | "uno" | "um" => 1,
        "zwei" | "deux" | "dos" | "due" | "dois" => 2,
        "drei" | "trois" | "tres" | "tre" | "três" => 3,
        "vier" | "quatre" | "cuatro" | "quattro" | "quatro" => 4,
        "fünf" | "cinq" | "cinco" | "cinque" => 5,
        "sechs" | "seis" | "sei" => 6,
        "sieben" | "sept" | "siete" | "sette" | "sete" => 7,
        "acht" | "huit" | "ocho" | "otto" | "oito" => 8,
        "neun" | "neuf" | "nueve" | "nove" => 9,
        "zehn" | "dix" | "diez" | "dieci" | "dez" => 10,
        "elf" | "onze" | "once" | "undici" => 11,
        "zwölf" | "douze" | "doce" | "dodici" | "doze" => 12,
        "dreizehn" | "treize" | "trece" | "tredici" | "treze" => 13,
        "vierzehn" | "quatorze" | "catorce" | "quattordici" | "catorze" => 14,
        "fünfzehn" | "quinze" | "quince" | "quindici" => 15,
        "sechzehn" | "seize" | "dieciséis" | "sedici" | "dezesseis" => 16,
        "siebzehn" | "diecisiete" | "diciassette" | "dezessete" => 17,
        "achtzehn" | "dieciocho" | "diciotto" | "dezoito" => 18,
        "neunzehn" | "diecinueve" | "diciannove" | "dezenove" => 19,
        "zwanzig" | "vingt" | "veinte" | "venti" | "vinte" => 20,
        "dreißig" | "trente" | "treinta" | "trenta" | "trinta" => 30,
        "vierzig" | "quarante" | "cuarenta" | "quaranta" | "quarenta" => 40,
        "fünfzig" | "cinquante" | "cincuenta" | "cinquanta" | "cinquenta" => 50,
        "sechzig" | "soixante" | "sesenta" | "sessanta" | "sessenta" => 60,
        "siebzig" | "setenta" | "settanta" => 70,
        "achtzig" | "ochenta" | "ottanta" | "oitenta" => 80,
        "neunzig" | "noventa" | "novanta" => 90,
        "hundert" | "cent" | "cien" | "ciento" | "cento" | "cem" => -100,
        "tausend" | "mille" | "mil" => -1000,
        _ => return None,
    };
    Some(value)
}

/// «vingt deux» → 22, «zwei hundert» → 200, «quatre vingt» → 80, «mil doscientos» нет.
fn combine_number(parts: &[i64]) -> i64 {
    let (mut total, mut current) = (0i64, 0i64);
    for (index, &part) in parts.iter().enumerate() {
        match part {
            -100 => current = current.max(1) * 100,
            -1000 => {
                total += current.max(1) * 1000;
                current = 0;
            }
            // Французское «quatre vingt» = 80: единица перед «vingt» — множитель.
            20 if index > 0 && (2..10).contains(&parts[index - 1]) && current < 10 => {
                current = current * 20;
            }
            value => current += value,
        }
    }
    total + current
}

fn flush_word(words: &mut Vec<String>, current: &mut String) {
    if !current.is_empty() {
        words.push(std::mem::take(current));
    }
}

fn strip_polite_wrappers(words: &mut Vec<String>) {
    loop {
        if words.first().is_some_and(|word| is_polite_word(word)) {
            words.remove(0);
            continue;
        }
        if starts_with(words, &["сколько", "будет"])
            || starts_with(words, &["what", "is"])
            || starts_with(words, &["what", "will", "be"])
            || starts_with(words, &["how", "much", "is"])
        {
            let count = if words.starts_with(&["what".into(), "will".into(), "be".into()])
                || words.starts_with(&["how".into(), "much".into(), "is".into()])
            {
                3
            } else {
                2
            };
            words.drain(..count);
            continue;
        }
        if words.first().is_some_and(|word| {
            matches!(
                word.as_str(),
                "calculate" | "compute" | "посчитай" | "посчитать"
            )
        }) {
            words.remove(0);
            continue;
        }
        break;
    }
    while words.last().is_some_and(|word| is_polite_word(word)) {
        words.pop();
    }
}

fn starts_with(words: &[String], prefix: &[&str]) -> bool {
    words.len() >= prefix.len()
        && words
            .iter()
            .zip(prefix)
            .all(|(word, prefix)| word == prefix)
}

fn is_polite_word(word: &str) -> bool {
    matches!(word, "пожалуйста" | "please" | "ну" | "слушай")
}

fn parse_operand(words: &[String], start: usize) -> Option<(Operand, usize)> {
    let mut cursor = start;
    let mut negative_prefix = false;
    if words
        .get(cursor)
        .is_some_and(|word| matches!(word.as_str(), "минус" | "minus" | "negative" | "-"))
    {
        negative_prefix = true;
        cursor += 1;
    }

    let (mut value, mut keys) = parse_numeric_atom(words.get(cursor)?)?;
    let mut pretty = keys.clone();
    cursor += 1;

    // Дробные слова остаются словами после общей нормализации и обрабатываются здесь,
    // не меняя textnorm, который обслуживает весь остальной голосовой конвейер.
    let decimal_part_start = words.get(cursor).and_then(|word| {
        (is_whole_marker(word) || matches!(word.as_str(), "и" | "and")).then_some(cursor + 1)
    });
    if let Some(decimal_part_start) = decimal_part_start {
        if let Some((fraction, after_fraction)) = parse_decimal_word_part(words, decimal_part_start)
        {
            value += fraction;
            keys = format_decimal(value);
            pretty = keys.clone();
            cursor = after_fraction;
        }
    } else if let Some(denominator) = words
        .get(cursor)
        .and_then(|word| fractional_denominator(word))
    {
        value /= denominator;
        keys = format_decimal(value);
        pretty = keys.clone();
        cursor += 1;
    } else if words
        .get(cursor)
        .is_some_and(|word| matches!(word.as_str(), "точка" | "запятая" | "point" | "decimal"))
    {
        if let Some(fraction_word) = words.get(cursor + 1) {
            if let Some(fraction_digits) = decimal_digits(fraction_word) {
                if value.fract() == 0.0 {
                    let integer = keys.trim_start_matches('-');
                    let sign = if value < 0.0 { "-" } else { "" };
                    let joined = format!("{sign}{integer}.{fraction_digits}");
                    if let Some((decimal_value, canonical)) = parse_decimal_literal(&joined) {
                        value = decimal_value;
                        keys = canonical;
                        pretty = keys.clone();
                        cursor += 2;
                    }
                }
            }
        }
    }

    // «N с половиной» однозначно задаёт дробную часть; просто «N и M» неоднозначно.
    if words.get(cursor).is_some_and(|word| word == "с")
        && words
            .get(cursor + 1)
            .is_some_and(|word| matches!(word.as_str(), "половиной" | "половина" | "half"))
    {
        value += 0.5;
        keys = format_decimal(value);
        pretty = keys.clone();
        cursor += 2;
    }

    let (power, after_power) = parse_power_suffix(words, cursor)?;
    let power_expression = after_power != cursor;
    if power_expression {
        cursor = after_power;
    }
    if power_expression {
        let base_value = value;
        let base_keys = keys.clone();
        let base_pretty = pretty.clone();
        if power == 0 {
            value = 1.0;
            keys = "1".into();
        } else if power > 1 {
            value = base_value.powi(power as i32);
            keys = std::iter::repeat_n(base_keys, power as usize)
                .collect::<Vec<_>>()
                .join("*");
        }
        pretty = match power {
            2 => format!("{base_pretty}²"),
            3 => format!("{base_pretty}³"),
            _ => format!("{base_pretty}^{power}"),
        };
    }

    if negative_prefix {
        value = -value;
        if !keys.starts_with('-') {
            keys.insert(0, '-');
        }
        pretty = format!("-{pretty}");
    }

    // «per cent» — двумя словами; одно «per» — итальянское «умножить» («12 per 12»).
    let percent_words = match (words.get(cursor).map(String::as_str), words.get(cursor + 1).map(String::as_str)) {
        (Some("per"), Some("cent")) => 2,
        (Some("per"), _) => 0,
        (Some(word), _) if is_percent_word(word) => 1,
        _ => 0,
    };
    let percent = percent_words > 0;
    if percent {
        if power_expression {
            // `3 в квадрате процентов` не является ни однозначной фразой, ни
            // однозначным вводом клавиш калькулятора.
            return None;
        }
        keys.push('%');
        pretty.push('%');
        cursor += percent_words;
    }

    Some((
        Operand {
            value,
            keys,
            pretty,
            percent,
            composite: power > 1,
            power_expression,
        },
        cursor,
    ))
}

fn parse_numeric_atom(token: &str) -> Option<(f64, String)> {
    match token {
        "полтора" | "полторы" => Some((1.5, "1.5".into())),
        "половина" | "half" => Some((0.5, "0.5".into())),
        _ => parse_decimal_literal(token),
    }
}

fn parse_decimal_literal(text: &str) -> Option<(f64, String)> {
    let text = text.replace(',', ".");
    let (negative, unsigned) = text
        .strip_prefix('-')
        .map(|rest| (true, rest))
        .unwrap_or((false, text.as_str()));
    let mut parts = unsigned.split('.');
    let integer = parts.next()?;
    let fraction = parts.next();
    if parts.next().is_some()
        || (integer.is_empty() && fraction.is_none())
        || !integer.chars().all(|character| character.is_ascii_digit())
        || fraction.is_some_and(|digits| {
            digits.is_empty() || !digits.chars().all(|character| character.is_ascii_digit())
        })
    {
        return None;
    }
    let value = text.parse::<f64>().ok()?;
    let integer = if integer.is_empty() {
        "0"
    } else {
        integer.trim_start_matches('0')
    };
    let integer = if integer.is_empty() { "0" } else { integer };
    let mut canonical = if let Some(fraction) = fraction {
        let fraction = fraction.trim_end_matches('0');
        if fraction.is_empty() {
            integer.to_string()
        } else {
            format!("{integer}.{fraction}")
        }
    } else {
        integer.to_string()
    };
    if negative && value != 0.0 {
        canonical.insert(0, '-');
    }
    Some((value, canonical))
}

fn parse_decimal_word_part(words: &[String], start: usize) -> Option<(f64, usize)> {
    let (fraction_value, _) = parse_numeric_atom(words.get(start)?)?;
    if fraction_value < 0.0 || fraction_value.fract() != 0.0 {
        return None;
    }
    let denominator = fractional_denominator(words.get(start + 1)?)?;
    (fraction_value < denominator).then_some((fraction_value / denominator, start + 2))
}

fn fractional_denominator(word: &str) -> Option<f64> {
    match word {
        "десятая" | "десятую" | "десятых" | "tenths" => Some(10.0),
        "сотая" | "сотую" | "сотых" | "hundredths" => Some(100.0),
        "тысячная" | "тысячную" | "тысячных" | "thousandths" => {
            Some(1000.0)
        }
        _ => None,
    }
}

fn decimal_digits(text: &str) -> Option<String> {
    if text.is_empty() || !text.chars().all(|character| character.is_ascii_digit()) {
        return None;
    }
    Some(text.to_string())
}

fn is_whole_marker(word: &str) -> bool {
    matches!(word, "целая" | "целых" | "целое" | "целый" | "whole")
}

fn is_percent_word(word: &str) -> bool {
    matches!(
        word,
        "%" | "процент"
            | "процента"
            | "процентов"
            | "проценты"
            | "percent"
            | "per"
            | "cent"
    )
}

fn parse_power_suffix(words: &[String], start: usize) -> Option<(u32, usize)> {
    match words.get(start).map(String::as_str) {
        Some("в") => match words.get(start + 1).map(String::as_str) {
            Some("квадрате") => Some((2, start + 2)),
            Some("кубе") => Some((3, start + 2)),
            Some("степени") => {
                let exponent = words.get(start + 2)?.parse::<u32>().ok()?;
                (exponent <= 12).then_some((exponent, start + 3))
            }
            _ => Some((1, start)),
        },
        Some("squared") => Some((2, start + 1)),
        Some("cubed") => Some((3, start + 1)),
        Some("to") if starts_with(words.get(start..)?, &["to", "the", "power", "of"]) => {
            let exponent = words.get(start + 4)?.parse::<u32>().ok()?;
            (exponent <= 12).then_some((exponent, start + 5))
        }
        _ => Some((1, start)),
    }
}

fn parse_operator(words: &[String], start: usize) -> Option<(Operator, usize)> {
    let rest = words.get(start..)?;
    let forms: &[(&[&str], Operator)] = &[
        (&["разделить", "на"], Operator::Divide),
        (&["поделить", "на"], Operator::Divide),
        (&["делить", "на"], Operator::Divide),
        (&["divided", "by"], Operator::Divide),
        (&["divide", "by"], Operator::Divide),
        (&["умножить", "на"], Operator::Multiply),
        (&["умножь", "на"], Operator::Multiply),
        (&["помножить", "на"], Operator::Multiply),
        (&["multiplied", "by"], Operator::Multiply),
        (&["multiply", "by"], Operator::Multiply),
        (&["сложить", "с"], Operator::Add),
        (&["прибавить"], Operator::Add),
        (&["прибавь"], Operator::Add),
        (&["отнять"], Operator::Subtract),
        (&["вычесть"], Operator::Subtract),
        (&["plus"], Operator::Add),
        (&["minus"], Operator::Subtract),
        (&["times"], Operator::Multiply),
        (&["over"], Operator::Divide),
        (&["плюс"], Operator::Add),
        (&["минус"], Operator::Subtract),
        (&["без"], Operator::Subtract),
        (&["на"], Operator::Multiply),
        // Языки из языковых пакетов (models.rs). Иероглифы normalize разбивает на знаки:
        // «乘以» — это «乘», «以».
        (&["geteilt", "durch"], Operator::Divide),
        (&["durch"], Operator::Divide),
        (&["mal"], Operator::Multiply),
        (&["divisé", "par"], Operator::Divide),
        (&["multiplié", "par"], Operator::Multiply),
        (&["fois"], Operator::Multiply),
        (&["moins"], Operator::Subtract),
        (&["dividido", "entre"], Operator::Divide),
        (&["dividido", "por"], Operator::Divide),
        (&["multiplicado", "por"], Operator::Multiply),
        (&["por"], Operator::Multiply),
        (&["más"], Operator::Add),
        (&["menos"], Operator::Subtract),
        (&["diviso", "per"], Operator::Divide),
        (&["diviso"], Operator::Divide),
        (&["moltiplicato", "per"], Operator::Multiply),
        (&["per"], Operator::Multiply),
        (&["più"], Operator::Add),
        (&["meno"], Operator::Subtract),
        (&["vezes"], Operator::Multiply),
        (&["mais"], Operator::Add),
        (&["乘", "以"], Operator::Multiply),
        (&["除", "以"], Operator::Divide),
        (&["乘"], Operator::Multiply),
        (&["加"], Operator::Add),
        (&["减"], Operator::Subtract),
        (&["掛", "け", "る"], Operator::Multiply),
        (&["か", "け", "る"], Operator::Multiply),
        (&["割", "る"], Operator::Divide),
        (&["わ", "る"], Operator::Divide),
        (&["足", "す"], Operator::Add),
        (&["引", "く"], Operator::Subtract),
        (&["+"], Operator::Add),
        (&["-"], Operator::Subtract),
        (&["*"], Operator::Multiply),
        (&["/"], Operator::Divide),
    ];
    for (phrase, operator) in forms {
        if rest.len() >= phrase.len()
            && rest
                .iter()
                .zip(*phrase)
                .all(|(word, expected)| word == expected)
        {
            return Some((*operator, start + phrase.len()));
        }
    }
    None
}

fn render_expression(operands: &[Operand], operators: &[Operator], keys: bool) -> String {
    let mut open = vec![false; operands.len()];
    let mut close = vec![false; operands.len()];

    // Ввод слева направо ошибётся только если после +/− идёт сегмент */.
    // Ограничиваем скобками сам такой сегмент, а одинаковоприоритетные выражения
    // оставляем без лишних скобок.
    for index in 1..operators.len() {
        if operators[index].is_high_precedence() && !operators[index - 1].is_high_precedence() {
            let mut end = index;
            while end + 1 < operators.len() && operators[end + 1].is_high_precedence() {
                end += 1;
            }
            open[index] = true;
            close[end + 1] = true;
        }
    }
    if keys {
        for index in 1..operands.len() {
            let starts_high_precedence_segment = operators
                .get(index)
                .is_some_and(|operator| operator.is_high_precedence());
            if operands[index].composite
                && !operators[index - 1].is_high_precedence()
                && !starts_high_precedence_segment
            {
                open[index] = true;
                close[index] = true;
            }
        }
    }

    let mut rendered = String::new();
    for (index, operand) in operands.iter().enumerate() {
        if open[index] {
            rendered.push('(');
        }
        rendered.push_str(if keys { &operand.keys } else { &operand.pretty });
        if close[index] {
            rendered.push(')');
        }
        if let Some(operator) = operators.get(index) {
            if !keys {
                rendered.push(' ');
            }
            rendered.push(if keys {
                operator.key()
            } else {
                operator.pretty()
            });
            if !keys {
                rendered.push(' ');
            }
        }
    }
    if keys {
        rendered.push('=');
    }
    rendered
}

fn evaluate(operands: &[Operand], operators: &[Operator]) -> Option<f64> {
    let first = operands.first()?;
    let mut terms: Vec<(f64, bool, Operator)> = Vec::new();
    let mut term = if first.percent {
        first.value / 100.0
    } else {
        first.value
    };
    let mut term_is_percent_only = first.percent;
    let mut low_operator = Operator::Add;

    for (index, operator) in operators.iter().enumerate() {
        let operand = operands.get(index + 1)?;
        if operator.is_high_precedence() {
            let right = if operand.percent {
                operand.value / 100.0
            } else {
                operand.value
            };
            term = apply_operator(term, right, *operator)?;
            term_is_percent_only = false;
        } else {
            terms.push((term, term_is_percent_only, low_operator));
            term = if operand.percent {
                operand.value / 100.0
            } else {
                operand.value
            };
            term_is_percent_only = operand.percent;
            low_operator = *operator;
        }
    }
    terms.push((term, term_is_percent_only, low_operator));

    let mut total = terms.first()?.0;
    for (term, percent_only, operator) in terms.iter().skip(1) {
        let right = if *percent_only { total * term } else { *term };
        total = apply_operator(total, right, *operator)?;
    }
    total.is_finite().then_some(total)
}

fn apply_operator(left: f64, right: f64, operator: Operator) -> Option<f64> {
    let result = match operator {
        Operator::Add => left + right,
        Operator::Subtract => left - right,
        Operator::Multiply => left * right,
        Operator::Divide if right != 0.0 => left / right,
        Operator::Divide => return None,
    };
    result.is_finite().then_some(result)
}

fn format_decimal(value: f64) -> String {
    if value == 0.0 {
        return "0".into();
    }
    if value.fract() == 0.0 && value.abs() < i64::MAX as f64 {
        return format!("{value:.0}");
    }
    let mut formatted = format!("{value:.12}");
    while formatted.ends_with('0') {
        formatted.pop();
    }
    if formatted.ends_with('.') {
        formatted.pop();
    }
    formatted
}

#[cfg(test)]
mod tests {
    use super::parse_expression;
    use crate::textnorm::normalize;

    fn parse(spoken: &str) -> Option<super::CalcExpr> {
        parse_expression(&normalize(spoken))
    }

    #[test]
    fn normalizer_turns_required_spoken_cardinals_into_digits() {
        assert_eq!(
            normalize("тридцать три умножить на семьдесят восемь"),
            "33 умножить на 78"
        );
        assert_eq!(
            normalize("сто двадцать пять разделить на пять"),
            "125 разделить на 5"
        );
        assert_eq!(
            normalize("две тысячи двадцать шесть минус девятьсот"),
            "2026 минус 900"
        );
        assert_eq!(normalize("ноль"), "0");
        assert_eq!(normalize("thirty three times seventy eight"), "33 times 78");
    }

    #[test]
    fn parses_russian_and_english_arithmetic() {
        let expr = parse("тридцать три умножить на семьдесят восемь").unwrap();
        assert_eq!(expr.keys, "33*78=");
        assert_eq!(expr.pretty, "33 × 78");
        assert_eq!(expr.value, Some(2574.0));

        let expr = parse("thirty three times seventy eight").unwrap();
        assert_eq!(expr.keys, "33*78=");
        assert_eq!(expr.value, Some(2574.0));

        let expr = parse("сто двадцать пять разделить на пять").unwrap();
        assert_eq!(expr.keys, "125/5=");
        assert_eq!(expr.value, Some(25.0));

        let expr = parse("две тысячи двадцать шесть минус девятьсот").unwrap();
        assert_eq!(expr.value, Some(1126.0));
    }

    #[test]
    fn supports_the_operator_phrases_from_the_contract() {
        for (spoken, keys, value) in [
            ("20 прибавить 5", "20+5=", 25.0),
            ("20 прибавь 5", "20+5=", 25.0),
            ("20 сложить с 5", "20+5=", 25.0),
            ("20 отнять 5", "20-5=", 15.0),
            ("20 вычесть 5", "20-5=", 15.0),
            ("20 без 5", "20-5=", 15.0),
            ("5 помножить на 4", "5*4=", 20.0),
            ("12 делить на 3", "12/3=", 4.0),
            ("12 поделить на 3", "12/3=", 4.0),
            ("12 divide by 3", "12/3=", 4.0),
            ("12 divided by 3", "12/3=", 4.0),
            ("12 over 3", "12/3=", 4.0),
        ] {
            let expr = parse(spoken).unwrap_or_else(|| panic!("failed to parse: {spoken}"));
            assert_eq!(expr.keys, keys, "{spoken}");
            assert_eq!(expr.value, Some(value), "{spoken}");
        }
    }

    #[test]
    fn preserves_math_precedence_with_only_needed_parentheses() {
        let expr = parse("два плюс три умножить на четыре").unwrap();
        assert_eq!(expr.keys, "2+(3*4)=");
        assert_eq!(expr.pretty, "2 + (3 × 4)");
        assert_eq!(expr.value, Some(14.0));

        let expr = parse("два умножить на три плюс четыре").unwrap();
        assert_eq!(expr.keys, "2*3+4=");
        assert_eq!(expr.value, Some(10.0));

        let expr = parse("два плюс три умножить на четыре минус пять умножить на два").unwrap();
        assert_eq!(expr.keys, "2+(3*4)-(5*2)=");
        assert_eq!(expr.value, Some(4.0));

        let powered = parse("два плюс три в квадрате умножить на четыре").unwrap();
        assert_eq!(powered.keys, "2+(3*3*4)=");
        assert_eq!(powered.value, Some(38.0));
    }

    #[test]
    fn handles_percent_of_and_calculator_percent_semantics() {
        let expr = parse("15 процентов от 200").unwrap();
        assert_eq!(expr.keys, "200*15%=");
        assert_eq!(expr.pretty, "200 × 15%");
        assert_eq!(expr.value, Some(30.0));

        let expr = parse("200 плюс 15 процентов").unwrap();
        assert_eq!(expr.keys, "200+15%=");
        assert_eq!(expr.value, Some(230.0));
    }

    #[test]
    fn handles_spoken_decimals_halves_and_negative_numbers() {
        let expr = parse("две целых пять десятых плюс полтора").unwrap();
        assert_eq!(expr.keys, "2.5+1.5=");
        assert_eq!(expr.value, Some(4.0));

        let expr = parse("два и пять десятых плюс одна десятая").unwrap();
        assert_eq!(expr.keys, "2.5+0.1=");
        assert_eq!(expr.value, Some(2.6));

        let expr = parse("два точка пять умножить на четыре").unwrap();
        assert_eq!(expr.keys, "2.5*4=");
        assert_eq!(expr.value, Some(10.0));

        let expr = parse("минус пять плюс три").unwrap();
        assert_eq!(expr.keys, "-5+3=");
        assert_eq!(expr.value, Some(-2.0));

        let expr = parse("полтора плюс половина").unwrap();
        assert_eq!(expr.value, Some(2.0));
    }

    #[test]
    fn squares_cubes_and_integer_powers_expand_to_multiplication() {
        let square = parse("три в квадрате плюс два").unwrap();
        assert_eq!(square.keys, "3*3+2=");
        assert_eq!(square.pretty, "3² + 2");
        assert_eq!(square.value, Some(11.0));

        let cube = parse("два в кубе").unwrap();
        assert_eq!(cube.keys, "2*2*2=");
        assert_eq!(cube.value, Some(8.0));

        let square_only = parse("три в квадрате").unwrap();
        assert_eq!(square_only.keys, "3*3=");
        assert_eq!(square_only.value, Some(9.0));

        let square_seven = parse("семь в квадрате").unwrap();
        assert_eq!(square_seven.keys, "7*7=");
        assert_eq!(square_seven.value, Some(49.0));

        let power = parse("три в степени четыре").unwrap();
        assert_eq!(power.keys, "3*3*3*3=");
        assert_eq!(power.value, Some(81.0));

        let power_one = parse("два в степени один").unwrap();
        assert_eq!(power_one.keys, "2=");
        assert_eq!(power_one.value, Some(2.0));

        let power_zero = parse("два в степени ноль").unwrap();
        assert_eq!(power_zero.keys, "1=");
        assert_eq!(power_zero.value, Some(1.0));

        let english_power = parse("two to the power of three").unwrap();
        assert_eq!(english_power.keys, "2*2*2=");
        assert_eq!(english_power.value, Some(8.0));

        assert!(parse("корень из девяти").is_none());
        assert!(parse("два и пять").is_none());
    }

    #[test]
    fn fillers_are_ignored_but_non_arithmetic_is_rejected() {
        let expr = parse("пожалуйста сколько будет двадцать плюс два").unwrap();
        assert_eq!(expr.keys, "20+2=");
        assert_eq!(expr.value, Some(22.0));
        assert_eq!(parse("посчитай овец"), None);
        assert_eq!(parse("посчитай до десяти"), None);
        assert_eq!(parse("сколько будет"), None);
        assert_eq!(parse("ноль"), None);
        assert_eq!(parse("два три"), None);
    }

    #[test]
    fn keeps_keys_when_value_is_undefined_or_overflows() {
        let zero = parse("пять разделить на ноль").unwrap();
        assert_eq!(zero.keys, "5/0=");
        assert_eq!(zero.value, None);

        let huge_integer = "9".repeat(400);
        let overflow = parse_expression(&format!("{huge_integer} умножить на 2")).unwrap();
        assert_eq!(overflow.keys, format!("{huge_integer}*2="));
        assert!(overflow
            .keys
            .chars()
            .all(|character| { character.is_ascii_digit() || ".+-*/%()=".contains(character) }));
        assert_eq!(overflow.value, None);

        let decimal = parse_expression("2.5 plus 1.25").unwrap();
        assert_eq!(decimal.keys, "2.5+1.25=");
        assert_eq!(decimal.value, Some(3.75));
    }

    #[test]
    fn language_pack_operators() {
        for (text, value) in [
            ("33 mal 78", 2574.0),
            ("125 geteilt durch 5", 25.0),
            ("12 fois 12", 144.0),
            ("10 menos 4", 6.0),
            ("7 più 3", 10.0),
            ("6 vezes 7", 42.0),
            ("2乘以3", 6.0),
            ("10减4", 6.0),
            ("8割る2", 4.0),
            ("zwölf mal zwölf", 144.0),
            ("douze fois douze", 144.0),
            ("zweiundzwanzig plus eins", 23.0),
            ("quatre vingt plus deux", 82.0),
            ("12 per 12", 144.0),
            ("12x12", 144.0),
            ("十二乘以十二", 144.0),
            ("一百零五减五", 100.0),
            ("veinte y dos menos dos", 20.0),
        ] {
            let expr = parse(text).unwrap_or_else(|| panic!("{text}"));
            assert_eq!(expr.value, Some(value), "{text}");
        }
    }
}
