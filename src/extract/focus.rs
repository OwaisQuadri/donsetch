//! BM25 block filter for focus=. Language-aware tokenization
//! with CJK bigram support, multi-language stopword lists, light
//! stemming, and accent folding. Hand-rolled: k1=1.2 b=0.75.
//! No hits → full content (never punish the agent for a bad
//! query).

use std::collections::{HashMap, HashSet};

use super::blocks::Block;
use super::language::{self, LanguageInfo};

const K1: f64 = 1.2;
const B: f64 = 0.75;

/// Max blocks for semantic scoring. Pages with more blocks
/// fall back to BM25-only — large pages are usually reference
/// docs where keyword matching works well, and the latency
/// of cross-encoder on 100+ blocks isn't worth it.
const SEMANTIC_MAX_BLOCKS: usize = 80;

/// Cross-encoder relevance threshold (sigmoid output [0,1]).
/// Blocks scoring above this are kept even if BM25 missed them.
/// 0.3 catches semantically relevant blocks while filtering
/// out navigation, boilerplate, and unrelated sections.
const XENC_THRESHOLD: f64 = 0.3;

// ── Stopwords ────────────────────────────────────────────────

const STOP_EN: &[&str] = &[
    "the", "a", "an", "and", "or", "of", "to", "in", "is", "are", "was", "were", "for", "on",
    "with", "as", "at", "by", "from", "it", "its", "this", "that", "be", "been", "has", "have",
    "had", "not", "but", "they", "their", "we", "you", "he", "she", "his", "her", "what", "which",
    "who", "how", "when", "do", "does", "did", "can", "could", "will", "would", "about", "than",
    "then", "so", "if", "no", "yes", "more", "most", "some", "any", "all", "each", "other", "such",
];

const STOP_ZH: &[&str] = &[
    "的",
    "了",
    "在",
    "是",
    "有",
    "和",
    "就",
    "不",
    "人",
    "都",
    "一",
    "也",
    "很",
    "到",
    "说",
    "要",
    "去",
    "会",
    "着",
    "没",
    "看",
    "好",
    "自己",
    "这",
    "那",
    "与",
    "及",
    "或",
    "但",
    "而",
    "因",
    "为",
    "把",
    "被",
    "让",
    "从",
    "向",
    "对",
    "跟",
    "给",
    "以",
    "之",
    "于",
    "所",
    "可",
    "能",
    "这个",
    "那个",
    "什么",
    "怎么",
    "为什么",
    "怎么",
    "些",
    "里",
    "上",
    "下",
    "中",
];

const STOP_JA: &[&str] = &[
    "は", "が", "を", "に", "で", "と", "から", "まで", "より", "へ", "の", "て", "た", "だ", "し",
    "も", "か", "な", "ん", "する", "いる", "ある", "これ", "それ", "あれ", "この", "その", "あの",
    "です", "ます", "こと", "もの", "たち", "たち", "さん", "よう", "たち",
];

const STOP_KO: &[&str] = &[
    "은",
    "는",
    "이",
    "가",
    "을",
    "를",
    "에",
    "에서",
    "의",
    "와",
    "과",
    "도",
    "로",
    "으로",
    "하다",
    "있다",
    "없다",
    "이",
    "그",
    "저",
    "우리",
    "너",
    "저희",
    "들",
    "등",
    "및",
    "또는",
    "그리고",
    "하지만",
    "때문",
];

const STOP_ES: &[&str] = &[
    "el", "la", "los", "las", "un", "una", "unos", "unas", "de", "del", "y", "o", "a", "en", "que",
    "es", "son", "por", "para", "con", "se", "su", "sus", "al", "lo", "no", "si", "mas", "pero",
    "como", "me", "te", "le", "les", "su", "mi", "tu", "eso", "esta", "este", "eso",
];

const STOP_FR: &[&str] = &[
    "le", "la", "les", "un", "une", "des", "du", "de", "et", "ou", "a", "en", "que", "qui", "est",
    "sont", "pour", "par", "avec", "se", "sa", "ses", "au", "ce", "ces", "ne", "pas", "mais",
    "comme", "mon", "ton", "son", "nous", "vous", "ils", "elles", "dans", "sur", "sous",
];

const STOP_DE: &[&str] = &[
    "der", "die", "das", "den", "dem", "des", "ein", "eine", "einer", "einen", "einem", "eines",
    "und", "oder", "in", "zu", "von", "mit", "ist", "sind", "auf", "nicht", "aber", "als", "auch",
    "wenn", "so", "den", "dem", "im", "am", "zum", "zur", "beim", "das", "daß",
];

const STOP_AR: &[&str] = &[
    "في",
    "من",
    "على",
    "إلى",
    "عن",
    "مع",
    "هذا",
    "هذه",
    "ذلك",
    "التي",
    "الذي",
    "الذين",
    "ما",
    "لا",
    "لم",
    "لن",
    "قد",
    "كان",
    "كانت",
    "هو",
    "هي",
    "هم",
    "هن",
    "إن",
    "أن",
    "أو",
    "ثم",
    "حتى",
    "كل",
    "بعض",
    "غير",
];

