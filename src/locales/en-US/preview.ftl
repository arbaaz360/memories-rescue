## S4 Preview, the dry run (FR-29).

preview-title = Preview
preview-lede = Nothing has been written yet. This is what { -product } will create.
preview-loading = Planning your library…

preview-totals-title = In total
preview-total-items = Memories to rescue
preview-total-photos = Photos
preview-total-videos = Videos
preview-total-captions = Captions added
preview-total-originals = Originals kept
preview-total-missing = Not in your export
preview-total-missing-hint = Listed in the data, but in a part you haven't added
preview-estimate-time = Estimated time
preview-estimate-duration = About { $duration }
preview-estimate-soon = Under a minute
preview-estimate-size = Space needed
preview-estimate-free = { $free } free on { $volume }

preview-free-title = The free version rescues { $limit } memories on this computer
preview-free-body = { $free } of these memories are fully processed: any this computer rescued before, then the earliest in date order. Unlock to rescue all { $total }; the other { $beyond } wait until you do, and nothing is redone.
preview-free-unlock = Unlock

preview-match-title = How well it matches
preview-confidence-title = Capture time
preview-location-title = Location
confidence-exact = Exact time
confidence-ordinal = Time from file order
confidence-none = Date only
location-applied = Location added
location-uncertain = Left out: uncertain match
location-none = No location in the export

preview-samples-title = Sample of the planned files
preview-col-preview = Preview
preview-col-name = New name
preview-col-date = Taken
preview-col-location = Place
preview-kind =
    { $kind ->
        [video] Video
       *[photo] Photo
    }
preview-folder = In { $folder }
preview-has-caption = Caption
preview-no-date = Unknown
preview-match-badge =
    { $confidence ->
        [exact] Exact time
        [ordinal] By file order
       *[none] Date only
    }
preview-loc-none = No location
preview-loc-uncertain = Not added: uncertain match
preview-coords =
    { $lat }° { $ns ->
        [n] N
       *[s] S
    }, { $lon }° { $ew ->
        [e] E
       *[w] W
    }

preview-tree-title = Folders that will be created
preview-tree-files =
    { $count ->
        [one] one file
       *[other] { $count } files
    }
preview-tree-more =
    { $count ->
        [one] and one more
       *[other] and { $count } more
    }

preview-clips-title =
    { $count ->
        [0] Split videos
        [one] One split video found
       *[other] { $count } split videos found
    }
preview-clips-intro = Each group is joined into one video. Untick a group to leave its clips as they are.
preview-clips-none = No split videos found in the chosen years.
preview-clips-off = Joining split videos is turned off.
preview-clip-label =
    { $count ->
        [one] One clip, { $seconds } seconds, from { $date }
       *[other] { $count } clips, { $seconds } seconds, from { $date }
    }

preview-warnings-title = Worth knowing
preview-report-export = Save preview report
preview-report-saved = Preview report saved
preview-report-open = Open report
