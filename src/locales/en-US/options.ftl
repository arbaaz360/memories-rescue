## S3 Options (FR-26, FR-28, FR-48).

options-title = Choose how to save your memories
options-lede = Safe defaults are already set. Change what you like, then preview or start.

options-dest-title = Where to save
options-dest-current = Destination folder
options-dest-none = No folder chosen yet
options-dest-choose = Choose folder…
options-dest-hint = A folder called “Memories Rescue” is created there, with a folder for each year and month.

options-space-label = Space on { $volume }
options-space-text = Needs about { $need }, with a safety margin. { $free } free on { $volume }.
options-space-used = Already used
options-space-need = Needed for this job
options-space-breakdown = What the estimate includes
options-space-output = Your library
options-space-originals = Originals
options-space-temp = Temporary files while working
options-space-zip = ZIP archive
options-space-margin = Safety margin (10%)
options-space-short-title = Not enough space on { $volume }. About { $short } more is needed.
options-space-short-body = Choose another folder, or save less: originals and the ZIP archive take extra space.
options-space-fix-dest = Choose another folder
options-space-fix-originals = Don't keep originals
options-space-fix-zip = Don't make a ZIP

options-overlay-title = Captions and stickers
overlay-burn-in-keep-originals = Add them to the picture, and keep the originals
overlay-burn-in-keep-originals-desc = Captions and stickers are drawn onto each photo and video. The untouched file is saved too, in an “originals” folder.
overlay-burn-in-only = Add them to the picture
overlay-burn-in-only-desc = A smaller library. The version without captions is not kept.
overlay-separate-file = Keep them as separate files
overlay-separate-file-desc = Each caption is saved next to its photo or video as a transparent image.
overlay-ignore = Leave them out
overlay-ignore-desc = Only the photos and videos are saved.

options-keep-originals = Keep originals
options-keep-originals-desc = Recommended. If you ever want the picture without its caption, it's there.
options-keep-originals-na = Only applies when captions are added to the picture.

options-more-title = More options
options-rejoin = Join split videos back together
options-rejoin-desc = Older Snapchat videos were often saved as 10-second clips. Clips that belong together are joined into one video; the clips are kept too.
options-rejoin-years-legend = Years to look for split videos
options-rejoin-from = From
options-rejoin-to = to
options-rejoin-all-years = All years
options-uncertain = Apply uncertain locations
options-uncertain-desc = Off by default. Turning it on puts a location on memories whose match is uncertain, so some may land on the wrong place on the map.
options-wincompat = Maximize Windows compatibility
options-wincompat-desc = Saves every video as H.264, which every Windows PC can play. Takes longer for HEVC videos.
options-zip = Also make a ZIP archive
options-zip-desc = Handy for copying to another computer. Needs about as much space again.
options-zip-volume = Archive size
options-zip-volume-none = One archive
options-zip-volume-fat32 = Split into 4 GB parts, for FAT32 drives
options-metadata-only = Only fix dates and places (fastest)
options-metadata-only-desc = Skips captions and joining videos. Photos and videos still get their dates, places and file times.
options-timezone = Home time zone
options-timezone-desc = Used for memories without a location. Memories with a location use the time zone of the place.

options-free-note = The free version rescues { $limit } memories on this computer; rescuing the same ones again, into any folder, stays free. You can unlock the rest at any time without starting again.
options-preview = Preview first

## FR-51: HEVC playback on Windows

hevc-title = Some videos may not play on this computer
hevc-body =
    { $hevc ->
        [one] One of { $sampled } videos checked is HEVC
       *[other] { $hevc } of { $sampled } videos checked are HEVC
    }, a format Windows plays only with Microsoft's HEVC Video Extensions. Turn on "Maximize Windows compatibility" to save your videos as H.264, which every Windows PC plays.
hevc-enable = Turn on Windows compatibility
