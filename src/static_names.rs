//! The station naming ladder for `stops.txt`.
//!
//! [`resolve_names`] is pure and deterministic: the same static names and the same
//! [`StationNames`] always give the same [`ResolvedName`]s, and the resolved `stop_name`s are
//! unique. The ladder runs on every build, so a station Amtrak renames or drops between two
//! renamings can never leave a duplicate. [`apply_names`] rewrites only plain stops (no
//! `location_type` or `0`, no `parent_station`) and returns the digest that
//! [`names_version_suffix`] turns into the `+names.{digest}` version suffix.

use crate::static_augment::{read_stops, write_stops, AugmentError};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Marks the names digest inside a static version, after `+tracks.{digest}`.
pub const NAMES_VERSION_MARKER: &str = "+names.";

/// What Amtrak's station status document says about one station.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SeenStation {
    /// The document's `name`, "City, ST".
    pub name: Option<String>,
    /// The board name (`displayName`).
    pub display_name: Option<String>,
    /// The document's facility text.
    pub facility: Option<String>,
    /// The spoken name (`audioName`).
    pub audio_name: Option<String>,
}

/// What Amtrak's station directory says about one station.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DirectoryStation {
    /// "City, ST".
    pub name: String,
    /// The city alone.
    pub city: String,
    /// `facilityName64`.
    pub facility: Option<String>,
}

/// Every name input, keyed by station code.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StationNames {
    /// The station directory.
    pub directory: BTreeMap<String, DirectoryStation>,
    /// Stations seen in status documents.
    pub seen: BTreeMap<String, SeenStation>,
}

/// The name fields resolved for one plain stop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedName {
    /// The unique `stop_name`.
    pub stop_name: String,
    /// The `stop_desc`, empty when it would equal `stop_name`.
    pub stop_desc: String,
    /// The `tts_stop_name`, empty when there is no audio name.
    pub tts_stop_name: String,
}

fn opt_str(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<Option<String>, String> {
    match object.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(format!("{key} is not a string")),
    }
}

fn req_str(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<String, String> {
    opt_str(object, key)?.ok_or_else(|| format!("{key} is missing"))
}

fn section<'a>(
    value: &'a serde_json::Value,
    key: &str,
) -> Result<&'a serde_json::Map<String, serde_json::Value>, String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| format!("{key} is not an object"))
}

impl StationNames {
    /// The persisted JSON form: `{"directory": {CODE: {name, city, facility?}}, "seen": {CODE:
    /// {name?, display_name?, facility?, audio_name?}}}`. Absent optional fields are omitted.
    pub fn to_json(&self) -> serde_json::Value {
        use serde_json::{json, Map, Value};
        let mut directory = Map::new();
        for (code, d) in &self.directory {
            let mut entry = Map::new();
            entry.insert("name".into(), json!(d.name));
            entry.insert("city".into(), json!(d.city));
            if let Some(facility) = &d.facility {
                entry.insert("facility".into(), json!(facility));
            }
            directory.insert(code.clone(), Value::Object(entry));
        }
        let mut seen = Map::new();
        for (code, s) in &self.seen {
            let mut entry = Map::new();
            for (key, value) in [
                ("name", &s.name),
                ("display_name", &s.display_name),
                ("facility", &s.facility),
                ("audio_name", &s.audio_name),
            ] {
                if let Some(value) = value {
                    entry.insert(key.into(), json!(value));
                }
            }
            seen.insert(code.clone(), Value::Object(entry));
        }
        json!({ "directory": directory, "seen": seen })
    }

