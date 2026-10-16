## Shell, navigation and strings shared by several screens.

-product = Memories Rescue

app-name = { -product }
skip-to-content = Skip to content
topnav-label = App
nav-settings = Settings
nav-about = About
page-back = Back

steps-label = Steps
steps-done-suffix = (done)
step-add-files = Add files
step-summary = Check
step-options = Options
step-preview = Preview
step-processing = Rescue
step-results = Results

header-free = Free: { $limit } per job
header-licensed = Unlocked
header-grace = License needs attention

action-back = Back
action-continue = Continue
action-start = Start
dialog-close = Close
notice-dismiss = Dismiss

picker-files-title = Choose your Snapchat export files
picker-files-filter = Snapchat export (ZIP)
picker-folder-title = Choose an extracted export folder
picker-part-title = Choose the missing part
picker-destination-title = Choose where to save your memories

status-short =
    { $status ->
        [done] Saved
        [metadata_partial] Date only
        [location_uncertain] No location
        [missing_media] Not in export
        [failed] Failed
       *[skipped_free_limit] Waiting for unlock
    }

notify-title =
    { $state ->
        [done] Your memories are ready
        [partial] Finished, with a few things to check
       *[failed] The job stopped
    }
notify-body =
    { $saved ->
        [one] One memory saved. Open { -product } for the details.
       *[other] { $saved } memories saved. Open { -product } for the details.
    }

dev-mock-badge =
    { $reason ->
        [engine_missing] Mock engine: Rust engine not wired yet
        [engine_error] Mock engine: engine did not answer
       *[other] Mock engine
    }
dev-mock-key = Mock engine: { $key } activates. A key containing OFFLINE simulates no network.

## Updates (§10.2)

update-title = Version { $version } is available
update-body = You have { $current }. The update installs when you choose, then { -product } restarts. { $channel ->
        [beta] This is the beta channel.
       *[stable] {""}
    }
update-wait-for-job = A job is running. Updates never install during a job: finish or pause it first.
update-install = Install and restart
update-later = Later
update-notes = What's new
update-failed = The update could not be installed. Try again later, or download it from the website.
