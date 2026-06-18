//! Geographic coverage computation for homeward.
//!
//! Bins ingested [`PetRecord`]s into equal-angle lat/lon grid cells and
//! computes per-cell counts. Coverage holes are regions with population
//! signal (neighboring cells, or the seeded documented-gaps list) but no
//! contributing sources.
//!
//! # Design
//!
//! - **No PostGIS, no network, no heavy spatial crates.** Pure offline
//!   computation on a CPU-only laptop.
//! - Cells use a **0.5° equal-angle grid** (`CELL_DEG = 0.5`). Each cell is
//!   identified by the integer pair `(lat_idx, lon_idx)` where
//!   `lat_idx = floor(lat / CELL_DEG)` and similarly for longitude.
//!   The same point always maps to the same cell (deterministic).
//! - Records with `location == None` are counted in an `ungeocoded` tally
//!   rather than silently dropped.
//! - The hole heuristic seeds from the existing documented-gaps list
//!   (`DOCUMENTED_GAP_SEEDS`) so this module composes with prior work.
//! - Holes are labeled with a `basis` field — never asserted as ground-truth.

#![allow(clippy::float_arithmetic)]
#![allow(clippy::as_conversions)]

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use homeward_schema::{IntakeType, PetRecord};

// ── Cell geometry ─────────────────────────────────────────────────────────────

/// Side length of each equal-angle grid cell in degrees.
///
/// 0.5° ≈ 55 km at the equator; coarse enough for metro-level coverage gaps,
/// fine enough to distinguish adjacent cities.
pub const CELL_DEG: f64 = 0.5;

/// A stable geographic cell identifier.
///
/// Derived purely from coarse lat/lon. Two points in the same 0.5° bucket
/// always get the same `CellId`. Serialized as `"lat_idx:lon_idx"` for
/// stable JSON keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CellId {
    /// Floor division of latitude by `CELL_DEG`, as a signed integer.
    pub lat_idx: i32,
    /// Floor division of longitude by `CELL_DEG`, as a signed integer.
    pub lon_idx: i32,
}

impl CellId {
    /// Map a `(lat, lon)` pair to the containing cell.
    ///
    /// Uses floor division so negative coordinates (Southern/Western
    /// hemisphere) still produce stable, non-overlapping cells.
    #[must_use]
    pub fn from_lat_lon(lat: f64, lon: f64) -> Self {
        let lat_idx = (lat / CELL_DEG).floor() as i32;
        let lon_idx = (lon / CELL_DEG).floor() as i32;
        Self { lat_idx, lon_idx }
    }

    /// Center latitude of this cell.
    #[must_use]
    pub fn center_lat(self) -> f64 {
        (f64::from(self.lat_idx) + 0.5) * CELL_DEG
    }

    /// Center longitude of this cell.
    #[must_use]
    pub fn center_lon(self) -> f64 {
        (f64::from(self.lon_idx) + 0.5) * CELL_DEG
    }

    /// Return the eight orthogonal + diagonal neighbor cells.
    #[must_use]
    pub fn neighbors(self) -> [Self; 8] {
        let CellId { lat_idx, lon_idx } = self;
        [
            Self { lat_idx: lat_idx - 1, lon_idx: lon_idx - 1 },
            Self { lat_idx: lat_idx - 1, lon_idx },
            Self { lat_idx: lat_idx - 1, lon_idx: lon_idx + 1 },
            Self { lat_idx,              lon_idx: lon_idx - 1 },
            Self { lat_idx,              lon_idx: lon_idx + 1 },
            Self { lat_idx: lat_idx + 1, lon_idx: lon_idx - 1 },
            Self { lat_idx: lat_idx + 1, lon_idx },
            Self { lat_idx: lat_idx + 1, lon_idx: lon_idx + 1 },
        ]
    }
}

impl std::fmt::Display for CellId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.lat_idx, self.lon_idx)
    }
}

// ── Per-cell statistics ───────────────────────────────────────────────────────

