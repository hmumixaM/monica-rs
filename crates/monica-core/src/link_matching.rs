//! Compatibility scoring for platform account review. All input is local; matching
//! never invokes Python, an external model, or a network service.
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::OnceLock,
};
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

pub const PROVIDERS: [&str; 3] = ["wechat", "instagram", "linkedin"];
pub const PAIRS: [(&str, &str); 6] = [
    ("instagram-wechat", "Instagram × 微信"),
    ("linkedin-wechat", "LinkedIn × 微信"),
    ("instagram-linkedin", "Instagram × LinkedIn"),
    ("wechat-instagram", "微信 × Instagram"),
    ("wechat-linkedin", "微信 × LinkedIn"),
    ("linkedin-instagram", "LinkedIn × Instagram"),
];
const FIELDS: [(&str, &str); 13] = [
    ("wechat.alias", "微信号"),
    ("wechat.remark", "备注"),
    ("wechat.nickname", "昵称"),
    ("wechat.group_nicknames", "群内名"),
    ("wechat.historical_nicknames", "曾用名"),
    ("instagram.username", ""),
    ("instagram.full_name", "Instagram 全名"),
    ("linkedin.name", "LinkedIn 姓名"),
    ("linkedin.headline", "LinkedIn 简介"),
    ("linkedin.profile_url", ""),
    ("email", "邮箱"),
    ("phone", "电话"),
    ("mobile", "电话"),
];
const SIGNAL_ORDER: [&str; 10] = [
    "备注",
    "微信号",
    "群内名",
    "昵称",
    "曾用名",
    "Instagram 全名",
    "LinkedIn 姓名",
    "LinkedIn 简介",
    "邮箱",
    "电话",
];
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AccountSignals {
    pub account_id: String,
    pub provider: String,
    pub external_id: String,
    pub display_name: String,
    pub username: String,
    pub profile_url: String,
    pub profile_id: Option<String>,
    pub profile_name: String,
    pub ready: bool,
    pub avatar_media_id: Option<String>,
    pub aliases: Vec<String>,
    pub handles: BTreeSet<String>,
    pub emails: BTreeSet<String>,
    pub phones: BTreeSet<String>,
}
impl AccountSignals {
    pub fn identity_values(&self) -> Vec<&str> {
        [
            &self.display_name,
            &self.profile_name,
            &self.username,
            &self.external_id,
        ]
        .into_iter()
        .chain(self.aliases.iter())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect()
    }
}
#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct CandidateScore {
    pub score: i64,
    pub reasons: Vec<String>,
}
pub fn ascii_text(s: &str) -> String {
    deunicode::deunicode(&s.nfkc().collect::<String>()).to_lowercase()
}
pub fn words(s: &str) -> String {
    ascii_text(s)
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}
pub fn compact(s: &str) -> String {
    ascii_text(s)
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect()
}
pub fn username_words(s: &str) -> String {
    words(s)
        .split_whitespace()
        .map(|token| {
            let mut out = String::new();
            let mut chars = token.chars().peekable();
            while let Some(c) = chars.next() {
                let mut n = 1;
                while chars.peek() == Some(&c) {
                    n += 1;
                    chars.next();
                }
                for _ in 0..if n >= 3 { 1 } else { n } {
                    out.push(c);
                }
            }
            out
        })
        .filter(|s| s.len() > 1)
        .collect::<Vec<_>>()
        .join(" ")
}
struct Pinyin {
    phrases: HashMap<&'static str, &'static str>,
    characters: HashMap<char, &'static str>,
    max_phrase: usize,
}
fn pinyin() -> &'static Pinyin {
    static DATA: OnceLock<Pinyin> = OnceLock::new();
    DATA.get_or_init(|| {
        let phrases: HashMap<_, _> = include_str!("../data/link-matching/pinyin-phrases.tsv")
            .lines()
            .filter_map(|s| s.split_once('\t'))
            .collect();
        let max_phrase = phrases.keys().map(|s| s.chars().count()).max().unwrap_or(1);
        let characters = include_str!("../data/link-matching/pinyin-characters.tsv")
            .lines()
            .filter_map(|s| s.split_once('\t'))
            .map(|(k, v)| (k.chars().next().unwrap(), v))
            .collect();
        Pinyin {
            phrases,
            characters,
            max_phrase,
        }
    })
}
fn syllables(s: &str, ignore_unknown: bool) -> Vec<String> {
    let norm = s.nfkc().collect::<String>();
    let starts = norm
        .char_indices()
        .map(|(i, _)| i)
        .chain([norm.len()])
        .collect::<Vec<_>>();
    let data = pinyin();
    let mut result = Vec::new();
    let mut i = 0;
    while i + 1 < starts.len() {
        let mut matched = false;
        for n in (2..=data.max_phrase.min(starts.len() - 1 - i)).rev() {
            if let Some(value) = data.phrases.get(&norm[starts[i]..starts[i + n]]) {
                result.extend(value.split_whitespace().map(str::to_owned));
                i += n;
                matched = true;
                break;
            }
        }
        if matched {
            continue;
        }
        let item = &norm[starts[i]..starts[i + 1]];
        let c = item.chars().next().unwrap();
        if let Some(value) = data.characters.get(&c) {
            result.push((*value).into());
        } else if !ignore_unknown {
            result.push(item.into());
        }
        i += 1;
    }
    result
}
pub fn pinyin_compact(s: &str) -> String {
    compact(&syllables(s, false).join(" "))
}
pub fn instagram_username(s: &str) -> String {
    let mut s = s.trim().to_string();
    if s.contains("://") {
        let Ok(u) = url::Url::parse(&s) else {
            return String::new();
        };
        if !["instagram.com", "www.instagram.com"].contains(&u.host_str().unwrap_or("")) {
            return String::new();
        }
        s = u
            .path()
            .trim_matches('/')
            .split('/')
            .next()
            .unwrap_or("")
            .into();
    }
    s.trim_start_matches('@').trim().to_lowercase()
}
pub fn linkedin_handle(s: &str) -> String {
    let mut s = s.trim().to_string();
    if s.contains("://") || s.starts_with("www.") {
        let url = if s.contains("://") {
            s.clone()
        } else {
            format!("https://{s}")
        };
        let Ok(u) = url::Url::parse(&url) else {
            return String::new();
        };
        if !["linkedin.com", "www.linkedin.com"].contains(&u.host_str().unwrap_or("")) {
            return String::new();
        }
        let mut parts = u.path().split('/').filter(|s| !s.is_empty());
        let first = parts.next().unwrap_or("");
        s = if ["in", "company"].contains(&first) {
            parts.next().unwrap_or("")
        } else {
            first
        }
        .into();
    }
    s.trim().to_lowercase()
}
pub fn field_texts(v: &Value) -> Vec<String> {
    match v {
        Value::String(s) => {
            if s.trim().is_empty() {
                vec![]
            } else {
                vec![s.clone()]
            }
        }
        Value::Null => vec![],
        Value::Bool(v) => vec![if *v { "True" } else { "False" }.into()],
        Value::Number(n) => vec![n.to_string()],
        Value::Array(items) => items.iter().flat_map(field_texts).collect(),
        Value::Object(m) => {
            let preferred = [
                "text", "value", "name", "url", "username", "headline", "alias",
            ]
            .iter()
            .filter_map(|k| m.get(*k))
            .flat_map(field_texts)
            .collect::<Vec<_>>();
            if preferred.is_empty() {
                m.values()
                    .filter(|v| v.is_string())
                    .flat_map(field_texts)
                    .collect()
            } else {
                preferred
            }
        }
    }
}
fn handle(provider: &str, s: &str) -> String {
    match provider {
        "instagram" => instagram_username(s),
        "linkedin" => linkedin_handle(s),
        _ => s.trim().to_lowercase(),
    }
}
// Normalized Indel similarity, the metric used by RapidFuzz's ratio. Inputs to
// the scorer are ASCII normalization results, so byte and character lengths agree.
pub fn ratio(a: &str, b: &str) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 100.0;
    }
    let mut row = vec![0usize; b.len() + 1];
    for ca in a.bytes() {
        let mut previous = 0;
        for (j, cb) in b.bytes().enumerate() {
            let old = row[j + 1];
            row[j + 1] = if ca == cb {
                previous + 1
            } else {
                row[j + 1].max(row[j])
            };
            previous = old;
        }
    }
    200.0 * row[b.len()] as f64 / (a.len() + b.len()) as f64
}
fn partial(a: &str, b: &str) -> f64 {
    fn one(a: &str, b: &str) -> f64 {
        if a.is_empty() {
            return if b.is_empty() { 100.0 } else { 0.0 };
        }
        let mut best: f64 = 0.0;
        for end in 1..a.len() {
            best = best.max(ratio(a, &b[..end]));
        }
        for start in 0..=b.len() - a.len() {
            best = best.max(ratio(a, &b[start..start + a.len()]));
            if best == 100.0 {
                return best;
            }
        }
        for start in b.len() - a.len() + 1..b.len() {
            best = best.max(ratio(a, &b[start..]));
        }
        best
    }
    if a.len() > b.len() {
        one(b, a)
    } else if a.len() == b.len() {
        one(a, b).max(one(b, a))
    } else {
        one(a, b)
    }
}
fn sorted(s: &str) -> String {
    let mut t = s.split_whitespace().collect::<Vec<_>>();
    t.sort();
    t.join(" ")
}
fn token_set(a: &str, b: &str, partial_mode: bool) -> f64 {
    let a = a.split_whitespace().collect::<BTreeSet<_>>();
    let b = b.split_whitespace().collect::<BTreeSet<_>>();
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let common = a.intersection(&b).copied().collect::<Vec<_>>().join(" ");
    if !common.is_empty() && (partial_mode || a.is_subset(&b) || b.is_subset(&a)) {
        return 100.0;
    }
    let joined = |set: &BTreeSet<&str>, other: &BTreeSet<&str>| {
        let mut v = vec![];
        if !common.is_empty() {
            v.push(common.as_str())
        }
        v.extend(set.difference(other).copied());
        v.join(" ")
    };
    let aa = joined(&a, &b);
    let bb = joined(&b, &a);
    let metric = if partial_mode { partial } else { ratio };
    let score = metric(&aa, &bb);
    if common.is_empty() {
        score
    } else {
        score.max(metric(&common, &aa)).max(metric(&common, &bb))
    }
}
pub fn weighted_ratio(a: &str, b: &str) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let lengths = a.len().max(b.len()) as f64 / a.len().min(b.len()) as f64;
    let base = ratio(a, b);
    if lengths < 1.5 {
        return base.max(ratio(&sorted(a), &sorted(b)).max(token_set(a, b, false)) * 0.95);
    }
    let scale = if lengths <= 8.0 { 0.9 } else { 0.6 };
    base.max(partial(a, b) * scale)
        .max(partial(&sorted(a), &sorted(b)).max(token_set(a, b, true)) * 0.95 * scale)
}
pub fn score_pair(source: &AccountSignals, target: &AccountSignals) -> CandidateScore {
    let raw_handle = [
        source.username.as_str(),
        source.external_id.as_str(),
        source.profile_url.as_str(),
    ]
    .into_iter()
    .find(|s| !s.is_empty())
    .unwrap_or("");
    let source_handle = handle(&source.provider, raw_handle);
    let identities = target.identity_values();
    let mut score = 0;
    let mut reasons = vec![];
    if !source_handle.is_empty() && target.handles.contains(&source_handle) {
        score = 100;
        reasons.push(format!("对方资料已填写相同 {}", source.provider));
    }
    if !source.emails.is_disjoint(&target.emails) {
        score = score.max(99);
        reasons.push("邮箱完全一致".into());
    }
    if !source.phones.is_disjoint(&target.phones) {
        score = score.max(98);
        reasons.push("电话完全一致".into());
    }
    let hc = compact(&source_handle);
    if hc.len() >= 4
        && let Some(value) = identities.iter().find(|v| compact(v) == hc)
    {
        score = score.max(97);
        reasons.push(format!("账号与「{value}」完全一致"));
    }
    let name = if source.display_name.is_empty() {
        source.aliases.first().map(String::as_str).unwrap_or("")
    } else {
        &source.display_name
    };
    let nc = compact(name);
    let name_is_username = !nc.is_empty() && ratio(&nc, &hc) >= 92.0;
    if !name_is_username
        && nc.len() >= 4
        && let Some(value) = identities.iter().find(|v| compact(v) == nc)
    {
        score = score.max(94);
        reasons.push(format!("姓名与「{value}」完全一致"));
    }
    let mut best = 0i64;
    let mut best_value = "";
    let mut method = "";
    if !name_is_username && nc.len() >= 4 {
        let nw = words(name);
        let np = pinyin_compact(name);
        for value in &identities {
            let vw = words(value);
            let vc = compact(value);
            if vc.len() < 4 {
                continue;
            }
            let cap = if nw.split_whitespace().count() >= 2 && vw.split_whitespace().count() == 1 {
                78.0
            } else {
                100.0
            };
            let fuzzy = weighted_ratio(&nw, &vw)
                .max(ratio(&sorted(&nw), &sorted(&vw)))
                .min(cap);
            if fuzzy > best as f64 {
                best = fuzzy.round_ties_even() as i64;
                best_value = value;
                method = "姓名";
            }
            let vp = pinyin_compact(value);
            if vp.len() >= 4 && np.len() >= 4 {
                let fuzzy = ratio(&nc, &vp)
                    .max(ratio(&np, &vp))
                    .max(ratio(&np, &vc))
                    .min(cap);
                if fuzzy > best as f64 {
                    best = fuzzy.round_ties_even() as i64;
                    best_value = value;
                    method = "姓名拼音";
                }
            }
        }
    }
    if best >= 70 {
        score = score.max(best.min(93));
        reasons.push(format!("{method} {best}% 接近「{best_value}」"));
    }
    best = 0;
    best_value = "";
    let uw = username_words(if source_handle.is_empty() {
        &source.username
    } else {
        &source_handle
    });
    if uw.replace(' ', "").len() >= 5 {
        for value in &identities {
            let vw = words(value);
            if vw.is_empty() {
                continue;
            }
            let fuzzy = weighted_ratio(&uw, &vw).max(token_set(&uw, &vw, false));
            if fuzzy > best as f64 {
                best = fuzzy.round_ties_even() as i64;
                best_value = value;
            }
        }
    }
    if best >= 78 {
        score = score.max((best - 2).min(90));
        reasons.push(format!("用户名词组 {best}% 接近「{best_value}」"));
    }
    if source.provider == "linkedin"
        && target.provider == "wechat"
        && let Some(headline) = source
            .aliases
            .iter()
            .find(|s| s.contains(' ') && s.chars().count() > 12)
    {
        let headline = compact(headline);
        if let Some(value) = identities.iter().find(|s| {
            let c = compact(s);
            c.len() >= 4 && headline.contains(&c)
        }) {
            score = score.max(76);
            reasons.push(format!("职位简介包含「{value}」"));
        }
    }
    if reasons.len() >= 2 && score < 97 {
        score = (score + 3).min(96)
    }
    if nc.len() < 5 && score < 90 {
        score = score.min(72)
    }
    reasons.truncate(3);
    CandidateScore { score, reasons }
}
pub fn confidence(score: i64) -> &'static str {
    match score {
        95.. => "very_high",
        85.. => "high",
        70.. => "medium",
        55.. => "low",
        _ => "weak",
    }
}
pub fn pair_key(provider: &str, available: &BTreeSet<String>) -> String {
    let preferred = if provider == "wechat" {
        "instagram"
    } else {
        "wechat"
    };
    let target = if available.contains(preferred) {
        Some(preferred)
    } else {
        PROVIDERS
            .into_iter()
            .find(|p| *p != provider && available.contains(*p))
    };
    target.map_or_else(|| provider.into(), |t| format!("{provider}-{t}"))
}
fn label(pair: &str) -> &str {
    PAIRS
        .into_iter()
        .find(|(p, _)| *p == pair)
        .map(|(_, l)| l)
        .unwrap_or(pair)
}
pub fn candidate_keys(a: &AccountSignals) -> BTreeSet<String> {
    let mut keys = a
        .emails
        .iter()
        .map(|s| format!("e:{s}"))
        .chain(a.phones.iter().map(|s| format!("p:{s}")))
        .collect::<BTreeSet<_>>();
    for h in &a.handles {
        let c = compact(h);
        if c.len() >= 4 {
            keys.insert(format!("c:{c}"));
        }
    }
    for value in a.identity_values() {
        for c in [compact(value), pinyin_compact(value)] {
            if c.len() >= 4 {
                keys.insert(format!("c:{c}"));
            }
        }
        for token in words(value)
            .split_whitespace()
            .chain(username_words(value).split_whitespace())
        {
            if token.len() >= 3 {
                keys.insert(format!("t:{token}"));
            }
        }
        let p = syllables(value, true)
            .iter()
            .map(|s| compact(s))
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>();
        for pair in p.windows(2) {
            keys.insert(format!("t:{}{}", pair[0], pair[1]));
        }
    }
    keys
}
#[derive(Default)]
pub struct Snapshot {
    pub accounts: Vec<AccountSignals>,
    pub providers: BTreeMap<String, BTreeSet<String>>,
    pub decisions: BTreeMap<(String, String), String>,
    pub blockers: BTreeMap<String, String>,
    pub signals: BTreeMap<String, Vec<Value>>,
}
fn groups(v: &Value) -> Vec<(String, String)> {
    field_texts(v)
        .join("\n")
        .lines()
        .filter_map(|s| {
            let parts = s.split_once(['｜', '|']);
            let (name, group) = parts
                .map(|(g, n)| (n.trim(), g.trim()))
                .unwrap_or((s.trim(), ""));
            (!name.is_empty()).then(|| (name.into(), group.into()))
        })
        .collect()
}
fn enrich(a: &mut AccountSignals, key: &str, texts: &[String]) {
    a.aliases.extend_from_slice(texts);
    if key.ends_with("username") || key.ends_with("alias") || key.ends_with("profile_url") {
        a.handles.extend(texts.iter().map(|s| s.to_lowercase()));
    }
    if key.contains("email") {
        a.emails.extend(texts.iter().map(|s| s.to_lowercase()));
    }
    if key.contains("phone") || key == "mobile" {
        a.phones.extend(texts.iter().cloned());
    }
}
fn extract_contacts(a: &mut AccountSignals) {
    static EMAIL: OnceLock<regex::Regex> = OnceLock::new();
    static PHONE: OnceLock<regex::Regex> = OnceLock::new();
    let email = EMAIL
        .get_or_init(|| regex::Regex::new(r"(?i)[A-Z0-9._%+\-]+@[A-Z0-9.\-]+\.[A-Z]{2,}").unwrap());
    let phone = PHONE.get_or_init(|| regex::Regex::new(r"\+?\d[\d\-\s]{7,}\d").unwrap());
    let texts = a
        .identity_values()
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    for text in texts {
        a.emails
            .extend(email.find_iter(&text).map(|s| s.as_str().to_lowercase()));
        a.phones
            .extend(phone.find_iter(&text).map(|s| s.as_str().into()));
    }
}
pub async fn load(pool: &PgPool) -> Result<Snapshot> {
    let mut result = Snapshot::default();
    let rows=sqlx::query("SELECT a.id,a.provider,a.external_id,a.display_name,a.username,a.profile_url,l.profile_id,p.display_name AS profile_name,coalesce((SELECT ml.media_id FROM crm.media_links ml WHERE ml.source_account_id=a.id AND ml.role IN ('wechat_avatar_small','wechat_avatar','instagram_avatar','linkedin_avatar') ORDER BY (ml.role='wechat_avatar_small') DESC,ml.media_id LIMIT 1),p.avatar_media_id) AS avatar_media_id FROM crm.source_accounts a LEFT JOIN crm.source_account_links l ON l.account_id=a.id LEFT JOIN crm.directory_profiles p ON p.id=l.profile_id WHERE NOT(a.provider='wechat' AND crm.wechat_is_official(a.external_id)) AND a.object_kind='person' AND (l.profile_id IS NULL OR (p.profile_type='person' AND p.archived_at IS NULL)) ORDER BY a.id").fetch_all(pool).await?;
    for row in rows {
        result.accounts.push(AccountSignals {
            account_id: row.get::<Uuid, _>("id").to_string(),
            provider: row.get("provider"),
            external_id: row.get("external_id"),
            display_name: row
                .get::<Option<String>, _>("display_name")
                .unwrap_or_default(),
            username: row.get::<Option<String>, _>("username").unwrap_or_default(),
            profile_url: row
                .get::<Option<String>, _>("profile_url")
                .unwrap_or_default(),
            profile_id: row
                .get::<Option<Uuid>, _>("profile_id")
                .map(|id| id.to_string()),
            profile_name: row
                .get::<Option<String>, _>("profile_name")
                .unwrap_or_default(),
            avatar_media_id: row
                .get::<Option<Uuid>, _>("avatar_media_id")
                .map(|id| id.to_string()),
            ready: true,
            ..Default::default()
        });
    }
    let mut tx = pool.begin().await?;
    let ids = result
        .accounts
        .iter()
        .filter_map(|a| a.account_id.parse().ok())
        .collect::<Vec<_>>();
    let blocked = crate::linking::link_blockers(&mut tx, &ids).await?;
    tx.commit().await?;
    for account in &mut result.accounts {
        if let Some(reason) = account
            .account_id
            .parse::<Uuid>()
            .ok()
            .and_then(|id| blocked.get(&id))
        {
            account.ready = false;
            result
                .blockers
                .insert(account.account_id.clone(), reason.clone());
        }
    }
    let fields=sqlx::query("SELECT c.profile_id,c.field_key,r.value,p.display_name FROM crm.field_current c JOIN crm.field_revisions r ON r.id=c.revision_id JOIN crm.directory_profiles p ON p.id=c.profile_id WHERE c.field_key=ANY($1)").bind(FIELDS.map(|(k,_)|k).to_vec()).fetch_all(pool).await?;
    let mut shown: BTreeMap<String, BTreeMap<String, Vec<String>>> = BTreeMap::new();
    for row in fields {
        let profile = row.get::<Uuid, _>("profile_id").to_string();
        let key: String = row.get("field_key");
        let value: Value = row.get("value");
        let display: String = row.get("display_name");
        let texts = if key == "wechat.group_nicknames" {
            groups(&value).into_iter().map(|(n, _)| n).collect()
        } else {
            field_texts(&value)
                .into_iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
        };
        for account in result
            .accounts
            .iter_mut()
            .filter(|a| a.profile_id.as_ref() == Some(&profile))
        {
            enrich(account, &key, &texts);
        }
        if let Some((_, label)) = FIELDS
            .iter()
            .find(|(k, label)| *k == key && !label.is_empty())
        {
            let texts = if key == "wechat.group_nicknames" {
                groups(&value)
                    .into_iter()
                    .map(|(n, g)| {
                        if g.is_empty() {
                            n
                        } else {
                            format!("{n}（{g}）")
                        }
                    })
                    .collect()
            } else {
                texts
            };
            let values = shown
                .entry(profile)
                .or_default()
                .entry((*label).into())
                .or_default();
            for text in texts {
                if text.to_lowercase() != display.to_lowercase() && !values.contains(&text) {
                    values.push(text);
                }
            }
        }
    }
    for (profile, labels) in shown {
        result.signals.insert(profile,SIGNAL_ORDER.iter().filter_map(|label|labels.get(*label).filter(|v|!v.is_empty()).map(|v|json!({"label":label,"values":v.iter().take(4).collect::<Vec<_>>(),"more":v.len().saturating_sub(4)}))).collect());
    }
    for a in &mut result.accounts {
        a.handles.insert(
            if a.username.is_empty() {
                &a.external_id
            } else {
                &a.username
            }
            .to_lowercase(),
        );
        let mut seen = BTreeSet::new();
        a.aliases.retain(|s| seen.insert(s.clone()));
        extract_contacts(a);
        if let Some(p) = &a.profile_id {
            result
                .providers
                .entry(p.clone())
                .or_default()
                .insert(a.provider.clone());
        }
    }
    for row in sqlx::query("SELECT account_id,profile_id,status FROM crm.platform_link_decisions")
        .fetch_all(pool)
        .await?
    {
        let status: String = row.get("status");
        result.decisions.insert(
            (
                row.get::<Uuid, _>("account_id").to_string(),
                row.get::<Uuid, _>("profile_id").to_string(),
            ),
            if status == "confirmed" {
                "accepted".into()
            } else {
                status
            },
        );
    }
    Ok(result)
}
/// Inverted identity index avoids the quadratic all-accounts cross-product.
pub fn queue(data: &Snapshot) -> Vec<Value> {
    let keys = data.accounts.iter().map(candidate_keys).collect::<Vec<_>>();
    let mut index: BTreeMap<(&str, &str), Vec<usize>> = BTreeMap::new();
    let mut profiles: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, a) in data.accounts.iter().enumerate() {
        if let Some(p) = &a.profile_id {
            profiles.entry(p).or_default().push(i);
            for key in &keys[i] {
                index.entry((&a.provider, key)).or_default().push(i);
            }
        }
    }
    let empty = BTreeSet::new();
    let mut rows = vec![];
    let mut claimed: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut moves: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    for (i, source) in data.accounts.iter().enumerate() {
        let already = source
            .profile_id
            .as_ref()
            .and_then(|p| data.providers.get(p))
            .unwrap_or(&empty);
        let missing = PROVIDERS
            .into_iter()
            .filter(|p| *p != source.provider && !already.contains(*p))
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        if missing.is_empty() {
            continue;
        }
        let mut nearby: BTreeSet<&usize> = BTreeSet::new();
        for provider in &missing {
            for key in &keys[i] {
                if let Some(bucket) = index
                    .get(&(provider.as_str(), key.as_str()))
                    .filter(|b| b.len() <= 200)
                {
                    nearby.extend(bucket);
                }
            }
        }
        for ((account, profile), _) in data
            .decisions
            .iter()
            .filter(|((a, _), _)| *a == source.account_id)
        {
            let _ = account;
            if let Some(positions) = profiles.get(profile.as_str()) {
                nearby.extend(
                    positions
                        .iter()
                        .filter(|&&i| missing.contains(&data.accounts[i].provider)),
                );
            }
        }
        let mut seen = BTreeSet::new();
        let mut candidates = Vec::new();
        for position in nearby {
            let other = &data.accounts[*position];
            let Some(profile) = &other.profile_id else {
                continue;
            };
            let present = data.providers.get(profile).unwrap_or(&empty);
            if other.account_id == source.account_id
                || Some(profile) == source.profile_id.as_ref()
                || !seen.insert(profile)
                || present.contains(&source.provider)
            {
                continue;
            }
            let status = data
                .decisions
                .get(&(source.account_id.clone(), profile.clone()))
                .map(String::as_str)
                .unwrap_or("pending");
            let scored = score_pair(source, other);
            if scored.score < 55 && status != "accepted" {
                continue;
            }
            let pair = pair_key(&source.provider, present);
            candidates.push(json!({"profile_id":profile,"display_name":if other.profile_name.is_empty(){&other.display_name}else{&other.profile_name},"providers":present,"username":if other.username.is_empty(){&other.external_id}else{&other.username},"score":scored.score,"confidence":confidence(scored.score),"reasons":scored.reasons,"pair":pair,"pair_label":label(&pair),"ready":source.ready,"status":status,"expected_profile_id":source.profile_id,"avatar_media_id":other.avatar_media_id,"face_score":null,"signals":data.signals.get(profile).cloned().unwrap_or_default()}));
        }
        candidates.sort_by(|a, b| {
            b["score"]
                .as_i64()
                .cmp(&a["score"].as_i64())
                .then_with(|| a["display_name"].as_str().cmp(&b["display_name"].as_str()))
        });
        let selected = ["accepted", "pending", "skipped"]
            .iter()
            .find_map(|s| candidates.iter().find(|c| c["status"] == *s))
            .or_else(|| candidates.first());
        if let Some(c) = selected.filter(|c| c["status"] != "rejected") {
            claimed
                .entry(c["profile_id"].as_str().unwrap().into())
                .or_default()
                .insert(source.account_id.clone());
        }
        let status = match selected.map(|c| c["status"].as_str().unwrap()) {
            Some("accepted") => "matched",
            Some("rejected") => "no_match",
            Some("skipped") => "skipped",
            None if data
                .decisions
                .iter()
                .any(|((a, _), s)| *a == source.account_id && s == "rejected") =>
            {
                "no_match"
            }
            _ => "unresolved",
        };
        if candidates.is_empty() && status == "unresolved" {
            continue;
        }
        let pair = selected
            .map(|c| c["pair"].as_str().unwrap().to_string())
            .unwrap_or_else(|| pair_key(&source.provider, &missing));
        if status == "unresolved"
            && let (Some(c), Some(origin)) = (selected, &source.profile_id)
        {
            moves
                .entry((origin.clone(), c["profile_id"].as_str().unwrap().into()))
                .or_default()
                .push(source.account_id.clone());
        }
        rows.push(json!({"account_id":source.account_id,"provider":source.provider,"provider_label":match source.provider.as_str(){"wechat"=>"微信","instagram"=>"Instagram",_=>"LinkedIn"},"display_name":source.display_name,"username":if source.username.is_empty(){&source.external_id}else{&source.username},"profile_id":source.profile_id,"profile_name":source.profile_name,"ready":source.ready,"blocked_reason":data.blockers.get(&source.account_id),"avatar_media_id":source.avatar_media_id,"pair":pair,"pair_label":label(&pair),"score":selected.map(|c|c["score"].clone()).unwrap_or(json!(0)),"confidence":selected.map(|c|c["confidence"].clone()).unwrap_or(json!("weak")),"status":status,"candidates":candidates.iter().take(8).collect::<Vec<_>>(),"signals":source.profile_id.as_ref().and_then(|p|data.signals.get(p)).cloned().unwrap_or_default()}));
    }
    let keeps = |p: &str| {
        let present = data.providers.get(p).unwrap_or(&empty);
        (present.contains("wechat"), present.len(), p.to_owned())
    };
    let remove = moves
        .iter()
        .filter(|((a, b), _)| moves.contains_key(&(b.clone(), a.clone())) && keeps(a) > keeps(b))
        .flat_map(|(_, ids)| ids)
        .collect::<BTreeSet<_>>();
    rows.retain(|r| !remove.contains(&r["account_id"].as_str().unwrap().to_string()));
    for row in &mut rows {
        row["conflict"] = json!(
            row["candidates"][0]["profile_id"]
                .as_str()
                .and_then(|p| claimed.get(p))
                .is_some_and(|ids| ids
                    .iter()
                    .any(|id| Some(id.as_str()) != row["account_id"].as_str()))
        );
    }
    rows
}
