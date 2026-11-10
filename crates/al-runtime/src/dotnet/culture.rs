//! System.Globalization: CultureInfo (and its NumberFormat / DateTimeFormat / TextInfo views),
//! DateTimeStyles, NumberStyles. The culture table covers the languages BC ships; month and day
//! names are English for every culture.

use super::*;

pub struct Culture {
    pub lcid: i32,
    pub name: &'static str,
    pub parent: &'static str,
    pub english: &'static str,
    pub three: &'static str,
    pub decimal: &'static str,
    pub group: &'static str,
    pub currency: &'static str,
    pub short_date: &'static str,
    pub long_date: &'static str,
    pub short_time: &'static str,
    pub long_time: &'static str,
    pub month_day: &'static str,
    pub year_month: &'static str,
    pub date_sep: &'static str,
    pub time_sep: &'static str,
    pub am: &'static str,
    pub pm: &'static str,
    pub month_names: [&'static str; 12],
    pub day_names: [&'static str; 7],
}

const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
const DAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

impl Culture {
    /// "mdy", "dmy" or "ymd" from the short date pattern.
    pub fn date_order(&self) -> &'static str {
        let p = self.short_date;
        let (y, m, d) = (p.find('y').unwrap_or(9), p.find('M').unwrap_or(9), p.find('d').unwrap_or(9));
        if y < m && m < d {
            "ymd"
        } else if m < d {
            "mdy"
        } else {
            "dmy"
        }
    }
    pub fn two_letter(&self) -> &'static str {
        if self.name.is_empty() {
            "iv"
        } else {
            self.name.split('-').next().unwrap_or(self.name)
        }
    }
}

macro_rules! c {
    ($lcid:expr, $name:expr, $parent:expr, $eng:expr, $three:expr, $dec:expr, $grp:expr, $cur:expr, $sd:expr, $ld:expr, $st:expr, $lt:expr, $ds:expr, $am:expr, $pm:expr) => {
        Culture {
            lcid: $lcid,
            name: $name,
            parent: $parent,
            english: $eng,
            three: $three,
            decimal: $dec,
            group: $grp,
            currency: $cur,
            short_date: $sd,
            long_date: $ld,
            short_time: $st,
            long_time: $lt,
            month_day: "MMMM d",
            year_month: "MMMM yyyy",
            date_sep: $ds,
            time_sep: ":",
            am: $am,
            pm: $pm,
            month_names: MONTHS,
            day_names: DAYS,
        }
    };
}

pub static INVARIANT: Culture = Culture {
    lcid: 127,
    name: "",
    parent: "",
    english: "Invariant Language (Invariant Country)",
    three: "IVL",
    decimal: ".",
    group: ",",
    currency: "¤",
    short_date: "MM/dd/yyyy",
    long_date: "dddd, dd MMMM yyyy",
    short_time: "HH:mm",
    long_time: "HH:mm:ss",
    month_day: "MMMM dd",
    year_month: "yyyy MMMM",
    date_sep: "/",
    time_sep: ":",
    am: "AM",
    pm: "PM",
    month_names: MONTHS,
    day_names: DAYS,
};

