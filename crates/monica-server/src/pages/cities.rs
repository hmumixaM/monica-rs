//! GeoNames attribution and exact data hashes are alongside the embedded tables.
use monica_core::pages::Coordinates;
use std::{collections::BTreeMap, sync::OnceLock};
use unicode_normalization::UnicodeNormalization;
struct City {
    name: &'static str,
    country: &'static str,
    state: &'static str,
    lat: f64,
    lon: f64,
}
struct Reference {
    cities: BTreeMap<u32, City>,
    names: Vec<(&'static str, &'static str)>,
    countries: BTreeMap<&'static str, &'static str>,
    states: BTreeMap<&'static str, &'static str>,
}
static REFERENCE: OnceLock<Reference> = OnceLock::new();
fn normalized(s: &str) -> String {
    s.nfkc()
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('ß', "ss")
        .replace('ς', "σ")
}
fn pairs(s: &'static str) -> impl Iterator<Item = (&'static str, &'static str)> {
    s.lines().filter_map(|l| l.split_once('\t'))
}
fn reference() -> &'static Reference {
    REFERENCE.get_or_init(|| {
        let cities = include_str!("geonames/cities.tsv")
            .lines()
            .map(|line| {
                let v: Vec<_> = line.split('\t').collect();
                (
                    v[0].parse().unwrap(),
                    City {
                        name: v[1],
                        country: v[2],
                        state: v[3],
                        lat: v[4].parse().unwrap(),
                        lon: v[5].parse().unwrap(),
                    },
                )
            })
            .collect();
        Reference {
            cities,
            names: pairs(include_str!("geonames/names.tsv")).collect(),
            countries: pairs(include_str!("geonames/countries.tsv")).collect(),
            states: pairs(include_str!("geonames/states.tsv")).collect(),
        }
    })
}
pub fn resolve(label: &str, country: &str, state: &str) -> Option<Coordinates> {
    let r = reference();
    let parts: Vec<String> = label
        .split([',', '，', '·'])
        .map(normalized)
        .filter(|s| !s.is_empty())
        .collect();
    let country = r
        .countries
        .get(normalized(country).as_str())
        .copied()
        .or_else(|| {
            parts
                .iter()
                .rev()
                .find_map(|p| r.countries.get(p.as_str()).copied())
        });
    let state = r
        .states
        .get(normalized(state).as_str())
        .copied()
        .or_else(|| parts.iter().find_map(|p| r.states.get(p.as_str()).copied()));
    let country = country.or(state.map(|_| "US"));
    for part in parts {
        let locate = |key: &str| r.names.binary_search_by_key(&key, |p| p.0).ok();
        let i = locate(&part).or_else(|| part.strip_suffix('市').and_then(locate));
        let Some(i) = i else { continue };
        let matches: Vec<_> = r.names[i]
            .1
            .split(',')
            .filter_map(|id| r.cities.get(&id.parse().ok()?))
            .filter(|c| {
                country.is_none_or(|v| c.country == v) && state.is_none_or(|v| c.state == v)
            })
            .collect();
        if matches.len() == 1 {
            let c = matches[0];
            return Some(Coordinates {
                label: format!("{} · {}", c.name, c.country),
                latitude: c.lat,
                longitude: c.lon,
                precision: "city".into(),
            });
        }
        if !matches.is_empty() {
            return None;
        }
    }
    None
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_city_names_are_conservative() {
        assert!(resolve("Springfield", "US", "").is_none());
        let c = resolve("Chicago, Illinois, United States", "", "").unwrap();
        assert!((c.latitude - 41.85).abs() < 0.2);
        assert!(resolve("火星城市不存在", "", "").is_none());
        assert!(resolve("上海市", "中国", "").is_some());
    }
}