const STOP_HI: &[&str] = &[
    "और",
    "यह",
    "वह",
    "इस",
    "का",
    "की",
    "के",
    "में",
    "से",
    "को",
    "ने",
    "है",
    "हैं",
    "था",
    "थी",
    "थे",
    "कि",
    "जो",
    "भी",
    "नहीं",
    "पर",
    "या",
    "तो",
    "ही",
    "व",
    "एक",
    "लिए",
    "द्वारा",
    "साथ",
    "पर",
];

const STOP_NE: &[&str] = &[
    "र",
    "यो",
    "त्यो",
    "यस",
    "को",
    "का",
    "की",
    "मा",
    "बाट",
    "लाई",
    "छ",
    "छन्",
    "थियो",
    "थिइन्",
    "थिए",
    "वा",
    "तर",
    "पनि",
    "होइन",
    "गर्न",
    "भएको",
    "गर्दा",
    "यहाँ",
    "त्यहाँ",
    "कुनै",
    "सबै",
    "एक",
];

const STOP_PT: &[&str] = &[
    "o", "a", "os", "as", "um", "uma", "de", "do", "da", "dos", "das", "e", "ou", "em", "que", "é",
    "são", "para", "por", "com", "se", "seu", "sua", "no", "na", "nos", "nas", "não", "mas",
    "como", "mais", "este", "essa", "isso", "aquele", "aquela",
];

const STOP_RU: &[&str] = &[
    "и",
    "в",
    "во",
    "что",
    "на",
    "с",
    "со",
    "для",
    "из",
    "от",
    "до",
    "по",
    "о",
    "об",
    "при",
    "как",
    "не",
    "но",
    "или",
    "чтобы",
    "же",
    "ли",
    "бы",
    "был",
    "была",
    "было",
    "были",
    "это",
    "этот",
    "эта",
    "эти",
    "тот",
    "та",
    "он",
    "она",
    "они",
    "мы",
    "вы",
    "вы",
    "них",
    "ней",
    "него",
];

fn stopword_set(lang: &str) -> &'static [&'static str] {
    match lang {
        "zh" => STOP_ZH,
        "ja" => STOP_JA,
        "ko" => STOP_KO,
        "es" => STOP_ES,
        "fr" => STOP_FR,
        "de" => STOP_DE,
        "ar" => STOP_AR,
        "hi" => STOP_HI,
        "ne" => STOP_NE,
        "pt" => STOP_PT,
        "ru" => STOP_RU,
        _ => STOP_EN,
    }
}

/// Stopwords for the page's language AND English (agents
/// often query in English even for non-English pages).
fn is_stopword(token: &str, lang: &str) -> bool {
    if STOP_EN.contains(&token) {
        return true;
    }
    if lang != "en" {
        return stopword_set(lang).contains(&token);
    }
    false
}

// ── Accent folding (Latin only) ──────────────────────────────

