//! Calculator, unit conversion and number formatting. Pure functions, no GTK.

use std::f64::consts;

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    Ident(String),
    Op(&'static str),
    LParen,
    RParen,
    Comma,
}

fn tokenize(src: &str) -> Option<Vec<Tok>> {
    let s: Vec<char> = src.chars().collect();
    let mut out: Vec<Tok> = Vec::new();
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c.is_ascii_digit() || (c == '.' && s.get(i + 1).is_some_and(|d| d.is_ascii_digit())) {
            let mut num = String::new();
            while i < s.len() {
                let d = s[i];
                if d.is_ascii_digit() || d == '.' {
                    num.push(d);
                } else if d == ',' && s.get(i + 1..i + 4).is_some_and(|w| w.iter().all(|x| x.is_ascii_digit()))
                    && !s.get(i + 4).is_some_and(|x| x.is_ascii_digit())
                {
                    // thousands separator: 1,000
                } else if (d == 'e' || d == 'E')
                    && s.get(i + 1).is_some_and(|x| x.is_ascii_digit() || ((*x == '-' || *x == '+') && s.get(i + 2).is_some_and(|y| y.is_ascii_digit())))
                {
                    num.push('e');
                    i += 1;
                    num.push(s[i]);
                } else {
                    break;
                }
                i += 1;
            }
            out.push(Tok::Num(num.parse().ok()?));
            continue;
        }
        if c.is_alphabetic() || c == 'π' || c == '√' {
            if c == 'π' {
                out.push(Tok::Ident("pi".into()));
                i += 1;
                continue;
            }
            if c == '√' {
                out.push(Tok::Ident("sqrt".into()));
                i += 1;
                continue;
            }
            let mut id = String::new();
            while i < s.len() && (s[i].is_alphanumeric() || s[i] == '_') {
                id.push(s[i].to_ascii_lowercase());
                i += 1;
            }
            if id == "of" || id == "x" && matches!(out.last(), Some(Tok::Num(_)) | Some(Tok::RParen)) {
                out.push(Tok::Op("*"));
            } else if id == "mod" {
                out.push(Tok::Op("%"));
            } else {
                out.push(Tok::Ident(id));
            }
            continue;
        }
        let two: String = s[i..(i + 2).min(s.len())].iter().collect();
        let op: Option<&'static str> = match two.as_str() {
            "**" => Some("^"),
            "//" => Some("//"),
            _ => None,
        };
        if let Some(op) = op {
            out.push(Tok::Op(op));
            i += 2;
            continue;
        }
        let tok = match c {
            '+' => Tok::Op("+"),
            '-' | '−' => Tok::Op("-"),
            '*' | '×' | '·' => Tok::Op("*"),
            '/' | '÷' => Tok::Op("/"),
            '^' => Tok::Op("^"),
            '!' => Tok::Op("!"),
            '%' => {
                // "15%" (postfix percent) vs "10 % 3" (modulo)
                let mut j = i + 1;
                while j < s.len() && s[j].is_whitespace() {
                    j += 1;
                }
                let next_is_operand = s.get(j).is_some_and(|n| n.is_ascii_digit() || *n == '(' || *n == '.');
                if next_is_operand { Tok::Op("%") } else { Tok::Op("pct") }
            }
            '(' => Tok::LParen,
            ')' => Tok::RParen,
            ',' => Tok::Comma,
            '=' if i == s.len() - 1 => {
                i += 1;
                continue;
            }
            _ => return None,
        };
        out.push(tok);
        i += 1;
    }
    // implicit multiplication: 2pi, 3(4), (2)(3), 2 sqrt(9)
    let mut fixed = Vec::with_capacity(out.len());
    for t in out {
        let needs_mul = matches!(fixed.last(), Some(Tok::Num(_)) | Some(Tok::RParen))
            && matches!(t, Tok::Ident(_) | Tok::LParen | Tok::Num(_));
        if needs_mul {
            fixed.push(Tok::Op("*"));
        }
        fixed.push(t);
    }
    Some(fixed)
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
    nontrivial: bool,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }
    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        self.pos += 1;
        t
    }
    fn expr(&mut self) -> Option<f64> {
        let mut v = self.term()?;
        while let Some(Tok::Op(op @ ("+" | "-"))) = self.peek().cloned() {
            self.pos += 1;
            self.nontrivial = true;
            let r = self.term()?;
            v = if op == "+" { v + r } else { v - r };
        }
        Some(v)
    }
    fn term(&mut self) -> Option<f64> {
        let mut v = self.unary()?;
        while let Some(Tok::Op(op @ ("*" | "/" | "//" | "%"))) = self.peek().cloned() {
            self.pos += 1;
            self.nontrivial = true;
            let r = self.unary()?;
            v = match op {
                "*" => v * r,
                "/" => {
                    if r == 0.0 {
                        return None;
                    }
                    v / r
                }
                "//" => (v / r).floor(),
                _ => v.rem_euclid(r),
            };
        }
        Some(v)
    }
    fn unary(&mut self) -> Option<f64> {
        match self.peek() {
            Some(Tok::Op("-")) => {
                self.pos += 1;
                Some(-self.unary()?)
            }
            Some(Tok::Op("+")) => {
                self.pos += 1;
                self.unary()
            }
            _ => self.power(),
        }
    }
    fn power(&mut self) -> Option<f64> {
        let base = self.postfix()?;
        if let Some(Tok::Op("^")) = self.peek() {
            self.pos += 1;
            self.nontrivial = true;
            let exp = self.unary()?;
            if exp.abs() > 2000.0 {
                return None;
            }
            return Some(base.powf(exp));
        }
        Some(base)
    }
    fn postfix(&mut self) -> Option<f64> {
        let mut v = self.primary()?;
        loop {
            match self.peek() {
                Some(Tok::Op("!")) => {
                    self.pos += 1;
                    self.nontrivial = true;
                    if v < 0.0 || v > 170.0 || v.fract() != 0.0 {
                        return None;
                    }
                    v = (1..=v as u64).fold(1.0, |a, b| a * b as f64);
                }
                Some(Tok::Op("pct")) => {
                    self.pos += 1;
                    self.nontrivial = true;
                    v /= 100.0;
                }
                _ => return Some(v),
            }
        }
    }
    fn primary(&mut self) -> Option<f64> {
        match self.next()? {
            Tok::Num(n) => Some(n),
            Tok::LParen => {
                let v = self.expr()?;
                matches!(self.next()?, Tok::RParen).then_some(v)
            }
            Tok::Ident(id) => {
                if let Some(Tok::LParen) = self.peek() {
                    self.pos += 1;
                    let mut args = vec![];
                    if !matches!(self.peek(), Some(Tok::RParen)) {
                        loop {
                            args.push(self.expr()?);
                            match self.next()? {
                                Tok::Comma => continue,
                                Tok::RParen => break,
                                _ => return None,
                            }
                        }
                    } else {
                        self.pos += 1;
                    }
                    self.nontrivial = true;
                    call(&id, &args)
                } else if let Some(Tok::Num(_)) = self.peek() {
                    // "sqrt 16", "sin 30"
                    let arg = self.postfix()?;
                    self.nontrivial = true;
                    call(&id, &[arg])
                } else {
                    constant(&id)
                }
            }
            _ => None,
        }
    }
}

