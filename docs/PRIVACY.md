# What MediaAnonymizer removes

This document is the reference for the three metadata policies and for the
extra clean-up performed when faces are redacted. The implementation lives in
[`crates/core/src/metadata`](../crates/core/src/metadata) (photos) and
[`crates/core/src/video/metadata.rs`](../crates/core/src/video/metadata.rs)
(videos). Keep this file in sync when changing the rules.

| Policy | CLI flag | GUI |
|---|---|---|
| `keep` | *(default)* | Keep metadata |
| `sensitive` | `--strip-sensitive` / `-s` | Remove sensitive (GPS & personal) |
| `all` | `--strip-metadata` / `-m` | Remove all metadata |

## Always kept

Blocks needed to display the media exactly as before are never removed:

* JPEG: `APP0 JFIF` header (its embedded thumbnail is removed whenever
  previews are), `APP2 ICC_PROFILE` color profile, `APP14 Adobe` color
  transform, all decoding tables.
* PNG: `iCCP`, `sRGB`, `gAMA`, `cHRM`, `cICP`, `mDCV`, `cLLI`, `sBIT`, `tRNS`,
  `bKGD`, `pHYs`, `sPLT`, `hIST`, and the APNG animation chunks.
* WebP: `VP8X` header, `ICCP` profile, image and animation chunks.
* Video: the audio, video and subtitle streams themselves, track language,
  disposition, display rotation (stream copy) and color description.

## Photos

| Data | `keep` | `keep` + faces | `sensitive` | `all` |
|---|:-:|:-:|:-:|:-:|
| EXIF camera make/model, lens, exposure, dates, software | kept | kept | kept | removed |
| EXIF GPS directory (location, altitude, direction, speed, time) | kept | kept | **removed** | removed |
| EXIF identity: Artist, Copyright, ImageDescription, UserComment, XP* tags, CameraOwnerName, Photographer, ImageEditor, HostComputer | kept | kept | **removed** | removed |
| EXIF device IDs: BodySerialNumber, LensSerialNumber, CameraSerialNumber, ImageUniqueID | kept | kept | **removed** | removed |
| EXIF MakerNote (vendor data: serials, face-detection boxes, previews) | kept | **removed** | **removed** | removed |
| EXIF thumbnail (IFD1) | kept | **removed** | **removed** | removed |
| XMP / Extended XMP (edit history, location, people regions, depth maps) | kept | **removed** | **removed** | removed |
| IPTC / Photoshop resources (APP13) | kept | thumbnails removed | **removed** | removed |
| JPEG comments (COM) | kept | kept | **removed** | removed |
| MPF / secondary images after the end of the image (depth maps, HDR gain maps, previews) | kept | **removed** | **removed** | removed |
| FlashPix, JFXX thumbnails, other vendor `APPn` segments | kept | **removed** | **removed** | removed |
| PNG text chunks (`tEXt`/`zTXt`/`iTXt`) | kept | XMP / raw profiles removed | removed except `Title`, `Software`, `Disclaimer`, `Warning` | removed |
| PNG `eXIf` | kept | as EXIF above | as EXIF above | removed |
| PNG `tIME` | kept | kept | kept | removed |
| Unknown ancillary PNG / WebP chunks (e.g. C2PA manifests, which embed thumbnails) | kept | **removed** | **removed** | removed |
| TIFF tags | — | all non-structural tags removed | all non-structural tags removed | all non-structural tags removed |

How EXIF is edited: removed entries are taken out of their directory and
**every byte that belonged to them is overwritten with zeros**; the directory
structure is compacted in place so that offsets used by the remaining tags stay
valid. If an EXIF block cannot be parsed it is removed entirely.

Why previews are removed with faces, even with `keep`: thumbnails, maker-note
previews, XMP-embedded images and multi-picture secondary images are separate
pictures of the same scene. Redacting the main image and leaving them would
leak the faces.

## Videos