pub static CULTURES: &[Culture] = &[
    c!(1033, "en-US", "en", "English (United States)", "ENU", ".", ",", "$", "M/d/yyyy", "dddd, MMMM d, yyyy", "h:mm tt", "h:mm:ss tt", "/", "AM", "PM"),
    c!(2057, "en-GB", "en", "English (United Kingdom)", "ENG", ".", ",", "£", "dd/MM/yyyy", "dd MMMM yyyy", "HH:mm", "HH:mm:ss", "/", "am", "pm"),
    c!(3081, "en-AU", "en", "English (Australia)", "ENA", ".", ",", "$", "d/MM/yyyy", "dddd, d MMMM yyyy", "h:mm tt", "h:mm:ss tt", "/", "am", "pm"),
    c!(4105, "en-CA", "en", "English (Canada)", "ENC", ".", ",", "$", "yyyy-MM-dd", "MMMM d, yyyy", "h:mm tt", "h:mm:ss tt", "-", "a.m.", "p.m."),
    c!(5129, "en-NZ", "en", "English (New Zealand)", "ENZ", ".", ",", "$", "d/MM/yyyy", "dddd, d MMMM yyyy", "h:mm tt", "h:mm:ss tt", "/", "am", "pm"),
    c!(16393, "en-IN", "en", "English (India)", "ENN", ".", ",", "₹", "dd-MM-yyyy", "dd MMMM yyyy", "HH:mm", "HH:mm:ss", "-", "AM", "PM"),
    c!(7177, "en-ZA", "en", "English (South Africa)", "ENS", ",", " ", "R", "yyyy/MM/dd", "dd MMMM yyyy", "HH:mm", "HH:mm:ss", "/", "AM", "PM"),
    c!(6153, "en-IE", "en", "English (Ireland)", "ENI", ".", ",", "€", "dd/MM/yyyy", "dddd d MMMM yyyy", "HH:mm", "HH:mm:ss", "/", "a.m.", "p.m."),
    c!(1030, "da-DK", "da", "Danish (Denmark)", "DAN", ",", ".", "kr.", "dd-MM-yyyy", "dddd 'den' d. MMMM yyyy", "HH:mm", "HH:mm:ss", "-", "", ""),
    c!(1031, "de-DE", "de", "German (Germany)", "DEU", ",", ".", "€", "dd.MM.yyyy", "dddd, d. MMMM yyyy", "HH:mm", "HH:mm:ss", ".", "", ""),
    c!(3079, "de-AT", "de", "German (Austria)", "DEA", ",", " ", "€", "dd.MM.yyyy", "dddd, d. MMMM yyyy", "HH:mm", "HH:mm:ss", ".", "", ""),
    c!(2055, "de-CH", "de", "German (Switzerland)", "DES", ".", "’", "CHF", "dd.MM.yyyy", "dddd, d. MMMM yyyy", "HH:mm", "HH:mm:ss", ".", "", ""),
    c!(1036, "fr-FR", "fr", "French (France)", "FRA", ",", " ", "€", "dd/MM/yyyy", "dddd d MMMM yyyy", "HH:mm", "HH:mm:ss", "/", "", ""),
    c!(2060, "fr-BE", "fr", "French (Belgium)", "FRB", ",", " ", "€", "dd-MM-yy", "dddd d MMMM yyyy", "HH:mm", "HH:mm:ss", "-", "", ""),
    c!(4108, "fr-CH", "fr", "French (Switzerland)", "FRS", ".", " ", "CHF", "dd.MM.yyyy", "dddd, d MMMM yyyy", "HH:mm", "HH:mm:ss", ".", "", ""),
    c!(3084, "fr-CA", "fr", "French (Canada)", "FRC", ",", " ", "$", "yyyy-MM-dd", "d MMMM yyyy", "HH 'h' mm", "HH 'h' mm 'min' ss 's'", "-", "a.m.", "p.m."),
    c!(1043, "nl-NL", "nl", "Dutch (Netherlands)", "NLD", ",", ".", "€", "d-M-yyyy", "dddd d MMMM yyyy", "HH:mm", "HH:mm:ss", "-", "", ""),
    c!(2067, "nl-BE", "nl", "Dutch (Belgium)", "NLB", ",", ".", "€", "d/MM/yyyy", "dddd d MMMM yyyy", "H:mm", "H:mm:ss", "/", "", ""),
    c!(1040, "it-IT", "it", "Italian (Italy)", "ITA", ",", ".", "€", "dd/MM/yyyy", "dddd d MMMM yyyy", "HH:mm", "HH:mm:ss", "/", "", ""),
    c!(2064, "it-CH", "it", "Italian (Switzerland)", "ITS", ".", "’", "CHF", "dd.MM.yyyy", "dddd, d MMMM yyyy", "HH:mm", "HH:mm:ss", ".", "", ""),
    c!(3082, "es-ES", "es", "Spanish (Spain)", "ESN", ",", ".", "€", "dd/MM/yyyy", "dddd, d 'de' MMMM 'de' yyyy", "H:mm", "H:mm:ss", "/", "a. m.", "p. m."),
    c!(1034, "es-ES_tradnl", "es", "Spanish (Spain, Traditional Sort)", "ESP", ",", ".", "€", "dd/MM/yyyy", "dddd, d 'de' MMMM 'de' yyyy", "H:mm", "H:mm:ss", "/", "a. m.", "p. m."),
    c!(2058, "es-MX", "es", "Spanish (Mexico)", "ESM", ".", ",", "$", "dd/MM/yyyy", "dddd, d 'de' MMMM 'de' yyyy", "hh:mm tt", "hh:mm:ss tt", "/", "a. m.", "p. m."),
    c!(1053, "sv-SE", "sv", "Swedish (Sweden)", "SVE", ",", " ", "kr", "yyyy-MM-dd", "'den' d MMMM yyyy", "HH:mm", "HH:mm:ss", "-", "", ""),
    c!(1044, "nb-NO", "nb", "Norwegian Bokmål (Norway)", "NOR", ",", " ", "kr", "dd.MM.yyyy", "dddd d. MMMM yyyy", "HH:mm", "HH:mm:ss", ".", "a.m.", "p.m."),
    c!(1035, "fi-FI", "fi", "Finnish (Finland)", "FIN", ",", " ", "€", "d.M.yyyy", "dddd d. MMMM yyyy", "H.mm", "H.mm.ss", ".", "ap.", "ip."),
    c!(1039, "is-IS", "is", "Icelandic (Iceland)", "ISL", ",", ".", "ISK", "d.M.yyyy", "dddd, d. MMMM yyyy", "HH:mm", "HH:mm:ss", ".", "f.h.", "e.h."),
    c!(1029, "cs-CZ", "cs", "Czech (Czechia)", "CSY", ",", " ", "Kč", "dd.MM.yyyy", "dddd d. MMMM yyyy", "H:mm", "H:mm:ss", ".", "dop.", "odp."),
    c!(1045, "pl-PL", "pl", "Polish (Poland)", "PLK", ",", " ", "zł", "dd.MM.yyyy", "dddd, d MMMM yyyy", "HH:mm", "HH:mm:ss", ".", "", ""),
    c!(1049, "ru-RU", "ru", "Russian (Russia)", "RUS", ",", " ", "₽", "dd.MM.yyyy", "dddd, d MMMM yyyy 'г.'", "H:mm", "H:mm:ss", ".", "", ""),
    c!(2070, "pt-PT", "pt", "Portuguese (Portugal)", "PTG", ",", " ", "€", "dd/MM/yyyy", "dddd, d 'de' MMMM 'de' yyyy", "HH:mm", "HH:mm:ss", "/", "da manhã", "da tarde"),
    c!(1046, "pt-BR", "pt", "Portuguese (Brazil)", "PTB", ",", ".", "R$", "dd/MM/yyyy", "dddd, d 'de' MMMM 'de' yyyy", "HH:mm", "HH:mm:ss", "/", "", ""),
    c!(1063, "lt-LT", "lt", "Lithuanian (Lithuania)", "LTH", ",", " ", "€", "yyyy-MM-dd", "yyyy 'm'. MMMM d 'd'., dddd", "HH:mm", "HH:mm:ss", "-", "priešpiet", "popiet"),
    c!(1062, "lv-LV", "lv", "Latvian (Latvia)", "LVI", ",", " ", "€", "dd.MM.yyyy", "dddd, yyyy. 'gada' d. MMMM", "HH:mm", "HH:mm:ss", ".", "priekšpusdienā", "pēcpusdienā"),
    c!(1061, "et-EE", "et", "Estonian (Estonia)", "ETI", ",", " ", "€", "dd.MM.yyyy", "dddd, d. MMMM yyyy", "HH:mm", "HH:mm:ss", ".", "AM", "PM"),
    c!(1038, "hu-HU", "hu", "Hungarian (Hungary)", "HUN", ",", " ", "Ft", "yyyy. MM. dd.", "yyyy. MMMM d., dddd", "H:mm", "H:mm:ss", ". ", "de.", "du."),
    c!(1051, "sk-SK", "sk", "Slovak (Slovakia)", "SKY", ",", " ", "€", "d. M. yyyy", "dddd d. MMMM yyyy", "H:mm", "H:mm:ss", ". ", "AM", "PM"),
    c!(1060, "sl-SI", "sl", "Slovenian (Slovenia)", "SLV", ",", ".", "€", "d. MM. yyyy", "dddd, d. MMMM yyyy", "HH:mm", "HH:mm:ss", ". ", "dop.", "pop."),
    c!(1050, "hr-HR", "hr", "Croatian (Croatia)", "HRV", ",", ".", "€", "dd.MM.yyyy.", "dddd, d. MMMM yyyy.", "HH:mm", "HH:mm:ss", ".", "AM", "PM"),
    c!(9242, "sr-Latn-RS", "sr-Latn", "Serbian (Latin, Serbia)", "SRM", ",", ".", "RSD", "d.M.yyyy.", "dddd, dd. MMMM yyyy.", "HH:mm", "HH:mm:ss", ".", "AM", "PM"),
    c!(1026, "bg-BG", "bg", "Bulgarian (Bulgaria)", "BGR", ",", " ", "лв.", "d.M.yyyy 'г.'", "dddd, d MMMM yyyy 'г.'", "H:mm 'ч.'", "H:mm:ss 'ч.'", ".", "пр.об.", "сл.об."),
    c!(1048, "ro-RO", "ro", "Romanian (Romania)", "ROM", ",", ".", "RON", "dd.MM.yyyy", "dddd, d MMMM yyyy", "HH:mm", "HH:mm:ss", ".", "a.m.", "p.m."),
    c!(1032, "el-GR", "el", "Greek (Greece)", "ELL", ",", ".", "€", "d/M/yyyy", "dddd d MMMM yyyy", "h:mm tt", "h:mm:ss tt", "/", "π.μ.", "μ.μ."),
    c!(1055, "tr-TR", "tr", "Turkish (Türkiye)", "TRK", ",", ".", "₺", "d.MM.yyyy", "d MMMM yyyy dddd", "HH:mm", "HH:mm:ss", ".", "ÖÖ", "ÖS"),
    c!(1041, "ja-JP", "ja", "Japanese (Japan)", "JPN", ".", ",", "¥", "yyyy/MM/dd", "yyyy'年'M'月'd'日'", "H:mm", "H:mm:ss", "/", "午前", "午後"),
    c!(2052, "zh-CN", "zh-Hans", "Chinese (Simplified, PRC)", "CHS", ".", ",", "¥", "yyyy/M/d", "yyyy'年'M'月'd'日'", "H:mm", "H:mm:ss", "/", "上午", "下午"),
    c!(1028, "zh-TW", "zh-Hant", "Chinese (Traditional, Taiwan)", "CHT", ".", ",", "$", "yyyy/M/d", "yyyy'年'M'月'd'日'", "tt hh:mm", "tt hh:mm:ss", "/", "上午", "下午"),
    c!(1042, "ko-KR", "ko", "Korean (Korea)", "KOR", ".", ",", "₩", "yyyy-MM-dd", "yyyy'년' M'월' d'일' dddd", "tt h:mm", "tt h:mm:ss", "-", "오전", "오후"),
    c!(1054, "th-TH", "th", "Thai (Thailand)", "THA", ".", ",", "฿", "d/M/yyyy", "dddd'ที่' d MMMM g yyyy", "H:mm", "H:mm:ss", "/", "AM", "PM"),
    c!(1058, "uk-UA", "uk", "Ukrainian (Ukraine)", "UKR", ",", " ", "₴", "dd.MM.yyyy", "dddd, d MMMM yyyy 'р.'", "HH:mm", "HH:mm:ss", ".", "дп", "пп"),
    c!(1025, "ar-SA", "ar", "Arabic (Saudi Arabia)", "ARA", ".", ",", "ر.س.\u{200f}", "dd/MM/yy", "dd/MMMM/yyyy", "hh:mm tt", "hh:mm:ss tt", "/", "ص", "م"),
    c!(1037, "he-IL", "he", "Hebrew (Israel)", "HEB", ".", ",", "₪", "dd/MM/yyyy", "dddd dd MMMM yyyy", "HH:mm", "HH:mm:ss", "/", "AM", "PM"),
    c!(5132, "fr-LU", "fr", "French (Luxembourg)", "FRL", ",", ".", "€", "dd/MM/yyyy", "dddd d MMMM yyyy", "HH:mm", "HH:mm:ss", "/", "", ""),
    c!(4103, "de-LU", "de", "German (Luxembourg)", "DEL", ",", ".", "€", "dd.MM.yyyy", "dddd, d. MMMM yyyy", "HH:mm", "HH:mm:ss", ".", "", ""),
    c!(1086, "ms-MY", "ms", "Malay (Malaysia)", "MSL", ".", ",", "RM", "d/MM/yyyy", "dddd, d MMMM yyyy", "h:mm tt", "h:mm:ss tt", "/", "PG", "PTG"),
    c!(1057, "id-ID", "id", "Indonesian (Indonesia)", "IND", ",", ".", "Rp", "dd/MM/yyyy", "dddd, dd MMMM yyyy", "HH.mm", "HH.mm.ss", "/", "AM", "PM"),
    c!(1066, "vi-VN", "vi", "Vietnamese (Vietnam)", "VIT", ",", ".", "₫", "dd/MM/yyyy", "dddd, d MMMM, yyyy", "HH:mm", "HH:mm:ss", "/", "SA", "CH"),
    c!(1027, "ca-ES", "ca", "Catalan (Catalan)", "CAT", ",", ".", "€", "d/M/yyyy", "dddd, d MMMM 'de' yyyy", "H:mm", "H:mm:ss", "/", "a. m.", "p. m."),
    c!(1069, "eu-ES", "eu", "Basque (Basque)", "EUQ", ",", ".", "€", "yyyy/M/d", "yyyy('e')'ko' MMMM'ren' d('a'), dddd", "HH:mm", "HH:mm:ss", "/", "AM", "PM"),
    c!(1110, "gl-ES", "gl", "Galician (Galician)", "GLC", ",", ".", "€", "dd/MM/yyyy", "dddd, d 'de' MMMM 'de' yyyy", "HH:mm", "HH:mm:ss", "/", "a.m.", "p.m."),
];