    /// Parses the form [`StationNames::to_json`] writes.
    ///
    /// # Errors
    ///
    /// Returns a message naming the first malformed part: a non-object input, a missing section,
    /// or a field of the wrong type.
    pub fn from_json(value: &serde_json::Value) -> Result<StationNames, String> {
        let mut names = StationNames::default();
        for (code, entry) in section(value, "directory")? {
            let object = entry
                .as_object()
                .ok_or_else(|| format!("directory {code} is not an object"))?;
            names.directory.insert(
                code.clone(),
                DirectoryStation {
                    name: req_str(object, "name")?,
                    city: req_str(object, "city")?,
                    facility: opt_str(object, "facility")?,
                },
            );
        }
        for (code, entry) in section(value, "seen")? {
            let object = entry
                .as_object()
                .ok_or_else(|| format!("seen {code} is not an object"))?;
            names.seen.insert(
                code.clone(),
                SeenStation {
                    name: opt_str(object, "name")?,
                    display_name: opt_str(object, "display_name")?,
                    facility: opt_str(object, "facility")?,
                    audio_name: opt_str(object, "audio_name")?,
                },
            );
        }
        Ok(names)
    }
}

fn non_empty(value: &Option<String>) -> Option<&str> {
    value.as_deref().map(str::trim).filter(|v| !v.is_empty())
}

fn city_part(name: &str) -> &str {
    name.split(',').next().unwrap_or(name).trim()
}

struct Work {
    name: String,
    city_key: String,
    city_state: Option<String>,
    /// `facilityName64` only: the facility step (b) adds to a name.
    name_facility: Option<String>,
    /// `facilityName64`, else the seen facility: the facility `stop_desc` shows.
    desc_facility: Option<String>,
}

fn duplicate_groups(work: &BTreeMap<String, Work>) -> Vec<Vec<String>> {
    let mut by_name: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for (code, item) in work {
        by_name.entry(&item.name).or_default().push(code.clone());
    }
    by_name.into_values().filter(|g| g.len() > 1).collect()
}

fn is_bare(item: &Work) -> bool {
    let lower = item.name.to_lowercase();
    lower == item.city_key
        || item
            .city_state
            .as_ref()
            .is_some_and(|cs| cs.to_lowercase() == lower)
}

/// Step (a): bare names that share a city take their "City, ST" form.
fn city_collisions(work: &mut BTreeMap<String, Work>) -> bool {
    let mut by_city: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (code, item) in work.iter() {
        if !item.city_key.is_empty() && is_bare(item) {
            by_city
                .entry(item.city_key.clone())
                .or_default()
                .push(code.clone());
        }
    }
    let mut changed = false;
    for code in by_city.into_values().filter(|g| g.len() > 1).flatten() {
        let item = work.get_mut(&code).expect("code came from work");
        if let Some(city_state) = item.city_state.clone() {
            if item.name != city_state {
                item.name = city_state;
                changed = true;
            }
        }
    }
    changed
}

/// Step (b): exact collisions add the facility.
fn exact_collisions(work: &mut BTreeMap<String, Work>) -> bool {
    let mut changed = false;
    for code in duplicate_groups(work).into_iter().flatten() {
        let item = work.get_mut(&code).expect("code came from work");
        if let Some(facility) = item.name_facility.clone() {
            let suffix = format!(" – {facility}");
            if !item.name.ends_with(&suffix) {
                item.name.push_str(&suffix);
                changed = true;
            }
        }
    }
    changed
}

/// Step (c): anything still equal gets the station code.
fn last_resort(work: &mut BTreeMap<String, Work>) -> bool {
    let mut changed = false;
    for code in duplicate_groups(work).into_iter().flatten() {
        let item = work.get_mut(&code).expect("code came from work");
        let suffix = format!(" ({code})");
        if !item.name.ends_with(&suffix) {
            item.name.push_str(&suffix);
            changed = true;
        }
    }
    changed
}