| Data | `keep` | `keep` + faces | `sensitive` | `all` |
|---|:-:|:-:|:-:|:-:|
| Container tags such as `title`, `creation_time`, `make`, `model`, `software` | kept | kept | kept | removed |
| Location tags (`location`, `location-eng`, `com.apple.quicktime.location.ISO6709`, `©xyz`, …) | kept | kept | **removed** | removed |
| Identity tags (`artist`, `author`, `composer`, `copyright`, `comment`, `description`, `keywords`, owner, serial and unique identifiers, …) | kept | kept | **removed** | removed |
| Per-stream tags (same rules); track `language` | kept | kept | filtered | language only |
| Chapters | kept | kept | kept | removed |
| Data streams: GPS/IMU telemetry (GoPro `gpmd`, Apple `mebx`, …) | kept | kept | **removed** | removed |
| Timecode tracks (`tmcd`) | kept | kept | kept | removed |
| Cover art (attached pictures) | kept | **removed** | kept | removed |
| Attachments (fonts…) | kept | kept | kept | removed |
| Additional video streams | kept | **removed** (not redacted) | kept | kept |
| Muxer `encoder` tag and creation time | rewritten | rewritten | rewritten | not written (`+bitexact`) |

Tags are matched case-insensitively by substring: `location`, `gps`, `xyz`,
`geo`, `artist`, `author`, `composer`, `performer`, `copyright`, `comment`,
`description`, `synopsis`, `keywords`, `owner`, `serial`, `identifier`,
`uuid`, `user`, `email`, `phone`, `address`, `lyrics`, `encoded_by`,
`publisher`, `director`, `producer`.

## Face redaction

* Detection runs on the upright image (EXIF orientation, video display
  rotation) with the [YuNet](https://github.com/ShiqiYu/libfacedetection)
  detector, which also locates the eyes, used to align the eyes-only band and
  the full-face ellipse with the head tilt.
* Regions are padded (`--padding`, default 15 % of the face size) and the
  full-face region is extended upwards to include the forehead.
* Video: each frame is also covered by the detections of the frames within
  `±persistence` (default 3), so short detection drop-outs do not leak a face.
* **Strength**: blur radius is 18 % of the face size, the mosaic has about 7
  blocks across the face. `--strength` scales both.

### Faces left visible

Every detected face is covered unless you mark it to stay visible (for
example a public official on duty, a speaker who consented, or yourself).
There is deliberately no automatic rule such as "leave police officers
visible": telling who someone is from a picture is unreliable, a mistake
exposes a member of the public, and the law has exceptions (in France, for
instance, members of specialised units must be anonymised). The decision
stays with the person publishing.

* **Photos**: a mark is a point on the face; the face containing it is left
  uncovered. A mark on no face changes nothing.
* **Videos**: the marked face is followed from the marked frame on, by its
  overlap with its position on the previous frame. When it is not found for
  `max(12, 2 × detect-every)` frames the track ends and the face is covered
  again; a track never jumps to a distant face. Frames before the mark, and
  the first `persistence` frames after it, stay covered.
* Review the result: in the desktop app, scrub through the video and check
  the green strip under the timeline, which shows where each marked face is
  left visible.

## Timestamp overlay

`--timestamp` deliberately *adds* information: the capture date becomes part
of the pixels and can no longer be removed by stripping metadata. Leave it off
when the date itself is sensitive. When no date is recorded, the file
modification time is used (and reported as such), which may not be the
capture date.

### Limitations

* Detectors miss some faces (strong profiles, tiny faces, heavy occlusion,
  unusual lighting). Always review sensitive material. Lower
  `--min-confidence` and increase `--detection-size` to be more aggressive.
* Blur and pixelation reduce information but are not cryptographic: for
  high-risk use cases use the black band.
* Faces are not the only identifying feature: tattoos, clothes, license
  plates, reflections, voices and backgrounds are left untouched.
* File-system metadata (names, timestamps, extended attributes) is not part
  of the file content and is not changed.