/// Neutral cultures: (lcid, name, english).
static NEUTRAL: &[(i32, &str, &str, &str)] = &[
    (9, "en", "English", "ENU"),
    (6, "da", "Danish", "DAN"),
    (7, "de", "German", "DEU"),
    (12, "fr", "French", "FRA"),
    (19, "nl", "Dutch", "NLD"),
    (16, "it", "Italian", "ITA"),
    (10, "es", "Spanish", "ESN"),
    (29, "sv", "Swedish", "SVE"),
    (31764, "nb", "Norwegian Bokmål", "NOR"),
    (11, "fi", "Finnish", "FIN"),
    (15, "is", "Icelandic", "ISL"),
    (5, "cs", "Czech", "CSY"),
    (21, "pl", "Polish", "PLK"),
    (25, "ru", "Russian", "RUS"),
    (22, "pt", "Portuguese", "PTB"),
    (39, "lt", "Lithuanian", "LTH"),
    (38, "lv", "Latvian", "LVI"),
    (37, "et", "Estonian", "ETI"),
    (14, "hu", "Hungarian", "HUN"),
    (27, "sk", "Slovak", "SKY"),
    (36, "sl", "Slovenian", "SLV"),
    (26, "hr", "Croatian", "HRV"),
    (2, "bg", "Bulgarian", "BGR"),
    (24, "ro", "Romanian", "ROM"),
    (8, "el", "Greek", "ELL"),
    (31, "tr", "Turkish", "TRK"),
    (17, "ja", "Japanese", "JPN"),
    (4, "zh-Hans", "Chinese (Simplified)", "CHS"),
    (31748, "zh-Hant", "Chinese (Traditional)", "CHT"),
    (18, "ko", "Korean", "KOR"),
    (30, "th", "Thai", "THA"),
    (34, "uk", "Ukrainian", "UKR"),
    (1, "ar", "Arabic", "ARA"),
    (13, "he", "Hebrew", "HEB"),
    (62, "ms", "Malay", "MSL"),
    (33, "id", "Indonesian", "IND"),
    (42, "vi", "Vietnamese", "VIT"),
    (3, "ca", "Catalan", "CAT"),
    (45, "eu", "Basque", "EUQ"),
    (86, "gl", "Galician", "GLC"),
    (31770, "sr-Latn", "Serbian (Latin)", "SRM"),
];

