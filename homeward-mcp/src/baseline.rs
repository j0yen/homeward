//! Species-level accuracy baselines for `match_photo`'s advisory metadata
//! (PRD goal: "cat matches (r1 0.346 vs dogs 0.538) carry the species
//! baseline in the response metadata so clients can calibrate").
//!
//! Sourced from the checked-in cross-session eval snapshot.
//!
//! File: `homeward/embed/scripts/xsession/results-large-20260826.json`,
//! `DINOv2` ViT-S/14 "large" variant -- see `EVAL.md`'s "Measured accuracy —
//! cross-session holdout" section. That snapshot's `per_query` array
//! carries a `true_rank`/`species` pair per query; rank-1/5/20 below are
//! `true_rank <= {1,5,20}` fractions computed per species from it (dog:
//! n=654, cat: n=567), matching the `dogs rank-1 0.538, cats 0.346` figure
//! `EVAL.md` already states. These are fixed constants from a committed
//! artifact, not re-derived per call -- `match_photo` doesn't re-run the
//! eval harness on every request.

use crate::dto::SpeciesBaseline;

const SNAPSHOT_SOURCE: &str = "homeward/embed/scripts/xsession/results-large-20260826.json \
     (DINOv2 large, cross-session holdout, 2026-08-26; see EVAL.md)";

/// Baseline for `species` ("dog" or "cat").
///
/// Any other value gets the species-mixed aggregate (rank-1/5/20
/// 0.449/0.693/0.880) as a fallback -- callers of this function are
/// expected to have already validated `species` against
/// [`homeward_schema::Species`] before reaching it, so the fallback arm is
/// defensive, not a normal path.
#[must_use]
pub fn for_species(species: &str) -> SpeciesBaseline {
    let (rank1, rank5, rank20) = match species {
        "dog" => (0.538, 0.772, 0.917),
        "cat" => (0.346, 0.601, 0.838),
        _ => (0.449, 0.693, 0.880),
    };
    SpeciesBaseline {
        species: species.to_owned(),
        rank1,
        rank5,
        rank20,
        source: SNAPSHOT_SOURCE.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::for_species;

    #[test]
    fn dog_baseline_matches_eval_md() {
        let b = for_species("dog");
        assert!((b.rank1 - 0.538).abs() < 1e-9);
        assert_eq!(b.species, "dog");
    }

    #[test]
    fn cat_baseline_matches_eval_md() {
        let b = for_species("cat");
        assert!((b.rank1 - 0.346).abs() < 1e-9);
        assert_eq!(b.species, "cat");
    }

    #[test]
    fn cat_rank1_is_worse_than_dog_rank1() {
        // The whole point of this metadata: callers must be able to see
        // that cats are the weak spot.
        assert!(for_species("cat").rank1 < for_species("dog").rank1);
    }

    #[test]
    fn unknown_species_falls_back_to_aggregate() {
        let b = for_species("ferret");
        assert!((b.rank1 - 0.449).abs() < 1e-9);
    }

    #[test]
    fn every_baseline_cites_a_source() {
        for species in ["dog", "cat", "other"] {
            assert!(!for_species(species).source.is_empty());
        }
    }
}
