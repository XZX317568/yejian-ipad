// Visual geometry adapted from the Wenku8 Flutter app v0.5.6
// (`.tools/wenku-v0.5.6/archive_design.dart`, `glass_dock.dart` and
// `motion_ui.dart`). The reference distribution retains its original licence
// records at `third_party/wenku8/LICENSE` (GNU GPL v2) and
// `third_party/wenku8/GPL-3.0.txt` (GNU GPL v3). Those full texts remain with
// the reference; no "or later" grant is asserted here. Only the measured
// proportions are kept here, so the app bar and the dock cannot drift apart.

/// The archive navigation's own measurements.
///
/// These are deliberately separate from [ShioriSpace] and [ShioriShape]: the
/// archive masthead and dock are a named visual language with its own fixed
/// proportions, not new steps in the shared spacing and radius ladders. A
/// different screen should still use the shared tokens.
abstract final class ShioriArchiveNav {
  /// The masthead is pinned at the reference's 56 px, even with large system
  /// text. Its content scales down inside that plane rather than growing the
  /// toolbar and pushing the shelf away.
  static const double toolbarHeight = 56;

  /// The Chinese product name's size and tight display tracking.
  static const double titleSize = 23;
  static const double titleHeight = 1.18;
  static const double titleTracking = -0.7;

  /// The small archive address under the name. Nine points is a mark, not a
  /// reading size, so it keeps its own fixed scalar.
  static const double codeSize = 9;
  static const double codeHeight = 1.35;
  static const double codeTracking = 1.05;

  /// Space between the name and its address.
  static const double titleGap = 3;

  /// The floating capsule is compact and hugs a four-destination bar rather
  /// than filling the width of a phone.
  static const double dockHeight = 54;
  static const double dockHorizontalMargin = 28;
  static const double dockMaxWidth = 460;
  static const double dockBottomGap = 8;

  /// The dock's vertical breathing room: the foreground row sits inside the
  /// capsule rather than touching its rounded caps.
  static const double dockVerticalPadding = 6;

  /// Icon and label sizes. The label shrinks to fit its lane instead of
  /// wrapping or clipping.
  static const double iconSize = 19;
  static const double labelSize = 10;

  /// The selected tab's short underline, and the gap that reserves its room
  /// in every lane so selection does not move the label.
  static const double underlineWidth = 12;
  static const double underlineHeight = 1.5;
  static const double underlineGap = 3;

  /// A touch compresses the item only enough to confirm contact.
  static const double pressScale = 0.97;

  /// How far the whole capsule steps back while the content is being read.
  static const double shrinkScale = 0.06;
  static const double shrinkTravel = 6;

  /// The glass is a thin milky layer over the page, not an opaque sheet.
  static const double glassBlur = 24;
  static const double glassHairline = 0.6;
  static const double glassShadowBlur = 18;
  static const double glassShadowOffset = 5;
}