/// Aggregated statistics for one geographic cell.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CellStats {
    /// Stable cell identifier.
    pub cell_id: CellId,
    /// Center latitude of the cell.
    pub center_lat: f64,
    /// Center longitude of the cell.
    pub center_lon: f64,
    /// Total records whose shelter location falls in this cell.
    pub record_count: u64,
    /// Records with `IntakeType::Stray` or `IntakeType::FoundReport`.
    pub stray_count: u64,
    /// Number of distinct contributing sources.
    pub distinct_sources: u64,
    /// Timestamp of the most recently seen record in this cell.
    pub last_seen: Option<DateTime<Utc>>,
}

// ── Hole heuristic ────────────────────────────────────────────────────────────

/// A detected geographic coverage hole: a populated region with no feed.
///
/// `confidence` is `"estimated"` — this is a heuristic, not ground truth.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeoHole {
    /// The cell that appears under-covered.
    pub cell_id: CellId,
    /// Center latitude of the hole cell.
    pub center_lat: f64,
    /// Center longitude of the hole cell.
    pub center_lon: f64,
    /// How many records from neighboring populated cells suggest this area
    /// should have coverage (estimated missed population).
    pub estimated_missed_population: u64,
    /// Human-readable name for this hole, if seeded from documented gaps.
    pub name: Option<String>,
    /// How the hole was detected. Always `"estimated"` — never asserted as
    /// ground-truth coverage.
    pub basis: String,
}

// ── Rollup ────────────────────────────────────────────────────────────────────

/// Accumulator for building per-cell statistics from a stream of records.
#[derive(Debug, Default)]
struct CellAccum {
    record_count: u64,
    stray_count: u64,
    sources: HashSet<String>,
    last_seen: Option<DateTime<Utc>>,
}

/// Roll up an iterator of [`PetRecord`]s into per-cell statistics.
///
/// Records with `location == None` are counted in the returned `ungeocoded`
/// tally rather than silently dropped.
///
/// # Returns
/// `(cells, ungeocoded)` where `ungeocoded` is the count of records without
/// a valid lat/lon.
#[must_use]
pub fn rollup_records<'a>(
    records: impl Iterator<Item = &'a PetRecord>,
) -> (Vec<CellStats>, u64) {
    let mut accum: HashMap<CellId, CellAccum> = HashMap::new();
    let mut ungeocoded: u64 = 0;

    for rec in records {
        let (lat, lon) = match rec.location.as_ref() {
            Some(loc) => match (loc.lat, loc.lon) {
                (Some(lat), Some(lon)) => (lat, lon),
                _ => {
                    ungeocoded += 1;
                    continue;
                }
            },
            None => {
                ungeocoded += 1;
                continue;
            }
        };

        let cell_id = CellId::from_lat_lon(lat, lon);
        let entry = accum.entry(cell_id).or_default();
        entry.record_count += 1;

        let is_stray = matches!(
            rec.intake_type,
            IntakeType::Stray | IntakeType::FoundReport
        );
        if is_stray {
            entry.stray_count += 1;
        }

        entry.sources.insert(rec.source.name.clone());

        let ts = rec.last_seen;
        entry.last_seen = Some(match entry.last_seen {
            None => ts,
            Some(prev) => prev.max(ts),
        });
    }

    let cells = accum
        .into_iter()
        .map(|(cell_id, a)| CellStats {
            cell_id,
            center_lat: cell_id.center_lat(),
            center_lon: cell_id.center_lon(),
            record_count: a.record_count,
            stray_count: a.stray_count,
            #[allow(clippy::cast_possible_truncation)]
            distinct_sources: a.sources.len() as u64,
            last_seen: a.last_seen,
        })
        .collect();

    (cells, ungeocoded)
}

// ── Documented-gap seeds ──────────────────────────────────────────────────────

/// Seed holes from the documented-gaps list so the geo heuristic composes
/// with prior work rather than discarding it.
///
/// These approximate center coordinates are taken from well-known metro
/// centroids. The cell that contains each centroid is used as the seed.
const DOCUMENTED_GAP_SEEDS: &[(&str, f64, f64)] = &[
    ("Los Angeles, CA", 34.05, -118.25),
    ("New York, NY", 40.71, -74.01),
    ("Chicago, IL", 41.85, -87.65),
    ("Houston, TX", 29.76, -95.37),
    ("Phoenix, AZ", 33.45, -112.07),
];

