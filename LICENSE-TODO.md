# License: not decided yet

No license has been chosen for this repository, so by default all rights are reserved.

Decision owner: Adam Goodman (NovaByteWorks). Decide before the first public release (M5, SRS §13).

Inputs to the decision:

- The app ships proprietary builds sold through Gumroad (SRS §9), so the application code need not be open source.
- The bundled ffmpeg must be an LGPL build and its build script and source commit must be published under `third_party/ffmpeg/` (SRS §10.4, NFR-LEG-02). That part has to carry a license compatible with the LGPL offer regardless of what the rest of the repo does.
- `tools/make_fixture` and the fixtures it produces contain no user data and could be released permissively if the test corpus is ever shared with volunteers (SRS §11.3).

When decided: add `LICENSE`, set `license` or `license-file` in every `Cargo.toml` and in `package.json`, and delete this file.
