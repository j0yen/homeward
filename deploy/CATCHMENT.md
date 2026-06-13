# Homeward Source Catchment

Live source catalog for the homeward shelter-pet aggregator.  Each row is one
Socrata / SODA open-data feed declared in `deploy/sources.toml`.

## Sources

| Name            | Metro                   | Last Validated | Verdict    |
|-----------------|-------------------------|----------------|------------|
| austin          | Austin TX               | 2026-06-13     | green      |
| dallas          | Dallas TX               | 2026-06-13     | green      |
| sonoma          | Sonoma County CA        | 2026-06-13     | green      |
| long_beach      | Long Beach CA           | 2026-06-13     | green      |
| bloomington_il  | Bloomington-Normal IL   | 2026-06-13     | green      |
| louisville_ky   | Louisville KY           | 2026-06-13     | green      |

## Re-validation

Run a live probe against all catalog entries (requires network access):

```sh
# Once homeward-source-probe ships:
homeward-source-probe --catalog deploy/sources.toml --all

# For a single source:
homeward-source-probe --catalog deploy/sources.toml --source austin

# Manual curl spot-check (Austin):
curl -s "https://data.austintexas.gov/resource/fdzn-9yqv.json?\$limit=1&\$where=upper(animal_type)+in+('DOG','CAT')" | jq length
```

Expected: `1` (one row returned confirms the endpoint is alive).

## Adding New Sources

1. Find the dataset 4x4 ID on the city's Socrata portal (look for Animal Services
   / Shelter Intake datasets).
2. Verify column names with:
   `curl -s "https://<domain>/api/views/<id>.json" | jq '.columns[].fieldName'`
3. Add a `[[socrata]]` entry in `deploy/sources.toml` with the correct
   `column_map`.  Required columns: `animal_id`, `animal_type`, `intake_type`.
4. Run `homeward-source-probe` to validate, update the table above.

## Known Gaps

The following large metros **do not have a usable Socrata STRAY animal feed**
as of 2026-06-13:

| Metro              | Reason                                                                 |
|--------------------|------------------------------------------------------------------------|
| New York City NY   | NYC Open Data has cat/dog licenses, not shelter intake records.        |
| Los Angeles CA     | LAAS uses an internal PetPoint system; no public Socrata endpoint.     |
| Chicago IL         | CACC data is available via periodic dump exports only (no live SODA).  |
| Houston TX         | BARC publishes aggregate statistics; individual intake records absent. |

These metros may be added via the RescueGroups or Petfinder connectors if they
publish partner feeds on those platforms.
