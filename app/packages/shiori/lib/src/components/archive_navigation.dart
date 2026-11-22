// The masthead and dock proportions are adapted from the Wenku8 Flutter app
// v0.5.6 (`archive_design.dart`, `glass_dock.dart`, `motion_ui.dart`). The
// reference distribution retains its original licence records at
// `third_party/wenku8/LICENSE` (GNU GPL v2) and
// `third_party/wenku8/GPL-3.0.txt` (GNU GPL v3); those full texts remain with
// the reference, and no "or later" grant is asserted here. See
// `archive_navigation_tokens.dart` for the measured values.

import 'dart:ui' as ui;

import 'package:material_ui/material_ui.dart';

import '../archive_navigation_tokens.dart';
import '../tokens.dart';

/// The name and archive address at the top of the shelf.
///
/// The reference keeps a fixed 56 px plane: the name is 23 px and heavy, the
/// address below it is a 9 px code. [FittedBox] lets large system text shrink
/// inside that plane instead of changing the toolbar's height. The address is
/// decorative, so a screen reader hears only the name.
class ShioriArchiveMasthead extends StatelessWidget {
  const ShioriArchiveMasthead({
    super.key,
    required this.title,
    required this.code,
  });

  /// The height the host's app bar must reserve for this masthead.
  static const double height = ShioriArchiveNav.toolbarHeight;

  final String title;
  final String code;

