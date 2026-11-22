# Wenku8 v0.5.6 physical shelf

The physical shelf is adapted from the public `mikufoxxx/Wenku8_Flutter`
v0.5.6 source, retrieved on 2026-11-17. Original source:
`https://github.com/mikufoxxx/Wenku8_Flutter/tree/v0.5.6/lib`

Original copyright and license texts are retained verbatim in
`third_party/wenku8/{LICENSE,NOTICE,GPL-3.0.txt}`. Those notices apply to
the adapted files. Other Shiori and application components retain their
existing notices. No Wenku8 novel text, covers, branding or fonts are bundled.

| Local file | Upstream source |
| --- | --- |
| `physical_reading_shelf.dart`, `archive_shelf/shelf_scene.dart` | `archive_wave.dart`, `archive_spine_title.dart`, `archive_design.dart` |
| `archive_shelf/archive_shelf_geometry.dart` | same filename |
| `archive_shelf/archive_scroll_simulation.dart` | same filename |
| `archive_shelf/archive_shelf_loading.dart` | same filename |
| `archive_shelf/archive_motion.dart` | same filename |
| `archive_shelf/paper_cover.dart` | paper-flight classes in `motion_ui.dart` and Hero endpoint in `app_ui.dart` |
| `archive_navigation_tokens.dart`, `components/archive_navigation.dart` | masthead/dock proportions in `archive_design.dart`, `glass_dock.dart`, `motion_ui.dart` |
| `archive_shelf/paper_page.dart` | Cupertino page route durations in `app_ui.dart` |

Changes by the Yejian project, 2026-11-17: replace the novel model with
`ShioriReadingEntry`; connect existing image providers, manga navigation,
long-press selection and history; integrate Shiori colors and material_ui;
keep the caller's selected identity when the collection reorders; keep only
nearby cover images decoded and only the focused copy participating in Hero.
The scene now builds only the visible neighbourhood, while preserving the
original logical lane coordinates, paint order and cover occlusion. A 1,000
entry fixture checks bounded construction, loop seams and correct open actions.
Selection mode uses the library's real selection state and paints checkmarks.
Original notices are bundled as Shiori assets and registered with the app's
existing open-source license screen.

Preserved motion: book projection (.66, -.30), spine thickness and pitch,
cosine-squared extraction wave (radius 1.48), 520 ms ink arrival, 420 ms
commanded selection, inertial coast (drag rate 4.4), smooth detent landing,
continuous reverse path, physical paint/hit-test order and cover peel mesh.
Reduced motion bypasses finite entrance and commanded-selection animations.

Reference release packages were not run on a device during this adaptation.
Matching source geometry/physics does not establish device frame pacing or
pixel identity across font/platform rendering. Acceptance evidence is kept
in the project root `IMPLEMENTATION.md`.
