//! The Rust contract types against JSON the TypeScript mock produced
//! (`src/ipc/samples.test.ts` writes `tests/ipc-samples/`). Each sample must
//! deserialise and serialise back to the same JSON: a field the window has and
//! Rust lacks disappears on the way back, a field Rust has and the window
//! lacks appears as `null` or fails to parse, and either fails here.

use std::path::Path;

use memories_rescue_lib::ipc::*;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

fn sample(name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("ipc-samples")
        .join(format!("{name}.json"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).expect("sample is JSON")
}

/// Equal as JSON, with numbers compared by value (1457 equals 1457.0).
fn same(a: &Value, b: &Value, at: &str) {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => {
            assert!(
                (x.as_f64().unwrap() - y.as_f64().unwrap()).abs() < 1e-9,
                "{at}: {x} != {y}"
            )
        }
        (Value::Object(x), Value::Object(y)) => {
            let mut kx: Vec<&String> = x.keys().collect();
            let mut ky: Vec<&String> = y.keys().collect();
            kx.sort();
            ky.sort();
            assert_eq!(kx, ky, "{at}: keys differ");
            for (k, v) in x {
                same(v, &y[k], &format!("{at}.{k}"));
            }
        }
        (Value::Array(x), Value::Array(y)) => {
            assert_eq!(x.len(), y.len(), "{at}: lengths differ");
            for (i, (u, v)) in x.iter().zip(y).enumerate() {
                same(u, v, &format!("{at}[{i}]"));
            }
        }
        _ => assert_eq!(a, b, "{at}"),
    }
}

fn round_trip<T: DeserializeOwned + Serialize>(name: &str) {
    let original = sample(name);
    let typed: T =
        serde_json::from_value(original.clone()).unwrap_or_else(|e| panic!("{name}: {e}"));
    let back = serde_json::to_value(&typed).expect("serialise");
    same(&original, &back, name);
}

#[test]
fn every_contract_shape_round_trips() {
    round_trip::<Vec<InputFile>>("input_files");
    round_trip::<ExportSummary>("export_summary");
    round_trip::<PlanPreview>("plan_preview");
    round_trip::<SpaceEstimate>("space_estimate");
    round_trip::<VolumeInfo>("volume_info");
    round_trip::<JobReport>("job_report");
    round_trip::<Vec<ResumableJob>>("resumable_jobs");
    round_trip::<Settings>("settings");
    round_trip::<DiagnosticsPreview>("diagnostics_preview");
    round_trip::<UpdateInfo>("update_info");
    round_trip::<LicenseStatus>("license_status");
    round_trip::<AppInfo>("app_info");
    round_trip::<ProgressEvent>("progress_event");
    round_trip::<ItemEvent>("item_event");
    round_trip::<JobStateEvent>("job_state_event");
    round_trip::<WarningEvent>("warning_event");
    round_trip::<IpcError>("ipc_error");
}

#[test]
fn error_codes_are_the_srs_table() {
    let names: Vec<String> = ErrorCode::ALL
        .iter()
        .map(|c| {
            serde_json::to_value(c)
                .unwrap()
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    let ts = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("src")
            .join("ipc")
            .join("types.ts"),
    )
    .unwrap();
    let start = ts.find("export const ERROR_CODES = [").unwrap();
    let block = &ts[start..ts[start..].find("] as const").unwrap() + start];
    let ts_codes: Vec<String> = block
        .split('"')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect();
    assert_eq!(names, ts_codes);
    for c in ErrorCode::ALL {
        let s = serde_json::to_value(c)
            .unwrap()
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(ErrorCode::from_engine(&s), c, "{s}");
    }
    assert_eq!(
        ErrorCode::from_engine("ENTRY_REFUSED"),
        ErrorCode::PartTruncated
    );
    assert_eq!(
        ErrorCode::from_engine("INPUT_UNAVAILABLE"),
        ErrorCode::PartMissing
    );
    assert_eq!(ErrorCode::from_engine("TEST_INJECTED"), ErrorCode::Unknown);
}