/// Resolves the name fields of every code in `static_names` (code to today's static name).
///
/// The `stop_name` base is the first available of the seen display name, the seen `name`, the
/// directory `name`, and the static name. Disambiguation then repeats the city, facility and code
/// steps until nothing changes, so the `stop_name`s are unique.
pub fn resolve_names(
    static_names: &BTreeMap<String, String>,
    inputs: &StationNames,
) -> BTreeMap<String, ResolvedName> {
    let mut work: BTreeMap<String, Work> = BTreeMap::new();
    for (code, static_name) in static_names {
        let seen = inputs.seen.get(code);
        let dir = inputs.directory.get(code);
        let seen_name = seen.and_then(|s| non_empty(&s.name));
        let dir_name = dir.map(|d| d.name.trim()).filter(|n| !n.is_empty());
        let city_state = seen_name.or(dir_name).map(str::to_string);
        let name = seen
            .and_then(|s| non_empty(&s.display_name))
            .or(seen_name)
            .or(dir_name)
            .unwrap_or(static_name.as_str())
            .to_string();
        let city_key = match dir.map(|d| d.city.trim()).filter(|c| !c.is_empty()) {
            Some(city) => city.to_lowercase(),
            None => city_part(&name).to_lowercase(),
        };
        let name_facility = dir.and_then(|d| non_empty(&d.facility)).map(str::to_string);
        let desc_facility = name_facility.clone().or_else(|| {
            seen.and_then(|s| non_empty(&s.facility))
                .map(str::to_string)
        });
        work.insert(
            code.clone(),
            Work {
                name,
                city_key,
                city_state,
                name_facility,
                desc_facility,
            },
        );
    }
    loop {
        let a = city_collisions(&mut work);
        let b = exact_collisions(&mut work);
        let c = last_resort(&mut work);
        if !(a || b || c) {
            break;
        }
    }
    work.into_iter()
        .map(|(code, item)| {
            let seen = inputs.seen.get(&code);
            let dir = inputs.directory.get(&code);
            let stop_desc = describe(&item, dir);
            let stop_desc = if stop_desc == item.name {
                String::new()
            } else {
                stop_desc
            };
            let tts_stop_name = seen
                .and_then(|s| non_empty(&s.audio_name))
                .unwrap_or_default()
                .to_string();
            (
                code,
                ResolvedName {
                    stop_name: item.name,
                    stop_desc,
                    tts_stop_name,
                },
            )
        })
        .collect()
}

fn describe(item: &Work, dir: Option<&DirectoryStation>) -> String {
    let Some(station) = item.city_state.as_deref() else {
        return String::new();
    };
    let mut desc = station.to_string();
    if let Some(facility) = &item.desc_facility {
        desc.push_str(" – ");
        desc.push_str(facility);
    }
    if let Some(dir) = dir {
        let city = dir.city.trim();
        if !city.is_empty()
            && !dir.name.trim().is_empty()
            && city.to_lowercase() != city_part(station).to_lowercase()
        {
            desc.push_str(&format!(" (in {})", dir.name.trim()));
        }
    }
    desc
}