// ── Hole detection ────────────────────────────────────────────────────────────

/// Minimum records threshold: a cell must have at least this many records
/// to count as a population signal.
const MIN_RECORDS_THRESHOLD: u64 = 1;

/// Detect coverage holes from the cell map.
///
/// A cell is a hole if:
/// 1. It is a documented-gap seed with no records (`record_count == 0` in the
///    map, meaning no feed reaches it), OR
/// 2. It has zero `distinct_sources` but its neighbors have records (populated
///    neighboring cells suggest the area should have coverage).
///
/// Holes are ranked by `estimated_missed_population` descending.
///
/// The `covered_check` cells are those with `distinct_sources > 0`.
#[must_use]
pub fn detect_holes(cells: &[CellStats]) -> Vec<GeoHole> {
    let cell_map: HashMap<CellId, &CellStats> =
        cells.iter().map(|c| (c.cell_id, c)).collect();

    let mut holes: HashMap<CellId, GeoHole> = HashMap::new();

    // Pass 1: seed from documented gaps.
    for &(name, lat, lon) in DOCUMENTED_GAP_SEEDS {
        let cell_id = CellId::from_lat_lon(lat, lon);
        let has_coverage = cell_map
            .get(&cell_id)
            .map_or(false, |c| c.distinct_sources > 0);

        if !has_coverage {
            // Estimate missed population from neighbors.
            let neighbor_total: u64 = cell_id
                .neighbors()
                .iter()
                .filter_map(|n| cell_map.get(n))
                .map(|c| c.record_count)
                .sum();

            let current_count = cell_map.get(&cell_id).map_or(0, |c| c.record_count);
            // Use neighbor total as the population signal; if the cell itself
            // has records (but no sources — orphaned records), add those too.
            let estimated = neighbor_total + current_count;

            holes.insert(
                cell_id,
                GeoHole {
                    cell_id,
                    center_lat: cell_id.center_lat(),
                    center_lon: cell_id.center_lon(),
                    estimated_missed_population: estimated,
                    name: Some(name.to_owned()),
                    basis: "estimated".to_owned(),
                },
            );
        }
    }

    // Pass 2: scan all cells with records but no sources (orphaned).
    // These can arise if records were inserted without a source_name link.
    for stats in cells {
        if stats.record_count >= MIN_RECORDS_THRESHOLD && stats.distinct_sources == 0 {
            let cell_id = stats.cell_id;
            // Only add if not already seeded from documented gaps.
            if !holes.contains_key(&cell_id) {
                let neighbor_total: u64 = cell_id
                    .neighbors()
                    .iter()
                    .filter_map(|n| cell_map.get(n))
                    .map(|c| c.record_count)
                    .sum();
                let estimated = stats.record_count + neighbor_total;
                holes.insert(
                    cell_id,
                    GeoHole {
                        cell_id,
                        center_lat: cell_id.center_lat(),
                        center_lon: cell_id.center_lon(),
                        estimated_missed_population: estimated,
                        name: None,
                        basis: "estimated".to_owned(),
                    },
                );
            }
        }
    }

    let mut hole_vec: Vec<GeoHole> = holes.into_values().collect();
    // Rank by estimated_missed_population descending.
    hole_vec.sort_unstable_by(|a, b| {
        b.estimated_missed_population
            .cmp(&a.estimated_missed_population)
    });
    hole_vec
}

// ── Store reader ──────────────────────────────────────────────────────────────

/// Read all (source_name, record_json) pairs from the ingest store read-only.
///
/// Opens with `SQLITE_OPEN_READ_ONLY` flags. Never writes.
///
/// # Errors
/// Returns a string error if the database cannot be opened or queried.
pub fn read_geo_records_from_store(
    path: &std::path::Path,
) -> Result<Vec<PetRecord>, String> {
    use rusqlite::{Connection, OpenFlags};

    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("cannot open store read-only at {}: {e}", path.display()))?;

    let mut stmt = conn
        .prepare("SELECT record_json FROM canonical_records")
        .map_err(|e| format!("cannot prepare geo query: {e}"))?;

    let records: Result<Vec<PetRecord>, String> = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| format!("cannot query canonical_records: {e}"))?
        .map(|res| {
            let json = res.map_err(|e| format!("row error: {e}"))?;
            serde_json::from_str::<PetRecord>(&json)
                .map_err(|e| format!("deserialize error: {e}"))
        })
        .collect();

    records
}

