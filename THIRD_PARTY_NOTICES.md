# Third-party notices

Components bundled into Memories Rescue or its build, with their licences
(SRS NFR-LEG-03). The Rust crate list is to be generated automatically
(`cargo about` is the intended tool) once the dependency set settles; the
entries below are the ones that carry obligations beyond a permissive
licence or that ship data, and are maintained by hand until then.

## Time zone data (crates/mr-tz, FR-19)

| Component | Version | Licence | Notes |
|---|---|---|---|
| [tzf-rs](https://github.com/ringsaturn/tzf-rs) | 2.x | MIT | Point-in-polygon lookup from coordinates to an IANA zone name. |
| [tzf-dist](https://github.com/ringsaturn/tzf-dist) (`lite.tzb`) | 0.0.2026 | Code MIT; **data ODbL-1.0** | Reduced-precision time zone boundaries compiled from [timezone-boundary-builder](https://github.com/evansiroky/timezone-boundary-builder), itself derived from OpenStreetMap. |
| [chrono-tz](https://github.com/chronotope/chrono-tz) | 0.10 | MIT OR Apache-2.0 | Bundles the IANA Time Zone Database (tzdb), which is in the public domain. |
| [iana-time-zone](https://github.com/strawlab/iana-time-zone) | 0.1 | MIT OR Apache-2.0 | Reads the operating system's zone name. |

### Open Database License notice (tzf-dist boundary data)

The time zone boundary data bundled in this application is made available
under the Open Database License (ODbL) v1.0:
<https://opendatacommons.org/licenses/odbl/1-0/>. It is derived from
timezone-boundary-builder by Evan Siroky, which is in turn derived from
OpenStreetMap data, copyright OpenStreetMap contributors
(<https://www.openstreetmap.org/copyright>). The data is used unmodified
apart from the precision reduction performed by tzf-dist; no derivative
database is distributed. Any rights in individual contents of the database
are licensed under the Database Contents License:
<https://opendatacommons.org/licenses/dbcl/1-0/>.

## Metadata (crates/mr-meta)

| Component | Version | Licence | Notes |
|---|---|---|---|
| [little_exif](https://github.com/TechnikTobi/little_exif) | 0.6 | MIT | EXIF read/write for JPEG and PNG. |

## Sidecars (not bundled in v1.0 unless a later decision says so)

See `third_party/README.md` for ffmpeg (LGPL build) and exiftool
(Perl Artistic / GPL dual licence) sources, versions and checksums, and
`docs/decisions/Q-02-metadata-writer.md` for why exiftool is a test oracle
only.

## Application framework

Tauri 2 (MIT OR Apache-2.0) and its plugins; the system web view belongs to
the operating system. Rust crates: see the generated list (pending).
