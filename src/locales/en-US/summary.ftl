## S2 Check and summary (FR-07, FR-13).

summary-title = Check your export
summary-title-job = Check the added part
summary-lede = Here is what { -product } found. Nothing has been written yet.
summary-lede-job = The part has been added to this job. Continue to rescue only the memories it makes available.
summary-loading = Checking your export…

summary-format-title = Format
summary-profile-name =
    { $profile ->
        [v2026a] 2026 format with links
        [v2026b] 2026 format
        [legacy] Older format: links only
        [mixed] Mixed 2026 formats
       *[unknown] Format not recognized
    }
summary-profile-explain =
    { $profile ->
        [v2026a] Each memory in the export's data points to its own photo or video, so every item gets its exact time.
        [v2026b] The export's data and the photos are matched by date and by their order inside the ZIP files.
        [legacy] This export has download links instead of photos and videos.
        [mixed] Part of this export points to each photo directly; the rest is matched by date and by order inside the ZIP files.
       *[unknown] Without the export's data file, times come from the file names, so items get their date only.
    }
summary-path-exact = Exact time
summary-path-exact-desc = The export links the memory to its file.
summary-path-ordinal = Time from file order
summary-path-ordinal-desc = Matched by date and by position in the ZIP file.
summary-path-day = Date only
summary-path-day-desc = No reliable match; the date comes from the file name.
summary-ordinal-note =
    { $count ->
        [one] The exact time of one memory depends on the order of files inside the ZIP files. Keep the original ZIP files: from an extracted folder it would get its date only.
       *[other] The exact times of { $count } memories depend on the order of files inside the ZIP files. Keep the original ZIP files: from an extracted folder, most of them would get their date only.
    }

summary-legacy-title = This export contains links rather than media
summary-legacy-body = These links expire and { -product } v1 does not download them. Please request a new export:

summary-part-missing-title = Part { $n } appears to be missing
summary-part-missing-body =
    { $count ->
        [0] Add it now, or continue without it and add it later.
        [one] One memory in the export's data is in that part. Add it now, or continue without it: you can add it later, and then only that memory is processed.
       *[other] { $count } memories in the export's data are in that part. Add it now, or continue without it: you can add it later, and then only those memories are processed.
    }
summary-add-missing = Add missing part
summary-continue-anyway = Continue anyway
summary-continue-job = Rescue these memories
summary-back-results = Back to results

summary-parts-title =
    { $count ->
        [one] One part
       *[other] { $count } parts
    }
summary-part-label = Part { $n }
summary-part-status-ok = Found
summary-part-status-missing = Missing
summary-part-status-truncated = Incomplete
summary-part-meta =
    { $count ->
        [one] { $size }, one item
       *[other] { $size }, { $count } items
    }

summary-counts-title = What's inside
summary-count-entries = Memories in the export's data
summary-count-photos = Photos
summary-count-videos = Videos
summary-count-overlays = Captions and stickers
summary-count-orphans =
    { $count ->
        [one] One belongs to a photo or video in a part you haven't added
       *[other] { $count } belong to photos or videos in a part you haven't added
    }
summary-count-location = With a location
summary-count-dates = Dates
summary-date-range = { $first } to { $last }
summary-date-none = Not known
summary-count-size = Total size
summary-issues-title = Worth knowing

## Issues attached to an export or a plan, with counts. The title is the value, .body explains.

issue-part-missing = Part { $part } appears to be missing
    .body = Its memories are listed in the export's data. Add the part to rescue them; nothing else is redone.
issue-part-truncated = Part { $part } is incomplete
    .body = Everything that can still be read is used. Download that part again if your link still works.
issue-json-missing = Exact times aren't available
    .body = The first part of the export, which holds the data file, wasn't added. Items get the date from their file name. Add the first part for exact times and places.
issue-json-parse = The export's data file is damaged
    .body = File names are used where possible. Download the first part again if your link still works.
issue-match-partial =
    { $count ->
        [0] Exact times need the original ZIP files
        [one] The exact time is unknown for one memory
       *[other] The exact time is unknown for { $count } memories
    }
    .body = These get the date from their file name. If you added an extracted folder, add the original ZIP files instead: the order of files inside them is what gives exact times.
issue-encoder-unavailable =
    { $count ->
        [one] The caption can't be added to the picture yet
       *[other] Captions can't be added to the pictures yet
    }
    .body = The part that draws captions onto photos and videos isn't available, so { $count } captions are saved next to their files instead. Dates and places are still fixed, and the captions can be added later without redoing anything else.
issue-location-uncertain =
    { $count ->
        [one] Location left out for one memory
       *[other] Location left out for { $count } memories
    }
    .body = Their match to the export's data is uncertain, and a wrong place on the map is worse than none. To add them anyway, turn on “Apply uncertain locations” in the options.
