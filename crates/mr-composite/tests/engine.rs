//! The seam: mr-job's engine runs the F-07 export with the real compositor
//! plugged in through `mr_job::Compositor`, the way the shell will wire it.
//! Every photo and video comes out composited, upright, with metadata
//! written by the engine and the originals kept (FR-23, FR-24, FR-25).

mod common;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use mr_composite::{Compositor, probe};
use mr_job::{
    CollectingSink, CompositeError, CompositeOutcome, CompositeRequest, Control, Job, Options,
    Outcome,
};

/// The adapter the engine side needs: about ten lines.
struct MediaCompositor(Compositor);

impl mr_job::Compositor for MediaCompositor {
    fn composite(&self, req: &CompositeRequest<'_>) -> Result<CompositeOutcome, CompositeError> {
        self.0
            .composite_any(req.main, req.overlay, req.out)
            .map(|_| CompositeOutcome::Composited)
            .map_err(|e| CompositeError {
                code: e.code().into(),
                detail: e.to_string(),
            })
    }

    fn name(&self) -> &str {
        "mr-composite"
    }
}

#[test]
fn engine_burns_in_f07_through_the_compositor_seam() {
    let ffmpeg = need_ffmpeg!();
    let Some((dir, truth)) = common::fixture("rotation") else {
        return;
    };
    if truth["videos_skipped"].is_string() {
        eprintln!("skip: F-07 was generated without its videos");
        return;
    }
    let base = common::scratch("engine");
    let (state, dest) = (base.join("state"), base.join("dest"));
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&dest).unwrap();
    let zips: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "zip"))
        .collect();
    let mut o = Options::new(&dest);
    o.home_tz = Some("Europe/London".into());
    o.unlocked = true;
    let mut job = Job::create(&state, &zips, o).unwrap();
    job.set_compositor(Arc::new(MediaCompositor(Compositor::new(&ffmpeg))));
    let outcome = job
        .run(&CollectingSink::default(), &Control::new())
        .unwrap();
    assert!(matches!(outcome, Outcome::Completed(_)), "{outcome:?}");

    let by_id: HashMap<String, &serde_json::Value> = truth["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| (i["id"].as_str().unwrap().to_ascii_uppercase(), i))
        .collect();
    let rows = job.items().unwrap();
    assert_eq!(rows.len(), by_id.len());
    for row in rows {
        let item = by_id[&row.plan.file_id.to_ascii_uppercase()];
        let case = item["case"].as_str().unwrap();
        assert!(!row.composite_pending, "{case}: composite_pending");
        assert_eq!(
            row.last_error_code, None,
            "{case}: {:?}",
            row.last_error_detail
        );
        assert!(
            row.originals_main.is_some(),
            "{case}: original kept (FR-25)"
        );
        let out = job.output_root().join(row.output_path.as_ref().unwrap());
        let info = probe(&out).unwrap();
        assert_eq!(
            info.display_size(),
            Some(common::size_of(&item["display_size"])),
            "{case}"
        );
        if let Some(v) = &info.video {
            assert_eq!((v.codec.as_str(), v.rotation), ("avc1", 0), "{case}");
            let frame = common::frame_at(&ffmpeg, &out, 0.517, &base.join(format!("{case}.png")));
            common::assert_markers(&frame, &item["markers"], case);
        } else {
            // The engine's EXIF carries no orientation: the pixels are upright.
            assert_eq!(info.image.as_ref().unwrap().orientation, 1, "{case}");
            common::assert_markers(&common::open_rgb(&out), &item["markers"], case);
        }
        eprintln!("{case}: {}", row.output_path.as_ref().unwrap());
    }
}