pub fn current() -> &'static Culture {
    &CULTURES[0]
}

pub fn invariant() -> &'static Culture {
    &INVARIANT
}

/// A culture by LCID or name; neutral cultures borrow the formats of their main specific culture.
pub fn find(key: &Val) -> Option<&'static Culture> {
    match key {
        Val::Int(_) | Val::BigInt(_) | Val::Dec(_) => {
            let id = key.as_i64() as i32;
            if id == 127 || id == 0 {
                return Some(&INVARIANT);
            }
            CULTURES.iter().find(|c| c.lcid == id).or_else(|| NEUTRAL.iter().find(|n| n.0 == id).and_then(|n| neutral(n.1)))
        }
        _ => find_name(&key.to_text()),
    }
}

pub fn find_name(name: &str) -> Option<&'static Culture> {
    let n = name.trim();
    if n.is_empty() {
        return Some(&INVARIANT);
    }
    CULTURES.iter().find(|c| c.name.eq_ignore_ascii_case(n)).or_else(|| neutral(n))
}

/// Neutral cultures as first-class objects (name/LCID differ from the specific culture).
fn neutral(name: &str) -> Option<&'static Culture> {
    thread_local! {
        static CACHE: std::cell::RefCell<Vec<&'static Culture>> = const { std::cell::RefCell::new(vec![]) };
    }
    let (lcid, nname, eng, three) = *NEUTRAL.iter().find(|x| x.1.eq_ignore_ascii_case(name))?;
    if let Some(c) = CACHE.with(|c| c.borrow().iter().find(|x| x.name == nname).copied()) {
        return Some(c);
    }
    let spec = CULTURES.iter().find(|c| c.parent == nname).unwrap_or(&CULTURES[0]);
    let c: &'static Culture = Box::leak(Box::new(Culture { lcid, name: nname, parent: "", english: eng, three, ..clone(spec) }));
    CACHE.with(|x| x.borrow_mut().push(c));
    Some(c)
}