/// The first 8 hex digits of the SHA-256 over the resolved output, sorted by `stop_id`.
pub fn names_digest(resolved: &BTreeMap<String, ResolvedName>) -> String {
    let mut hasher = Sha256::new();
    for (code, name) in resolved {
        for field in [code, &name.stop_name, &name.stop_desc, &name.tts_stop_name] {
            hasher.update(field.as_bytes());
            hasher.update([0]);
        }
        hasher.update(b"\n");
    }
    hasher
        .finalize()
        .iter()
        .take(4)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The version suffix for a digest: `+names.{digest}`.
pub fn names_version_suffix(digest: &str) -> String {
    format!("{NAMES_VERSION_MARKER}{digest}")
}

/// Renames the plain stops of `stops_csv` and returns the new bytes and the names digest.
///
/// Adds the `stop_desc` and `tts_stop_name` columns when absent. Rows with a `location_type` other
/// than empty or `0`, or with a `parent_station`, are left as they are.
///
/// # Errors
///
/// Returns [`AugmentError`] when `stops.txt` can't be parsed or has no `stop_id` or `stop_name`.
pub fn apply_names(
    stops_csv: &[u8],
    inputs: &StationNames,
) -> Result<(Vec<u8>, String), AugmentError> {
    let (headers, mut rows) = read_stops(stops_csv, &["stop_desc", "tts_stop_name"])?;
    let column = |name: &str| headers.iter().position(|header| header == name);
    let id = column("stop_id").ok_or_else(|| AugmentError("stops.txt has no stop_id".into()))?;
    let name =
        column("stop_name").ok_or_else(|| AugmentError("stops.txt has no stop_name".into()))?;
    let desc = column("stop_desc").unwrap_or_default();
    let tts = column("tts_stop_name").unwrap_or_default();
    let location = column("location_type");
    let parent = column("parent_station");

    let plain: Vec<usize> = (0..rows.len())
        .filter(|&index| {
            let row = &rows[index];
            location.is_none_or(|l| matches!(row[l].trim(), "" | "0"))
                && parent.is_none_or(|p| row[p].trim().is_empty())
        })
        .collect();
    let static_names: BTreeMap<String, String> = plain
        .iter()
        .map(|&index| (rows[index][id].clone(), rows[index][name].clone()))
        .collect();
    let resolved = resolve_names(&static_names, inputs);
    for index in plain {
        if let Some(found) = resolved.get(&rows[index][id].clone()) {
            rows[index][name].clone_from(&found.stop_name);
            rows[index][desc].clone_from(&found.stop_desc);
            rows[index][tts].clone_from(&found.tts_stop_name);
        }
    }

    let bytes = write_stops(&headers, &rows)?;
    Ok((bytes, names_digest(&resolved)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seen(name: Option<&str>, display: Option<&str>) -> SeenStation {
        SeenStation {
            name: name.map(str::to_string),
            display_name: display.map(str::to_string),
            ..SeenStation::default()
        }
    }

    fn dir(name: &str, city: &str, facility: Option<&str>) -> DirectoryStation {
        DirectoryStation {
            name: name.into(),
            city: city.into(),
            facility: facility.map(str::to_string),
        }
    }

    fn inputs(seen: &[(&str, SeenStation)], dir: &[(&str, DirectoryStation)]) -> StationNames {
        StationNames {
            seen: seen
                .iter()
                .map(|(c, s)| (c.to_string(), s.clone()))
                .collect(),
            directory: dir
                .iter()
                .map(|(c, d)| (c.to_string(), d.clone()))
                .collect(),
        }
    }

    fn statics(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(c, n)| (c.to_string(), n.to_string()))
            .collect()
    }

    fn names(resolved: &BTreeMap<String, ResolvedName>) -> Vec<(&str, &str)> {
        resolved
            .iter()
            .map(|(c, r)| (c.as_str(), r.stop_name.as_str()))
            .collect()
    }

    #[test]
    fn board_names_kept_when_distinct() {
        let i = inputs(
            &[
                ("BOS", seen(None, Some("Boston"))),
                ("BBY", seen(None, Some("Boston Back Bay"))),
                ("BON", seen(None, Some("Boston North Station"))),
            ],
            &[
                ("BOS", dir("Boston, MA", "Boston", None)),
                ("BBY", dir("Boston, MA", "Boston", None)),
                ("BON", dir("Boston, MA", "Boston", None)),
            ],
        );
        let s = statics(&[("BOS", "Boston"), ("BBY", "Boston"), ("BON", "Boston")]);
        assert_eq!(
            names(&resolve_names(&s, &i)),
            [
                ("BBY", "Boston Back Bay"),
                ("BON", "Boston North Station"),
                ("BOS", "Boston")
            ]
        );
    }

    #[test]
    fn city_collision_adds_state_to_bare_names() {
        let i = inputs(
            &[("SPG", seen(None, Some("Springfield")))],
            &[
                ("SPG", dir("Springfield, MA", "Springfield", None)),
                ("SPI", dir("Springfield, IL", "Springfield", None)),
            ],
        );
        let s = statics(&[("SPG", "Springfield"), ("SPI", "Springfield")]);
        assert_eq!(
            names(&resolve_names(&s, &i)),
            [("SPG", "Springfield, MA"), ("SPI", "Springfield, IL")]
        );
    }

    #[test]
    fn distinct_board_name_survives_city_collision() {
        let i = inputs(
            &[
                ("EWR", seen(None, Some("Newark Airport"))),
                ("NWK", seen(None, Some("Newark, NJ"))),
                ("NRK", seen(None, Some("Newark, DE"))),
            ],
            &[
                ("EWR", dir("Newark, NJ", "Newark", None)),
                ("NWK", dir("Newark, NJ", "Newark", None)),
                ("NRK", dir("Newark, DE", "Newark", None)),
            ],
        );
        let s = statics(&[("EWR", "Newark"), ("NWK", "Newark"), ("NRK", "Newark")]);
        assert_eq!(
            names(&resolve_names(&s, &i)),
            [
                ("EWR", "Newark Airport"),
                ("NRK", "Newark, DE"),
                ("NWK", "Newark, NJ")
            ]
        );
    }

    #[test]
    fn exact_collision_adds_facility() {
        let i = inputs(
            &[
                ("LAK", seen(None, Some("Lakeland"))),
                ("LKL", seen(None, Some("Lakeland"))),
            ],
            &[
                (
                    "LAK",
                    dir("Lakeland, FL", "Lakeland", Some("To/from points north")),
                ),
                (
                    "LKL",
                    dir("Lakeland, FL", "Lakeland", Some("To/from points south")),
                ),
            ],
        );
        let s = statics(&[("LAK", "Lakeland"), ("LKL", "Lakeland")]);
        assert_eq!(
            names(&resolve_names(&s, &i)),
            [
                ("LAK", "Lakeland, FL – To/from points north"),
                ("LKL", "Lakeland, FL – To/from points south")
            ]
        );
    }

    #[test]
    fn last_resort_adds_code() {
        let i = inputs(
            &[],
            &[
                ("NRG", dir("Los Angeles, CA", "Los Angeles", None)),
                ("WES", dir("Los Angeles, CA", "Los Angeles", None)),
            ],
        );
        let s = statics(&[("NRG", "Los Angeles"), ("WES", "Los Angeles")]);
        assert_eq!(
            names(&resolve_names(&s, &i)),
            [
                ("NRG", "Los Angeles, CA (NRG)"),
                ("WES", "Los Angeles, CA (WES)")
            ]
        );
    }

    #[test]
    fn stop_without_any_source_keeps_static_name() {
        let s = statics(&[("CBN", "Canadian Border New York")]);
        let resolved = resolve_names(&s, &StationNames::default());
        assert_eq!(resolved["CBN"].stop_name, "Canadian Border New York");
        assert_eq!(resolved["CBN"].stop_desc, "");
    }

    #[test]
    fn stop_desc_details() {
        let i = inputs(
            &[
                ("RTE", seen(Some("Route 128, MA"), Some("Route 128"))),
                ("NYP", seen(Some("New York, NY"), None)),
            ],
            &[
                ("RTE", dir("Westwood, MA", "Westwood", None)),
                (
                    "NYP",
                    dir(
                        "New York, NY",
                        "New York",
                        Some("Moynihan Train Hall at Penn Station"),
                    ),
                ),
                ("SPI", dir("Springfield, IL", "Springfield", None)),
            ],
        );
        let s = statics(&[
            ("RTE", "Route 128"),
            ("NYP", "NY Penn"),
            ("SPI", "Springfield"),
        ]);
        let r = resolve_names(&s, &i);
        assert_eq!(r["RTE"].stop_desc, "Route 128, MA (in Westwood, MA)");
        assert_eq!(
            r["NYP"].stop_desc,
            "New York, NY – Moynihan Train Hall at Penn Station"
        );
        assert_eq!(r["SPI"].stop_name, "Springfield, IL");
        assert_eq!(r["SPI"].stop_desc, "");
    }

    #[test]
    fn tts_from_audio_name() {
        let mut nwk = seen(None, Some("Newark, NJ"));
        nwk.audio_name = Some("Newark New Jersey".into());
        let i = inputs(&[("NWK", nwk), ("BOS", seen(None, Some("Boston")))], &[]);
        let s = statics(&[("NWK", "Newark"), ("BOS", "Boston")]);
        let r = resolve_names(&s, &i);
        assert_eq!(r["NWK"].tts_stop_name, "Newark New Jersey");
        assert_eq!(r["BOS"].tts_stop_name, "");
    }

    #[test]
    fn ladder_never_leaves_duplicates_for_any_input() {
        let codes: Vec<String> = (0..50).map(|n| format!("S{n:02}")).collect();
        let s: BTreeMap<String, String> =
            codes.iter().map(|c| (c.clone(), "Same".into())).collect();
        let mut i = StationNames::default();
        for c in &codes {
            i.seen.insert(c.clone(), seen(None, Some("Same")));
            i.directory
                .insert(c.clone(), dir("Same, ST", "Same", Some("Platform")));
        }
        let resolved = resolve_names(&s, &i);
        let distinct: std::collections::BTreeSet<_> =
            resolved.values().map(|r| r.stop_name.clone()).collect();
        assert_eq!(resolved.len(), 50);
        assert_eq!(distinct.len(), 50);
    }

    #[test]
    fn digest_is_deterministic_and_output_only() {
        let s = statics(&[("BOS", "Boston"), ("NWK", "Newark")]);
        let mut i = inputs(&[("BOS", seen(None, Some("Boston")))], &[]);
        let digest = names_digest(&resolve_names(&s, &i));
        assert_eq!(digest.len(), 8);
        assert!(digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
        assert_eq!(digest, names_digest(&resolve_names(&s, &i)));
        i.directory
            .insert("ZZZ".into(), dir("Nowhere, NA", "Nowhere", None));
        assert_eq!(digest, names_digest(&resolve_names(&s, &i)));
        assert_eq!(names_version_suffix(&digest), format!("+names.{digest}"));
    }

    #[test]
    fn apply_names_adds_columns_and_skips_generated_rows() {
        let csv = "\u{feff}stop_id,stop_name,location_type,parent_station\n\
BOS,Boston South,,\n\
BOS:station,Boston South,1,\n\
BOS:track:1,Boston South Track 1,0,BOS:station\n";
        let i = inputs(&[("BOS", seen(None, Some("Boston")))], &[]);
        let (bytes, digest) = apply_names(csv.as_bytes(), &i).unwrap();
        assert_eq!(digest.len(), 8);
        let text = String::from_utf8(bytes).unwrap();
        assert_eq!(
            text,
            "stop_id,stop_name,location_type,parent_station,stop_desc,tts_stop_name\n\
BOS,Boston,,,,\n\
BOS:station,Boston South,1,,,\n\
BOS:track:1,Boston South Track 1,0,BOS:station,,\n"
        );
    }

    #[test]
    fn station_names_json_round_trip() {
        let mut full = seen(Some("Newark, NJ"), Some("Newark"));
        full.facility = Some("Concourse".into());
        full.audio_name = Some("Newark New Jersey".into());
        let x = inputs(
            &[("NWK", full), ("BOS", seen(None, None))],
            &[
                ("NWK", dir("Newark, NJ", "Newark", Some("Penn"))),
                ("BOS", dir("Boston, MA", "Boston", None)),
            ],
        );
        assert_eq!(StationNames::from_json(&x.to_json()), Ok(x));
        assert!(StationNames::from_json(&serde_json::json!([])).is_err());
    }

    #[test]
    fn seen_facility_only_feeds_stop_desc_not_stop_name() {
        let mut a = seen(None, Some("Lakeland"));
        a.facility = Some("North side".into());
        let mut b = seen(None, Some("Lakeland"));
        b.facility = Some("South side".into());
        let i = inputs(
            &[("LAK", a), ("LKL", b)],
            &[
                ("LAK", dir("Lakeland, FL", "Lakeland", None)),
                ("LKL", dir("Lakeland, FL", "Lakeland", None)),
            ],
        );
        let s = statics(&[("LAK", "Lakeland"), ("LKL", "Lakeland")]);
        let r = resolve_names(&s, &i);
        assert_eq!(r["LAK"].stop_name, "Lakeland, FL (LAK)");
        assert_eq!(r["LKL"].stop_name, "Lakeland, FL (LKL)");
        assert_eq!(r["LAK"].stop_desc, "Lakeland, FL – North side");
        assert_eq!(r["LKL"].stop_desc, "Lakeland, FL – South side");
    }
}
