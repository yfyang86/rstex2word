//! siunitx units and numbers -> Unicode text (port of `frontend/siunitx.py`).
//!
//! Renders `\si`/`\unit`/`\SI`/`\qty`/`\num`/`\ang` to plain Unicode (e.g.
//! `\SI{5}{\kilo\gram}` -> `5 kg`) when the user hasn't redefined them (the
//! macro expander runs first).

use std::sync::LazyLock;

use tex2word_ir::pystr;
use tex2word_ir::re::Regex;
use tex2word_latexwalker::{Node, NodeKind};

/// Thin space between a value and its unit / between units.
pub const THIN: &str = "\u{2009}";

/// SI prefixes (`\kilo`, `\milli`, …) -> symbols.
fn prefix(name: &str) -> Option<&'static str> {
    Some(match name {
        "yocto" => "y",
        "zepto" => "z",
        "atto" => "a",
        "femto" => "f",
        "pico" => "p",
        "nano" => "n",
        "micro" => "µ",
        "milli" => "m",
        "centi" => "c",
        "deci" => "d",
        "deca" => "da",
        "deka" => "da",
        "hecto" => "h",
        "kilo" => "k",
        "mega" => "M",
        "giga" => "G",
        "tera" => "T",
        "peta" => "P",
        "exa" => "E",
        "zetta" => "Z",
        "yotta" => "Y",
        _ => return None,
    })
}

/// SI units (`\metre`, `\joule`, …) -> symbols.
fn unit(name: &str) -> Option<&'static str> {
    Some(match name {
        "metre" => "m",
        "meter" => "m",
        "gram" => "g",
        "second" => "s",
        "kilogram" => "kg",
        "kelvin" => "K",
        "ampere" => "A",
        "mole" => "mol",
        "candela" => "cd",
        "newton" => "N",
        "pascal" => "Pa",
        "joule" => "J",
        "watt" => "W",
        "coulomb" => "C",
        "volt" => "V",
        "farad" => "F",
        "ohm" => "Ω",
        "siemens" => "S",
        "weber" => "Wb",
        "tesla" => "T",
        "henry" => "H",
        "hertz" => "Hz",
        "lumen" => "lm",
        "lux" => "lx",
        "becquerel" => "Bq",
        "gray" => "Gy",
        "sievert" => "Sv",
        "katal" => "kat",
        "litre" => "L",
        "liter" => "L",
        "electronvolt" => "eV",
        "celsius" => "°C",
        "degreeCelsius" => "°C",
        "percent" => "%",
        "degree" => "°",
        "arcminute" => "′",
        "arcsecond" => "″",
        "bar" => "bar",
        "angstrom" => "Å",
        "day" => "d",
        "hour" => "h",
        "minute" => "min",
        "tonne" => "t",
        "dalton" => "Da",
        "radian" => "rad",
        "steradian" => "sr",
        _ => return None,
    })
}

/// postfix powers: `\metre\squared`
fn power(name: &str) -> Option<&'static str> {
    match name {
        "squared" => Some("²"),
        "cubed" => Some("³"),
        _ => None,
    }
}

/// prefix powers: `\cubic\metre`
fn prepower(name: &str) -> Option<&'static str> {
    match name {
        "square" => Some("²"),
        "cubic" => Some("³"),
        _ => None,
    }
}

fn sup(c: char) -> char {
    match c {
        '0' => '⁰',
        '1' => '¹',
        '2' => '²',
        '3' => '³',
        '4' => '⁴',
        '5' => '⁵',
        '6' => '⁶',
        '7' => '⁷',
        '8' => '⁸',
        '9' => '⁹',
        '+' => '⁺',
        '-' => '⁻',
        other => other,
    }
}

static NUM_EXP: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([+-]?[\d.,]+)\s*[eE]\s*([+-]?\d+)$"));

/// Render a sequence of unit macros/chars (a `\si` argument) to symbols.
pub fn units_to_text(nodes: &[Node]) -> String {
    let mut out = String::new();
    // next unit attaches without a separator (start, or after a prefix/per)
    let mut attach = true;
    // a \cubic/\square waiting to apply to the next unit
    let mut pending_power = "";
    for n in nodes {
        match &n.kind {
            NodeKind::Macro { macroname, .. } => {
                let name = macroname.as_str();
                if let Some(p) = prepower(name) {
                    pending_power = p;
                } else if let Some(p) = prefix(name) {
                    if !out.is_empty() && !attach {
                        out.push_str(THIN);
                    }
                    out.push_str(p);
                    attach = true;
                } else if let Some(u) = unit(name) {
                    if !out.is_empty() && !attach {
                        out.push_str(THIN);
                    }
                    out.push_str(u);
                    out.push_str(pending_power);
                    pending_power = "";
                    attach = false;
                } else if let Some(p) = power(name) {
                    out.push_str(p);
                    attach = false;
                } else if name == "per" {
                    out.push('/');
                    attach = true;
                }
            }
            NodeKind::Chars { chars } => {
                out.push_str(chars);
                attach = false;
            }
            NodeKind::Group { nodelist, .. } => {
                out.push_str(&units_to_text(nodelist));
                attach = false;
            }
            _ => {}
        }
    }
    pystr::strip(&out).to_string()
}

/// Render a number: `1.5e3` -> `1.5×10³`; otherwise pass through.
pub fn num_to_text(s: &str) -> String {
    let s = pystr::strip(s);
    if let Some(m) = NUM_EXP.match_start(s) {
        let mant = m.group_str(1);
        let exp = pystr::lstrip_chars(m.group_str(2), "+");
        let exp: String = exp.chars().map(sup).collect();
        return format!("{mant}×10{exp}");
    }
    s.to_string()
}

/// Render an angle: `30` -> `30°`; `30;15;0` -> `30°15′`.
pub fn ang_to_text(s: &str) -> String {
    let parts: Vec<&str> = s.split(';').map(pystr::strip).collect();
    if parts.len() == 3 {
        let mut out = String::new();
        if !parts[0].is_empty() {
            out.push_str(&format!("{}°", parts[0]));
        }
        if !parts[1].is_empty() && parts[1] != "0" {
            out.push_str(&format!("{}′", parts[1]));
        }
        if !parts[2].is_empty() && parts[2] != "0" {
            out.push_str(&format!("{}″", parts[2]));
        }
        return if out.is_empty() {
            "0°".to_string()
        } else {
            out
        };
    }
    format!("{}°", pystr::strip(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_and_angles() {
        assert_eq!(num_to_text(" 1.5e+3 "), "1.5×10³");
        assert_eq!(num_to_text("-2E-12"), "-2×10⁻¹²");
        assert_eq!(num_to_text("42"), "42");
        assert_eq!(ang_to_text("30;15;0"), "30°15′");
        assert_eq!(ang_to_text(";;"), "0°");
        assert_eq!(ang_to_text(" 30 "), "30°");
    }
}