/// Fold accented Latin → ASCII (café→cafe, naïve→naive).
/// Non-Latin scripts pass through unchanged. Helps
/// cross-lingual search; applied to tokens before stemming.
fn fold_ascii(c: char) -> char {
    let u = c as u32;
    // Latin-1 Supplement: common Western European.
    match u {
        0x00C0..=0x00C5 => 'A',          // À-Å
        0x00C8..=0x00CB => 'E',          // È-Ë
        0x00CC..=0x00CF => 'I',          // Ì-Ï
        0x00D2..=0x00D6 | 0x00D8 => 'O', // Ò-Ö, Ø
        0x00D9..=0x00DC => 'U',          // Ù-Ü
        0x00C7 => 'C',                   // Ç
        0x00D1 => 'N',                   // Ñ
        0x00E0..=0x00E5 => 'a',          // à-å
        0x00E8..=0x00EB => 'e',          // è-ë
        0x00EC..=0x00EF => 'i',          // ì-ï
        0x00F2..=0x00F6 | 0x00F8 => 'o', // ò-ö, ø
        0x00F9..=0x00FC => 'u',          // ù-ü
        0x00E7 => 'c',                   // ç
        0x00F1 => 'n',                   // ñ
        0x00DD | 0x00FD | 0x00FF => 'y', // Ý ý ÿ
        0x0178 => 'Y',                   // Ÿ
        // German ß → ss handled in fold_str (1→2 expansion).
        _ => {
            // Latin Extended-A: try common mappings.
            match u {
                0x0100..=0x0105 => {
                    if u.is_multiple_of(2) {
                        'a'
                    } else {
                        'A'
                    }
                } // Ā-ą (alternating)
                0x0106..=0x010D => {
                    if u.is_multiple_of(2) {
                        'c'
                    } else {
                        'C'
                    }
                } // Ć-č
                0x010E..=0x0113 => {
                    if u.is_multiple_of(2) {
                        'd'
                    } else {
                        'D'
                    }
                } // Ď-ď Ď-ď
                0x0114..=0x011B => {
                    if u.is_multiple_of(2) {
                        'e'
                    } else {
                        'E'
                    }
                } // Ĕ-ě
                0x011C..=0x0123 => {
                    if u.is_multiple_of(2) {
                        'g'
                    } else {
                        'G'
                    }
                } // Ĝ-ģ
                0x0124..=0x0127 => {
                    if u.is_multiple_of(2) {
                        'h'
                    } else {
                        'H'
                    }
                } // Ĥ-ħ
                0x0128..=0x0131 => {
                    if u.is_multiple_of(2) {
                        'i'
                    } else {
                        'I'
                    }
                } // Ĩ-ı
                0x0134..=0x0135 => {
                    if u == 0x0134 {
                        'J'
                    } else {
                        'j'
                    }
                } // Ĵ ĵ
                0x0136..=0x013B => {
                    let m = (u - 0x0136) % 2;
                    if u < 0x0138 {
                        if m == 0 { 'k' } else { 'K' }
                    } else {
                        if m == 0 { 'l' } else { 'L' }
                    }
                }
                0x013C..=0x0142 => {
                    if u.is_multiple_of(2) {
                        'l'
                    } else {
                        'L'
                    }
                }
                0x0143..=0x014B => {
                    if u.is_multiple_of(2) {
                        'n'
                    } else {
                        'N'
                    }
                }
                0x014C..=0x0151 => {
                    if u.is_multiple_of(2) {
                        'o'
                    } else {
                        'O'
                    }
                }
                0x0152 => 'O', // Œ
                0x0153 => 'o', // œ
                0x0154..=0x0159 => {
                    if u.is_multiple_of(2) {
                        'r'
                    } else {
                        'R'
                    }
                }
                0x015A..=0x0161 => {
                    if u.is_multiple_of(2) {
                        's'
                    } else {
                        'S'
                    }
                }
                0x0162..=0x0167 => {
                    if u.is_multiple_of(2) {
                        't'
                    } else {
                        'T'
                    }
                }
                0x0168..=0x0173 => {
                    if u.is_multiple_of(2) {
                        'u'
                    } else {
                        'U'
                    }
                }
                0x0174..=0x0175 => {
                    if u == 0x0174 {
                        'W'
                    } else {
                        'w'
                    }
                }
                0x0176..=0x0177 => {
                    if u == 0x0176 {
                        'Y'
                    } else {
                        'y'
                    }
                }
                0x0179..=0x017B => {
                    if u.is_multiple_of(2) {
                        'z'
                    } else {
                        'Z'
                    }
                }
                _ => c,
            }
        }
    }
}

/// Fold a string to ASCII for Latin scripts. Handles ß → ss.
fn fold_str(s: &str) -> String {
    // Quick check: already ASCII?
    if s.is_ascii() {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == 'ß' {
            out.push_str("ss");
        } else if c as u32 <= 0x024F {
            out.push(fold_ascii(c));
        } else {
            out.push(c);
        }
    }
    out
}

// ── Light English stemmer ────────────────────────────────────

/// Simplified Porter-like stemmer. Covers the 80% of
/// English inflection: plurals, -ing, -ed, common suffixes.
/// Conservative: only strips when stem ≥ 3 chars. Better to
/// under-stem than over-stem (over-stemming merges
/// unrelated words, hurting BM25 precision).
fn stem_en(word: &str) -> String {
    let w = word;
    if w.len() < 4 {
        return w.to_string();
    }
    // -ness (before -ss guard, since "happiness" ends in "ss" but
    // the real suffix is "-ness")
    if w.ends_with("ness") && w.len() > 5 {
        return w[..w.len() - 4].to_string();
    }
    // -ment (before -ss guard for same reason)
    if w.ends_with("ment") && w.len() > 6 {
        return w[..w.len() - 4].to_string();
    }
    // -tion → -t
    if w.ends_with("tion") && w.len() > 5 {
        return format!("{}t", &w[..w.len() - 4]);
    }
    // -sses → -ss
    if w.ends_with("sses") {
        return w[..w.len() - 2].to_string();
    }
    // -ies → -i
    if w.ends_with("ies") && w.len() > 4 {
        return format!("{}i", &w[..w.len() - 3]);
    }
    // -ss → -ss (don't strip)
    if w.ends_with("ss") {
        return w.to_string();
    }
    // -ing
    if w.ends_with("ing") && w.len() > 4 {
        let stem = &w[..w.len() - 3];
        if stem.len() >= 3 {
            return stem_en_double(stem);
        }
    }
    // -edly → -e (looked, walked, talked)
    if w.ends_with("edly") && w.len() > 4 {
        let stem = &w[..w.len() - 4];
        if stem.len() >= 3 {
            return stem.to_string();
        }
    }
    // -ed
    if w.ends_with("ed") && w.len() > 3 {
        let stem = &w[..w.len() - 2];
        if stem.len() >= 3 {
            return stem_en_double(stem);
        }
    }
    // -ly
    if w.ends_with("ly") && w.len() > 3 {
        let stem = &w[..w.len() - 2];
        if stem.len() >= 3 {
            return stem.to_string();
        }
    }
    // -ers → -er
    if w.ends_with("ers") && w.len() > 4 {
        return w[..w.len() - 1].to_string();
    }
    // -er
    if w.ends_with("er") && w.len() > 4 {
        let stem = &w[..w.len() - 2];
        if stem.len() >= 3 {
            return stem.to_string();
        }
    }
    // -est
    if w.ends_with("est") && w.len() > 5 {
        return w[..w.len() - 3].to_string();
    }
    // -s (plural, after all other rules)
    if w.ends_with('s') && !w.ends_with("us") && !w.ends_with("ss") {
        let stem = &w[..w.len() - 1];
        if stem.len() >= 3 {
            return stem.to_string();
        }
    }
    w.to_string()
}

