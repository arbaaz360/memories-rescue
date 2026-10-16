## Settings (FR-47, FR-48, NFR-PRV-04, §10.2) and About (§7.2, NFR-LEG-01/02).

settings-title = Settings
settings-lede = These are the defaults for new jobs. A running job keeps the options it started with.
settings-processing = Processing
settings-workers = Files processed at once
settings-workers-auto = Automatic (recommended)
settings-workers-n =
    { $count ->
        [one] One at a time
       *[other] { $count } at a time
    }
settings-workers-desc = More is faster on a powerful computer but makes it busier.
settings-template = File name pattern
settings-template-example = Example: { $example }
settings-template-tokens = Placeholders: { $tokens }
settings-template-problem =
    { $problem ->
        [empty] Enter a pattern.
        [token] Use only the placeholders listed below.
        [chars] File names can't contain < > : " / \ | ? or *.
        [ext] Include {"{"}ext{"}"} so files keep their type.
       *[id] Include {"{"}shortid{"}"} or {"{"}id{"}"} so every file name stays unique.
    }

settings-license = License
settings-license-tier =
    { $tier ->
        [licensed] Unlocked
        [grace] Unlocked, check needed
       *[free] Free version
    }
settings-license-verified = Last verified on { $date }.
settings-license-grace = Gumroad reported this key as no longer valid. The free version returns on { $date } unless it is resolved.
settings-license-unlock = Unlock
settings-license-deactivate = Deactivate on this computer
settings-license-buy-page = Purchase page
settings-deactivate-title = Deactivate on this computer?
settings-deactivate-body = The key is removed from this computer and the free version returns. You can activate again later with the same key.
settings-deactivate-keep = Keep it
settings-deactivate-confirm = Deactivate

settings-privacy = Privacy
settings-telemetry = Send anonymous usage counts
settings-telemetry-desc = Off by default. Only counts are sent: app version, system, a rough number of items and error counts. Never photos, file names, places or your license.
settings-telemetry-preview = Show exactly what would be sent
settings-telemetry-preview-desc = This is the whole message, as it would be sent today.

settings-updates = Updates
settings-update-checks = Check for updates
settings-update-checks-desc = Updates install only when you agree, and never during a job.
settings-channel = Update channel
settings-channel-stable = Stable
settings-channel-beta = Beta (early versions)

settings-language-title = Language
settings-language = Language
settings-language-system = Same as the system
settings-language-en-us = English (United States)
settings-language-desc = Dates and numbers follow { $locale }. English is the only language so far.

settings-running-note = A job is running. Changes here apply to the next job.
settings-revert = Undo changes
settings-save = Save
settings-saved = Settings saved.

about-title = About { -product }
about-lede = Repairs your own Snapchat Memories export on this computer. Nothing leaves it.
about-version-title = Version
about-version = Version
about-publisher = Made by
about-publisher-name = NovaByteWorks
about-tools = Bundled tools
about-exiftool = ExifTool { $version }
about-tools-failed = The bundled tools didn't answer: { $message }
about-tools-unavailable = Not checked: the bundled tools run only inside the installed app.
about-legal-title = Legal
about-disclaimer = Not affiliated with, endorsed by or sponsored by Snap Inc. Snapchat, Memories and My Eyes Only are trademarks of Snap Inc., used only to describe compatibility.
about-ffmpeg = Videos are processed with FFmpeg, licensed under the LGPL. It runs as a separate program that you can replace, and its source code is available.
about-licenses = Licenses
about-notices = Third-party notices
about-ffmpeg-source = FFmpeg source code
about-privacy = Privacy policy
about-support-title = Support
about-support-body = Questions, problems or ideas: write to us. Exporting diagnostics from the results screen helps us help you, and holds no photos.
about-website = NovaByteWorks website
