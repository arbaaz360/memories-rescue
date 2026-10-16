## S5 Processing (FR-32, FR-34, FR-36) and the cancel confirmation.

processing-title = Rescuing your memories
processing-title-paused = Paused
processing-title-free-limit = Free limit reached
processing-lede = You can keep using your computer. If you close the app, the job pauses and continues where it stopped.
processing-overall = Overall progress
processing-valuetext = { $pct }, { $done } of { $total } items
processing-stage =
    { $stage ->
        [indexing] Reading the export…
        [matching] Matching files to their dates…
        [extracting] Unpacking files
        [compositing] Adding captions
        [tagging] Writing dates and places
        [writing] Saving files
        [verifying] Checking saved files
        [rejoining] Joining split videos
        [zipping] Making the ZIP archive
        [reporting] Writing the report
       *[idle] Waiting
    }

processing-items = Items
processing-of = { $done } of { $total }
processing-of-text = { $done } of { $total }
processing-data = Data
processing-time-left = Time left
processing-eta = About { $duration } left
processing-eta-soon = Less than a minute left
processing-eta-unknown = Working it out…
processing-eta-paused = Paused
processing-speed = Speed
processing-rate =
    { $rate ->
        [one] { $rate } item per second
       *[other] { $rate } items per second
    }
processing-warnings =
    { $count ->
        [0] No warnings
        [one] One saved without exact time or place
       *[other] { $count } saved without exact time or place
    }
processing-failures =
    { $count ->
        [0] No failures
        [one] One failed
       *[other] { $count } failed
    }
processing-awake = Keeping this computer awake until the job is done. The screen can still turn off.
processing-free-note = Free version: the job stops after { $limit } memories.
processing-free-limit-title = You've rescued { $limit } memories for free
processing-free-limit-body = Unlock to finish the rest, or finish for now: the rest are listed in the report and can be done later without redoing anything.
processing-unlock = Unlock to finish
processing-finish-free = Finish for now
processing-recent-title = Just finished
processing-recent-empty = Nothing finished yet.

processing-pause = Pause
processing-resume = Resume
processing-cancel = Cancel
processing-open-output = Open output folder

processing-announce =
    { $pct } done, { $done } of { $total } items. { $etaKind ->
        [soon] Less than a minute left.
        [known] About { $eta } left.
       *[unknown] Working out the time left.
    }
processing-announce-paused = Paused.
processing-announce-paused-error = Paused because of a problem. The message is on screen.
processing-announce-running = Running again.
processing-announce-free-limit = Stopped after { $limit } memories, the free limit.

cancel-title = Cancel this job?
cancel-body = Everything finished so far stays in the output folder, and the job can be continued later.
cancel-keep-going = Keep going
cancel-confirm = Cancel job