/// Handle Porter step 1b double-consonant: "running" → stem
/// "runn" → "run" (double n → single n). "hopping" → "hop"
/// → "hopping" → "hopp" → "hop". But "typing" → "typ" (no
/// double consonant, stays).
fn stem_en_double(stem: &str) -> String {
    let chars: Vec<char> = stem.chars().collect();
    let n = chars.len();
    if n >= 2 && chars[n - 1] == chars[n - 2] {
        let c = chars[n - 1];
        // Only double consonants (not vowels, not 'l'/'s'/'z'
        // which Porter treats specially — but we keep it simple).
        if !"aeioulsz".contains(c) {
            return chars[..n - 1].iter().collect();
        }
    }
    stem.to_string()
}

/// Light suffix stripping for Romance languages. Not a full
/// stemmer — just strips common inflectional endings to
/// improve cross-form matching. Conservative: stem ≥ 3 chars.
fn stem_romance(word: &str, lang: &str) -> String {
    let w = word;
    if w.len() < 5 {
        return w.to_string();
    }
    let suffixes: &[&str] = match lang {
        "es" => &[
            "amiento", "imiento", "acion", "aciones", "ando", "iendo", "ar", "er", "ir", "ado",
            "ido", "ando", "an", "en", "es", "os", "as", "a",
        ],
        "fr" => &[
            "ement", "ation", "ations", "issant", "er", "ir", "re", "ée", "ées", "ant", "ent",
            "ons", "ez", "s",
        ],
        "pt" => &[
            "amento", "imento", "ação", "ções", "ando", "endo", "indo", "ar", "er", "ir", "ado",
            "ido", "ão", "ões", "os", "as", "a",
        ],
        _ => &["tion", "ment", "ing", "ed", "es", "er"],
    };
    for suf in suffixes {
        if let Some(stem) = w.strip_suffix(suf).filter(|s| s.len() >= 3) {
            return stem.to_string();
        }
    }
    w.to_string()
}

/// Light suffix stripping for Germanic.
fn stem_german(word: &str) -> String {
    let w = word;
    if w.len() < 5 {
        return w.to_string();
    }
    for suf in &["en", "er", "es", "em", "e", "s", "n"] {
        if let Some(stem) = w.strip_suffix(suf).filter(|s| s.len() >= 3) {
            return stem.to_string();
        }
    }
    w.to_string()
}

fn stem(token: &str, lang: &str) -> String {
    match lang {
        "en" => stem_en(token),
        "es" | "fr" | "pt" => stem_romance(token, lang),
        "de" => stem_german(token),
        _ => token.to_string(), // CJK, Arabic, etc: no stemming
    }
}

// ── Tokenizer ────────────────────────────────────────────────

/// Tokenize text for BM25 indexing. Language-aware:
/// - CJK/Thai: character unigrams + bigrams
/// - Latin/Cyrillic/etc.: word-boundary split
/// - Accent folding for Latin
/// - Language-specific stopword removal
/// - Light stemming for English, Romance, German
///
/// The result is a list of normalized tokens ready for
/// BM25 scoring.
pub fn tokenize(text: &str, lang: &LanguageInfo) -> Vec<String> {
    if language::needs_char_tokenize(lang.script) {
        tokenize_cjk(text, &lang.code)
    } else {
        tokenize_word_split(text, &lang.code)
    }
}

/// Backwards-compatible tokenize for callers that don't
/// have language info (defaults to English).
#[allow(dead_code)]
pub fn tokenize_simple(text: &str) -> Vec<String> {
    tokenize_word_split(text, "en")
}

fn tokenize_word_split(text: &str, lang: &str) -> Vec<String> {
    let folded = if lang == "en" || lang == "es" || lang == "fr" || lang == "de" || lang == "pt" {
        fold_str(text)
    } else {
        text.to_string()
    };
    let folded = folded.to_lowercase();
    let mut tokens = Vec::new();
    for part in folded.split(|c: char| !c.is_alphanumeric()) {
        let t = part.trim();
        if t.is_empty() {
            continue;
        }
        // Latin: skip single chars (noise). CJK handled separately.
        if t.chars().count() < 2 {
            continue;
        }
        if is_stopword(t, lang) {
            continue;
        }
        let stemmed = stem(t, lang);
        if !stemmed.is_empty() && stemmed.chars().count() >= 2 {
            tokens.push(stemmed);
        }
    }
    tokens
}

