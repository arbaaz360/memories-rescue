## S1 Add export files.

inputs-title = Add your export files
inputs-lede = Add every ZIP file from your Snapchat download, or the folder you extracted them into. { -product } only reads them; it never changes or deletes them.
inputs-drop-title = Drop your export files here
inputs-drop-release = Release to add these files
inputs-drop-hint = ZIP files or folders. You can add more at any time.
inputs-add-files = Add files…
inputs-add-folder = Add folder…
inputs-list-title = Added
inputs-list-meta =
    { $count ->
        [one] One usable file, { $size }
       *[other] { $count } usable files, { $size }
    }
inputs-empty = No files added yet.
inputs-remove = Remove
inputs-remove-label = Remove { $name }
inputs-removed = Removed { $name }.
inputs-added =
    { $count ->
        [0] No new files added.
        [one] Added one file.
       *[other] Added { $count } files.
    }
inputs-added-with-ignored =
    { $count ->
        [0] No export files added.
        [one] Added one file.
       *[other] Added { $count } files.
    } { $ignored ->
        [one] One file was ignored because it isn't part of an export.
       *[other] { $ignored } files were ignored because they aren't part of an export.
    }
inputs-ignored-title =
    { $count ->
        [one] One file was ignored
       *[other] { $count } files were ignored
    }
inputs-ignored-body =
    { $count ->
        [one] { $names } is not a Snapchat export file, so it was left out.
       *[other] { $names } are not Snapchat export files, so they were left out.
    }

inputs-role-primary = Primary
inputs-role-part = Part { $n }
inputs-role-unknown = Part unknown
inputs-role-ignored = Ignored
inputs-status-ok = Ready
inputs-status-pending = Checking
inputs-status-truncated = Incomplete
inputs-status-not-export = Not an export
inputs-status-duplicate = Duplicate, read once