fn clone(c: &Culture) -> Culture {
    Culture { month_names: c.month_names, day_names: c.day_names, ..*c }
}

impl Clone for Culture {
    fn clone(&self) -> Self {
        Culture { ..*self }
    }
}
impl Copy for Culture {}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum View {
    Culture,
    NumberFormat,
    DateTimeFormat,
    TextInfo,
    CompareInfo,
}

impl View {
    pub fn type_name(&self) -> &'static str {
        match self {
            View::Culture => "System.Globalization.CultureInfo",
            View::NumberFormat => "System.Globalization.NumberFormatInfo",
            View::DateTimeFormat => "System.Globalization.DateTimeFormatInfo",
            View::TextInfo => "System.Globalization.TextInfo",
            View::CompareInfo => "System.Globalization.CompareInfo",
        }
    }
}

pub fn culture_val(c: &'static Culture, v: View) -> Val {
    net(NetObj::Culture(c, v))
}

pub fn construct(a: &Args) -> R<Val> {
    match find(&a.get(0)) {
        Some(c) => Ok(culture_val(c, View::Culture)),
        None => net_err("CultureInfo", "CultureInfo", format!("Culture is not supported. Parameter name: culture\n{} is an invalid culture identifier.", a.text(0))),
    }
}

pub fn call(n: &NetRef, ty: &'static str, m: &str, a: &Args) -> R<Out> {
    match ty {
        "System.Globalization.DateTimeStyles" => {
            let v = match m {
                "none" => 0,
                "allowleadingwhite" => 1,
                "allowtrailingwhite" => 2,
                "allowinnerwhite" => 4,
                "allowwhitespaces" => 7,
                "nocurrentdatedefault" => 8,
                "adjusttouniversal" => 16,
                "assumelocal" => 32,
                "assumeuniversal" => 64,
                "roundtripkind" => 128,
                _ => return Ok(Out::Unknown),
            };
            return ok(enum_val(ty, v));
        }
        "System.Globalization.NumberStyles" => {
            let v = match m {
                "none" => 0,
                "allowleadingwhite" => 1,
                "allowtrailingwhite" => 2,
                "allowleadingsign" => 4,
                "integer" => 7,
                "allowtrailingsign" => 8,
                "allowparentheses" => 16,
                "allowdecimalpoint" => 32,
                "allowthousands" => 64,
                "number" => 111,
                "allowexponent" => 128,
                "float" => 167,
                "allowcurrencysymbol" => 256,
                "currency" => 383,
                "allowhexspecifier" => 512,
                "hexnumber" => 515,
                "any" => 511,
                _ => return Ok(Out::Unknown),
            };
            return ok(enum_val(ty, v));
        }
        "System.Globalization.CompareOptions" => {
            let v = match m {
                "none" => 0,
                "ignorecase" => 1,
                "ordinal" => 0x4000_0000,
                "ordinalignorecase" => 0x1000_0000,
                _ => return Ok(Out::Unknown),
            };
            return ok(enum_val(ty, v));
        }
        _ => {}
    }
    // static members
    match m {
        "invariantculture" => return ok(culture_val(&INVARIANT, View::Culture)),
        "currentculture" | "currentuiculture" | "installeduiculture" | "defaultthreadcurrentculture" => return ok(culture_val(current(), View::Culture)),
        "getcultureinfo" | "createspecificculture" | "getcultureinfobyietflanguagetag" => {
            return match find(&a.get(0)) {
                Some(c) => ok(culture_val(c, View::Culture)),
                None => net_err("CultureInfo", "GetCultureInfo", format!("Culture is not supported. {} is an invalid culture identifier.", a.text(0))),
            };
        }
        "invariantinfo" if ty.ends_with("NumberFormatInfo") => return ok(culture_val(&INVARIANT, View::NumberFormat)),
        "invariantinfo" => return ok(culture_val(&INVARIANT, View::DateTimeFormat)),
        "currentinfo" if ty.ends_with("NumberFormatInfo") => return ok(culture_val(current(), View::NumberFormat)),
        "currentinfo" => return ok(culture_val(current(), View::DateTimeFormat)),
        "getcultures" => {
            let v: Vec<Val> = CULTURES.iter().map(|c| culture_val(c, View::Culture)).collect();
            return ok(net(NetObj::Array("System.Globalization.CultureInfo", v)));
        }
        _ => {}
    }
    let (c, view) = match &*n.borrow() {
        NetObj::Culture(c, v) => (*c, *v),
        _ => return Ok(Out::Unknown),
    };
    let t = |s: &str| ok(Val::Text(s.to_string()));
    match (view, m) {
        (View::Culture, "name") | (View::Culture, "ietflanguagetag") | (View::Culture, "tostring") => t(c.name),
        (View::Culture, "lcid") | (View::Culture, "keyboardlayoutid") => ok(Val::Int(c.lcid)),
        (View::Culture, "displayname") | (View::Culture, "englishname") | (View::Culture, "nativename") => t(c.english),
        (View::Culture, "twoletterisolanguagename") => t(c.two_letter()),
        (View::Culture, "threeletterisolanguagename") => t(match c.two_letter() {
            "en" => "eng",
            "de" => "deu",
            "fr" => "fra",
            "da" => "dan",
            "nl" => "nld",
            "it" => "ita",
            "es" => "spa",
            "sv" => "swe",
            "fi" => "fin",
            other => other,
        }),
        (View::Culture, "threeletterwindowslanguagename") => t(c.three),
        (View::Culture, "parent") => ok(match find_name(c.parent) {
            Some(p) => culture_val(p, View::Culture),
            None => culture_val(&INVARIANT, View::Culture),
        }),
        (View::Culture, "isneutralculture") => ok(Val::Bool(!c.name.contains('-') && !c.name.is_empty())),
        (View::Culture, "numberformat") => ok(culture_val(c, View::NumberFormat)),
        (View::Culture, "datetimeformat") => ok(culture_val(c, View::DateTimeFormat)),
        (View::Culture, "textinfo") => ok(culture_val(c, View::TextInfo)),
        (View::Culture, "compareinfo") => ok(culture_val(c, View::CompareInfo)),
        (View::Culture, "getformat") => ok(culture_val(c, if a.text(0).contains("Number") { View::NumberFormat } else { View::DateTimeFormat })),
        (View::NumberFormat, "numberdecimalseparator") | (View::NumberFormat, "currencydecimalseparator") | (View::NumberFormat, "percentdecimalseparator") => t(c.decimal),
        (View::NumberFormat, "numbergroupseparator") | (View::NumberFormat, "currencygroupseparator") | (View::NumberFormat, "percentgroupseparator") => t(c.group),
        (View::NumberFormat, "currencysymbol") => t(c.currency),
        (View::NumberFormat, "negativesign") => t("-"),
        (View::NumberFormat, "positivesign") => t("+"),
        (View::NumberFormat, "numberdecimaldigits") | (View::NumberFormat, "currencydecimaldigits") => ok(Val::Int(2)),
        (View::NumberFormat, "currencypositivepattern") => ok(Val::Int(if c.name.starts_with("en") || c.name.is_empty() { 0 } else { 3 })),
        (View::NumberFormat, "currencynegativepattern") => ok(Val::Int(if c.name == "en-US" { 0 } else { 8 })),
        (View::DateTimeFormat, "shortdatepattern") => t(c.short_date),
        (View::DateTimeFormat, "longdatepattern") => t(c.long_date),
        (View::DateTimeFormat, "shorttimepattern") => t(c.short_time),
        (View::DateTimeFormat, "longtimepattern") => t(c.long_time),
        (View::DateTimeFormat, "fulldatetimepattern") => t(&format!("{} {}", c.long_date, c.long_time)),
        (View::DateTimeFormat, "monthdaypattern") => t(c.month_day),
        (View::DateTimeFormat, "yearmonthpattern") => t(c.year_month),
        (View::DateTimeFormat, "dateseparator") => t(c.date_sep),
        (View::DateTimeFormat, "timeseparator") => t(c.time_sep),
        (View::DateTimeFormat, "amdesignator") => t(c.am),
        (View::DateTimeFormat, "pmdesignator") => t(c.pm),
        (View::DateTimeFormat, "sortabledatetimepattern") => t("yyyy'-'MM'-'dd'T'HH':'mm':'ss"),
        (View::DateTimeFormat, "universalsortabledatetimepattern") => t("yyyy'-'MM'-'dd HH':'mm':'ss'Z'"),
        (View::DateTimeFormat, "rfc1123pattern") => t("ddd, dd MMM yyyy HH':'mm':'ss 'GMT'"),
        (View::DateTimeFormat, "firstdayofweek") => ok(enum_val("System.DayOfWeek", if c.name == "en-US" || c.name.is_empty() { 0 } else { 1 })),
        (View::DateTimeFormat, "getmonthname") => t(c.month_names[(a.int(0).clamp(1, 12) - 1) as usize]),
        (View::DateTimeFormat, "getabbreviatedmonthname") => t(&c.month_names[(a.int(0).clamp(1, 12) - 1) as usize][..3]),
        (View::DateTimeFormat, "getdayname") => t(c.day_names[a.int(0).clamp(0, 6) as usize]),
        (View::DateTimeFormat, "getabbreviateddayname") => t(&c.day_names[a.int(0).clamp(0, 6) as usize][..3]),
        (View::DateTimeFormat, "monthnames") => ok(net(NetObj::Array("System.String", c.month_names.iter().map(|x| Val::Text(x.to_string())).chain([Val::Text(String::new())]).collect()))),
        (View::DateTimeFormat, "daynames") => ok(net(NetObj::Array("System.String", c.day_names.iter().map(|x| Val::Text(x.to_string())).collect()))),
        (View::TextInfo, "totitlecase") => ok(Val::Text(
            a.text(0)
                .split(' ')
                .map(|w| {
                    let mut ch = w.chars();
                    match ch.next() {
                        Some(f) if !w.chars().all(|x| x.is_uppercase()) => f.to_uppercase().collect::<String>() + &ch.as_str().to_lowercase(),
                        Some(_) => w.to_string(),
                        None => String::new(),
                    }
                })
                .collect::<Vec<_>>()
                .join(" "),
        )),
        (View::TextInfo, "oemcodepage") => ok(Val::Int(match c.two_letter() {
            "en" | "nl" | "it" | "fr" | "de" | "es" | "sv" | "fi" | "da" | "nb" | "pt" | "is" => if c.name == "en-US" || c.name.is_empty() { 437 } else { 850 },
            "cs" | "pl" | "hu" | "sk" | "sl" | "hr" | "ro" => 852,
            "ru" | "uk" | "bg" => 866,
            _ => 437,
        })),
        (View::TextInfo, "ansicodepage") => ok(Val::Int(match c.two_letter() {
            "cs" | "pl" | "hu" | "sk" | "sl" | "hr" | "ro" => 1250,
            "ru" | "uk" | "bg" => 1251,
            "el" => 1253,
            "tr" => 1254,
            "he" => 1255,
            "ar" => 1256,
            "lt" | "lv" | "et" => 1257,
            _ => 1252,
        })),
        (View::TextInfo, "toupper") => ok(Val::Text(a.text(0).to_uppercase())),
        (View::TextInfo, "tolower") => ok(Val::Text(a.text(0).to_lowercase())),
        (View::TextInfo, "listseparator") => t(if c.decimal == "," { ";" } else { "," }),
        (View::CompareInfo, "compare") => ok(Val::Int(a.text(0).to_lowercase().cmp(&a.text(1).to_lowercase()) as i32)),
        (View::CompareInfo, "indexof") => ok(Val::Int(a.text(0).find(&a.text(1)).map(|i| a.text(0)[..i].chars().count() as i32).unwrap_or(-1))),
        _ => Ok(Out::Unknown),
    }
}