fn tokenize_cjk(text: &str, lang: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut cjk_buf: Vec<char> = Vec::new();
    let mut word_buf = String::new();

    let flush_cjk = |buf: &mut Vec<char>, tokens: &mut Vec<String>| {
        if buf.is_empty() {
            return;
        }
        // Unigrams.
        for &c in buf.iter() {
            let s = c.to_string();
            if !is_stopword(&s, lang) {
                tokens.push(s);
            }
        }
        // Bigrams.
        for w in buf.windows(2) {
            let bg: String = w.iter().collect();
            let s1 = w[0].to_string();
            let s2 = w[1].to_string();
            if !is_stopword(&s1, lang) || !is_stopword(&s2, lang) {
                tokens.push(bg);
            }
        }
        buf.clear();
    };

    let flush_word = |buf: &mut String, tokens: &mut Vec<String>| {
        let t = buf.trim().to_lowercase();
        if t.chars().count() >= 2 && !is_stopword(&t, "en") {
            let stemmed = stem(&t, "en");
            if !stemmed.is_empty() && stemmed.chars().count() >= 2 {
                tokens.push(stemmed);
            }
        }
        buf.clear();
    };

    for c in text.chars() {
        let s = language::char_script(c);
        if language::needs_char_tokenize(s) {
            // CJK/Kana/Hangul/Thai char — flush any Latin word.
            flush_word(&mut word_buf, &mut tokens);
            cjk_buf.push(c);
        } else if c.is_alphanumeric() {
            // Latin/Cyrillic/etc — flush CJK buffer, collect word.
            flush_cjk(&mut cjk_buf, &mut tokens);
            word_buf.push(c);
        } else {
            // Whitespace/punctuation — flush both buffers.
            flush_cjk(&mut cjk_buf, &mut tokens);
            flush_word(&mut word_buf, &mut tokens);
        }
    }
    flush_cjk(&mut cjk_buf, &mut tokens);
    flush_word(&mut word_buf, &mut tokens);

    tokens
}

// ── BM25 filter ─────────────────────────────────────────────

/// Compute BM25 scores for each block against the query.
/// Returns a vector of scores (one per block, 0.0 = no match).
/// Used by both `filter` (BM25-only) and `filter_semantic`
/// (BM25 + cross-encoder union).
fn bm25_scores(blocks: &[Block], query: &str, lang: &LanguageInfo) -> Vec<f64> {
    let qterms = tokenize(query, lang);
    if qterms.is_empty() || blocks.is_empty() {
        return vec![0.0; blocks.len()];
    }

    // Document stats.
    let docs: Vec<Vec<String>> = blocks.iter().map(|b| tokenize(&b.text(), lang)).collect();
    let mut df: HashMap<&str, usize> = HashMap::new();
    for doc in &docs {
        let mut seen = std::collections::HashSet::new();
        for t in doc {
            if seen.insert(t.as_str()) {
                *df.entry(t.as_str()).or_insert(0) += 1;
            }
        }
    }
    let n = blocks.len() as f64;
    let avgdl = docs.iter().map(|d| d.len()).sum::<usize>() as f64 / n.max(1.0);

    // Score each block.
    let mut scores = vec![0.0f64; blocks.len()];
    for (i, doc) in docs.iter().enumerate() {
        let mut tf: HashMap<&str, usize> = HashMap::new();
        for t in doc {
            *tf.entry(t.as_str()).or_insert(0) += 1;
        }
        let dl = doc.len() as f64;
        for q in &qterms {
            let Some(&term_df) = df.get(q.as_str()) else {
                continue;
            };
            let idf = (1.0 + (n - term_df as f64 + 0.5) / (term_df as f64 + 0.5)).ln();
            let f = tf.get(q.as_str()).copied().unwrap_or(0) as f64;
            if f > 0.0 {
                scores[i] +=
                    idf * (f * (K1 + 1.0)) / (f + K1 * (1.0 - B + B * dl / avgdl.max(1.0)));
            }
        }
    }
    scores
}

/// BM25 block filter. Returns (kept blocks, fell_back).
/// fell_back = true when the query matched nothing and we
/// returned the full page — the CALLER must signal this,
/// or the agent mistakes full content for focus matches.
///
/// BM25-only version. Production code uses `filter_semantic`
/// (BM25 + cross-encoder union). This function is kept for
/// tests and as a pure-BM25 baseline.
#[allow(dead_code)]
pub fn filter<'a>(blocks: &'a [Block], query: &str, lang: &LanguageInfo) -> (Vec<&'a Block>, bool) {
    let qterms = tokenize(query, lang);
    if qterms.is_empty() || blocks.is_empty() {
        return (blocks.iter().collect(), false);
    }

    let scores = bm25_scores(blocks, query, lang);
    let max_score = scores.iter().cloned().fold(0.0f64, f64::max);
    if max_score <= 0.0 {
        return (blocks.iter().collect(), true); // no hits → full, SIGNAL it
    }

    // Keep blocks above a fraction of the max score, in doc order.
    let threshold = max_score * 0.15;
    let kept: Vec<&Block> = scores
        .iter()
        .enumerate()
        .filter(|(_, s)| **s >= threshold)
        .map(|(i, _)| &blocks[i])
        .collect();
    (kept, false)
}