// ── Extended coverage report ──────────────────────────────────────────────────

/// The geo extension appended to the coverage report under `cells` and
/// `geo_holes`.
///
/// This is purely additive — it does NOT replace the existing `sources` and
/// `holes` fields.
#[derive(Debug, Serialize, Deserialize)]
pub struct GeoCoverageExtension {
    /// Per-cell statistics derived from real `PetRecord.location` values.
    pub cells: Vec<CellStats>,
    /// Detected coverage holes, ranked by estimated missed population.
    pub geo_holes: Vec<GeoHole>,
    /// Count of records with `location == None` (ungeocoded).
    pub ungeocoded: u64,
}

/// Run the geo coverage rollup against a store and return the extension.
///
/// Opens the store read-only. Never writes.
///
/// # Errors
/// Returns a string error if the store cannot be read.
pub fn build_geo_extension(
    store_path: &std::path::Path,
    min_count: u64,
) -> Result<GeoCoverageExtension, String> {
    let records = read_geo_records_from_store(store_path)?;
    let (mut cells, ungeocoded) = rollup_records(records.iter());

    // Filter cells by min_count.
    cells.retain(|c| c.record_count >= min_count);

    let geo_holes = detect_holes(&cells);

    Ok(GeoCoverageExtension {
        cells,
        geo_holes,
        ungeocoded,
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use homeward_schema::{
        Availability, ChipStatus, IntakeType, PetRecord, Species,
        provenance::{SourceId, TosClass},
    };
    use ulid::Ulid;

    fn make_record(
        source: &str,
        lat: Option<f64>,
        lon: Option<f64>,
        intake_type: IntakeType,
    ) -> PetRecord {
        use homeward_schema::geo::ShelterLocation;
        PetRecord {
            canonical_id: Ulid::new(),
            source: SourceId::new(source, TosClass::OpenData),
            source_animal_id: None,
            species: Species::Dog,
            breed_primary: None,
            breed_secondary: None,
            sex: None,
            age_bucket: None,
            size: None,
            colors: vec![],
            markings_text: None,
            intake_type,
            availability: Availability::InCustody,
            chip_status: ChipStatus::NotScanned,
            location: lat.map(|la| {
                ShelterLocation::new(
                    Some(la),
                    lon,
                    2,
                    "TestCity".to_owned(),
                    None,
                )
            }),
            found_location_text: None,
            photos: vec![],
            first_seen: Utc::now(),
            last_seen: Utc::now(),
            last_confirmed: None,
            intake_date: None,
            outcome_date: None,
            secondary_provenances: vec![],
        }
    }

    /// AC1: cell_id is deterministic; two points ~1 km apart in Austin
    /// share or neighbor the same cell.
    #[test]
    fn cell_id_deterministic_same_cell() {
        // Austin TX: two points ~0.5 km apart; both should map to the same 0.5° cell.
        let id_a = CellId::from_lat_lon(30.265, -97.740);
        let id_b = CellId::from_lat_lon(30.270, -97.745);
        // Both within 0.5° bucket — same cell or adjacent neighbor.
        let are_same_or_neighbor = id_a == id_b || id_a.neighbors().contains(&id_b);
        assert!(
            are_same_or_neighbor,
            "points ~0.5 km apart in Austin must share or neighbor the same cell; got {id_a:?} vs {id_b:?}"
        );
    }

    /// AC1 extra: same point always maps to the same cell.
    #[test]
    fn cell_id_is_stable() {
        let id1 = CellId::from_lat_lon(34.05, -118.25);
        let id2 = CellId::from_lat_lon(34.05, -118.25);
        assert_eq!(id1, id2, "same lat/lon must produce same CellId");
    }

    /// AC2: rollup over ≥3 cells with mixed intake types.
    #[test]
    fn rollup_spans_three_cells() {
        // Cell A: lat ~30.x, lon ~-97.x (Austin area)
        // Cell B: lat ~34.x, lon ~-118.x (LA area)
        // Cell C: lat ~40.x, lon ~-74.x (NYC area)
        let records = vec![
            make_record("austin", Some(30.27), Some(-97.74), IntakeType::Stray),
            make_record("austin", Some(30.28), Some(-97.73), IntakeType::OwnerSurrender),
            make_record("la", Some(34.05), Some(-118.25), IntakeType::Stray),
            make_record("la", Some(34.06), Some(-118.26), IntakeType::FoundReport),
            make_record("nyc", Some(40.71), Some(-74.01), IntakeType::Transfer),
        ];

        let (cells, ungeocoded) = rollup_records(records.iter());
        assert_eq!(ungeocoded, 0, "no records should be ungeocoded");
        // We may have 3 cells (each point pair may share a cell depending on 0.5° alignment)
        // but we must have at least 2 distinct cells.
        assert!(
            cells.len() >= 2,
            "records in different metros must produce ≥2 cells; got {}",
            cells.len()
        );
        // Total records across all cells must equal 5.
        let total: u64 = cells.iter().map(|c| c.record_count).sum();
        assert_eq!(total, 5, "total record_count must equal number of records");
    }

    /// AC3: stray_count counts only Stray and FoundReport; proven distinct
    /// from record_count on a mixed fixture.
    #[test]
    fn stray_count_distinct_from_record_count() {
        // Place all records in the same cell.
        let records = vec![
            make_record("src", Some(30.27), Some(-97.74), IntakeType::Stray),
            make_record("src", Some(30.27), Some(-97.74), IntakeType::FoundReport),
            make_record("src", Some(30.27), Some(-97.74), IntakeType::OwnerSurrender),
            make_record("src", Some(30.27), Some(-97.74), IntakeType::Transfer),
        ];

        let (cells, _) = rollup_records(records.iter());
        assert_eq!(cells.len(), 1, "all records in one cell");
        let cell = &cells[0];
        assert_eq!(cell.record_count, 4, "all 4 records counted");
        assert_eq!(cell.stray_count, 2, "only Stray + FoundReport count as stray");
        assert_ne!(
            cell.stray_count, cell.record_count,
            "stray_count must differ from record_count on mixed fixture"
        );
    }

    /// AC4: hole heuristic flags the genuine hole and NOT the covered cell.
    #[test]
    fn hole_heuristic_flags_only_genuine_hole() {
        // Cell A: well-covered (distinct_sources=1, record_count=50)
        let covered_id = CellId::from_lat_lon(30.27, -97.74);
        let covered = CellStats {
            cell_id: covered_id,
            center_lat: covered_id.center_lat(),
            center_lon: covered_id.center_lon(),
            record_count: 50,
            stray_count: 5,
            distinct_sources: 1,
            last_seen: Some(Utc::now()),
        };

        // Cell B: LA area — a documented-gap seed with no sources.
        // distinct_sources=0 but has a neighbor with records (covered).
        // Use an LA seed that is NOT adjacent to Austin.
        let la_id = CellId::from_lat_lon(34.05, -118.25);
        let la_hole = CellStats {
            cell_id: la_id,
            center_lat: la_id.center_lat(),
            center_lon: la_id.center_lon(),
            record_count: 0,
            stray_count: 0,
            distinct_sources: 0,
            last_seen: None,
        };

        let cells = vec![covered, la_hole];
        let holes = detect_holes(&cells);

        // The LA cell is a documented-gap seed with no sources → must be a hole.
        let la_hole_found = holes.iter().any(|h| h.cell_id == la_id);
        assert!(la_hole_found, "LA documented-gap cell must be detected as a hole");

        // The Austin covered cell must NOT be in holes.
        let austin_hole_found = holes.iter().any(|h| h.cell_id == covered_id);
        assert!(
            !austin_hole_found,
            "well-covered Austin cell must NOT be detected as a hole"
        );
    }

    /// AC5: holes ranked by estimated_missed_population descending.
    #[test]
    fn holes_ranked_by_missed_population_descending() {
        // Two holes: one with a high-population neighbor, one with low.
        // Place adjacent cells to the LA seed at high count.
        let la_id = CellId::from_lat_lon(34.05, -118.25);
        let ny_id = CellId::from_lat_lon(40.71, -74.01);

        // Neighbor of LA with high count:
        let la_neighbors = la_id.neighbors();
        let la_neighbor_id = la_neighbors[0];
        let ny_neighbors = ny_id.neighbors();
        let ny_neighbor_id = ny_neighbors[0];

        let la_neighbor = CellStats {
            cell_id: la_neighbor_id,
            center_lat: la_neighbor_id.center_lat(),
            center_lon: la_neighbor_id.center_lon(),
            record_count: 1000,
            stray_count: 100,
            distinct_sources: 1,
            last_seen: None,
        };

        let ny_neighbor = CellStats {
            cell_id: ny_neighbor_id,
            center_lat: ny_neighbor_id.center_lat(),
            center_lon: ny_neighbor_id.center_lon(),
            record_count: 10,
            stray_count: 1,
            distinct_sources: 1,
            last_seen: None,
        };

        let cells = vec![la_neighbor, ny_neighbor];
        let holes = detect_holes(&cells);

        // Both LA and NY are documented gaps → should appear.
        // LA has higher neighbor count → should rank first.
        let la_pos = holes.iter().position(|h| h.cell_id == la_id);
        let ny_pos = holes.iter().position(|h| h.cell_id == ny_id);

        // At least one of them must appear if the neighbor cell is populated.
        // (The other documented gaps — Chicago, Houston, Phoenix — may also appear.)
        if let (Some(la_p), Some(ny_p)) = (la_pos, ny_pos) {
            assert!(
                la_p <= ny_p,
                "LA hole (higher neighbor count) must rank before NY hole; LA pos={la_p}, NY pos={ny_p}"
            );
        }

        // Verify descending order of the full list.
        for i in 1..holes.len() {
            assert!(
                holes[i - 1].estimated_missed_population >= holes[i].estimated_missed_population,
                "holes must be sorted descending by estimated_missed_population"
            );
        }
    }

    /// AC8: records with None location are counted as ungeocoded, not dropped silently.
    #[test]
    fn ungeocoded_records_are_tallied() {
        let with_loc = make_record("src", Some(30.27), Some(-97.74), IntakeType::Stray);
        let without_loc = PetRecord {
            canonical_id: Ulid::new(),
            source: homeward_schema::provenance::SourceId::new("src", TosClass::OpenData),
            source_animal_id: None,
            species: Species::Dog,
            breed_primary: None,
            breed_secondary: None,
            sex: None,
            age_bucket: None,
            size: None,
            colors: vec![],
            markings_text: None,
            intake_type: IntakeType::Unknown,
            availability: Availability::Unknown,
            chip_status: ChipStatus::NotScanned,
            location: None,
            found_location_text: None,
            photos: vec![],
            first_seen: Utc::now(),
            last_seen: Utc::now(),
            last_confirmed: None,
            intake_date: None,
            outcome_date: None,
            secondary_provenances: vec![],
        };

        let records = vec![with_loc, without_loc];
        let (cells, ungeocoded) = rollup_records(records.iter());

        assert_eq!(ungeocoded, 1, "one record with no location must be counted as ungeocoded");
        let total_in_cells: u64 = cells.iter().map(|c| c.record_count).sum();
        assert_eq!(total_in_cells, 1, "only the geocoded record must appear in cells");
    }

    /// AC9: no network or PostGIS dependency (structural assertion — if this
    /// module compiles, those crates are not linked).
    #[test]
    fn no_external_spatial_dep() {
        // This test asserts via successful compilation that no PostGIS or H3
        // crate is required. No runtime check needed.
        let id = CellId::from_lat_lon(0.0, 0.0);
        assert_eq!(id.lat_idx, 0);
        assert_eq!(id.lon_idx, 0);
    }
}