  @override
  Widget build(BuildContext context) {
    final c = context.shiori.color;
    return Semantics(
      header: true,
      label: title,
      child: ExcludeSemantics(
        child: FittedBox(
          fit: BoxFit.scaleDown,
          alignment: Alignment.centerLeft,
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(
                title,
                maxLines: 1,
                style: TextStyle(
                  color: c.fg,
                  fontSize: ShioriArchiveNav.titleSize,
                  height: ShioriArchiveNav.titleHeight,
                  fontWeight: FontWeight.w800,
                  letterSpacing: ShioriArchiveNav.titleTracking,
                ),
              ),
              const SizedBox(height: ShioriArchiveNav.titleGap),
              Text(
                code,
                maxLines: 1,
                textScaler: TextScaler.noScaling,
                style: TextStyle(
                  // The reference sets the file code in a terminal face.
                  // shiori: the archive address is a fixed-width registration
                  // mark, not body text.
                  fontFamily: 'monospace',
                  color: c.fgMuted,
                  fontSize: ShioriArchiveNav.codeSize,
                  height: ShioriArchiveNav.codeHeight,
                  fontWeight: FontWeight.w500,
                  letterSpacing: ShioriArchiveNav.codeTracking,
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

/// A narrow milky-glass capsule with an underline under the selected tab.
///
/// The destinations are the app's own [NavigationDestination]s, so labels,
/// selected glyphs and update badges keep working. Selection moves no pill:
/// each lane owns its short underline, which grows in place. Pressing an item
/// compresses only that item; both transitions stop at their endpoints, and
/// neither starts a ticker while the bar is at rest.
class ShioriArchiveDock extends StatefulWidget {
  const ShioriArchiveDock({
    super.key,
    required this.destinations,
    required this.currentIndex,
    required this.onSelected,
    this.shrink = 0,
    this.onWake,
    this.floating = true,
  });

  final List<NavigationDestination> destinations;
  final int currentIndex;
  final ValueChanged<int> onSelected;

  /// 0 at rest, 1 while the page underneath is being scrolled down.
  final double shrink;

  /// Any contact with the capsule brings it back to rest.
  final VoidCallback? onWake;

  /// A floating dock sits over the page and blurs what is behind it. A docked
  /// one can skip the blur, where there is only the scaffold's ground.
  final bool floating;

  @override
  State<ShioriArchiveDock> createState() => _ShioriArchiveDockState();
}

class _ShioriArchiveDockState extends State<ShioriArchiveDock> {
  int? _pressed;

  void _setPressed(int? index) {
    if (_pressed == index) return;
    setState(() => _pressed = index);
  }

  @override
  Widget build(BuildContext context) {
    final c = context.shiori.color;
    final light = Theme.of(context).brightness == Brightness.light;
    final safeBottom = MediaQuery.paddingOf(context).bottom;
    // Six or more lanes need the extra width more than the edge margin does.
    final sideMargin = widget.destinations.length > 5
        ? ShioriSpace.s16
        : ShioriArchiveNav.dockHorizontalMargin;
    final bottom = widget.floating
        ? (safeBottom - 12 < ShioriArchiveNav.dockBottomGap
              ? ShioriArchiveNav.dockBottomGap
              : safeBottom - 12)
        : safeBottom + ShioriArchiveNav.dockBottomGap;

    final milk = light
        ? Color.alphaBlend(Colors.white.withValues(alpha: 0.76), c.ink1)
        : c.ink1.withValues(alpha: 0.88);

    Widget capsule = SizedBox(
      height: ShioriArchiveNav.dockHeight,
      child: DecoratedBox(
        decoration: BoxDecoration(
          borderRadius: BorderRadius.circular(ShioriArchiveNav.dockHeight / 2),
          boxShadow: [
            BoxShadow(
              color: ShioriScrim.shadow.withValues(alpha: light ? 0.10 : 0.28),
              blurRadius: ShioriArchiveNav.glassShadowBlur,
              offset: const Offset(0, ShioriArchiveNav.glassShadowOffset),
            ),
          ],
        ),
        child: ClipRRect(
          borderRadius: BorderRadius.circular(ShioriArchiveNav.dockHeight / 2),
          child: BackdropFilter(
            filter: ui.ImageFilter.blur(
              sigmaX: widget.floating ? ShioriArchiveNav.glassBlur : 0,
              sigmaY: widget.floating ? ShioriArchiveNav.glassBlur : 0,
            ),
            child: DecoratedBox(
              decoration: BoxDecoration(
                color: milk,
                border: Border.all(
                  color: c.hairline,
                  width: ShioriArchiveNav.glassHairline,
                ),
                borderRadius: BorderRadius.circular(
                  ShioriArchiveNav.dockHeight / 2,
                ),
              ),
              child: Padding(
                padding: const EdgeInsets.symmetric(
                  vertical: ShioriArchiveNav.dockVerticalPadding,
                  horizontal: ShioriSpace.s4,
                ),
                child: Row(
                  children: [
                    for (var i = 0; i < widget.destinations.length; i++)
                      Expanded(
                        child: _ArchiveDockItem(
                          destination: widget.destinations[i],
                          selected:
                              i ==
                              widget.currentIndex.clamp(
                                0,
                                widget.destinations.length - 1,
                              ),
                          pressed: _pressed == i,
                          onPressStart: () => _setPressed(i),
                          onPressEnd: () => _setPressed(null),
                          onTap: () {
                            widget.onSelected(i);
                          },
                        ),
                      ),
                  ],
                ),
              ),
            ),
          ),
        ),
      ),
    );

    // Kept mounted even at rest so a released scroll animates the capsule
    // back, rather than swapping the transform out and snapping it home.
    capsule = TweenAnimationBuilder<double>(
      tween: Tween<double>(end: widget.shrink.clamp(0.0, 1.0)),
      duration: ShioriMotion.of(
        context,
        widget.shrink > 0 ? ShioriMotion.exitSlow : ShioriMotion.standard,
      ),
      curve: ShioriMotion.exitCurve,
      builder: (context, value, child) => Transform.translate(
        offset: Offset(0, value * ShioriArchiveNav.shrinkTravel),
        child: Transform.scale(
          scale: 1 - value * ShioriArchiveNav.shrinkScale,
          alignment: Alignment.bottomCenter,
          child: child,
        ),
      ),
      child: capsule,
    );

    return Padding(
      padding: EdgeInsets.fromLTRB(sideMargin, 0, sideMargin, bottom),
      child: Center(
        heightFactor: 1,
        child: ConstrainedBox(
          constraints: const BoxConstraints(
            maxWidth: ShioriArchiveNav.dockMaxWidth,
          ),
          child: Listener(
            onPointerDown: (_) => widget.onWake?.call(),
            behavior: HitTestBehavior.translucent,
            child: capsule,
          ),
        ),
      ),
    );
  }
}

class _ArchiveDockItem extends StatelessWidget {
  const _ArchiveDockItem({
    required this.destination,
    required this.selected,
    required this.pressed,
    required this.onPressStart,
    required this.onPressEnd,
    required this.onTap,
  });

  final NavigationDestination destination;
  final bool selected;
  final bool pressed;
  final VoidCallback onPressStart;
  final VoidCallback onPressEnd;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final c = context.shiori.color;
    final color = selected ? c.fg : c.fgMuted;
    final label = destination.label;
    final semanticLabel =
        destination.tooltip != null && destination.tooltip!.isNotEmpty
        ? destination.tooltip!
        : label;
    final duration = ShioriMotion.of(context, ShioriMotion.fast);

    return Semantics(
      button: true,
      enabled: destination.enabled,
      selected: selected,
      label: semanticLabel,
      onTap: destination.enabled ? onTap : null,
      child: GestureDetector(
        behavior: HitTestBehavior.opaque,
        onTapDown: destination.enabled ? (_) => onPressStart() : null,
        onTapUp: destination.enabled ? (_) => onPressEnd() : null,
        onTapCancel: destination.enabled ? onPressEnd : null,
        onTap: destination.enabled ? onTap : null,
        child: AnimatedScale(
          scale: pressed ? ShioriArchiveNav.pressScale : 1,
          duration: duration,
          curve: ShioriMotion.standardCurve,
          child: Column(
            mainAxisAlignment: MainAxisAlignment.center,
            mainAxisSize: MainAxisSize.min,
            children: [
              IconTheme(
                data: IconThemeData(
                  size: ShioriArchiveNav.iconSize,
                  color: color,
                ),
                child: selected
                    ? (destination.selectedIcon ?? destination.icon)
                    : destination.icon,
              ),
              const SizedBox(height: ShioriSpace.s4),
              Flexible(
                child: FittedBox(
                  fit: BoxFit.scaleDown,
                  child: AnimatedDefaultTextStyle(
                    duration: duration,
                    curve: ShioriMotion.standardCurve,
                    style: context.shiori.text.meta.copyWith(
                      color: color,
                      fontSize: ShioriArchiveNav.labelSize,
                      height: 1.15,
                      fontWeight: selected ? FontWeight.w700 : FontWeight.w500,
                    ),
                    child: Text(
                      label,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                    ),
                  ),
                ),
              ),
              const SizedBox(height: ShioriArchiveNav.underlineGap),
              SizedBox(
                height: ShioriArchiveNav.underlineHeight,
                child: AnimatedContainer(
                  duration: duration,
                  curve: ShioriMotion.standardCurve,
                  width: selected ? ShioriArchiveNav.underlineWidth : 0,
                  height: ShioriArchiveNav.underlineHeight,
                  decoration: BoxDecoration(
                    color: selected ? c.fg : Colors.transparent,
                    borderRadius: BorderRadius.circular(
                      ShioriArchiveNav.underlineHeight,
                    ),
                  ),
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}