fn constant(id: &str) -> Option<f64> {
    Some(match id {
        "pi" => consts::PI,
        "e" => consts::E,
        "tau" => consts::TAU,
        "phi" => (1.0 + 5f64.sqrt()) / 2.0,
        _ => return None,
    })
}

fn call(f: &str, a: &[f64]) -> Option<f64> {
    let x = *a.first()?;
    let v = match (f, a.len()) {
        ("sqrt", 1) => x.sqrt(),
        ("cbrt", 1) => x.cbrt(),
        ("abs", 1) => x.abs(),
        ("sin", 1) => x.sin(),
        ("cos", 1) => x.cos(),
        ("tan", 1) => x.tan(),
        ("sind", 1) => x.to_radians().sin(),
        ("cosd", 1) => x.to_radians().cos(),
        ("tand", 1) => x.to_radians().tan(),
        ("asin", 1) => x.asin(),
        ("acos", 1) => x.acos(),
        ("atan", 1) => x.atan(),
        ("sinh", 1) => x.sinh(),
        ("cosh", 1) => x.cosh(),
        ("tanh", 1) => x.tanh(),
        ("ln", 1) => x.ln(),
        ("log", 1) | ("log10", 1) => x.log10(),
        ("log", 2) => x.log(a[1]),
        ("log2", 1) => x.log2(),
        ("exp", 1) => x.exp(),
        ("floor", 1) => x.floor(),
        ("ceil", 1) => x.ceil(),
        ("round", 1) => x.round(),
        ("round", 2) => (x * 10f64.powf(a[1])).round() / 10f64.powf(a[1]),
        ("trunc", 1) => x.trunc(),
        ("radians", 1) => x.to_radians(),
        ("degrees", 1) => x.to_degrees(),
        ("factorial", 1) if (0.0..=170.0).contains(&x) && x.fract() == 0.0 => (1..=x as u64).fold(1.0, |p, n| p * n as f64),
        ("min", n) if n > 0 => a.iter().cloned().fold(f64::INFINITY, f64::min),
        ("max", n) if n > 0 => a.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
        ("hypot", 2) => x.hypot(a[1]),
        ("atan2", 2) => x.atan2(a[1]),
        ("gcd", 2) => gcd(x as i64, a[1] as i64) as f64,
        ("lcm", 2) => {
            let g = gcd(x as i64, a[1] as i64);
            if g == 0 { 0.0 } else { (x as i64 / g * a[1] as i64).abs() as f64 }
        }
        _ => return None,
    };
    Some(v)
}