/// Hybrid BM25 + cross-encoder block filter.
///
/// Runs BM25 first (microseconds, zero dependency). If the
/// cross-encoder model is already cached on disk (downloaded
/// during search reranking), it runs a second pass on all blocks
/// and adds semantically relevant blocks that BM25 missed —
/// catching cases where the query and block use different
/// vocabulary ("backpropagation" vs "backward pass computes
/// gradients").
///
/// If the model isn't cached, falls back to pure BM25 (same as
/// `filter`). No model download is triggered during fetch.
///
/// The cross-encoder also rescues the BM25 fell_back case: when
/// BM25 finds zero keyword matches, the cross-encoder may still
/// find semantic matches, preventing a full-page fallback.
pub fn filter_semantic<'a>(
    blocks: &'a [Block],
    query: &str,
    lang: &LanguageInfo,
) -> (Vec<&'a Block>, bool) {
    let qterms = tokenize(query, lang);
    if qterms.is_empty() || blocks.is_empty() {
        return (blocks.iter().collect(), false);
    }

    // ── Phase 1: BM25 (always — microseconds) ──
    let scores = bm25_scores(blocks, query, lang);
    let max_bm25 = scores.iter().cloned().fold(0.0f64, f64::max);
    let has_bm25_hits = max_bm25 > 0.0;

    let mut kept: Vec<usize> = if has_bm25_hits {
        let threshold = max_bm25 * 0.15;
        scores
            .iter()
            .enumerate()
            .filter(|(_, s)| **s >= threshold)
            .map(|(i, _)| i)
            .collect()
    } else {
        Vec::new()
    };

    // ── Phase 2: Cross-encoder semantic augmentation ──
    // Only when the model is already cached (from search use).
    // Never triggers a model download during a plain fetch.
    if blocks.len() <= SEMANTIC_MAX_BLOCKS && crate::search::rerank::is_model_cached() {
        let docs: Vec<(String, String)> =
            blocks.iter().map(|b| (b.text(), String::new())).collect();
        if let Some(xenc_scores) = crate::search::rerank::cross_encoder_scores(query, &docs) {
            if !has_bm25_hits {
                // BM25 found nothing — cross-encoder rescues.
                for (i, xenc) in xenc_scores.iter().enumerate() {
                    if *xenc >= XENC_THRESHOLD {
                        kept.push(i);
                    }
                }
            } else {
                // Union: add cross-encoder matches BM25 missed.
                let bm25_set: HashSet<usize> = kept.iter().copied().collect();
                for (i, xenc) in xenc_scores.iter().enumerate() {
                    if *xenc >= XENC_THRESHOLD && !bm25_set.contains(&i) {
                        kept.push(i);
                    }
                }
            }
        }
    }

    if kept.is_empty() {
        return (blocks.iter().collect(), true);
    }

    // Sort by index to preserve document order.
    kept.sort_unstable();
    let kept_blocks = kept.into_iter().map(|i| &blocks[i]).collect();
    (kept_blocks, false)
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::super::language::Script;
    use super::*;

    fn en() -> LanguageInfo {
        LanguageInfo {
            code: "en".to_string(),
            script: Script::Latin,
            scripts: vec![Script::Latin],
        }
    }

    fn zh() -> LanguageInfo {
        LanguageInfo {
            code: "zh".to_string(),
            script: Script::Han,
            scripts: vec![Script::Han],
        }
    }

    fn ja() -> LanguageInfo {
        LanguageInfo {
            code: "ja".to_string(),
            script: Script::Han,
            scripts: vec![Script::Han, Script::Kana],
        }
    }

    fn ko() -> LanguageInfo {
        LanguageInfo {
            code: "ko".to_string(),
            script: Script::Hangul,
            scripts: vec![Script::Hangul],
        }
    }

    // ── English tokenizer ──

    #[test]
    fn tokenize_en_basic() {
        let tokens = tokenize("The quick brown fox", &en());
        assert!(tokens.contains(&"quick".to_string()));
        assert!(tokens.contains(&"brown".to_string()));
        assert!(tokens.contains(&"fox".to_string()));
        assert!(!tokens.contains(&"the".to_string())); // stopword
    }

    #[test]
    fn tokenize_en_stemming() {
        let tokens = tokenize("running jumped quickly", &en());
        assert!(tokens.contains(&"run".to_string())); // running → run
        assert!(tokens.contains(&"jump".to_string())); // jumped → jump
        assert!(tokens.contains(&"quick".to_string())); // quickly → quick
    }

    #[test]
    fn tokenize_en_plural() {
        assert!(tokenize("cats", &en()).contains(&"cat".to_string()));
        assert!(tokenize("buses", &en()).contains(&"buse".to_string())); // buses → buse
        assert!(tokenize("berries", &en()).contains(&"berri".to_string())); // berries → berri
    }

    #[test]
    fn tokenize_en_min_length() {
        let tokens = tokenize("a I be to do", &en());
        assert!(tokens.is_empty()); // all stopwords or < 2 chars
    }

    // ── Chinese tokenizer ──

    #[test]
    fn tokenize_zh_unigrams_and_bigrams() {
        let tokens = tokenize("机器学习", &zh());
        // Unigrams.
        assert!(tokens.contains(&"机".to_string()));
        assert!(tokens.contains(&"器".to_string()));
        assert!(tokens.contains(&"学".to_string()));
        assert!(tokens.contains(&"习".to_string()));
        // Bigrams.
        assert!(tokens.contains(&"机器".to_string()));
        assert!(tokens.contains(&"器学".to_string()));
        assert!(tokens.contains(&"学习".to_string()));
    }

    #[test]
    fn tokenize_zh_stopwords() {
        let tokens = tokenize("的是在", &zh());
        assert!(!tokens.contains(&"的".to_string()));
        assert!(!tokens.contains(&"是".to_string()));
        assert!(!tokens.contains(&"在".to_string()));
    }

    #[test]
    fn tokenize_zh_mixed_latin() {
        let tokens = tokenize("Python编程语言", &zh());
        // CJK unigrams.
        assert!(tokens.contains(&"编".to_string()));
        assert!(tokens.contains(&"程".to_string()));
        // Latin word.
        assert!(tokens.contains(&"python".to_string()));
    }

    // ── Japanese tokenizer ──

    #[test]
    fn tokenize_ja_kana_and_kanji() {
        let tokens = tokenize("機械学習の分野", &ja());
        // Kanji unigrams.
        assert!(tokens.contains(&"機".to_string()));
        assert!(tokens.contains(&"械".to_string()));
        // "の" is a stopword — should be filtered.
        assert!(!tokens.contains(&"の".to_string()));
        // Non-stopword kana should be present.
        // "分" is a kanji (not kana), but let's test a non-stopword.
        assert!(tokens.contains(&"分".to_string()));
        // Bigrams.
        assert!(tokens.contains(&"機械".to_string()));
    }

    // ── Korean tokenizer ──

    #[test]
    fn tokenize_ko_unigrams_bigrams() {
        let tokens = tokenize("한국어", &ko());
        assert!(tokens.contains(&"한".to_string()));
        assert!(tokens.contains(&"국".to_string()));
        assert!(tokens.contains(&"어".to_string()));
        assert!(tokens.contains(&"한국".to_string()));
        assert!(tokens.contains(&"국어".to_string()));
    }

    // ── Accent folding ──

    #[test]
    fn fold_cafe() {
        assert_eq!(fold_str("café"), "cafe");
        assert_eq!(fold_str("naïve"), "naive");
        assert_eq!(fold_str("München"), "Munchen");
        assert_eq!(fold_str("résumé"), "resume");
    }

    #[test]
    fn fold_german_ss() {
        assert_eq!(fold_str("Straße"), "Strasse");
    }

    #[test]
    fn fold_preserves_non_latin() {
        assert_eq!(fold_str("日本語"), "日本語");
        assert_eq!(fold_str("العربية"), "العربية");
    }

    #[test]
    fn tokenize_accent_folding_en() {
        let tokens = tokenize("café résumé naïve", &en());
        assert!(tokens.contains(&"cafe".to_string()));
        assert!(tokens.contains(&"resume".to_string()));
        assert!(tokens.contains(&"naive".to_string()));
    }

    // ── Stemming ──

    #[test]
    fn stem_ing() {
        assert_eq!(stem_en("running"), "run");
        assert_eq!(stem_en("typing"), "typ");
        assert_eq!(stem_en("flying"), "fly");
    }

    #[test]
    fn stem_ed() {
        assert_eq!(stem_en("jumped"), "jump");
        assert_eq!(stem_en("walked"), "walk");
    }

    #[test]
    fn stem_ness_ment() {
        assert_eq!(stem_en("happiness"), "happi");
        assert_eq!(stem_en("development"), "develop");
    }

    #[test]
    fn stem_short_words() {
        assert_eq!(stem_en("cat"), "cat"); // too short to strip
        assert_eq!(stem_en("is"), "is"); // too short
    }

    #[test]
    fn stem_preserves_us() {
        assert_eq!(stem_en("status"), "status"); // -us not stripped
        assert_eq!(stem_en("genius"), "genius");
    }

    #[test]
    fn stem_german_basic() {
        assert_eq!(stem_german("machen"), "mach");
        assert_eq!(stem_german("Häuser"), "Häus"); // ä not folded here
        assert_eq!(stem_german("sagen"), "sag");
    }

    // ── BM25 filter ──

    use super::super::blocks::Block;

    fn para(text: &str) -> Block {
        Block::Para {
            md: text.to_string(),
            link_density: 0.0,
            path: vec![],
        }
    }

    #[test]
    fn bm25_basic_match() {
        let blocks = vec![
            para("Machine learning is a subset of artificial intelligence"),
            para("The weather is nice today"),
            para("Deep learning uses neural networks"),
        ];
        let (kept, fell_back) = filter(&blocks, "machine learning", &en());
        assert!(!fell_back);
        assert!(!kept.is_empty());
        // The block with "machine learning" should be kept.
        assert!(kept.iter().any(|b| b.text().contains("Machine learning")));
    }

    #[test]
    fn bm25_no_match_fell_back() {
        let blocks = vec![para("The weather is nice today"), para("I like pizza")];
        let (kept, fell_back) = filter(&blocks, "quantum physics", &en());
        assert!(fell_back);
        assert_eq!(kept.len(), 2); // all blocks returned
    }

    #[test]
    fn bm25_empty_query() {
        let blocks = vec![para("Some content")];
        let (kept, fell_back) = filter(&blocks, "", &en());
        assert!(!fell_back);
        assert_eq!(kept.len(), 1);
    }

    #[test]
    fn bm25_chinese_match() {
        let blocks = vec![
            para("机器学习是人工智能的一个分支领域"),
            para("今天天气很好"),
            para("深度学习使用神经网络"),
        ];
        let (kept, fell_back) = filter(&blocks, "机器学习", &zh());
        assert!(!fell_back);
        assert!(!kept.is_empty());
    }

    #[test]
    fn bm25_japanese_match() {
        let blocks = vec![
            para("機械学習は人工知能の一分野である"),
            para("今日はいい天気ですね"),
        ];
        let (kept, fell_back) = filter(&blocks, "機械学習", &ja());
        assert!(!fell_back);
        assert!(!kept.is_empty());
    }

    #[test]
    fn bm25_stemming_match() {
        let blocks = vec![para("The runner was running fast"), para("Cooking is fun")];
        // Query "run" should match "running" via stemming.
        let (kept, fell_back) = filter(&blocks, "run", &en());
        assert!(!fell_back);
        assert!(
            kept.iter()
                .any(|b| b.text().contains("runner") || b.text().contains("running"))
        );
    }

    #[test]
    fn bm25_accent_match() {
        let blocks = vec![para("Le café est délicieux"), para("The weather is nice")];
        // Query "cafe" should match "café" via accent folding.
        let (_kept, fell_back) = filter(&blocks, "cafe", &en());
        assert!(!fell_back);
    }

    // ── bm25_scores unit tests ──

    #[test]
    fn bm25_scores_positive_for_match() {
        let blocks = vec![
            para("Machine learning is a subset of artificial intelligence"),
            para("The weather is nice today"),
        ];
        let scores = bm25_scores(&blocks, "machine learning", &en());
        assert!(scores[0] > 0.0); // matching block
        assert_eq!(scores[1], 0.0); // non-matching block
    }

    #[test]
    fn bm25_scores_empty_query_zeros() {
        let blocks = vec![para("Some content")];
        let scores = bm25_scores(&blocks, "", &en());
        assert!(scores.iter().all(|s| *s == 0.0));
    }

    // ── filter_semantic tests ──
    // These tests assert properties that hold regardless of
    // whether the cross-encoder model is cached. filter_semantic
    // is a union (BM25 ∪ cross-encoder), so it always keeps at
    // least the BM25 matches.

    #[test]
    fn filter_semantic_empty_query() {
        let blocks = vec![para("Some content"), para("Other content")];
        let (kept, fell_back) = filter_semantic(&blocks, "", &en());
        assert!(!fell_back);
        assert_eq!(kept.len(), 2);
    }

    #[test]
    fn filter_semantic_keeps_bm25_matches() {
        let blocks = vec![
            para("Machine learning is a subset of artificial intelligence"),
            para("The weather is nice today"),
            para("Deep learning uses neural networks"),
        ];
        let (kept, fell_back) = filter_semantic(&blocks, "machine learning", &en());
        assert!(!fell_back);
        assert!(!kept.is_empty());
        assert!(kept.iter().any(|b| b.text().contains("Machine learning")));
    }

    #[test]
    fn filter_semantic_union_property() {
        // filter_semantic is a union: it keeps at least every
        // block that filter (BM25-only) keeps.
        let blocks = vec![
            para("Machine learning is a subset of artificial intelligence"),
            para("The weather is nice today"),
            para("Deep learning uses neural networks"),
        ];
        let (bm25_kept, _) = filter(&blocks, "machine learning", &en());
        let (sem_kept, _) = filter_semantic(&blocks, "machine learning", &en());
        assert!(sem_kept.len() >= bm25_kept.len());
    }

    #[test]
    fn filter_semantic_preserves_doc_order() {
        let blocks = vec![
            para("Alpha block about machine learning"),
            para("Beta block about weather"),
            para("Gamma block about neural networks"),
        ];
        let (kept, _) = filter_semantic(&blocks, "machine learning", &en());
        // Kept blocks should be in document order (by index).
        for w in kept.windows(2) {
            assert!(
                blocks.iter().position(|b| std::ptr::eq(b, w[0]))
                    <= blocks.iter().position(|b| std::ptr::eq(b, w[1]))
            );
        }
    }
}
