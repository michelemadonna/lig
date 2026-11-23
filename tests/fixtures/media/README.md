# Media preview fixtures

These recordings were generated with FFmpeg 9 from its `testsrc2` and `sine` filters.

- `clip.ts` checks that Lig recognizes an MPEG transport stream with a `.ts` extension.
- `clip.webm` checks file metadata fallback and detection by signature for the configured MediaInfo backend.
- `tail.mp4` checks MP4 detection, extension-rule precedence and the file metadata fallback.
- `tracks.mkv` checks Matroska detection and the MediaInfo summary for multiple audio and subtitle tracks.
- `tone.flac` checks that audio files use the configured media category backend.