fn gcd(a: i64, b: i64) -> i64 {
    if b == 0 { a.abs() } else { gcd(b, a % b) }
}

/// Evaluate an arithmetic expression; None for anything that isn't a real calculation
/// (bare numbers, words, syntax errors, division by zero).
pub fn calculate(q: &str) -> Option<f64> {
    let q = q.trim();
    if q.is_empty() || !q.chars().any(|c| c.is_ascii_digit() || c == 'π') {
        return None;
    }
    let toks = tokenize(q)?;
    let mut p = Parser { toks, pos: 0, nontrivial: false };
    let v = p.expr()?;
    if p.pos != p.toks.len() || !p.nontrivial || !v.is_finite() {
        return None;
    }
    Some(v)
}

pub fn format_number(v: f64) -> String {
    if !v.is_finite() {
        return v.to_string();
    }
    if v.fract() == 0.0 && v.abs() < 1e15 {
        return group_thousands(&format!("{}", v as i64));
    }
    if v.abs() >= 1e15 || v.abs() < 1e-6 {
        return format!("{:e}", v);
    }
    let s = format!("{:.10}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    let (int, frac) = s.split_once('.').unwrap_or((s, ""));
    if frac.is_empty() { group_thousands(int) } else { format!("{}.{}", group_thousands(int), frac) }
}

fn group_thousands(int: &str) -> String {
    let (sign, digits) = if let Some(d) = int.strip_prefix('-') { ("-", d) } else { ("", int) };
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    format!("{sign}{out}")
}

pub fn plain(v: f64) -> String {
    format_number(v).replace(',', "")
}

// ------------------------------------------------------------------ units

const UNITS: &[(&str, f64, &str)] = &[
    ("length", 1e-3, "mm millimeter millimeters millimetre millimetres"),
    ("length", 1e-2, "cm centimeter centimeters centimetre centimetres"),
    ("length", 1.0, "m meter meters metre metres"),
    ("length", 1e3, "km kilometer kilometers kilometre kilometres kms"),
    ("length", 0.0254, "in inch inches"),
    ("length", 0.3048, "ft foot feet"),
    ("length", 0.9144, "yd yard yards"),
    ("length", 1609.344, "mi mile miles"),
    ("length", 1852.0, "nmi nauticalmile nauticalmiles"),
    ("length", 9.4607e15, "ly lightyear lightyears"),
    ("mass", 1e-6, "mg milligram milligrams"),
    ("mass", 1e-3, "g gram grams gm"),
    ("mass", 1.0, "kg kilogram kilograms kgs kilo kilos"),
    ("mass", 1000.0, "t tonne tonnes ton tons"),
    ("mass", 0.45359237, "lb lbs pound pounds"),
    ("mass", 0.028349523125, "oz ounce ounces"),
    ("mass", 6.35029318, "st stone stones"),
    ("volume", 1e-3, "ml milliliter milliliters millilitre millilitres"),
    ("volume", 1.0, "l liter liters litre litres ltr"),
    ("volume", 1000.0, "m3 cubicmeter cubicmeters"),
    ("volume", 3.785411784, "gal gallon gallons"),
    ("volume", 0.946352946, "qt quart quarts"),
    ("volume", 0.473176473, "pt pint pints"),
    ("volume", 0.2365882365, "cup cups"),
    ("volume", 0.0295735295625, "floz"),
    ("volume", 0.01478676478125, "tbsp tablespoon tablespoons"),
    ("volume", 0.00492892159375, "tsp teaspoon teaspoons"),
    ("time", 1e-3, "ms millisecond milliseconds"),
    ("time", 1.0, "s sec secs second seconds"),
    ("time", 60.0, "min mins minute minutes"),
    ("time", 3600.0, "h hr hrs hour hours"),
    ("time", 86400.0, "d day days"),
    ("time", 604800.0, "wk week weeks"),
    ("time", 2629746.0, "month months"),
    ("time", 31556952.0, "y yr yrs year years"),
    ("data", 0.125, "bit bits"),
    ("data", 1.0, "b byte bytes"),
    ("data", 1e3, "kb kilobyte kilobytes"),
    ("data", 1e6, "mb megabyte megabytes"),
    ("data", 1e9, "gb gigabyte gigabytes"),
    ("data", 1e12, "tb terabyte terabytes"),
    ("data", 1024.0, "kib"),
    ("data", 1048576.0, "mib"),
    ("data", 1073741824.0, "gib"),
    ("data", 1099511627776.0, "tib"),
    ("speed", 1.0, "m/s mps"),
    ("speed", 1.0 / 3.6, "km/h kmh kph kmph"),
    ("speed", 0.44704, "mph"),
    ("speed", 0.514444, "kn knot knots"),
    ("speed", 0.3048, "ft/s fps"),
    ("area", 1e-4, "cm2 sqcm"),
    ("area", 1.0, "m2 sqm"),
    ("area", 1e6, "km2 sqkm"),
    ("area", 0.09290304, "ft2 sqft"),
    ("area", 0.83612736, "yd2 sqyd"),
    ("area", 4046.8564224, "acre acres ac"),
    ("area", 1e4, "ha hectare hectares"),
    ("area", 2589988.110336, "mi2 sqmi"),
    ("angle", consts::PI / 180.0, "deg degree degrees °"),
    ("angle", 1.0, "rad radian radians"),
    ("energy", 1.0, "j joule joules"),
    ("energy", 1e3, "kj"),
    ("energy", 4.184, "cal calorie calories"),
    ("energy", 4184.0, "kcal"),
    ("energy", 3.6e6, "kwh"),
    ("energy", 3600.0, "wh"),
];

const PARTNER: &[(&str, &str)] = &[
    ("km", "mi"), ("mi", "km"), ("m", "ft"), ("ft", "m"), ("cm", "in"), ("in", "cm"),
    ("kg", "lb"), ("lb", "kg"), ("lbs", "kg"), ("g", "oz"), ("oz", "g"), ("l", "gal"),
    ("gal", "l"), ("ml", "floz"), ("c", "f"), ("f", "c"), ("k", "c"), ("km/h", "mph"),
    ("mph", "km/h"), ("gb", "mb"), ("mb", "kb"), ("m2", "ft2"), ("ft2", "m2"),
    ("acre", "m2"), ("ha", "acre"), ("h", "min"), ("min", "s"), ("kcal", "kj"),
];

fn norm_unit(u: &str) -> String {
    let u = u.trim().to_lowercase().replace('²', "2").replace('³', "3");
    let u = u
        .replace("square ", "sq")
        .replace("cubic ", "cubic")
        .replace("fluid ounce", "floz")
        .replace("fl oz", "floz")
        .replace("light year", "lightyear")
        .replace("nautical mile", "nauticalmile")
        .replace("degrees ", "deg")
        .replace("degree ", "deg");
    u.replace(' ', "")
}

fn temp(u: &str) -> Option<char> {
    match u {
        "c" | "°c" | "celsius" | "degc" => Some('c'),
        "f" | "°f" | "fahrenheit" | "degf" => Some('f'),
        "k" | "kelvin" => Some('k'),
        _ => None,
    }
}

fn lookup(u: &str) -> Option<(&'static str, f64)> {
    let find = |u: &str| UNITS.iter().find(|(_, _, names)| names.split(' ').any(|n| n == u)).map(|(g, f, _)| (*g, *f));
    find(u).or_else(|| u.strip_suffix('s').and_then(find))
}

pub struct Conversion {
    pub value: f64,
    pub label: String,
    pub from: String,
    pub implicit: bool,
}

/// "5 km to miles", "70 f", "100 kg in lbs"
pub fn convert(q: &str) -> Option<Conversion> {
    let q = q.trim();
    let num_end = q
        .char_indices()
        .find(|(i, c)| !(c.is_ascii_digit() || *c == '.' || *c == ',' || (*i == 0 && *c == '-')))
        .map(|(i, _)| i)?;
    if num_end == 0 {
        return None;
    }
    let num: f64 = q[..num_end].replace(',', "").parse().ok()?;
    let rest = q[num_end..].trim();
    if rest.is_empty() || rest.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    let lower = rest.to_lowercase();
    let mut split = None;
    for sep in [" to ", " in ", " into ", " as ", " = ", " -> "] {
        if let Some(i) = lower.find(sep) {
            split = Some((&rest[..i], &rest[i + sep.len()..]));
            break;
        }
    }
    let (ua, ub, implicit) = match split {
        Some((a, b)) => (a.to_string(), b.trim().to_string(), false),
        None => {
            let a = norm_unit(rest);
            let key = temp(&a).map(|c| c.to_string()).unwrap_or(a);
            let b = PARTNER.iter().find(|(k, _)| *k == key)?.1;
            (rest.to_string(), b.to_string(), true)
        }
    };
    let (a, b) = (norm_unit(&ua), norm_unit(&ub));
    if let (Some(ta), Some(tb)) = (temp(&a), temp(&b)) {
        let k = match ta {
            'c' => num + 273.15,
            'f' => (num - 32.0) * 5.0 / 9.0 + 273.15,
            _ => num,
        };
        let v = match tb {
            'c' => k - 273.15,
            'f' => (k - 273.15) * 9.0 / 5.0 + 32.0,
            _ => k,
        };
        let label = match tb {
            'c' => "°C",
            'f' => "°F",
            _ => "K",
        };
        return Some(Conversion { value: v, label: label.into(), from: format!("{} {}", format_number(num), ua.trim()), implicit });
    }
    let (ga, fa) = lookup(&a)?;
    let (gb, fb) = lookup(&b)?;
    if ga != gb {
        return None;
    }
    Some(Conversion { value: num * fa / fb, label: ub, from: format!("{} {}", format_number(num), ua.trim()), implicit })
}

pub fn round6(v: f64) -> f64 {
    (v * 1e6).round() / 1e6
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn calc() {
        assert_eq!(calculate("2+2*3"), Some(8.0));
        assert_eq!(calculate("sqrt(144)+2^10"), Some(1036.0));
        assert_eq!(calculate("20% of 150"), Some(30.0));
        assert_eq!(calculate("5!"), Some(120.0));
        assert_eq!(calculate("10 % 3"), Some(1.0));
        assert_eq!(calculate("2pi").map(|v| (v * 1000.0).round()), Some(6283.0));
        assert_eq!(calculate("1,000 * 3"), Some(3000.0));
        assert_eq!(calculate("-2^2"), Some(-4.0));
        assert_eq!(calculate("2024"), None);
        assert_eq!(calculate("100/0"), None);
        assert_eq!(calculate("firefox"), None);
        assert_eq!(format_number(1234567.0), "1,234,567");
        assert_eq!(format_number(3.14159265358979), "3.1415926536");
    }
    #[test]
    fn units() {
        assert!((convert("5 km to miles").unwrap().value - 3.10686).abs() < 1e-4);
        assert!((convert("70 f").unwrap().value - 21.1111).abs() < 1e-3);
        assert!((convert("100 kg in lbs").unwrap().value - 220.462).abs() < 1e-2);
        assert!(convert("100 usd to inr").is_none());
    }
}
