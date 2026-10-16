## S6 Results (FR-38, FR-39, FR-40, FR-41). "Not in your export" is never shown as a failure (NFR-USE-04).

results-title =
    { $state ->
        [done] All done
        [partial] Finished, with a few things to check
        [failed] The job stopped
        [cancelled] Job canceled
       *[other] Results
    }
results-lede =
    { $count ->
        [0] No memories were saved in this job.
        [one] One memory is in your library:
       *[other] { $count } memories are in your library:
    }
results-loading = Loading the results…

results-group-saved =
    { $count ->
        [one] Saved to your library
       *[other] Saved to your library
    }
results-group-missing = Not in your export
results-group-skipped = Not done yet
results-group-failed = Couldn't be processed

outcome-label =
    { $status ->
        [done] Done
        [metadata_partial] Saved with the date only
        [location_uncertain] Saved without a location
        [missing_media] Not in the parts you added
        [failed] Failed
       *[skipped_free_limit] Waiting for unlock
    }
outcome-explain =
    { $status ->
        [done] Dates, places and captions are restored.
        [metadata_partial] The exact time wasn't in the export, so these have the date from their file name.
        [location_uncertain] Their match was uncertain, so no place was added rather than risk a wrong one.
        [missing_media] The export's data lists these, but their photos and videos are in a part you haven't added. This isn't an error: add the missing part to rescue them.
        [failed] Something went wrong with these files. Retrying usually fixes it.
       *[skipped_free_limit] The free version stops after its limit. Unlock to finish these; the memories already done are not redone.
    }

results-uncertain-hint = To add them anyway, turn on “Apply uncertain locations” in Settings and retry.
results-missing-parts =
    { $count ->
        [one] Missing: part { $parts }.
       *[other] Missing: parts { $parts }.
    }
results-add-part = Add missing part
results-unlock = Unlock to finish
results-retry =
    { $count ->
        [one] Retry the failed item
       *[other] Retry { $count } failed items
    }

results-cancelled-title = The job was canceled
results-cancelled-body = What finished is in the output folder. You can continue the job; finished memories are not redone.
results-continue-job = Continue job

results-report-title = Full report
results-report-body = Every memory is listed with what happened to it, in report.html and report.csv inside the _report folder.
results-open-report = Open report
results-report-pending = The report is written when the job finishes.
results-open-folder = Open folder
results-diagnostics = Export diagnostics
results-diagnostics-saved = Diagnostics saved
results-diagnostics-body = The file holds logs and the job summary, with no photos, videos or export data. Attach it to an email to support.
results-new-job = New job

results-announce-done =
    { $saved ->
        [one] Done. One memory saved.
       *[other] Done. { $saved } memories saved.
    }
results-announce-partial =
    { $problems ->
        [one] Finished. { $saved } saved, one needs attention.
       *[other] Finished. { $saved } saved, { $problems } need attention.
    }
results-announce-failed = The job stopped.
results-announce-cancelled = Job canceled.
